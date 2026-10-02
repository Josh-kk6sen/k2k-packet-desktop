// ── AX.25 Protocol Layer ──────────────────────────────────
//
// AX.25 (Amateur X.25) is the link-layer protocol used in amateur
// packet radio. This module handles:
//   - Address encoding/decoding (callsigns, SSIDs, digipeaters)
//   - Control field parsing (I-frames, S-frames, U-frames)
//   - Frame building (SABM, UA, DISC, I, UI, RR)
//   - Display formatting (linpac-style radio headers)
//
// AX.25 2.2 spec reference: http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf

// ── AX.25 Control Field Constants ─────────────────────────
//
// Control field format (8 bits):
//   I-frame:  [N(R) 3b] [P] [N(S) 3b] [0]     — bit 0 = 0
//   S-frame:  [N(R) 3b] [P/F] [SS 2b] [0 1]   — bits 0-1 = 01
//   U-frame:  [MMM 3b]  [P/F] [MMM 3b] [1 1]  — bits 0-1 = 11

pub const SABM_P: u8 = 0x3F;  // Set Asynchronous Balanced Mode, P-bit set (initiate connect)
pub const UA: u8 = 0x73;      // Unnumbered Acknowledgment, F-bit set (accept connect)
pub const DISC: u8 = 0x53;    // Disconnect, P-bit set (request disconnect)
pub const UI_P: u8 = 0x13;    // Unnumbered Information, P-bit set (connectionless data)

// P/F-cleared control values for comparing parsed frames. The state
// machine masks bit 4 (P/F) then compares — these named forms keep the
// magic numbers (0x2F/0x43/0x63/0x0F) in one place.
pub const SABM_CTL: u8 = SABM_P & 0xEF;  // SABM with P/F ignored
pub const UA_CTL: u8 = UA & 0xEF;        // UA with P/F ignored
pub const DISC_CTL: u8 = DISC & 0xEF;    // DISC with P/F ignored
pub const DM_CTL: u8 = 0x0F;             // DM with P/F ignored

/// True when the control byte is an I-frame (bit 0 clear).
pub fn is_iframe_ctl(c: u8) -> bool { (c & 0x01) == 0 }
/// True when the control byte is a UI frame (0x03 or 0x13).
pub fn is_ui_ctl(c: u8) -> bool { c == 0x03 || c == 0x13 }
/// True when the control byte is SABM/SABME with the P/F bit ignored.
pub fn is_sabm_ctl(c: u8) -> bool { (c & 0xEF) == SABM_CTL }
/// True when the control byte is UA with the P/F bit ignored.
pub fn is_ua_ctl(c: u8) -> bool { (c & 0xEF) == UA_CTL }
/// True when the control byte is DISC with the P/F bit ignored.
pub fn is_disc_ctl(c: u8) -> bool { (c & 0xEF) == DISC_CTL }
/// True when the control byte is DM with the P/F bit ignored.
pub fn is_dm_ctl(c: u8) -> bool { (c & 0xEF) == DM_CTL }

// ── Address Encoding ─────────────────────────────────────
//
// Each AX.25 address is 7 bytes:
//   [0..5]  Callsign characters, left-shifted by 1 bit (ASCII << 1)
//   [6]     SSID byte:  C/R 1 1 SSSS S EXT  (bits 7=C/R, 6-5=RR=11, 4-1=SSID<<1, 0=HDLC ext)
//
// Multiple addresses are chained via the HDLC extension bit (bit 0 of byte 6):
//   bit 0 = 0 → more addresses follow
//   bit 0 = 1 → this is the last address

/// Encode a single callsign+SSID into a 7-byte AX.25 address field.
///
/// Per AX.25 §2.2.13, each ASCII character is shifted left by 1 bit
/// and padded with spaces to exactly 6 characters.
fn encode_callsign(call: &str, ssid: u8, c_bit: bool, last: bool) -> Vec<u8> {
    let call = call.to_uppercase();
    let mut buf = vec![0u8; 7];
    let chars: Vec<char> = call.chars().collect();
    // Shift each character left by 1 (AX.25 address encoding)
    for i in 0..6.min(chars.len()) {
        buf[i] = (chars[i] as u8) << 1;
    }
    // Pad remaining positions with space << 1
    for i in chars.len()..6 {
        buf[i] = b' ' << 1;
    }
    // SSID byte: C/R|1|1|S|S|S|S|EXT  — bits 7=C/R, 6-5=RR=11, 4-1=SSID<<1, 0=EXT
    let mut ssid_byte = (ssid & 0x0F) << 1;
    ssid_byte |= 0x60; // RR = 11 (compatible with Direwolf/KISS convention)
    if last {
        ssid_byte |= 0x01; // Bit 0 = extension — this is the final address
    }
    if c_bit {
        ssid_byte |= 0x80; // Bit 7 = C/R bit — set for Command frames
    }
    buf[6] = ssid_byte;
    buf
}

