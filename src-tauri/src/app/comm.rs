// ─ K2K Packet Desktop — app display & send helpers ────────────
//
// Seam 3 (thin commands): pure display-formatting and AX.25 send-guard
// helpers moved out of the commands.rs god-file. These are self-contained
// and unit-testable with no Tauri/radio dependency — the first slice of
// decomposing commands.rs into thin delegates + a testable app layer.

use crate::agw;
use crate::kernel::{ax25, output::KernelOutput, session};
use tokio::net::TcpStream;

/// Open a TCP transport to a TNC and configure TCP keepalive, returning the
/// split reader/writer halves. Shared by the kiss_connect / agw_connect
/// commands (which then load their own timing and build the client).
pub async fn open_tcp_io(
    addr: &str,
) -> Result<
    (
        tokio::net::tcp::OwnedReadHalf,
        tokio::net::tcp::OwnedWriteHalf,
    ),
    String,
> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;
    // Configure TCP keepalive so dead links are detected in ~25 seconds
    // instead of the OS default of 2 hours.
    session::configure_keepalive(&stream);
    Ok(stream.into_split())
}

/// Open a TCP transport and load the KISS timing settings from disk.
/// Returns (reader, writer, timing). Used by the kiss_connect command.
pub async fn open_tcp_stream(
    addr: &str,
    out: &dyn KernelOutput,
) -> Result<
    (
        tokio::net::tcp::OwnedReadHalf,
        tokio::net::tcp::OwnedWriteHalf,
        session::KissTiming,
    ),
    String,
> {
    let (reader, writer) = open_tcp_io(addr).await?;
    // Load timing + TNC parameter settings from disk
    let timing = session::KissTiming::from_settings_or_default(out);
    Ok((reader, writer, timing))
}

/// Build the linpac-style radio header line for a sent frame:
/// `"radio: fm SRC to DST [via ... ]ctl LABEL [pid] len N"`.
/// Pass `pid: ""` for control frames that carry no PID byte.
pub fn format_radio_header(
    src: &str,
    dest: &str,
    via: &str,
    ctl: &str,
    pid: &str,
    len: usize,
) -> String {
    if pid.is_empty() {
        format!(
            "radio: fm {} to {} {}ctl {} len {}",
            src, dest, via, ctl, len
        )
    } else {
        format!(
            "radio: fm {} to {} {}ctl {} {} len {}",
            src, dest, via, ctl, pid, len
        )
    }
}

/// Emit a frame we just sent to the unproto monitor panel: builds the
/// radio header, raw-KISS hex, and "SRC>DST: body" line, then fires the
/// event with sent=true. `body` is the payload text ("" for control
/// frames, which render as "[CTL]" in the panel).
pub fn emit_sent_frame(
    app_handle: &dyn crate::kernel::output::KernelOutput,
    my_call: &str,
    my_ssid: u8,
    remote: &str,
    remote_ssid: u8,
    digis: &[String],
    ctl_label: &str,
    pid_label: &str,
    body: &str,
    frame_type: &str,
    len: usize,
    frame_bytes: &[u8],
) {
    let src_label = ax25::fmt_call(my_call, my_ssid);
    let dest_label = ax25::fmt_call(remote, remote_ssid);
    let via_part = ax25::format_via(digis);
    let radio_header = format_radio_header(
        &src_label,
        &dest_label,
        &via_part,
        ctl_label,
        pid_label,
        len,
    );
    let raw_kiss = ax25::fmt_raw_kiss(frame_bytes);
    let formatted = if body.is_empty() {
        format!("{}>{}: [{}]", src_label, dest_label, ctl_label)
    } else {
        format!("{}>{}: {}", src_label, dest_label, body)
    };
    crate::session::emit_unproto_frame(
        app_handle,
        &src_label,
        &dest_label,
        body,
        &formatted,
        frame_type,
        digis,
        &radio_header,
        &raw_kiss,
        true,
    );
}