/// Encode a complete AX.25 address header: destination + source + digipeaters.
///
/// Address order per AX.25 §2.2.13:
///   1. Destination (first, C-bit = command flag)
///   2. Source (ALWAYS second, C-bit = !command)
///   3. Digipeaters (zero or more, last one has extension bit = 1)
pub fn encode_ax25_header(
    dest: &str, dest_ssid: u8,
    src: &str, src_ssid: u8,
    command: bool,
    digis: &[String],
) -> Vec<u8> {
    let mut header = Vec::new();
    let has_digis = !digis.is_empty();

    // Destination — first address in the header
    header.extend_from_slice(&encode_callsign(dest, dest_ssid, command, false));

    // Source — ALWAYS second address per AX.25 spec §2.2.13
    // Extension bit = 0 if digis follow, 1 if this is the last address
    let src_c = !command;
    header.extend_from_slice(&encode_callsign(src, src_ssid, src_c, !has_digis));

    // Digipeaters — come AFTER source; last one gets extension = 1
    for (i, digi) in digis.iter().enumerate() {
        let is_last = i == digis.len() - 1;
        let (d_call, d_ssid) = parse_callsign_ssid(digi);
        header.extend_from_slice(&encode_callsign(&d_call, d_ssid, false, is_last));
    }

    header
}

/// Split a "CALL-SSID" string into its callsign and numeric SSID parts.
/// Returns (callsign, ssid) with SSID clamped to 0-15.
///
/// Uses `rfind('-')` (last dash) to match the AGW module's parse_callsign_ssid.
/// For standard AX.25 callsigns like "KK6SEN-7" both approaches produce the
/// same result — but using the last dash is more robust for malformed input
/// with multiple dashes (e.g. "KK6SEN-15-A" -> base="KK6SEN", ssid=15).
pub fn parse_callsign_ssid(s: &str) -> (String, u8) {
    if let Some(dash_pos) = s.rfind('-') {
        let base = &s[..dash_pos];
        let ssid_str = &s[dash_pos + 1..];
        let ssid = ssid_str.parse::<u8>().unwrap_or(0).min(15);
        (base.to_string(), ssid)
    } else {
        (s.to_string(), 0)
    }
}

/// One decoded 7-byte AX.25 address field.
pub struct DecodedAddress {
    pub callsign: String,
    pub ssid: u8,
    /// Command/Response bit (also the H-bit for digipeaters).
    pub c_bit: bool,
    /// Same as c_bit for normal addresses; digipeaters carry the H-bit here.
    pub h_bit: bool,
    /// HDLC extension bit (true = last address in chain).
    pub e_bit: bool,
}

/// Decode a 7-byte AX.25 address field into its components.
pub fn decode_address_field(data: &[u8]) -> Option<DecodedAddress> {
    if data.len() < 7 {
        return None;
    }
    // Extract callsign: each byte shifted right by 1, mask off top bit
    let mut chars = String::with_capacity(6);
    for i in 0..6 {
        let c = (data[i] >> 1) & 0x7F;
        if c >= 32 && c <= 126 {
            chars.push(c as u8 as char);
        }
    }
    let callsign = chars.trim().to_string();
    let ssid_byte = data[6];
    let ssid = (ssid_byte >> 1) & 0x0F;
    let c_or_h = (ssid_byte & 0x80) != 0;
    let e_bit = (ssid_byte & 0x01) != 0;
    Some(DecodedAddress { callsign, ssid, c_bit: c_or_h, h_bit: c_or_h, e_bit })
}

/// Format a digipeater callsign with * marker if the H-bit (has-been-repeated) is set.
/// E.g., "KBANN" → "KBANN*" if the digipeater has already repeated the frame.
pub fn format_digi(call: &str, ssid: u8, h_bit: bool) -> String {
    let base = if ssid > 0 {
        format!("{}-{}", call, ssid)
    } else {
        call.to_string()
    };
    if h_bit { format!("{}*", base) } else { base }
}

/// Format a digipeater list for display (`format_digi` over each entry).
pub fn fmt_digi_labels(digis: &[(String, u8, bool)]) -> Vec<String> {
    digis.iter().map(|(c, s, h)| format_digi(c, *s, *h)).collect()
}

/// Format a callsign/SSID pair for display, omitting "-0" for readability.
/// E.g., "KK6SEN-0" → "KK6SEN", "KF6DQU-10" → "KF6DQU-10"
pub fn fmt_call(call: &str, ssid: u8) -> String {
    if ssid > 0 {
        format!("{}-{}", call.to_uppercase(), ssid)
    } else {
        call.to_uppercase()
    }
}

/// Format the digipeater via path string like "via KBANN KBANN*"
pub fn fmt_via(digis: &[(String, u8, bool)]) -> String {
    if digis.is_empty() { return String::new(); }
    let parts: Vec<String> = fmt_digi_labels(digis);
    format!("via {}", parts.join(" "))
}