/// Apply the send guards and pull the state an I-frame needs: the session
/// must be Connected or TimerRecovery, the remote not RNR-busy, and the
/// window=1 slot free (no unacked frame in flight).
pub fn session_send_state(
    session: &mut session::Ax25Session,
    app_handle: &dyn crate::kernel::output::KernelOutput,
) -> Result<(String, u8, Vec<String>, u8, u8), String> {
    // Kernel allows transmission in state 3 (Connected) and state 4
    // (Timer Recovery) — block everywhere else.
    if session.state != session::Ax25ConnState::Connected
        && session.state != session::Ax25ConnState::TimerRecovery
    {
        return Err("Not connected".to_string());
    }
    // Check if remote is busy (RNR received)
    if session.remote_busy {
        let _ = app_handle.emit(
            "system-message",
            serde_json::json!({
                "text": "Remote is busy (RNR) — waiting..."
            }),
        );
        return Err("Remote busy".to_string());
    }
    // Window=1 guard: only one outstanding I-frame is tracked. A second
    // send while pending would reuse N(S) (after a REJ reset) and the
    // retransmit would carry the wrong text. Kernel queues (window=2);
    // we block instead to avoid silent data loss.
    if session.pending_ns.is_some() {
        let _ = app_handle.emit(
            "system-message",
            serde_json::json!({
                "text": "Previous frame not yet acknowledged — waiting..."
            }),
        );
        return Err("Frame in flight".to_string());
    }
    Ok((
        session.remote.clone(),
        session.remote_ssid,
        session.digis.clone(),
        session.ns,
        session.nr,
    ))
}

/// Advance N(S) and record the frame just sent as pending for
/// retransmission. Only the oldest unacknowledged frame is tracked — if
/// one is already pending we don't overwrite it; the retry task resends
/// the oldest and the remote's N(R) ACKs everything up to that point
/// (AX.25 modulo-8 sequencing).
pub fn store_pending_iframe(session: &mut session::Ax25Session, text: &str, sent_ns: u8) {
    session.ns = (session.ns + 1) & 0x07;
    if session.pending_ns.is_none() {
        session.pending_text = text.to_string();
        session.pending_ns = Some(sent_ns);
    }
    session.last_send_time = Some(std::time::Instant::now());
}

/// Frame-type fallback when the verbose header is absent: pull the
/// "ctl <TYPE>" token out of the AGW frame's first line.
pub fn fallback_frame_type(frame: &agw::AgwFrame) -> String {
    let agw_header = String::from_utf8_lossy(&frame.data);
    let fl = agw_header.lines().next().unwrap_or("");
    if let Some(ctl_pos) = fl.find("ctl ") {
        let rest = &fl[ctl_pos + 4..];
        rest.split_whitespace().next().unwrap_or("S").to_uppercase()
    } else {
        "S".to_string()
    }
}

// ── AGW monitor / verbose-line parsing (pure) ─────────────
/// Strip the AGWPE line-number prefix: "1:Fm ..." → "Fm ...".
/// Returns the trimmed line unchanged when there is no such prefix.
pub fn strip_agw_line_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    if let Some(colon) = trimmed.find(':') {
        if colon > 0
            && colon <= 3
            && trimmed[..colon].chars().all(|c| c.is_ascii_digit())
            && trimmed[colon + 1..].trim_start().starts_with("Fm ")
        {
            trimmed[colon + 1..].trim_start()
        } else {
            trimmed
        }
    } else {
        trimmed
    }
}