/// Format a plain callsign digipeater list as `"via CALL1 CALL2 "` (trailing
/// space), or empty for a direct path. Single source of truth for the
/// `String`-list form (the rich `(String,u8,bool)` form lives in `fmt_via`).
/// The trailing space is load-bearing: header builders slot this directly
/// before a `ctl` token (`...{} {}ctl ...`), so the space separates the last
/// digi from `ctl`. Callers that want no trailing space should `.trim_end()`.
pub fn format_via(digis: &[String]) -> String {
    if digis.is_empty() {
        String::new()
    } else {
        format!(
            "via {} ",
            digis
                .iter()
                .map(|d| d.to_uppercase())
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

/// Decode a NET/ROM 7-byte node callsign (same shifted-left format as AX.25 addresses).
pub fn decode_netrom_node(data: &[u8]) -> Option<(String, u8)> {
    if data.len() < 7 { return None; }
    let mut chars = String::with_capacity(6);
    for i in 0..6 {
        let c = (data[i] >> 1) & 0x7F;
        if c >= 32 && c <= 126 { chars.push(c as u8 as char); }
    }
    let callsign = chars.trim().to_string();
    let ssid = (data[6] >> 1) & 0x0F;
    Some((callsign, ssid))
}

// ── AX.25 Frame Parsing ──────────────────────────────────

/// A fully-decoded AX.25 frame.
pub struct ParsedAx25Frame {
    pub dest: String,
    pub dest_ssid: u8,
    pub src: String,
    pub src_ssid: u8,
    /// Digipeaters with H-bit info — the bool is the H-bit.
    pub digis: Vec<(String, u8, bool)>,
    pub control: u8,
    pub pid: u8,
    pub info: Vec<u8>,
    /// C/R bit of the source address: 0=command, 1=response.
    pub src_c_bit: bool,
}

/// Full AX.25 frame parser — extracts all fields from raw bytes (after KISS port byte).
pub fn parse_ax25_frame(data: &[u8]) -> Option<ParsedAx25Frame> {
    if data.len() < 15 {
        return None;
    }
    let mut offset = 0;
    let mut addresses = Vec::new();

    // Walk through chained addresses terminated by HDLC extension bit
    while offset + 7 <= data.len() {
        let addr = decode_address_field(&data[offset..])?;
        let e_bit = addr.e_bit;
        addresses.push(addr);
        offset += 7;
        if e_bit {
            break;
        }
    }

    if addresses.len() < 2 {
        return None;
    }

    let dest = addresses[0].callsign.clone();
    let dest_ssid = addresses[0].ssid;
    let src = addresses[1].callsign.clone();
    let src_ssid = addresses[1].ssid;

    // Digipeaters with H-bit info (H-bit = C/R bit for non-first addresses)
    let digis: Vec<(String, u8, bool)> = addresses[2..].iter().map(|a| {
        (a.callsign.clone(), a.ssid, a.h_bit)
    }).collect();

    if offset >= data.len() {
        return None;
    }

    let control = data[offset];
    offset += 1;

    // I-frames (bit 0 = 0) and UI frames have a PID byte after the control field
    let is_iframe = is_iframe_ctl(control);
    let is_ui = is_ui_ctl(control);

    let pid = if (is_iframe || is_ui) && offset < data.len() {
        let p = data[offset];
        offset += 1;
        p
    } else {
        0xF0 // No PID — report as "no layer 3 protocol"
    };

    // The raw KISS payload from the TNC does NOT include the 2-byte FCS
    // (CRC-16). Per the original KISS specification (KA9Q, 1987), the TNC
    // strips the FCS before passing the frame to the host. Direwolf and
    // other modern KISS implementations follow this standard.
    //
    // Therefore everything after the control+PID fields is pure info data.
    let info = if offset < data.len() {
        data[offset..].to_vec()
    } else {
        Vec::new()
    };

    let src_c_bit = addresses[1].c_bit;
    Some(ParsedAx25Frame { dest, dest_ssid, src, src_ssid, digis, control, pid, info, src_c_bit })
}

// ── AX.25 Frame Builders ─────────────────────────────────

/// Build a complete AX.25 frame (addresses + control + PID + info bytes).
///
/// This is the lowest-level frame builder — use build_sabm, build_disc, etc. instead.
pub fn build_ax25_frame(
    dest: &str, dest_ssid: u8,
    src: &str, src_ssid: u8,
    control: u8, pid: u8,
    info: &[u8],
    digis: &[String],
    command: bool,
) -> Vec<u8> {
    let mut frame = encode_ax25_header(dest, dest_ssid, src, src_ssid, command, digis);
    frame.push(control);
    // I-frames and UI frames include a PID byte (protocol identifier)
    let is_iframe = is_iframe_ctl(control);
    let is_ui = is_ui_ctl(control);
    if is_iframe || is_ui {
        frame.push(pid);
    }
    frame.extend_from_slice(info);
    frame
}

/// Build a SABM (Set Asynchronous Balanced Mode) frame — initiates a connection.
pub fn build_sabm(dest: &str, dest_ssid: u8, src: &str, src_ssid: u8, digis: &[String]) -> Vec<u8> {
    build_ax25_frame(dest, dest_ssid, src, src_ssid, SABM_P, 0xF0, &[], digis, true)
}

/// Build a DISC (Disconnect) frame — requests orderly connection teardown.
pub fn build_disc(dest: &str, dest_ssid: u8, src: &str, src_ssid: u8, digis: &[String]) -> Vec<u8> {
    build_ax25_frame(dest, dest_ssid, src, src_ssid, DISC, 0xF0, &[], digis, true)
}

/// Build an I-frame (Information frame) — carries sequenced data on a connected session.
///
/// N(S) = send sequence number (0-7, modulo 8)
/// N(R) = receive sequence number (piggybacked ACK)
pub fn build_iframe(
    dest: &str, dest_ssid: u8,
    src: &str, src_ssid: u8,
    ns: u8, nr: u8,
    info: &[u8],
    digis: &[String],
) -> Vec<u8> {
    // I-frame control: bits 5-7 = N(R), bit 4 = P/F (1 = poll for ACK), bits 1-3 = N(S), bit 0 = 0
    // P-bit is NOT set by default — the caller can set it if a poll is needed.
    // Setting P=1 on every I-frame causes the remote to send RR+F per frame,
    // wasting airtime. Linpac/BPQ send I-frames with P=0 (see working Direwolf log).
    let control = (nr << 5) | (ns << 1);
    build_ax25_frame(dest, dest_ssid, src, src_ssid, control, 0xF0, info, digis, true)
}

/// Build a UI frame (Unnumbered Information) — connectionless data (e.g., APRS, beacons).
pub fn build_ui(
    dest: &str, dest_ssid: u8,
    src: &str, src_ssid: u8,
    info: &[u8],
    digis: &[String],
) -> Vec<u8> {
    build_ax25_frame(dest, dest_ssid, src, src_ssid, UI_P, 0xF0, info, digis, true)
}

// ── Display Helpers ──────────────────────────────────────

/// Convert raw AX.25 info field bytes to a readable string.
/// Replaces non-printable control chars (0x00-0x1F except \r, \n, \t)
/// and 0x7f (DEL — typically RF noise corruption) with '·' (middle dot).
/// Falls back to '.' per-byte for bytes that aren't valid UTF-8 continuation.
pub fn info_to_string(info: &[u8]) -> String {
    String::from_utf8(info.to_vec()).unwrap_or_else(|_| {
        info.iter().map(|&b| {
            if b >= 32 && b <= 126 {
                b as char
            } else if b == b'\r' || b == b'\n' || b == b'\t' {
                b as char
            } else {
                '·'
            }
        }).collect()
    })
}

/// Convert line endings to the linpac convention: \r\n and bare \n both
/// become a single \r, bare \r is kept, and a trailing \r is ensured so the
/// remote treats the text as one complete line.
///
/// \r\n must collapse to ONE \r — the board packs multi-line replies with
/// \r\n joins, and a naive \n→\r map would turn every join into \r\r,
/// inserting a blank line between every line on the wire. Consecutive \r
/// (intentional blank lines, e.g. empty body lines) are preserved.
pub fn normalize_linpac_text(text: &str) -> Vec<u8> {
    let mut out = String::with_capacity(text.len() + 1);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                out.push('\r');
                if chars.peek() == Some(&'\n') {
                    chars.next(); // collapse \r\n -> \r
                }
            }
            '\n' => out.push('\r'),
            _ => out.push(ch),
        }
    }
    if !out.ends_with('\r') {
        out.push('\r');
    }
    out.into_bytes()
}

/// Format raw AX.25 bytes as a hex dump string (space-separated, e.g. "C0 01 02 ...").
/// Used for the Raw KISS Display mode and debugging.
pub fn fmt_raw_kiss(payload: &[u8]) -> String {
    payload.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
}

/// Build a linpac-style radio header line like:
/// "radio: fm KF6DQU-10 to BEACON via DIGI DIGI* ctl UI pid=F0(Text) len 31"
///
/// This matches the format used by linpac and QtTermTCP for radio-layer display,
/// giving operators a consistent view of frame metadata across tools.
pub fn build_radio_header(
    src_label: &str,
    dest_label: &str,
    digis: &[(String, u8, bool)],
    frame_type: &str,
    control: u8,
    pid: u8,
    info_len: usize,
) -> String {
    // Only I-frames and UI frames carry a PID byte on the wire. Control
    // frames (SABM, UA, DISC, DM, RR, REJ, FRMR, ...) have no PID field —
    // don't fabricate one in the display.
    let is_iframe = is_iframe_ctl(control);
    let is_ui = is_ui_ctl(control);
    let has_pid = is_iframe || is_ui;

    let pid_part = if has_pid {
        if pid == 0xF0 {
            " pid=F0(Text)".to_string()
        } else {
            format!(" pid={:02X}", pid)
        }
    } else {
        String::new()
    };
    let control_hex = if frame_type == "?" {
        format!(" {:02X}", control)
    } else {
        String::new()
    };
    let via_str = fmt_via(digis);
    if via_str.is_empty() {
        format!(
            "radio: fm {} to {} ctl {}{}{} len {}",
            src_label, dest_label, frame_type, control_hex, pid_part, info_len
        )
    } else {
        format!(
            "radio: fm {} to {} {} ctl {}{}{} len {}",
            src_label, dest_label, via_str, frame_type, control_hex, pid_part, info_len
        )
    }
}

// ── Frame Type Classification ────────────────────────────
//
// Classifies an AX.25 control byte into a human-readable frame type string
// using the bit-pattern approach from AX.25 §2.3:
//   bit 0 = 0 → I-frame  (Information)
//   bits 0-1 = 01 → S-frame (Supervisory: RR, RNR, REJ, SREJ)
//   bits 0-1 = 11 → U-frame (Unnumbered: SABM, UA, DISC, DM, UI, FRMR, SABME)