/// Parse the digipeater path from a verbose line: "Fm CALL To DEST Via CALL1,... <UI ...>".
/// Returns "via CALL1 CALL2" or "" when there is no via section.
pub fn parse_verbose_via(clean_first: &str) -> String {
    if let Some(via_start) = clean_first.find(" Via ") {
        let after = &clean_first[via_start + 5..];
        if let Some(end) = after.find(" <") {
            format!("via {}", &after[..end])
        } else if let Some(end) = after.find(" [") {
            format!("via {}", &after[..end])
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

/// Parse the PID token from a verbose line: "pid=xx" → "XX".
pub fn parse_verbose_pid(clean_first: &str) -> String {
    if let Some(pid_start) = clean_first.find("pid=") {
        let after = &clean_first[pid_start + 4..];
        after
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_uppercase()
    } else {
        String::new()
    }
}

/// Verbose frame type, full form: folds the P/F token (and I-frame N(S)/N(R))
/// into the display label like the KISS path — "UI+" / "UI-" / "I00+".
/// Direwolf format: "<UI pid=F0 Len=4 P=1 >" / "<I S0 R0 pid=F0 Len=4 P=1 >".
pub fn parse_verbose_frame_type(clean_first: &str) -> String {
    if let Some(lt_start) = clean_first.find(" <") {
        let after = &clean_first[lt_start + 2..];
        let toks: Vec<&str> = after.split_whitespace().collect();
        let t = toks.first().copied().unwrap_or("");
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_uppercase()) {
            let mut ft = t.to_string();
            if ft == "I" {
                // I-frame: "<I S0 R0 pid=... P=1 >" → "I00+"
                let mut ns = String::new();
                let mut nr = String::new();
                for tok in toks.iter().skip(1) {
                    if let Some(v) = tok.strip_prefix('S') {
                        ns = v.to_string();
                    } else if let Some(v) = tok.strip_prefix('R') {
                        nr = v.to_string();
                    } else if let Some(v) =
                        tok.strip_prefix("P=").or_else(|| tok.strip_prefix("F="))
                    {
                        ft = format!("I{}{}{}", ns, nr, if v == "1" { "+" } else { "-" });
                        break;
                    }
                }
            } else {
                for tok in toks.iter().skip(1) {
                    if let Some(v) = tok.strip_prefix("P=").or_else(|| tok.strip_prefix("F=")) {
                        ft.push_str(if v == "1" { "+" } else { "-" });
                        break;
                    }
                }
            }
            ft
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

/// Split AGWPE monitor data after its verbose header line: first after the
/// "Fm ..." line, else after the first newline. Returns None when the data
/// contains no newline at all (caller applies its own fallback).
pub fn split_after_verbose_header(data: &[u8]) -> Option<Vec<u8>> {
    // Strategy 1: Find "Fm" in the data and find the first \n or \r after it
    let after_fm = data.windows(3).position(|w| w == b"Fm ");
    let split_pos = after_fm.and_then(|fm_pos| {
        // Start searching for newline after the end of "Fm ..." line
        let search_start = fm_pos + 3;
        data[search_start..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
            .map(|p| search_start + p)
    });
    // Fallback: first newline (may include verbose header if no Fm found)
    let pos = split_pos.or_else(|| data.iter().position(|&b| b == b'\n' || b == b'\r'))?;
    let mut start = pos + 1;
    // Skip \n after \r (CRLF)
    if start < data.len() && data[start] == b'\n' {
        start += 1;
    }
    Some(
        data[start..]
            .iter()
            .take_while(|&&b| b != 0)
            .copied()
            .collect(),
    )
}

/// Map the verbose PID token back to a byte; falls back to the AGW header
/// PID when the verbose line carried no pid= token.
pub fn actual_pid(verbose_pid: &str, header_pid: u8) -> u8 {
    if verbose_pid == "F0" {
        0xF0
    } else if verbose_pid == "CF" {
        0xCF
    } else {
        header_pid
    }
}

/// Build the display triple for a monitored/own payload: NET/ROM gets its
/// decoded text (or a hex dump), text PID gets "(Text)" labeling, anything
/// else shows the raw hex PID. `payload` feeds NET/ROM detection (raw
/// bytes — the monitor arm keeps these unstripped), `display` is what the
/// text branches render (the monitor arm strips line-number prefixes here).
pub fn format_monitor_payload(
    payload: &[u8],
    display: &[u8],
    actual_pid_byte: u8,
    fallback_ft: &str,
) -> (String, String, String) {
    let is_netrom = actual_pid_byte == 0xCF || payload.first() == Some(&0xFF);
    if is_netrom {
        if let Some((netrom_display, _suffix)) = crate::netrom::parse_netrom_frame(payload) {
            (
                netrom_display,
                "CF(NET/ROM)".to_string(),
                "NET/ROM".to_string(),
            )
        } else {
            let hex: String = ax25::fmt_raw_kiss(payload);
            (
                format!("[NET/ROM] {}", hex),
                "CF(NET/ROM)".to_string(),
                "NET/ROM".to_string(),
            )
        }
    } else if actual_pid_byte == 0xF0 {
        (
            String::from_utf8_lossy(display).trim().to_string(),
            "F0(Text)".to_string(),
            fallback_ft.to_string(),
        )
    } else {
        (
            String::from_utf8_lossy(display).trim().to_string(),
            format!("{:02X}", actual_pid_byte),
            fallback_ft.to_string(),
        )
    }
}

/// Radio header line for AGW monitor echoes (via optional).
pub fn agw_radio_header(
    src_label: &str,
    dst_label: &str,
    via: &str,
    frame_type: &str,
    pid_label: &str,
    len: usize,
) -> String {
    if via.is_empty() {
        format!(
            "radio: {} to {} ctl {} pid={} len {}",
            src_label, dst_label, frame_type, pid_label, len
        )
    } else {
        format!(
            "radio: {} to {} {} ctl {} pid={} len {}",
            src_label, dst_label, via, frame_type, pid_label, len
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_via_empty_for_direct_path() {
        assert_eq!(ax25::format_via(&[]), "");
    }

    #[test]
    fn format_via_upperand_spaced() {
        let digis = vec!["kjohn".to_string(), "KBANN".to_string()];
        assert_eq!(ax25::format_via(&digis), "via KJOHN KBANN ");
    }

    #[test]
    fn radio_header_no_pid_for_control() {
        let h = format_radio_header("KK6SEN-0", "COOL", "via KBANN ", "SABM", "", 0);
        assert_eq!(h, "radio: fm KK6SEN-0 to COOL via KBANN ctl SABM len 0");
    }

    #[test]
    fn radio_header_with_pid() {
        let h = format_radio_header("KK6SEN-0", "COOL", "", "I", "F0", 5);
        assert_eq!(h, "radio: fm KK6SEN-0 to COOL ctl I F0 len 5");
    }

    #[test]
    fn session_send_state_rejects_awaiting_connect() {
        let mut s = session::Ax25Session::new(0);
        s.state = session::Ax25ConnState::AwaitingConnect;
        assert_eq!(
            session_send_state(&mut s, &crate::kernel::output::NullOutput),
            Err("Not connected".to_string())
        );
    }

    #[test]
    fn store_pending_advances_ns_only_first() {
        let mut s = session::Ax25Session::new(0);
        store_pending_iframe(&mut s, "hello", 0);
        assert_eq!(s.ns, 1);
        assert_eq!(s.pending_ns, Some(0));
        store_pending_iframe(&mut s, "second", 1);
        assert_eq!(s.ns, 2);
        // oldest kept
        assert_eq!(s.pending_ns, Some(0));
        assert_eq!(s.pending_text, "hello");
    }

    #[test]
    fn strip_agw_line_prefix_removes_line_number() {
        assert_eq!(strip_agw_line_prefix("1:Fm COOL To KK6SEN <I> nope"), "Fm COOL To KK6SEN <I> nope");
        assert_eq!(strip_agw_line_prefix("Fm COOL no prefix"), "Fm COOL no prefix");
    }

    #[test]
    fn parse_verbose_via_extracts_path() {
        let l = "Fm COOL To KK6SEN Via KBANN,KJOHN <UI pid=F0 Len=4 P=1 >";
        assert_eq!(parse_verbose_via(l), "via KBANN,KJOHN");
        // no via
        let direct = "Fm COOL To KK6SEN <UI pid=F0 Len=4 >";
        assert_eq!(parse_verbose_via(direct), "");
    }

    #[test]
    fn parse_verbose_frame_type_folds_ui_pbit() {
        let l = "Fm COOL To KK6SEN <UI pid=F0 Len=4 P=1 >";
        assert_eq!(parse_verbose_frame_type(l), "UI+");
        // no frame token -> empty
        assert_eq!(parse_verbose_frame_type("Fm COOL To KK6SEN"), "");
    }

    #[test]
    fn actual_pid_maps_text_and_cf() {
        assert_eq!(actual_pid("F0", 0x00), 0xF0);
        assert_eq!(actual_pid("CF", 0x00), 0xCF);
        // fallback to header pid when verbose pid unknown
        assert_eq!(actual_pid("xx", 0xAA), 0xAA);
    }

    #[test]
    fn agw_radio_header_no_via_and_with_via() {
        assert_eq!(agw_radio_header("KK6SEN-0", "COOL", "", "UI", "F0(Text)", 4),
                   "radio: KK6SEN-0 to COOL ctl UI pid=F0(Text) len 4");
        assert_eq!(agw_radio_header("KK6SEN-0", "COOL", "via KBANN", "I", "F0", 3),
                   "radio: KK6SEN-0 to COOL via KBANN ctl I pid=F0 len 3");
    }
}