/// Classify a control byte into a human-readable frame type string.
/// Returns strings like "I", "RR", "SABM", "UI", "UA", "DISC", etc.
/// ALL frame types get a +/- suffix for the P/F bit: "+" = P/F set,
/// "-" = P/F clear (e.g., "RR1+", "UI-"). Matches the Android app
/// (MonitorPacket.kt) and the Direwolf/TNC-2 convention.
pub fn classify_frame_type(control: u8) -> String {
    if is_iframe_ctl(control) {
        // I-frame: bit 0 = 0.  Returns e.g. "I40+" or "I40-"
        let ns = (control >> 1) & 0x07;
        let nr = (control >> 5) & 0x07;
        let pf = if (control & 0x10) != 0 { "+" } else { "-" };
        format!("I{}{}{}", ns, nr, pf)
    } else if (control & 0x03) == 0x01 {
        // S-frame: bits 0-1 = 01.  Returns e.g. "RR1+" or "RR1-"
        let nr = (control >> 5) & 0x07;
        let pf = if (control & 0x10) != 0 { "+" } else { "-" };
        match control & 0x0F {
            0x01 => format!("RR{}{}", nr, pf),
            0x05 => format!("RNR{}{}", nr, pf),
            0x09 => format!("REJ{}{}", nr, pf),
            0x0D => format!("SREJ{}{}", nr, pf),
            _    => format!("S{}{}", nr, pf),
        }
    } else {
        // U-frame: bits 0-1 = 11
        let u_type = control & 0xEF;
        let pf = if (control & 0x10) != 0 { "+" } else { "-" };
        match u_type {
            0x03 => format!("UI{}", pf),
            0x2F => format!("SABM{}", pf),
            0x63 => format!("UA{}", pf),
            0x43 => format!("DISC{}", pf),
            0x0F => format!("DM{}", pf),
            0x87 => format!("FRMR{}", pf),
            0x6F => format!("SABME{}", pf),
            0xAF => format!("XID{}", pf),
            _    => format!("?{}", pf),
        }
    }
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The C/R-bit convention that frame handlers rely on: the SOURCE
    /// address carries the INVERTED command flag (encode_ax25_header:
    /// src_c = !command), so src_c_bit == "this frame is a response".
    /// The bbs_link poll handling had this backwards and never answered
    /// command polls — a lost data frame made the peer poll forever.
    #[test]
    fn src_c_bit_polarity_command_vs_response() {
        // Command frame: dest carries C=1, src carries !command = 0.
        let hdr = encode_ax25_header("KK6ABC", 1, "KK6SEN", 0, true, &["KBANN".to_string()]);
        let dest = decode_address_field(&hdr[0..7]).unwrap();
        let src = decode_address_field(&hdr[7..14]).unwrap();
        assert!(dest.c_bit, "dest of a command must carry the C bit");
        assert!(!src.c_bit, "src of a command carries 0 — src_c_bit=false means command");
        // Response frame: dest C=0, src C=1.
        let hdr2 = encode_ax25_header("KK6ABC", 1, "KK6SEN", 0, false, &["KBANN".to_string()]);
        let dest2 = decode_address_field(&hdr2[0..7]).unwrap();
        let src2 = decode_address_field(&hdr2[7..14]).unwrap();
        assert!(!dest2.c_bit, "dest of a response must NOT carry the C bit");
        assert!(src2.c_bit, "src of a response carries 1 — src_c_bit=true means response");
    }

    #[test]
    fn test_encode_callsign_basic() {
        // KK6SEN, no SSID, not last, no C/R
        let result = encode_callsign("KK6SEN", 0, false, false);
        assert_eq!(result.len(), 7);
        // Check first char: 'K' = 0x4B, shifted = 0x96
        assert_eq!(result[0], b'K' << 1);
        // SSID byte: no extension, no C/R, SSID=0, RR=11 → 0x60
        assert_eq!(result[6], 0x60);
    }

    #[test]
    fn test_encode_callsign_ssid_padded() {
        // Short callsign padded with spaces
        let result = encode_callsign("KB", 7, true, true);
        assert_eq!(result.len(), 7);
        assert_eq!(result[0], b'K' << 1);
        assert_eq!(result[1], b'B' << 1);
        assert_eq!(result[2], b' ' << 1); // padded
        // SSID=7, C/R=1, last=1: C/R=1, RR=11, SSSS=0111, EXT=1 = 1110 1111 = 0xEF
        assert_eq!(result[6], 0xEF);
    }

    #[test]
    fn test_parse_callsign_ssid() {
        assert_eq!(parse_callsign_ssid("KK6SEN"), ("KK6SEN".to_string(), 0));
        assert_eq!(parse_callsign_ssid("KF6DQU-10"), ("KF6DQU".to_string(), 10));
        assert_eq!(parse_callsign_ssid("KB-15"), ("KB".to_string(), 15));
        assert_eq!(parse_callsign_ssid("-5"), ("".to_string(), 5));
    }

    #[test]
    fn test_encode_ax25_header_roundtrip() {
        // Encode a simple dest→src header with no digis
        let header = encode_ax25_header("KK6SEN", 0, "KB7XYZ", 5, true, &[]);
        assert_eq!(header.len(), 14); // 2 × 7 bytes

        // Decode it back
        let frame = parse_ax25_frame(
            &[&header[..], &[0x03u8, 0xF0], b"HELLO"].concat()
        ).unwrap();
        assert_eq!(frame.dest, "KK6SEN");
        assert_eq!(frame.dest_ssid, 0);
        assert_eq!(frame.src, "KB7XYZ");
        assert_eq!(frame.src_ssid, 5);
        assert!(frame.digis.is_empty());
    }

    #[test]
    fn test_encode_ax25_header_with_digis() {
        let digis = vec!["WIDE1-1".to_string(), "WIDE2".to_string()];
        let header = encode_ax25_header("KK6SEN", 0, "KB7XYZ", 5, true, &digis);
        // Dest SSID: SSID=0, C/R=1, last=0 → C/R=1, RR=11, SSSS=0000, EXT=0 = 1110 0000 = 0xE0
        assert_eq!(header[6], 0xE0);

        // Check extension bits (bit 0 = 0x01): dest=0, src=0, WIDE1-1=0, WIDE2=1
        // Check C/R bits (bit 7 = 0x80): dest=1 (command), src=0 (!command), digis=0
        assert_eq!(header[6] & 0x01, 0);   // dest EXT = 0 (more follow)
        assert_eq!(header[13] & 0x01, 0);  // src EXT = 0 (more follow)
        assert_eq!(header[20] & 0x01, 0);  // WIDE1-1 EXT = 0 (more follow)
        assert_eq!(header[27] & 0x01, 1);  // WIDE2 EXT = 1 (last)
        assert_eq!(header[6] & 0x80, 0x80);  // dest C/R = 1 (command)
        assert_eq!(header[13] & 0x80, 0);    // src C/R = 0 (!command)
    }

    #[test]
    fn test_decode_address_field_too_short() {
        assert!(decode_address_field(&[0u8; 3]).is_none());
    }

    #[test]
    fn test_build_sabm_and_parse() {
        let sabm = build_sabm("KK6SEN", 0, "KB7XYZ", 5, &[]);
        // Should be 14 bytes header + 1 control + 0 info = 15
        assert_eq!(sabm.len(), 15);
        assert_eq!(sabm[14], SABM_P); // control byte
    }

    #[test]
    fn test_build_iframe_and_parse() {
        let data = b"Hello\r";
        let iframe = build_iframe("KK6SEN", 0, "KB7XYZ", 5, 2, 3, data, &[]);
        // 14 bytes header + 1 control + 1 PID + 6 data = 22
        assert_eq!(iframe.len(), 22);
        // Check control: N(R)=3<<5 | N(S)=2<<1 | P=0 = 0x64
        assert_eq!(iframe[14], 0x64);
    }

    #[test]
    fn test_classify_frame_type() {
        assert_eq!(classify_frame_type(0x00), "I00-");   // I-frame, N(S)=0, N(R)=0
        assert_eq!(classify_frame_type(0x03), "UI-");    // UI, P/F clear
        assert_eq!(classify_frame_type(0x13), "UI+");    // UI with P/F set
        assert_eq!(classify_frame_type(0x3F), "SABM+");  // SABM with P/F set
        assert_eq!(classify_frame_type(0x73), "UA+");    // UA with F-bit set
        assert_eq!(classify_frame_type(0x63), "UA-");    // UA without F-bit
        assert_eq!(classify_frame_type(0x53), "DISC+");  // DISC with P-bit set
        assert_eq!(classify_frame_type(0x43), "DISC-");  // DISC without P-bit
        assert_eq!(classify_frame_type(0x01), "RR0-");   // RR, N(R)=0
        assert_eq!(classify_frame_type(0x11), "RR0+");   // RR with P/F, N(R)=0
        assert_eq!(classify_frame_type(0x05), "RNR0-");  // RNR
        assert_eq!(classify_frame_type(0x09), "REJ0-");  // REJ
        // I-frame with N(S)=4, N(R)=0, P=1: 0x10 | (0<<5) | (4<<1) = 0x18
        assert_eq!(classify_frame_type(0x18), "I40+");   // +/- convention
        // FRMR and XID
        assert_eq!(classify_frame_type(0x87), "FRMR-");
        assert_eq!(classify_frame_type(0x97), "FRMR+");
        assert_eq!(classify_frame_type(0xAF), "XID-");  // XID, P/F clear (0xAF bit 0x10 = 0)
    }

    #[test]
    fn test_build_radio_header() {
        let digis = vec![("WIDE1".to_string(), 1, false), ("WIDE2".to_string(), 0, true)];
        let hdr = build_radio_header("KK6SEN", "BEACON", &digis, "UI+", 0x13, 0xF0, 12);
        assert!(hdr.contains("fm KK6SEN to BEACON"));
        assert!(hdr.contains("via WIDE1-1 WIDE2*"));
        assert!(hdr.contains("ctl UI+ pid=F0(Text) len 12"));
    }

    #[test]
    fn test_build_radio_header_no_digis() {
        let hdr = build_radio_header("KK6SEN", "BEACON", &[], "UI-", 0x03, 0xF0, 5);
        assert_eq!(hdr, "radio: fm KK6SEN to BEACON ctl UI- pid=F0(Text) len 5");
    }

    #[test]
    fn test_build_radio_header_control_frame_no_pid() {
        // SABM (0x3F) is a U-frame — no PID byte on the wire, so the header
        // must NOT fabricate pid=F0(Text).
        let hdr = build_radio_header("KK6SEN", "KBETH", &[], "SABM+", 0x3F, 0xF0, 0);
        assert_eq!(hdr, "radio: fm KK6SEN to KBETH ctl SABM+ len 0");
        // RR (0x11) is an S-frame — also no PID byte.
        let hdr = build_radio_header("KK6SEN", "KBETH", &[], "RR0+", 0x11, 0xF0, 0);
        assert_eq!(hdr, "radio: fm KK6SEN to KBETH ctl RR0+ len 0");
        // I-frame still shows the real PID.
        let hdr = build_radio_header("KK6SEN", "KBETH", &[], "I00-", 0x00, 0xF0, 3);
        assert_eq!(hdr, "radio: fm KK6SEN to KBETH ctl I00- pid=F0(Text) len 3");
    }

    #[test]
    fn test_info_to_string_valid_utf8() {
        assert_eq!(info_to_string(b"Hello"), "Hello");
    }

    #[test]
    fn test_info_to_string_invalid_utf8() {
        let result = info_to_string(&[0xFF, 0xFE, 0x00]);
        assert_eq!(result, "···"); // all non-printable → middle dot
    }

    #[test]
    fn test_fmt_call_omits_ssid_zero() {
        assert_eq!(fmt_call("KK6SEN", 0), "KK6SEN");
        assert_eq!(fmt_call("KK6SEN", 10), "KK6SEN-10");
    }

    #[test]
    fn test_fmt_via() {
        let digis = vec![("WIDE1".to_string(), 1, false), ("WIDE2".to_string(), 0, true)];
        assert_eq!(fmt_via(&digis), "via WIDE1-1 WIDE2*");
        assert_eq!(fmt_via(&[]), "");
    }

    #[test]
    fn test_normalize_linpac_text_collapses_crlf() {
        // \r\n joins must collapse to ONE \r — a naive \n→\r map would turn
        // every packed line join into \r\r (blank line between every line).
        assert_eq!(normalize_linpac_text("a\r\nb\r\nc"), b"a\rb\rc\r".to_vec());
        // Bare \n also becomes \r.
        assert_eq!(normalize_linpac_text("a\nb"), b"a\rb\r".to_vec());
        // Intentional blank lines (\r\r) are preserved.
        assert_eq!(normalize_linpac_text("a\r\n\r\nb"), b"a\r\rb\r".to_vec());
        // Lone \r kept, trailing \r ensured.
        assert_eq!(normalize_linpac_text("a\rb"), b"a\rb\r".to_vec());
        assert_eq!(normalize_linpac_text(""), b"\r".to_vec());
        // Already-terminated text gets no second trailing \r.
        assert_eq!(normalize_linpac_text("a\r"), b"a\r".to_vec());
        assert_eq!(normalize_linpac_text("a\r\n"), b"a\r".to_vec());
    }
}

