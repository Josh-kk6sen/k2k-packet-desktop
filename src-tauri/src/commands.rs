// ── Tauri Command Handlers ─────────────────────────────────
//
// These are the IPC commands exposed to the frontend via Tauri's invoke().
// Each command is an async function annotated with #[tauri::command].
// See the `///` doc comment on each command for its purpose.

use std::sync::Arc;
use tauri::Emitter;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::sync::Mutex;

use crate::agw;
use crate::app::comm::{actual_pid, agw_radio_header, emit_sent_frame, fallback_frame_type, format_monitor_payload, format_radio_header, parse_verbose_frame_type, parse_verbose_pid, parse_verbose_via, session_send_state, split_after_verbose_header, store_pending_iframe, strip_agw_line_prefix};
use crate::ax25;
use crate::kiss;
use crate::session;

/// Hard cap on a single KISS / AGW frame we are willing to buffer from the
/// wire. AX.25 frames on 1200/9600 baud are at most a few hundred bytes, but a
/// corrupt or malicious TNC peer could declare a huge AGW data_len (u32) or
/// stream an unterminated KISS frame. This bound turns any such stream into a
/// bounded, droppable error instead of unbounded memory growth.
pub const MAX_FRAME_BYTES: usize = 65536;

/// Convert an app-layer `AppHandle` into the kernel's owned output handle.
/// `AppHandle` implements `KernelOutput` (see app/mod.rs), so arithmetic of
/// ownership is confined to this one helper.
fn out_handle(app: &tauri::AppHandle) -> std::sync::Arc<dyn crate::kernel::output::KernelOutput> {
    let o: std::sync::Arc<dyn crate::kernel::output::KernelOutput> =
        std::sync::Arc::new(app.clone());
    o
}

// ── Application Log ───────────────────────────────────────
/// Hex+ASCII dump of a raw KISS frame — goes to today's raw log file.
pub(crate) fn log_packet(data: &[u8], label: &str) {
    use std::io::Write;
    let hex = ax25::fmt_raw_kiss(&data);
    let ascii: String = data
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::daily_log_path("raw"))
    {
        let _ = writeln!(
            f,
            "[{}] {}  {}\n         {}  |{}|",
            chrono::Local::now().format("%H:%M:%S%.3f"),
            label,
            hex,
            " ".repeat(label.len()),
            ascii
        );
    }
}

/// Log a plain text message to today's raw log file (timestamped).
/// Used for in-app diagnostics — view via the daily YYMMDD_raw.log files
/// in the app's data directory.
pub(crate) fn log_line(msg: &str) {
    crate::append_log(
        "raw",
        &format!("[{}] {}", chrono::Local::now().format("%H:%M:%S%.3f"), msg),
    );
}

// ── Display-string helpers ─────────────────────────────────

/// Format a digipeater via path: `"via CALL CALL"` or `""` when empty.






/// Strip the AGWPE line-number prefix ("1:Fm ..." → "Fm ...") from each
/// line of a monitor payload — TNCs prefix received lines with the
/// station line number, which is noise for display.
fn strip_agw_line_prefixes(payload: &[u8]) -> Vec<u8> {
    let as_str = String::from_utf8_lossy(payload);
    let cleaned: Vec<String> = as_str
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if let Some(colon) = trimmed.find(':') {
                if colon > 0 && colon <= 3 && trimmed[..colon].chars().all(|c| c.is_ascii_digit()) {
                    trimmed[colon + 1..].trim().to_string()
                } else {
                    trimmed.to_string()
                }
            } else {
                trimmed.to_string()
            }
        })
        .collect();
    cleaned.join("\n").into_bytes()
}

// ── TNC Reader Tasks ──────────────────────────────────────

/// Common handling for a read failure: count strikes, and after 3
/// consecutive failures mark the connection dead, notify the frontend,
/// and return true so the reader loop can exit. Otherwise sleep briefly
/// to ride through network blips (WiFi re-association, router reboot).
async fn handle_read_failure(
    client: &Arc<Mutex<session::TcpClient>>,
    app_handle: &tauri::AppHandle,
    conn_id: &str,
    read_failures: &mut u32,
) -> bool {
    *read_failures += 1;
    if *read_failures >= 3 {
        let mut c = client.lock().await;
        c.info.connected = false;
        let _ = app_handle.emit("tnc-disconnected", serde_json::json!({ "id": conn_id }));
        true
    } else {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        false
    }
}

/// Spawn the KISS reader task: reads raw bytes from the TNC stream,
/// reassembles KISS frames (delimited by FEND bytes), unescapes them,
/// strips the command byte, and feeds each AX.25 payload to the state
/// machine.
fn spawn_kiss_reader(
    reader: tokio::net::tcp::OwnedReadHalf,
    client: Arc<Mutex<session::TcpClient>>,
    app_handle: tauri::AppHandle,
    conn_id: String,
) -> tokio::task::JoinHandle<()> {
    let out = out_handle(&app_handle);
    tokio::spawn(async move {
        let mut buf_reader = BufReader::new(reader);
        let mut frame_buf = Vec::new();
        let mut in_frame = false;
        let mut read_failures: u32 = 0;

        loop {
            let mut byte = [0u8; 1];
            match buf_reader.read_exact(&mut byte).await {
                Ok(_) => {
                    read_failures = 0; // Reset on successful read
                    let b = byte[0];
                    if b == kiss::FEND {
                        // FEND marks both the start AND end of a KISS frame.
                        // We only process when we have both a start and some data.
                        if in_frame && !frame_buf.is_empty() {
                            // Guard against an unterminated / over-sized frame
                            // (a peer that streams data without ever closing it,
                            // or a frame beyond any plausible AX.25 size). Drop
                            // the whole frame rather than growing unboundedly.
                            if frame_buf.len() <= MAX_FRAME_BYTES {
                                let unescaped = kiss::kiss_unescape(&frame_buf);
                                if unescaped.len() > 1 {
                                    // First byte is the KISS command/port byte
                                    let payload = &unescaped[1..];
                                    log_packet(&unescaped, "KISS RX");
                                    session::process_incoming_frame(payload, &client, &out).await;
                                }
                            }
                            frame_buf.clear();
                        } else if in_frame {
                            // An empty FEND closes an empty frame — drop it.
                            frame_buf.clear();
                        }
                        in_frame = true;
                    } else if in_frame {
                        frame_buf.push(b);
                        // Hard guard: if a peer streams long past a plausible
                        // frame size without a closing FEND, stop accumulating
                        // (stay in-frame so we resync on the next FEND).
                        if frame_buf.len() > MAX_FRAME_BYTES {
                            frame_buf.truncate(MAX_FRAME_BYTES);
                        }
                    }
                }
                Err(_) => {
                    if handle_read_failure(&client, &app_handle, &conn_id, &mut read_failures).await
                    {
                        break;
                    }
                }
            }
        }
    })
}

/// Spawn the AGW reader task: buffers incoming bytes, parses AGWPE
/// frames out of the stream, and routes each to the frame handlers.
fn spawn_agw_reader(
    reader: tokio::net::tcp::OwnedReadHalf,
    client: Arc<Mutex<session::TcpClient>>,
    app_handle: tauri::AppHandle,
    conn_id: String,
    my_call: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf_reader = BufReader::new(reader);
        let mut buf = [0u8; 4096];
        let mut accum = Vec::new();
        let mut tab_map: Vec<agw::TabMapping> = Vec::new();

        // NOTE: Burst coalescing is handled by the frontend (200ms debounce timer),
        // so no backend-side rx_buffers are needed for AGW.
        let mut read_failures: u32 = 0;
        loop {
            match buf_reader.read(&mut buf).await {
                Ok(0) => {
                    // Zero bytes means EOF (other end closed cleanly).
                    // Still retry briefly in case it's a transient issue.
                    if handle_read_failure(&client, &app_handle, &conn_id, &mut read_failures).await
                    {
                        break;
                    }
                }
                Ok(n) => {
                    read_failures = 0; // Reset on successful read
                    accum.extend_from_slice(&buf[..n]);
                    // Hard cap: a peer-controlled AGW data_len (full u32 range)
                    // must not drive unbounded buffering. If a frame declares a
                    // length beyond MAX_FRAME_BYTES, drain into the void so the
                    // stream stays byte-synced instead of growing accum forever.
                    let mut dropped = false;
                    if accum.len() > MAX_FRAME_BYTES * 2 {
                        accum.truncate(MAX_FRAME_BYTES);
                        dropped = true;
                    }
                    loop {
                        if accum.len() < agw::AGW_HEADER_SIZE {
                            break;
                        }
                        if let Some(frame) = agw::parse_frame(&accum) {
                            let declared = frame.data_len as usize;
                            if declared > MAX_FRAME_BYTES {
                                // Impossible frame — skip past the header and
                                // keep scanning for the next plausible frame.
                                accum.remove(0);
                                continue;
                            }
                            let total = agw::AGW_HEADER_SIZE + declared;
                            if accum.len() < total {
                                break;
                            }
                            log_packet(&accum[..total], "AGW RX");

                            route_agw_frame(&frame, &client, &app_handle, &my_call, &mut tab_map)
                                .await;

                            accum.drain(0..total);
                        } else {
                            accum.remove(0);
                        }
                    }
                    let _ = dropped;
                }
                Err(_) => {
                    if handle_read_failure(&client, &app_handle, &conn_id, &mut read_failures).await
                    {
                        break;
                    }
                }
            }
        }
    })
}

// ── Connection teardown helpers ───────────────────────────

/// Drop any existing connection to the same host:port before creating a
/// new one. Prevents duplicate beacon timers when auto-connect fires and
/// the user also clicks Connect manually.
async fn remove_connections_to(state: &tauri::State<'_, session::AppState>, host: &str, port: u16) {
    state.core.lock().await.remove_connections_to(host, port).await;
}

/// True when the named connection is an AGWPE (Direwolf) link.
async fn connection_is_agw(state: &tauri::State<'_, session::AppState>, conn_id: &str) -> bool {
    state.core.lock().await.connection_is_agw(conn_id).await
}

/// Lock the connection with the given id from the global list.
/// Returns None if no matching connection exists.
async fn find_client(
    state: &tauri::State<'_, session::AppState>,
    conn_id: &str,
) -> Option<Arc<Mutex<session::TcpClient>>> {
    state.core.lock().await.find_client(conn_id).await
}

/// Validate a session tab index against MAX_TABS.
fn check_tab(tab: usize) -> Result<(), String> {
    if tab >= session::MAX_TABS {
        Err(format!("Tab must be 0-{}", session::MAX_TABS - 1))
    } else {
        Ok(())
    }
}

/// Remove the connection with the given id from the global list, aborting its
/// background tasks. Returns true if a connection was actually removed.
async fn remove_connection_by_id(state: &tauri::State<'_, session::AppState>, id: &str) -> bool {
    state.core.lock().await.remove_connection_by_id(id).await
}

// ── Beacon Timer ──────────────────────────────────────────
//
// One self-contained periodic beacon task per connection. The task polls
// settings from disk every 10 seconds and drives a small pure scheduler
// (beacon_tick), so changes to the beacon text / dest / digi string /
// interval take effect within one poll — and crucially, saving UNRELATED
// settings (unproto To/Via auto-saves, UI toggles, connect, etc.) never
// touches the timer.
//
// The previous design restarted this task on every save_settings call, and
// every restart sent an immediate beacon ~5s later. With the frontend
// auto-saving on 300ms-debounced input changes, editing a digi string made
// the beacon fire repeatedly ("went crazy re-beaconing"). Fixed by removing
// the restart coupling entirely: the task owns its schedule.

/// Scheduler state for one beacon task. Pure and unit-testable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BeaconSched {
    /// When the next beacon is due (epoch ms). None = not scheduled.
    pub next_send_at_ms: Option<u64>,
    /// Snapshot of (text, dest, via) the schedule is based on.
    pub last_cfg: Option<(String, String, String)>,
    /// Snapshot of the interval (seconds) the schedule is based on.
    pub last_interval_secs: u64,
}

impl Default for BeaconSched {
    fn default() -> Self {
        BeaconSched {
            next_send_at_ms: None,
            last_cfg: None,
            last_interval_secs: 0,
        }
    }
}

/// One poll tick of the beacon scheduler. Pure: given the wall-clock time
/// and the current settings snapshot, decide whether a beacon is due now
/// and what the next schedule looks like.
///
/// Rules:
/// - Not configured (disabled / missing text / missing dest) → not scheduled.
/// - Fresh enable (was not scheduled) → send ~5s from now (verification).
/// - Text / dest / via changed → send ~5s from now (hear the edit), then
///   cadence continues at the current interval.
/// - Interval changed only → reset the countdown to the new interval (no
///   surprise early beacon).
/// - Nothing changed → schedule untouched (the core fix: unrelated settings
///   saves can never reset or trigger the beacon).
pub(crate) fn beacon_tick(
    now_ms: u64,
    configured: bool,
    text: &str,
    dest: &str,
    via: &str,
    interval_secs: u64,
    st: &BeaconSched,
) -> (BeaconSched, bool) {
    if !configured {
        return (BeaconSched::default(), false);
    }
    let cfg = (text.to_string(), dest.to_string(), via.to_string());
    let mut next = st.next_send_at_ms;
    if st.last_cfg.is_none() {
        // Fresh enable: first beacon shortly so the user hears it on air.
        next = Some(now_ms + 5_000);
    } else {
        let text_changed = st.last_cfg.as_ref() != Some(&cfg);
        let interval_changed = st.last_interval_secs != interval_secs;
        if text_changed {
            // One verification beacon; the (possibly new) interval governs
            // the cadence after it.
            next = Some(now_ms + 5_000);
        } else if interval_changed {
            // No early beacon — restart the countdown at the new interval.
            next = Some(now_ms + interval_secs * 1000);
        }
        // else: nothing changed → schedule untouched.
    }
    let mut sched = BeaconSched {
        next_send_at_ms: next,
        last_cfg: Some(cfg),
        last_interval_secs: interval_secs,
    };
    let mut send_now = false;
    if let Some(t) = next {
        if now_ms >= t {
            send_now = true;
            sched.next_send_at_ms = Some(now_ms + interval_secs * 1000);
        }
    }
    (sched, send_now)
}

/// Spawn a periodic beacon timer for a connection.
///
/// Polls settings from disk every 10s; sends a beacon only when the
/// scheduler says it is due. KISS builds a raw AX.25 UI frame; AGW builds
/// an AGW unproto frame. The task exits when the connection drops.
pub(crate) fn spawn_beacon(
    client_arc: Arc<Mutex<session::TcpClient>>,
    app_handle: tauri::AppHandle,
    is_agw: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // ── Helper: send one beacon frame ─────────────────────────────
        async fn send_one_beacon(
            client: &mut crate::session::TcpClient,
            settings: &crate::settings::AppSettings,
            is_agw: bool,
            app_handle: &tauri::AppHandle,
        ) -> Result<(), String> {
            let digis = crate::session::parse_via(&settings.beacon_via);
            if is_agw {
                let agw_frame = if digis.is_empty() {
                    crate::agw::build_unproto(
                        0,
                        &client.callsign,
                        client.ssid,
                        &settings.beacon_dest,
                        0,
                        settings.beacon.trim().as_bytes(),
                    )
                } else {
                    crate::agw::build_unproto_via(
                        0,
                        &client.callsign,
                        client.ssid,
                        &settings.beacon_dest,
                        0,
                        &digis,
                        settings.beacon.trim().as_bytes(),
                    )
                };
                if let Some(w) = &mut client.stream {
                    w.write_all(&agw_frame)
                        .await
                        .map_err(|e| format!("AGW beacon write: {}", e))?;
                } else {
                    return Err("Not connected to TNC".to_string());
                }
            } else {
                let ui = crate::ax25::build_ui(
                    &settings.beacon_dest,
                    0,
                    &client.callsign,
                    client.ssid,
                    settings.beacon.trim().as_bytes(),
                    &digis,
                );
                crate::kiss::send_kiss(client, &ui).await?;

                // Emit unproto frame so the beacon appears in the UI panel
                emit_sent_frame(
                    app_handle,
                    &client.callsign,
                    client.ssid,
                    &settings.beacon_dest,
                    0,
                    &digis,
                    "UI+",
                    "pid=F0(Text)",
                    settings.beacon.trim(),
                    "UI+",
                    settings.beacon.trim().len(),
                    &ui,
                );
            }
            Ok(())
        }

        const POLL_MS: u64 = 10_000;
        let mut sched = BeaconSched::default();
        loop {
            {
                let c = client_arc.lock().await;
                if !c.info.connected {
                    break;
                }
            }
            let settings = crate::settings::load_settings_or_default(&app_handle);
            let configured = crate::settings::beacon_configured(&settings);
            let interval_secs = (settings.beacon_interval.max(1) * 60) as u64;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let (new_sched, send_now) = beacon_tick(
                now_ms,
                configured,
                settings.beacon.trim(),
                &settings.beacon_dest,
                &settings.beacon_via,
                interval_secs,
                &sched,
            );
            sched = new_sched;
            if send_now {
                let mut c = client_arc.lock().await;
                if !c.info.connected {
                    break;
                }
                if let Err(e) = send_one_beacon(&mut c, &settings, is_agw, &app_handle).await {
                    log_line(&format!("Beacon send failed: {}", e));
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(POLL_MS)).await;
        }
    })
}

// ── KISS Connect ─────────────────────────────────────────

/// Connect to a KISS-over-TCP TNC at the given host:port.
///
/// Spawns:
///   - A reader task that reads KISS frames from the TNC and routes them
///     to process_incoming_frame in the session module.
///   - An optional AX.25 ID beacon (FCC Part 97 §97.119) if enabled.
///
/// The connection is stored in AppState so other commands (ax25_connect,
/// ax25_send_iframe, etc.) can find it by ID.
#[tauri::command]
pub async fn kiss_connect(
    id: String,
    label: String,
    host: String,
    port: u16,
    callsign: String,
    ssid: u8,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let addr = format!("{}:{}", host, port);

    // Drop any existing connection to the same host:port first (see
    // remove_connections_to) — prevents duplicate beacon timers when
    // auto-connect fires and the user also clicks Connect manually.
    remove_connections_to(&state, &host, port).await;

    let (reader, writer, timing) =
        crate::app::comm::open_tcp_stream(&addr, &app_handle).await?;

    // Build the client struct with 4 empty sessions (C1–C4)
    let client = session::TcpClient::new(
        id.clone(),
        label,
        host,
        port,
        session::Protocol::KISS,
        writer,
        callsign,
        ssid,
        &out_handle(&app_handle),
        timing,
    );

    let client_arc = Arc::new(Mutex::new(client));

    // ── Start KISS reader task ──
    // Reassembles FEND-delimited frames from the TNC stream and feeds
    // each AX.25 payload to the state machine (see spawn_kiss_reader).
    let reader_handle =
        spawn_kiss_reader(reader, client_arc.clone(), app_handle.clone(), id.clone());

    // Phase 3: attach the live transport adapter so outbound KISS TX routes
    // through the `Link` seam (framed byte-identically). The TCP stream is
    // already up, so the adapter is marked connected to its endpoint here.
    {
        let mut c = client_arc.lock().await;
        let mut link = crate::link::kiss::KissLink::new();
        // Mark the adapter connected to the client's own stored endpoint
        // (host/port were already moved into TcpClient::new).
        let _ = crate::link::Link::connect(&mut link, &c.info.host, c.info.port);
        c.link = Some(Box::new(link));
    }

    // Store the reader handle and mark as connected
    {
        let mut c = client_arc.lock().await;
        c.reader_handle = Some(reader_handle);
        c.info.connected = true;

        // ── Send KISS TNC parameters (TXDELAY, P, SlotTime) ──
        // These three settings were previously saved but never applied to the
        // TNC. Send them now that the link is up so they actually take effect.
        let _ = kiss::send_kiss_command(&mut c, kiss::CMD_TXDELAY, timing.txdelay).await;
        let _ = kiss::send_kiss_command(&mut c, kiss::CMD_P, timing.p).await;
        let _ = kiss::send_kiss_command(&mut c, kiss::CMD_SLOT_TIME, timing.slot_time).await;
    }

    // Register the connection in global state
    {
            let mut core = state.core.lock().await;
    let conns = &mut core.connections;
        conns.push(client_arc.clone());
    }

    // Start beacon timer if enabled in settings (reads settings from disk
    // each cycle so changes take effect without reconnect).
    {
        let beacon_arc = client_arc.clone();
        let beacon_ah = app_handle.clone();
        let handle = spawn_beacon(beacon_arc, beacon_ah, false);
        let mut c = client_arc.lock().await;
        c.beacon_handle = Some(handle);
    }

    // Notify frontend
    let _ = app_handle.emit("tnc-connected", serde_json::json!({ "id": id }));

    Ok(())
}

// ── AX.25 Connect (SABM) ────────────────────────────────

/// Initiate an AX.25 connected-mode session by sending a SABM frame.
///
/// The frontend sends this when the user types ":c CALLSIGN" in the compose box.
/// Sets the session state to AwaitingConnect and sends the SABM.
/// The session moves to Connected when a UA is received back (handled in session.rs).
#[tauri::command]
pub async fn ax25_connect(
    conn_id: String,
    tab: usize,
    remote: String,
    remote_ssid: u8,
    via: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    check_tab(tab)?;

    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;

    // Parse digipeater path: split on spaces/commas, uppercase, strip empty
    // (uses shared helper from session.rs — shared with AX.25 connect path)
    let digis: Vec<String> = session::parse_via(&via);

    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;

    // Set session state to AwaitingConnect
    {
        let session = &mut c.info.sessions[tab];
        session.state = session::Ax25ConnState::AwaitingConnect;
        session.remote = remote.to_uppercase();
        session.remote_ssid = remote_ssid;
        session.digis = digis.clone();
        session.reset();
    }

    // Send SABM
    let sabm = ax25::build_sabm(&remote, remote_ssid, &my_call, my_ssid, &digis);
    kiss::send_kiss(&mut c, &sabm).await?;

    // Emit sent SABM to the Unproto monitor panel
    emit_sent_frame(
        &app_handle,
        &my_call,
        my_ssid,
        &remote,
        remote_ssid,
        &digis,
        "SABM+",
        "",
        "",
        "SABM+",
        0,
        &sabm,
    );

    // Notify frontend that we're connecting
    if !c.info.sessions[tab].invisible {
        let _ = app_handle.emit(
            "session-connecting",
            serde_json::json!({
                "tab": tab,
                "remote": remote,
                "remote_ssid": remote_ssid,
            }),
        );
    }

    // Spawn T1 retry for SABM — resends SABM if no UA arrives
    let sabm_handle = spawn_connect_retry(
        RetryTarget::Sabm,
        client_arc.clone(),
        session::SessionLoc::Tab(tab),
        out_handle(&app_handle),
    );
    c.info.sessions[tab].sabm_handle = Some(sabm_handle);

    return Ok(());
}

// ── AX.25 Send I-Frame ──────────────────────────────────

/// Send an I-frame (Information frame) on an established connected-mode session.
///
/// Handles:
///   - Converting \n to \r (linpac line-termination convention)
///   - Appending \r if not already present
///   - Building the I-frame with current N(S)/N(R)
///   - Spawning a T1 retransmission timer (up to N2 retries)
///
/// The T1 retry task runs in the background and handles automatic
/// retransmission if no ACK is received within T1_MS milliseconds.
/// On N2_MAX failures, the session is disconnected.
#[tauri::command]
pub async fn ax25_send_iframe(
    conn_id: String,
    tab: usize,
    text: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    check_tab(tab)?;

    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;

    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;

    // Extract session state we need (guards + state read in one place)
    let (remote, remote_ssid, digis, ns, nr) =
        session_send_state(&mut c.info.sessions[tab], &app_handle)?;

    // Format text: convert \n to \r (linpac convention), ensure trailing \r
    let info_bytes = ax25::normalize_linpac_text(&text);

    // Build and send the I-frame
    let iframe = ax25::build_iframe(
        &remote,
        remote_ssid,
        &my_call,
        my_ssid,
        ns,
        nr,
        &info_bytes,
        &digis,
    );
    kiss::send_kiss(&mut c, &iframe).await?;

    // Emit sent I-frame to Unproto monitor
    emit_sent_frame(
        &app_handle,
        &my_call,
        my_ssid,
        &remote,
        remote_ssid,
        &digis,
        &format!("I N(S)={} N(R)={}", ns, nr),
        "pid=F0(Text)",
        &text,
        "I",
        info_bytes.len(),
        &iframe,
    );

    // Advance N(S) and store pending retry state (oldest frame only)
    let sent_ns = ns;
    store_pending_iframe(&mut c.info.sessions[tab], &text, sent_ns);

    // Read T1/N2 for the retry countdown emit below
    let t1_ms = c.t1_ms;
    let n2_max = c.n2_max;

    // Spawn kernel T1 recovery timer if none is running (single task
    // per session; polls with RR enquiry on the kernel T1 schedule,
    // retransmits only when the peer's reply shows frames still unacked).
    session::ensure_recovery_timer(
        &mut c,
        &client_arc,
        session::SessionLoc::Tab(tab),
        &out_handle(&app_handle),
    );

    // Capture the invisible flag before dropping the lock — the
    // session-sent / session-retry emits below must be suppressed for
    // invisible (server) sessions.
    let invisible = c.info.sessions[tab].invisible;

    // Drop the lock before emitting to prevent deadlock on re-entry
    drop(c);

    // Emit sent event to the frontend
    if !invisible {
        let _ = app_handle.emit(
            "session-sent",
            serde_json::json!({
                "tab": tab,
                "text": text,
            }),
        );
    }

    // Show retry countdown immediately (not after first T1 timeout)
    if !invisible {
        let _ = app_handle.emit(
            "session-retry",
            serde_json::json!({
                "tab": tab,
                "retry": 0,
                "max": n2_max,
                "text": text,
                "t1_ms": t1_ms,
            }),
        );
    }

    return Ok(());
}

// ── AX.25 Disconnect (DISC) ─────────────────────────────

/// Tear down an AX.25 connected-mode session by sending a DISC frame.
///
/// If the session is Connected, sends DISC and transitions to AwaitingDisconnect.
/// If the session is AwaitingConnect (never established), transitions directly
/// to Disconnected without sending anything.
#[tauri::command]
pub async fn ax25_disconnect(
    conn_id: String,
    tab: usize,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    check_tab(tab)?;

    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;

    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;

    let (remote, remote_ssid, digis) = {
        let session = &mut c.info.sessions[tab];
        // Kernel-faithful gate: DISC is valid from every established
        // state — Connected, TimerRecovery (an RR retry loop MUST be
        // tear-downable), and AwaitingConnect (an in-progress SABM
        // must be aborted on the far end). Only a fully idle session
        // is refused. AwaitingDisconnect is already tearing down —
        // treat as success without re-sending DISC.
        if session.state == session::Ax25ConnState::Disconnected {
            return Err("Not connected or connecting".to_string());
        }
        if session.state == session::Ax25ConnState::AwaitingDisconnect {
            return Ok(());
        }
        (
            session.remote.clone(),
            session.remote_ssid,
            session.digis.clone(),
        )
    };

    // Send the first DISC now.
    let disc = ax25::build_disc(&remote, remote_ssid, &my_call, my_ssid, &digis);
    kiss::send_kiss(&mut c, &disc).await?;

    // Emit sent DISC to Unproto.
    emit_sent_frame(
        &app_handle,
        &my_call,
        my_ssid,
        &remote,
        remote_ssid,
        &digis,
        "DISC+",
        "",
        "",
        "DISC+",
        0,
        &disc,
    );

    // Transition to AwaitingDisconnect.
    let session = &mut c.info.sessions[tab];
    session.state = session::Ax25ConnState::AwaitingDisconnect;
    session.pending_ns = None;

    // Clear the tab NOW — the frontend blanks on session-disconnected, so
    // the session snaps shut immediately instead of waiting for the UA.
    if !session.invisible {
        let _ = app_handle.emit(
            "session-disconnected",
            serde_json::json!({
                "tab": tab, "reason": "Disconnect requested",
            }),
        );
    }

    // Spawn a single best-effort backup DISC after T1 (total 2). If the
    // first DISC was eaten by noise, this gives the far end one more
    // chance to learn we're down — then we give up locally.
    let disc_handle = spawn_connect_retry(
        RetryTarget::Disc,
        client_arc.clone(),
        session::SessionLoc::Tab(tab),
        out_handle(&app_handle),
    );
    session.disc_handle = Some(disc_handle);

    return Ok(());
}

// ── AX.25 Send UI Frame ─────────────────────────────────

/// Send a UI (Unnumbered Information) frame — connectionless data.
///
/// UI frames are used for APRS, beacons, and other broadcast-style
/// transmissions that don't require a connected session.
#[tauri::command]
pub async fn ax25_send_ui(
    conn_id: String,
    dest: String,
    dest_ssid: u8,
    text: String,
    via: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;

    // Parse digipeater path using shared helper
    let digis: Vec<String> = session::parse_via(&via);

    // Build and send the UI frame
    let ui = ax25::build_ui(
        &dest,
        dest_ssid,
        &c.callsign,
        c.ssid,
        text.as_bytes(),
        &digis,
    );
    kiss::send_kiss(&mut c, &ui).await?;

    // Build radio header and emit to frontend
    let src_label = ax25::fmt_call(&c.callsign, c.ssid);
    let dest_label = ax25::fmt_call(&dest, dest_ssid);
    let via_str = ax25::format_via(&digis);
    let radio_header = format_radio_header(
        &src_label,
        &dest_label,
        &via_str,
        "UI+",
        "pid=F0(Text)",
        text.len(),
    );
    let raw_kiss = ax25::fmt_raw_kiss(&ui);

    let _ = app_handle.emit(
        "ui-sent",
        serde_json::json!({
            "dest": dest,
            "text": text,
            "radio_header": radio_header,
            "raw_kiss": raw_kiss,
        }),
    );

    return Ok(());
}

// ── KISS Disconnect ─────────────────────────────────────

/// Disconnect from the KISS TCP TNC and clean up all resources.
///
/// Aborts the reader task, all keep-alive timers, and the ID beacon.
/// Removes the connection from global AppState.
#[tauri::command]
pub async fn kiss_disconnect(
    id: String,
    state: tauri::State<'_, session::AppState>,
) -> Result<(), String> {
    let found = remove_connection_by_id(&state, &id).await;
    if found {
        Ok(())
    } else {
        Err("Connection not found".to_string())
    }
}

// ── AGW Connect ──────────────────────────────────────────

/// Connect to an AGWPE server and register our callsign.
#[tauri::command]
pub async fn agw_connect(
    id: String,
    label: String,
    host: String,
    port: u16,
    callsign: String,
    ssid: u8,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let addr = format!("{}:{}", host, port);

    // Drop any existing connection to the same host:port first (see
    // remove_connections_to) — prevents duplicate beacon timers when
    // auto-connect fires and the user also clicks Connect manually.
    remove_connections_to(&state, &host, port).await;

    let (reader, writer) = crate::app::comm::open_tcp_io(&addr).await?;
    let my_call = if callsign.is_empty() {
        "N0CALL".to_string()
    } else {
        callsign.clone()
    };

    let client = session::TcpClient::new(
        id.clone(),
        label,
        host,
        port,
        session::Protocol::AGW,
        writer,
        my_call.clone(),
        ssid,
        &out_handle(&app_handle),
        session::KissTiming::default(),
    );

    let client_arc = Arc::new(Mutex::new(client));

    // Send Register ('X') frame
    {
        let mut c = client_arc.lock().await;
        let register_frame = agw::build_register(&my_call, ssid);
        send_agw_frame(&mut c, &register_frame).await?;
        // Register the message board callsign too, so inbound connects to
        // it are routed to this client (Direwolf dispatches by registered
        // callsign). Read from settings (not the connect-time snapshot) and
        // register even while disabled — the connect gate decides whether
        // to serve or refuse, so enabling the board never requires a
        // desktop reconnect.
        if let crate::bbs_link::BbsGate::Disabled(call, ssid)
        | crate::bbs_link::BbsGate::Enabled(call, ssid) =
            crate::kernel::output::bbs_gate(&app_handle)
        {
            let bbs_register = agw::build_register(&call, ssid);
            let _ = send_agw_frame(&mut c, &bbs_register).await;
        }
    }

    // Enable Monitoring ('m') frame
    {
        let mut c = client_arc.lock().await;
        let mon_frame = agw::build_monitor_toggle();
        send_agw_frame(&mut c, &mon_frame).await?;
    }

    // Start AGW reader task (see spawn_agw_reader)
    let reader_handle = spawn_agw_reader(
        reader,
        client_arc.clone(),
        app_handle.clone(),
        id.clone(),
        my_call.clone(),
    );

    {
        let mut c = client_arc.lock().await;
        c.reader_handle = Some(reader_handle);
        c.info.connected = true;
    }

    // Start beacon timer if enabled in settings
    {
        let beacon_arc = client_arc.clone();
        let beacon_ah = app_handle.clone();
        let handle = spawn_beacon(beacon_arc, beacon_ah, true);
        let mut c = client_arc.lock().await;
        c.beacon_handle = Some(handle);
    }

    {
            let mut core = state.core.lock().await;
    let conns = &mut core.connections;
        conns.push(client_arc);
    }

    let _ = app_handle.emit("tnc-connected", serde_json::json!({ "id": id }));
    Ok(())
}

// ── AGW verbose-monitor parsing helpers ──────────────────
//
// Direwolf's AGWPE monitor echo ("RSP_UNPROTO_MON" etc.) and its own
// transmission echo ("RSP_OWN_TX") carry the same verbose header shape:
//   "1:Fm CALL To DEST Via ... <UI pid=xx Len=xx P=1>...\n<info>"
// Both arms parse the same fields; the two frame-type parsers differ
// deliberately — see below.


// ── AGW frame handlers ───────────────────────────────────

/// RSP_CONNECTED — a connect we initiated succeeded. Route it to the tab
/// that asked (last matching outgoing connect wins) and create the mapping.
async fn on_agw_connected(
    frame: &agw::AgwFrame,
    client: &Arc<Mutex<session::TcpClient>>,
    app_handle: &tauri::AppHandle,
    my_call: &str,
    tab_map: &mut Vec<agw::TabMapping>,
) {
    let remote = frame.call_from.trim();
    let (remote_call, remote_ssid) = agw::parse_callsign_ssid(remote);
    let my_label = agw::parse_callsign_ssid(my_call).0;
    let remote_label = remote_call.clone();

    // ── Message board: inbound connect to the board callsign ──
    // Direwolf accepted a connection addressed to the board callsign —
    // start a BBS session (no UI tab; engine text flows over AGW data
    // frames). The gate reads settings from DISK so enabling the board
    // mid-session works without a desktop reconnect; a connect to a
    // configured-but-disabled board is refused politely.
    let (ct_base, ct_ssid) = agw::parse_callsign_ssid(frame.call_to.trim());
    let board = crate::kernel::output::bbs_gate(app_handle);
    let is_bbs_connect = match &board {
        crate::bbs_link::BbsGate::Enabled(call, ssid) => ct_base == *call && ct_ssid == *ssid,
        crate::bbs_link::BbsGate::Disabled(call, ssid) if ct_base == *call && ct_ssid == *ssid => {
            let mut c = client.lock().await;
            log_line(&format!(
                "Message board: connect from {} refused (board disabled)",
                remote
            ));
            let d = agw::build_disconnect(0, call, *ssid, &remote_call, remote_ssid);
            let _ = send_agw_frame(&mut c, &d).await;
            return;
        }
        _ => false,
    };
    if is_bbs_connect {
        let mut c = client.lock().await;
        // Refresh the snapshot if settings changed since connect (e.g. the
        // board was enabled mid-session) — cfg/store follow current settings.
        if !c.bbs_enabled {
            log_line(&format!(
                "Message board: listener for {} active (settings picked up mid-session)",
                ct_base
            ));
            c.refresh_bbs_setup(app_handle);
        }
        let busy = c.server_sessions.iter().flatten().count() >= crate::bbs_link::MAX_BBS_SESSIONS;
        if busy {
            // No capacity — tell Direwolf to drop the link
            log_line(&format!(
                "Message board: connect from {} refused ({} users max)",
                remote,
                crate::bbs_link::MAX_BBS_SESSIONS
            ));
            let d =
                agw::build_disconnect(0, &c.bbs_callsign, c.bbs_ssid, &remote_call, remote_ssid);
            let _ = send_agw_frame(&mut c, &d).await;
            return;
        }
        let free = c.server_sessions.iter().position(|s| s.is_none()).unwrap();
        log_line(&format!(
            "Message board: {} connected to board callsign ({} of {} users)",
            remote,
            c.server_sessions.iter().flatten().count() + 1,
            crate::bbs_link::MAX_BBS_SESSIONS
        ));
        // Server-role session in the shared pool. AGW mode: Direwolf owns
        // the link state (link_external=true) — no kernel timers or
        // window-1 queue; replies go out via agw_bbs_send_line.
        let mut sess = session::Ax25Session::new(0);
        sess.role = session::SessionRole::Server;
        sess.invisible = true;
        sess.link_external = true;
        sess.bbs = Some(crate::bbs::BbsSession::new(&remote_call));
        sess.remote = remote_call.clone();
        sess.remote_ssid = remote_ssid;
        sess.max_packet_size = c.max_packet_size;
        sess.state = session::Ax25ConnState::Connected;
        sess.loc = session::SessionLoc::Server(free);
        sess.last_rx_time = Some(std::time::Instant::now());
        c.server_sessions[free] = Some(sess);
        if let Some(store) = c.bbs_store.as_mut() {
            store.record_heard(&remote_call);
        }
        let banner: Vec<String> = {
            let session::TcpClient {
                server_sessions,
                bbs_store,
                bbs_cfg,
                ..
            } = &mut *c;
            // Degrade gracefully instead of panicking if the board store or the
            // fresh server session failed to initialize — never take down the
            // whole reader task over a missing engine.
            let Some(s) = server_sessions[free].as_mut() else { return; };
            let Some(store) = bbs_store.as_mut() else { return; };
            let Some(engine) = s.bbs.as_mut() else { return; };
            let mut lines = engine.connect_output(store, bbs_cfg);
            // Command list line — the kernel server path appends the
            // prompt to the welcome; the AGW path must too, or the
            // connected client never learns the commands.
            lines.push(engine.prompt(bbs_cfg));
            lines
        };
        // Pack the banner to the session's max packet size — one AGW 'D'
        // frame per packed chunk (Direwolf transmits each as one I-frame).
        for packed in session::pack_bbs_lines(&banner, c.max_packet_size) {
            agw_bbs_send_line(&mut c, free, &packed, app_handle).await;
        }
        return;
    }

    // Find the tab that initiated this connection via outgoing_connects.
    // Uses .rfind() (last match) so that if the same callsign was
    // requested from two different tabs, the most recent connect
    // wins — .find() would always return the first (oldest) tab.
    let tab = {
        let c = client.lock().await;
        c.info
            .outgoing_connects
            .iter()
            .rfind(|(remote, _)| remote == &remote_call.to_uppercase())
            .map(|(_, tab)| *tab)
    };
    let tab = match tab {
        Some(t) => t,
        None => return,
    };

    // Clean up the tracked outgoing connect
    {
        let mut c = client.lock().await;
        c.info.outgoing_connects.retain(|(_, t)| *t != tab);
    }

    tab_map.push(agw::TabMapping {
        tab,
        call_from: remote_label.clone(),
        call_to: my_label.clone(),
        remote: remote_call.clone(),
        remote_ssid,
    });

    // Suppress the UI event for invisible (server) sessions.
    let mut invisible = false;
    {
        let mut c = client.lock().await;
        if tab < c.info.sessions.len() {
            let s = &mut c.info.sessions[tab];
            s.state = session::Ax25ConnState::Connected;
            s.remote = remote_call.clone();
            s.remote_ssid = remote_ssid;
            invisible = s.invisible;
        }
    }

    if !invisible {
        let _ = app_handle.emit(
            "session-connected",
            serde_json::json!({
                "tab": tab, "remote": remote_call, "remote_ssid": remote_ssid,
            }),
        );
    }
}

/// RSP_DATA — connected-mode data from Direwolf's AX.25 stack.
async fn on_agw_data(
    frame: &agw::AgwFrame,
    client: &Arc<Mutex<session::TcpClient>>,
    app_handle: &tauri::AppHandle,
    tab_map: &mut Vec<agw::TabMapping>,
) {
    // ── Message board: feed the engine ──
    // DIRECTIONAL, matching the KISS path (route_bbs_frame gates on
    // dest == board callsign). Only feed frames ADDRESSED TO the board
    // (client→board): a self-connect through a digipeater (e.g. KK6SEN
    // → own KK6SEN-1 via KBANN) loops the board's OWN reply frames back
    // into this app with board as the SOURCE — re-feeding those would
    // inject the banner/prompt text back into the board engine as bogus
    // "input" ("Not a valid command", growing each digipeater cycle).
    // The disconnected handler keeps the bidirectional match (either side
    // may report the drop), but data routing is strictly destination-gated.
    let bbs_i = {
        let c = client.lock().await;
        if !dest_is_board(
            c.bbs_enabled,
            &c.bbs_callsign,
            c.bbs_ssid,
            frame.call_to.trim(),
        ) {
            None
        } else {
            agw_bbs_slot(&c, frame.call_from.trim(), frame.call_to.trim())
        }
    };
    if let Some(i) = bbs_i {
        let text = String::from_utf8_lossy(&frame.data).to_string();
        let (out_lines, hangup) = {
            let mut c = client.lock().await;
            let session::TcpClient {
                server_sessions,
                bbs_store,
                bbs_cfg,
                ..
            } = &mut *c;
            let Some(s) = server_sessions.get_mut(i).and_then(|o| o.as_mut()) else {
                return;
            };
            let Some(store) = bbs_store.as_mut() else {
                return;
            };
            // Shared engine feed (KISS + AGW — ONE implementation).
            session::run_engine_lines(s, store, bbs_cfg, &text)
        };
        if hangup {
            // B(ye) — ask Direwolf to drop the link, then clear the session
            let mut c = client.lock().await;
            let (remote, remote_ssid) = {
                let s = c.server_sessions[i].as_ref().unwrap();
                (s.remote.clone(), s.remote_ssid)
            };
            let d = agw::build_disconnect(0, &c.bbs_callsign, c.bbs_ssid, &remote, remote_ssid);
            let _ = send_agw_frame(&mut c, &d).await;
            c.server_sessions[i] = None;
            return;
        }
        // Pack reply lines to the session's max packet size (KPC-3P style,
        // same as the KISS path — one AGW 'D' frame per packed chunk).
        for packed in {
            let c = client.lock().await;
            let limit = c.server_sessions[i]
                .as_ref()
                .map(|s| s.max_packet_size)
                .unwrap_or(session::BBS_MAX_INFO);
            session::pack_bbs_lines(&out_lines, limit)
        } {
            let mut c = client.lock().await;
            agw_bbs_send_line(&mut c, i, &packed, app_handle).await;
        }
        return;
    }

    let cf = agw::parse_callsign_ssid(frame.call_from.trim()).0;
    let ct = agw::parse_callsign_ssid(frame.call_to.trim()).0;
    let tab = tab_map
        .iter()
        .find(|m| (m.call_from == cf || m.call_from == ct) && (m.call_to == cf || m.call_to == ct))
        .map(|m| m.tab);

    if let Some(tab) = tab {
        let text = String::from_utf8_lossy(&frame.data)
            .to_string()
            .replace("\r\n", "\n")
            .replace('\r', "\n");

        // Emit immediately — the frontend debounces rapid I-frame
        // bursts with a 200ms coalescing timer, so we don't need
        // backend-side buffering for AGW. This keeps the connected
        // tab in sync with the unproto monitor.
        let invisible = {
            let c = client.lock().await;
            tab < c.info.sessions.len() && c.info.sessions[tab].invisible
        };
        if !invisible {
            let _ = app_handle.emit(
                "session-iframe",
                serde_json::json!({
                    "tab": tab, "text": text, "ns": 0,
                }),
            );
        }
    }
}

/// RSP_DISCONNECTED — Direwolf reports the link dropped (or a connect
/// attempt failed). Reset the mapped session, or the awaiting-connect
/// session Direwolf never confirmed.
async fn on_agw_disconnected(
    frame: &agw::AgwFrame,
    client: &Arc<Mutex<session::TcpClient>>,
    app_handle: &tauri::AppHandle,
    tab_map: &mut Vec<agw::TabMapping>,
) {
    let cf = agw::parse_callsign_ssid(frame.call_from.trim()).0;
    let ct = agw::parse_callsign_ssid(frame.call_to.trim()).0;

    // ── Message board: Direwolf dropped a BBS link ──
    let bbs_i = {
        let c = client.lock().await;
        agw_bbs_slot(&c, frame.call_from.trim(), frame.call_to.trim())
    };
    if let Some(i) = bbs_i {
        let mut c = client.lock().await;
        c.server_sessions[i] = None;
        return;
    }

    if let Some(pos) = tab_map.iter().position(|m| {
        (m.call_from == cf || m.call_from == ct) && (m.call_to == cf || m.call_to == ct)
    }) {
        let mapping = tab_map.remove(pos);
        let tab = mapping.tab;

        // Suppress the UI event for invisible (server) sessions.
        let mut invisible = false;
        {
            let mut c = client.lock().await;
            if tab < c.info.sessions.len() {
                let s = &mut c.info.sessions[tab];
                s.state = session::Ax25ConnState::Disconnected;
                s.remote.clear();
                s.remote_ssid = 0;
                s.reset();
                invisible = s.invisible;
            }
        }
        if !invisible {
            let _ = app_handle.emit(
                "session-disconnected",
                serde_json::json!({
                    "tab": tab, "reason": "Remote disconnected",
                }),
            );
        }
    }
    // No tab mapping — check if this is a failed connect attempt
    // (session was in AwaitingConnect but Direwolf never got a UA)
    else {
        let mut c = client.lock().await;
        for (tab, s) in c.info.sessions.iter_mut().enumerate() {
            if s.state == session::Ax25ConnState::AwaitingConnect
                && (s.remote == cf || s.remote == ct)
            {
                s.state = session::Ax25ConnState::Disconnected;
                s.remote.clear();
                s.remote_ssid = 0;
                s.reset();
                if !s.invisible {
                    let _ = app_handle.emit(
                        "session-disconnected",
                        serde_json::json!({
                            "tab": tab, "reason": "Failed to connect",
                        }),
                    );
                }
                break;
            }
        }
    }
}

/// RSP_REGISTER_RESULT — callsign registration with the AGWPE engine.
fn on_agw_register_result(frame: &agw::AgwFrame, app_handle: &tauri::AppHandle) {
    let success = frame.data.first().copied().unwrap_or(0) == 1;
    if success {
        let _ = app_handle.emit(
            "system-message",
            serde_json::json!({
                "text": format!("AGW registered: {}", frame.call_from.trim()),
            }),
        );
    }
}

/// RSP_UNPROTO_MON / RSP_INFO_MON / RSP_SUPER_MON — frames heard on
/// channel. Parsed from the AGWPE verbose-monitor header.
async fn on_agw_monitor(frame: &agw::AgwFrame, app_handle: &tauri::AppHandle) {
    // ── Always try to strip AGWPE verbose-monitor header ──
    // The verbose format looks like:
    //   "1:Fm CALL To DEST Via ... <UI pid=xx Len=xx>...\n<info>"
    // or sometimes "...[timestamp]<info>" without newline.
    // Parse what we can from the first line, and always try to split
    // the raw info field from any header prefix.
    let data_str_lossy = String::from_utf8_lossy(&frame.data);
    let first_line = data_str_lossy.lines().next().unwrap_or("");

    let clean_first = strip_agw_line_prefix(first_line);
    let is_verbose = clean_first.starts_with("Fm ");

    // ── Extract verbose-line metadata (via, pid, frame type) ──
    let (verbose_via, verbose_pid_str, verbose_frame_type) = if is_verbose {
        let via = parse_verbose_via(clean_first);
        let pid_str = parse_verbose_pid(clean_first);
        // Full parser folds the P/F token (and I-frame N(S)/N(R)) into the
        // label — "UI+" / "I00+" — matching the KISS path and OWN_TX
        // (v0.9.3 +/- parity). The fallback below stays bare because the
        // non-verbose path has no P-bit information to append.
        let vft = parse_verbose_frame_type(clean_first);
        (via, pid_str, vft)
    } else {
        (String::new(), String::new(), String::new())
    };

    // ── Extract raw info field (strip verbose header if present) ──
    let payload_bytes: Vec<u8> =
        split_after_verbose_header(&frame.data).unwrap_or_else(|| frame.data.clone());

    // ── Callsigns from AGW frame header ──
    let cf = frame.call_from.trim();
    let ct = frame.call_to.trim();
    let (src_call, src_ssid) = agw::parse_callsign_ssid(cf);
    let (dst_call, dst_ssid) = agw::parse_callsign_ssid(ct);
    let src_label = agw::fmt_display_call(&src_call, src_ssid);
    let dst_label = agw::fmt_display_call(&dst_call, dst_ssid);

    // ── Frame type (UI / I / RR / SABM etc) ──
    let ft = if !verbose_frame_type.is_empty() {
        verbose_frame_type
    } else {
        match frame.kind {
            agw::RSP_UNPROTO_MON => "UI".to_string(),
            agw::RSP_INFO_MON => "I".to_string(),
            _ => fallback_frame_type(frame),
        }
    };

    // ── PID label ──
    // Use verbose PID if available (more accurate than AGW header PID)
    let actual_pid_byte = actual_pid(&verbose_pid_str, frame.pid);

    // Strip AGWPE line-number prefixes from the payload
    let payload_prefix_stripped = strip_agw_line_prefixes(&payload_bytes);

    // ── NET/ROM detection and display ──
    let (display_text, pid_label, frame_type) = format_monitor_payload(
        &payload_bytes,
        &payload_prefix_stripped,
        actual_pid_byte,
        &ft,
    );

    // ── Radio header with via path ──
    let radio_header = agw_radio_header(
        &src_label,
        &dst_label,
        &verbose_via,
        &frame_type,
        &pid_label,
        payload_prefix_stripped.len(),
    );

    // ── Emit unproto frame ──
    // RSP_UNPROTO_MON shows frames heard on channel — including digipeater
    // repeats of our own frames (different via paths with asterisks).
    // No dedup here — each frame has potentially different via path info
    // that the user wants to see (which digipeaters repeated).
    let formatted = format!("{}>{}: {}", src_label, dst_label, display_text);
    crate::session::emit_unproto_frame(
        app_handle,
        &src_label,
        &dst_label,
        &display_text,
        &formatted,
        &frame_type,
        &[],
        &radio_header,
        "",
        false,
    );

    // NOTE: The frontend handles I-frame burst coalescing (200ms debounce),
    // so no backend-side flush-on-supervisory is needed for AGW. The old
    // rx_buffers code was never populated and has been removed. Cleanup.
}

/// RSP_OWN_TX — Direwolf echoes our own transmission. Same verbose parsing
/// as monitored frames, but with the full frame-type label (P/F-folded) and
/// sent=true so the frontend styles it as an outgoing packet.
async fn on_agw_own_tx(frame: &agw::AgwFrame, app_handle: &tauri::AppHandle) {
    let cf = frame.call_from.trim();
    let ct = frame.call_to.trim();
    let (src_call, src_ssid) = agw::parse_callsign_ssid(cf);
    let (dst_call, dst_ssid) = agw::parse_callsign_ssid(ct);
    let src_label = agw::fmt_display_call(&src_call, src_ssid);
    let dst_label = agw::fmt_display_call(&dst_call, dst_ssid);

    // Parse the verbose AGWPE echo to extract via path, PID, frame type
    let data_str = String::from_utf8_lossy(&frame.data);
    let first_line = data_str.lines().next().unwrap_or("");

    let clean_first = strip_agw_line_prefix(first_line);

    // Extract via path from verbose line
    let via = if clean_first.starts_with("Fm ") {
        parse_verbose_via(clean_first)
    } else {
        String::new()
    };

    // Parse verbose PID: "pid=xx"
    let verbose_pid = parse_verbose_pid(clean_first);

    // Parse verbose frame type with P/F folding (see verbose_ft_full)
    let verbose_ft = parse_verbose_frame_type(clean_first);

    // Determine actual PID byte
    let actual_pid_byte = actual_pid(&verbose_pid, frame.pid);

    // Extract payload bytes (after verbose header)
    let payload_bytes: Vec<u8> = split_after_verbose_header(&frame.data).unwrap_or_else(|| {
        // Fallback: use raw data minus the verbose header via bracket
        let data_s = String::from_utf8_lossy(&frame.data);
        if let Some(bracket) = data_s.rfind(']') {
            let raw_start = bracket + 1;
            if raw_start < frame.data.len() {
                frame.data[raw_start..]
                    .iter()
                    .take_while(|&&b| b != 0)
                    .copied()
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            frame
                .data
                .iter()
                .take_while(|&&b| b != 0)
                .copied()
                .collect()
        }
    });

    // ── NET/ROM detection ──
    let (display_text, pid_label, frame_type) =
        format_monitor_payload(&payload_bytes, &payload_bytes, actual_pid_byte, &verbose_ft);

    let radio_header = agw_radio_header(
        &src_label,
        &dst_label,
        &via,
        &frame_type,
        &pid_label,
        payload_bytes.len(),
    );

    // ── Register sent frame (frontend dedup toggle handles display dedup) ──
    let formatted = format!("{}>{}: {}", src_label, dst_label, display_text);
    crate::session::emit_unproto_frame(
        app_handle,
        &src_label,
        &dst_label,
        &display_text,
        &formatted,
        &frame_type,
        &[],
        &radio_header,
        "",
        true,
    );
}

/// RSP_ERROR — AGWPE engine reported an error.
fn on_agw_error(frame: &agw::AgwFrame, app_handle: &tauri::AppHandle) {
    let err = String::from_utf8_lossy(&frame.data).to_string();
    let _ = app_handle.emit(
        "system-message",
        serde_json::json!({ "text": format!("AGW: {}", err) }),
    );
}

/// Route an incoming AGWPE frame to the handler for its kind.
async fn route_agw_frame(
    frame: &agw::AgwFrame,
    client: &Arc<Mutex<session::TcpClient>>,
    app_handle: &tauri::AppHandle,
    my_call: &str,
    tab_map: &mut Vec<agw::TabMapping>,
) {
    match frame.kind {
        agw::RSP_CONNECTED => on_agw_connected(frame, client, app_handle, my_call, tab_map).await,
        agw::RSP_DATA => on_agw_data(frame, client, app_handle, tab_map).await,
        agw::RSP_DISCONNECTED => on_agw_disconnected(frame, client, app_handle, tab_map).await,
        agw::RSP_REGISTER_RESULT => on_agw_register_result(frame, app_handle),
        agw::RSP_UNPROTO_MON | agw::RSP_INFO_MON | agw::RSP_SUPER_MON => {
            on_agw_monitor(frame, app_handle).await
        }
        agw::RSP_OWN_TX => on_agw_own_tx(frame, app_handle).await,
        agw::RSP_ERROR => on_agw_error(frame, app_handle),
        // A kind this build doesn't handle (new AGWPE response, version
        // mismatch) — log it so missing frames are diagnosable instead
        // of silently vanishing.
        _ => log::warn!("AGW: unhandled frame kind {:?}", frame.kind),
    }
}

// ── AGW Send Frame ─────────────────────────────────────
async fn send_agw_frame(client: &mut session::TcpClient, data: &[u8]) -> Result<(), String> {
    let w = client.writer()?;
    w.write_all(data)
        .await
        .map_err(|e| format!("Send failed: {}", e))
}

// ── AGW Message Board helpers ──────────────────────────

/// Find the BBS session whose call pair matches an AGW frame's
/// call_from/call_to (one side is the board callsign, the other the
/// session's remote). Compares base callsign + SSID.
fn agw_bbs_slot(c: &session::TcpClient, call_from: &str, call_to: &str) -> Option<usize> {
    if !c.bbs_enabled {
        return None;
    }
    let (cf, cfs) = agw::parse_callsign_ssid(call_from.trim());
    let (ct, cts) = agw::parse_callsign_ssid(call_to.trim());
    c.server_sessions.iter().position(|s| {
        s.as_ref().is_some_and(|x| {
            let bbs_matches = |base: &str, ss: u8| base == c.bbs_callsign && ss == c.bbs_ssid;
            let remote_matches = |base: &str, ss: u8| base == x.remote && ss == x.remote_ssid;
            (bbs_matches(&cf, cfs) && remote_matches(&ct, cts))
                || (bbs_matches(&ct, cts) && remote_matches(&cf, cfs))
        })
    })
}

/// True when a frame's DESTINATION (call_to) is the board callsign. Used to
/// gate the board-data feed: only frames addressed TO the board are treated
/// as client→board input. Mirrors the KISS board gate (dest == board), so a
/// self-connect echo (board as the frame SOURCE) is never re-fed as input.
fn dest_is_board(bbs_enabled: bool, bbs_call: &str, bbs_ssid: u8, call_to: &str) -> bool {
    if !bbs_enabled {
        return false;
    }
    let (ct, cts) = agw::parse_callsign_ssid(call_to.trim());
    ct == bbs_call && cts == bbs_ssid
}

#[cfg(test)]
mod agw_bbs_direction_tests {
    use super::*;

    /// dest_is_board must distinguish the frame DIRECTION: only frames whose
    /// destination (call_to) is the board are client→board input. The board's
    /// own reply frames looped back through a digipeater have the board as the
    /// SOURCE (call_to = client), so they must NOT be considered board input.
    #[test]
    fn dest_is_board_requires_destination_is_board() {
        // Client → board: dest is the board, so it's real board input.
        assert!(dest_is_board(true, "KK6SEN", 1, "KK6SEN-1"));
        assert!(dest_is_board(true, "KK6SEN", 1, "KK6SEN-1 "));
        // Board → client (self-connect echo through digi): dest is the client,
        // so it must NOT be fed to the board engine as input.
        assert!(!dest_is_board(true, "KK6SEN", 1, "KK6SEN"));
        // Board disabled → nothing is board input.
        assert!(!dest_is_board(false, "KK6SEN", 1, "KK6SEN-1"));
    }
}

/// Send one BBS output line as an AGW data frame from the board callsign,
/// with an unproto-monitor echo (AGW mode has no KISS TX echo).
async fn agw_bbs_send_line(
    c: &mut session::TcpClient,
    i: usize,
    line: &str,
    app_handle: &tauri::AppHandle,
) {
    let Some(s) = c.server_sessions.get(i).and_then(|o| o.as_ref()) else {
        return;
    };
    let (remote, remote_ssid) = (s.remote.clone(), s.remote_ssid);
    let mut text = line.to_string();
    text.push_str("\r\n");
    let frame = agw::build_data(
        0,
        &c.bbs_callsign,
        c.bbs_ssid,
        &remote,
        remote_ssid,
        text.as_bytes(),
    );
    if send_agw_frame(c, &frame).await.is_err() {
        return;
    }
    let src_label = agw::fmt_display_call(&c.bbs_callsign, c.bbs_ssid);
    let dst_label = agw::fmt_display_call(&remote, remote_ssid);
    let radio_header = agw_radio_header(&src_label, &dst_label, "", "I", "F0(Text)", text.len());
    crate::session::emit_unproto_frame(
        app_handle,
        &src_label,
        &dst_label,
        &text,
        &format!("{}>{}: {}", src_label, dst_label, text.trim_end()),
        "I",
        &[],
        &radio_header,
        "",
        true,
    );
}

// ── AGW AX.25 Connect (Send 'C' frame) ──────────────────

/// Initiate AX.25 connection via AGWPE.
#[tauri::command]
pub async fn agw_ax25_connect(
    conn_id: String,
    tab: usize,
    remote: String,
    remote_ssid: u8,
    via: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    check_tab(tab)?;
    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;
    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;

    {
        let s = &mut c.info.sessions[tab];
        s.state = session::Ax25ConnState::AwaitingConnect;
        s.remote = remote.to_uppercase();
        s.remote_ssid = remote_ssid;
        // NOTE: s.digis is intentionally NOT set here — AGWPE/Direwolf
        // handles digipeater routing internally. The via string is parsed
        // below and used only to build the AGW connect-via frame.
        s.reset();
    }

    // Track this outgoing connect so RSP_CONNECTED routes to the right tab
    c.info.outgoing_connects.retain(|(_, t)| *t != tab);
    c.info.outgoing_connects.push((remote.to_uppercase(), tab));

    // Parse digipeater path using shared helper
    let digis: Vec<String> = session::parse_via(&via);

    let agw_frame = if digis.is_empty() {
        agw::build_connect(0, &my_call, my_ssid, &remote, remote_ssid)
    } else {
        agw::build_connect_via(0, &my_call, my_ssid, &remote, remote_ssid, &digis)
    };
    send_agw_frame(&mut c, &agw_frame).await?;

    // App-side echo: Direwolf's RSP_OWN_TX gate only echoes UI frames
    // (server.c `datakind != 'U'` → break) — the SABM we asked Direwolf
    // to transmit is never echoed back to us. Emit it ourselves so the
    // connect attempt shows in the unproto panel (KISS-mode parity).
    // Label is SABM+ (P=1 connect request); Direwolf may actually send
    // SABME first if its config uses v2.2 for this station.
    {
        let (my_base, my_s) = agw::parse_callsign_ssid(&my_call);
        let src_label = agw::fmt_display_call(&my_base, if my_s != 0 { my_s } else { my_ssid });
        let (r_base, r_s) = agw::parse_callsign_ssid(&remote);
        let dst_label = agw::fmt_display_call(&r_base, if r_s != 0 { r_s } else { remote_ssid });
        let via_disp = digis.join(",");
        let radio_header = agw_radio_header(&src_label, &dst_label, &via_disp, "SABM+", "00", 0);
        crate::session::emit_unproto_frame(
            &app_handle,
            &src_label,
            &dst_label,
            "",
            &format!("{}>{}: [SABM+]", src_label, dst_label),
            "SABM+",
            &digis,
            &radio_header,
            "",
            true,
        );
    }

    if !c.info.sessions[tab].invisible {
        let _ = app_handle.emit(
            "session-connecting",
            serde_json::json!({ "tab": tab, "remote": remote, "remote_ssid": remote_ssid }),
        );
    }

    return Ok(());
}

// ── AGW Send Connected Data ──────────────────────────────

/// Send data on established AGW session.
#[tauri::command]
pub async fn agw_send_data(
    conn_id: String,
    tab: usize,
    text: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    check_tab(tab)?;
    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;
    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;
    let (remote, remote_ssid) = {
        let s = &c.info.sessions[tab];
        if s.state != session::Ax25ConnState::Connected {
            return Err("Not connected".to_string());
        }
        (s.remote.clone(), s.remote_ssid)
    };

    let info_bytes = ax25::normalize_linpac_text(&text);

    let agw_frame = agw::build_data(0, &my_call, my_ssid, &remote, remote_ssid, &info_bytes);
    send_agw_frame(&mut c, &agw_frame).await?;

    // App-side echo: Direwolf never echoes I-frames back ('T' gate is
    // UI-only), so emit our own transmission here with sent=true so it
    // shows in the unproto panel in red, alongside the remote's replies.
    // Label is bare "I" — N(S)/N(R) belong to Direwolf's stack in AGWPE
    // mode, so the app can't show sequence digits like received frames do.
    {
        let display_text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (my_base, my_s) = agw::parse_callsign_ssid(&my_call);
        let src_label = agw::fmt_display_call(&my_base, if my_s != 0 { my_s } else { my_ssid });
        let (r_base, r_s) = agw::parse_callsign_ssid(&remote);
        let dst_label = agw::fmt_display_call(&r_base, if r_s != 0 { r_s } else { remote_ssid });
        let radio_header = agw_radio_header(
            &src_label,
            &dst_label,
            "",
            "I",
            "F0(Text)",
            info_bytes.len(),
        );
        crate::session::emit_unproto_frame(
            &app_handle,
            &src_label,
            &dst_label,
            &display_text,
            &format!("{}>{}: {}", src_label, dst_label, display_text),
            "I",
            &[],
            &radio_header,
            "",
            true,
        );
    }

    // The session-sent event below still drives the connected-mode message window.
    if !c.info.sessions[tab].invisible {
        let _ = app_handle.emit(
            "session-sent",
            serde_json::json!({ "tab": tab, "text": text }),
        );
    }
    return Ok(());
}

// ── AGW Disconnect Session ──────────────────────────────

/// Disconnect an AGW session.
#[tauri::command]
pub async fn agw_disconnect(
    conn_id: String,
    tab: usize,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    check_tab(tab)?;
    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;
    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;
    let (remote, remote_ssid, connected) = {
        let s = &mut c.info.sessions[tab];
        // Kernel-faithful gate: allow disconnect from every established
        // state, including TimerRecovery (an RR retry loop MUST be
        // tear-downable). Only a fully idle session is refused.
        let connected = s.state != session::Ax25ConnState::Disconnected;
        (s.remote.clone(), s.remote_ssid, connected)
    };
    if !connected {
        return Err("Not connected".to_string());
    }

    let agw_frame = agw::build_disconnect(0, &my_call, my_ssid, &remote, remote_ssid);
    send_agw_frame(&mut c, &agw_frame).await?;

    // App-side echo: same rationale as the connect echo — Direwolf's
    // RSP_OWN_TX is UI-only, so our DISC never comes back. Emit it
    // ourselves so the disconnect attempt is visible in the panel.
    {
        let (my_base, my_s) = agw::parse_callsign_ssid(&my_call);
        let src_label = agw::fmt_display_call(&my_base, if my_s != 0 { my_s } else { my_ssid });
        let (r_base, r_s) = agw::parse_callsign_ssid(&remote);
        let dst_label = agw::fmt_display_call(&r_base, if r_s != 0 { r_s } else { remote_ssid });
        let radio_header = agw_radio_header(&src_label, &dst_label, "", "DISC+", "00", 0);
        crate::session::emit_unproto_frame(
            &app_handle,
            &src_label,
            &dst_label,
            "",
            &format!("{}>{}: [DISC+]", src_label, dst_label),
            "DISC+",
            &[],
            &radio_header,
            "",
            true,
        );
    }

    c.info.sessions[tab].state = session::Ax25ConnState::Disconnected;
    c.info.sessions[tab].remote.clear();
    c.info.sessions[tab].pending_ns = None;

    return Ok(());
}

// ── AGW Send UI Frame ───────────────────────────────────

/// Send unproto (UI) frame via AGWPE.
#[tauri::command]
pub async fn agw_send_ui(
    conn_id: String,
    dest: String,
    dest_ssid: u8,
    text: String,
    via: String,
    state: tauri::State<'_, session::AppState>,
    _app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let client_arc = find_client(&state, &conn_id)
        .await
        .ok_or_else(|| "Connection not found".to_string())?;
    let mut c = client_arc.lock().await;
    let my_call = c.callsign.clone();
    let my_ssid = c.ssid;

    let digis: Vec<String> = session::parse_via(&via);

    let agw_frame = if digis.is_empty() {
        agw::build_unproto(0, &my_call, my_ssid, &dest, dest_ssid, text.as_bytes())
    } else {
        agw::build_unproto_via(
            0,
            &my_call,
            my_ssid,
            &dest,
            dest_ssid,
            &digis,
            text.as_bytes(),
        )
    };
    send_agw_frame(&mut c, &agw_frame).await?;

    // NOTE: no app-side sent echo here — AGWPE mode relies on the TNC's
    // RSP_OWN_TX echo so each transmission appears exactly once in the panel.

    return Ok(());
}

// ── AGW Disconnect from Server ──────────────────────────

/// Disconnect from the AGWPE server.
#[tauri::command]
pub async fn agw_disconnect_server(
    id: String,
    state: tauri::State<'_, session::AppState>,
) -> Result<(), String> {
    let found = remove_connection_by_id(&state, &id).await;
    if found {
        Ok(())
    } else {
        Err("Connection not found".to_string())
    }
}

// ── Daily Log Files ─────────────────────────────────────────
//
// The app writes one log file per day per kind into the app data dir
// (where settings.json lives): YYMMDD_raw.log, YYMMDD_unproto.log,
// YYMMDD_connected.log, YYMMDD_admin.log, YYMMDD_settings.log. The raw
// log is written by Rust (log_line); the others are fed by frontend
// event listeners via append_log.

/// Append a line to today's log file of the given kind. Kinds are
/// whitelisted so the frontend can never write outside the log dir.
#[tauri::command]
pub fn append_log(kind: String, text: String) -> Result<(), String> {
    if !matches!(
        kind.as_str(),
        "raw" | "unproto" | "connected" | "admin" | "settings"
    ) {
        return Err("Unknown log kind".to_string());
    }
    crate::append_log(&kind, &text);
    Ok(())
}

/// Open the app's log folder in the system file manager.
#[tauri::command]
pub fn open_log_dir() -> Result<(), String> {
    let dir = crate::LOG_DIR
        .get()
        .ok_or_else(|| "Log dir not initialized".to_string())?;
    opener::open(dir).map_err(|e| format!("Cannot open log folder: {e}"))
}

// ── PBBS Admin Console ─────────────────────────────────────
//
// A LOCAL, no-RF administration console for the message board. The admin
// session is a plain BBS engine session (bbs::BbsSession) with NO link
// layer — commands run straight against the shared store via handle_line,
// exactly like an RF user's input, but nothing touches the radio. The
// session identifies as the station callsign, which is always a sysop
// (see BbsConfig::from_app_settings), so delete/pin work and privacy mode
// never hides mail from the local admin.

/// One admin-console exchange: reply lines + whether the session hung up
/// (B(ye) — the frontend closes the overlay).
#[derive(serde::Serialize)]
pub struct PbbsAdminView {
    pub lines: Vec<String>,
    pub hangup: bool,
}

/// Load a board store + config fresh from current settings, and ensure the
/// shared admin-console session (`AppCore::admin_bbs`) is ready. This is the
/// LOCAL, no-RF admin console — it does NOT require a connected TNC; the
/// board data lives on disk, so an enabled board in Settings is enough.
/// Returns `(admin session ref, store, cfg, operator callsign)`.
async fn ensure_admin_console<'a>(
    core: &'a mut crate::app::state::AppCore,
    app_handle: &tauri::AppHandle,
) -> Result<(&'a mut Option<crate::bbs::BbsSession>, crate::bbs::BbsStore, crate::bbs::BbsConfig, String), String> {
    let s = crate::settings::load_settings_or_default(app_handle);
    let setup = crate::bbs_link::BbsSetup::from_settings(&s, &out_handle(app_handle).data_dir());
    if !setup.enabled {
        return Err("Message board is not enabled — enable it in Settings first".to_string());
    }
    let store = setup
        .store
        .ok_or_else(|| "Message board store not loaded".to_string())?;
    let operator = if s.callsign.trim().is_empty() {
        "N0CALL".to_string()
    } else {
        s.callsign.trim().to_uppercase()
    };
    Ok((&mut core.admin_bbs, store, setup.cfg, operator))
}

/// Open the admin console: ALWAYS starts a fresh session (like the Clear
/// button) — banner + message list + prompt, no preserved state from a
/// previous visit. Local only, nothing goes over RF.
#[tauri::command]
pub async fn pbbs_admin_open(
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<PbbsAdminView, String> {
    pbbs_admin_reset(state, app_handle).await
}

/// Reset the admin session to a fresh one and return the full main menu
/// (banner + message list + prompt) — the Clear button's "reload the
/// main menu" behavior. Local only, nothing goes over RF.
#[tauri::command]
pub async fn pbbs_admin_reset(
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<PbbsAdminView, String> {
    let mut core = state.core.lock().await;
    let (admin_bbs, store, cfg, operator) =
        ensure_admin_console(&mut core, &app_handle).await?;
    *admin_bbs = Some(crate::bbs::BbsSession::new(&operator));
    let sess = admin_bbs.as_ref().unwrap();
    let mut lines = Vec::new();
    lines.extend(sess.connect_output(&store, &cfg));
    lines.push(sess.prompt(&cfg));
    Ok(PbbsAdminView {
        lines,
        hangup: false,
    })
}

/// Send one line to the local admin session. Returns the reply lines and
/// the prompt when the session is back at the command menu. `hangup: true`
/// means B(ye) — the session resets and the frontend closes the overlay.
/// Nothing here goes over RF.
#[tauri::command]
pub async fn pbbs_admin_send(
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
    line: String,
) -> Result<PbbsAdminView, String> {
    let mut core = state.core.lock().await;
    let (admin_bbs, mut store, cfg, operator) =
        ensure_admin_console(&mut core, &app_handle).await?;
    if admin_bbs.is_none() {
        *admin_bbs = Some(crate::bbs::BbsSession::new(&operator));
    }
    let reply = {
        let sess = admin_bbs.as_mut().unwrap();
        sess.handle_line(&line, &mut store, &cfg)
    };
    let mut lines = reply.lines;
    if reply.hangup {
        // B(ye) — reset the local session so the console starts fresh next open.
        *admin_bbs = Some(crate::bbs::BbsSession::new(&operator));
        return Ok(PbbsAdminView {
            lines,
            hangup: true,
        });
    }
    if admin_bbs.as_ref().unwrap().at_command() {
        lines.push(admin_bbs.as_ref().unwrap().prompt(&cfg));
    }
    Ok(PbbsAdminView {
        lines,
        hangup: false,
    })
}

/// Live count of connected message-board users, for the PBBS badge.
/// Sums occupied slots in the shared board `server_sessions` pool across the
/// active connection(s) — 0..=MAX_BBS_SESSIONS. Read-only: does not touch the
/// kernel, so it reflects the same slots the keepalive/recovery timers free.
#[tauri::command]
pub async fn pbbs_users(
    state: tauri::State<'_, session::AppState>,
) -> Result<PbbsUsers, String> {
    let core = state.core.lock().await;
    let mut users = 0usize;
    for client in &core.connections {
        if let Ok(c) = client.try_lock() {
            users += c.server_sessions.iter().flatten().count();
        }
    }
    Ok(PbbsUsers {
        users,
        max: crate::bbs_link::MAX_BBS_SESSIONS,
    })
}

/// Live board-user count result.
#[derive(serde::Serialize)]
pub struct PbbsUsers {
    pub users: usize,
    pub max: usize,
}

// ── Connect/Disconnect Retry Timer ────────────────────────
//
// If no UA/DM is received within T1, resend the SABM (or DISC) up to N2_MAX
// times with linear backoff. On exhaustion, force the session to Disconnected.

/// Which connect-mode control frame the retry loop resends on T1 expiry.
#[derive(Clone, Copy)]
pub(crate) enum RetryTarget {
    Sabm,
    Disc,
}

impl RetryTarget {
    /// The state the session must be in for this retry to be valid.
    fn expected_state(&self) -> session::Ax25ConnState {
        match self {
            RetryTarget::Sabm => session::Ax25ConnState::AwaitingConnect,
            RetryTarget::Disc => session::Ax25ConnState::AwaitingDisconnect,
        }
    }

    fn build_frame(
        &self,
        dest: &str,
        dest_ssid: u8,
        src: &str,
        src_ssid: u8,
        digis: &[String],
    ) -> Vec<u8> {
        match self {
            RetryTarget::Sabm => ax25::build_sabm(dest, dest_ssid, src, src_ssid, digis),
            RetryTarget::Disc => ax25::build_disc(dest, dest_ssid, src, src_ssid, digis),
        }
    }

    fn timeout_reason(&self) -> &'static str {
        match self {
            RetryTarget::Sabm => "Connect timeout (no UA response)",
            RetryTarget::Disc => "Disconnect timeout (no UA response)",
        }
    }

    /// Clear this target's retry-task handle on the session (task is done).
    fn clear_handle(&self, session: &mut session::Ax25Session) {
        match self {
            RetryTarget::Sabm => session.sabm_handle = None,
            RetryTarget::Disc => session.disc_handle = None,
        }
    }
}

pub(crate) fn spawn_connect_retry(
    target: RetryTarget,
    client_arc: std::sync::Arc<tokio::sync::Mutex<session::TcpClient>>,
    loc: session::SessionLoc,
    app_handle: std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Read T1 and N2 from the client's configured values
        let (t1_ms, n2_max) = {
            let c = client_arc.lock().await;
            (c.t1_ms, c.n2_max)
        };
        // DISC option C: fire exactly ONE best-effort backup (2 total), then
        // give up locally right after. SABM keeps the full N2 cycle.
        let force_after_send = matches!(target, RetryTarget::Disc);
        let ceiling = if force_after_send { 1 } else { n2_max };
        for retry_num in 1..=ceiling {
            let delay_ms = if retry_num == 1 {
                t1_ms
            } else {
                session::kernel_t1_ms(t1_ms, (retry_num - 1) as u8)
            };
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let mut c = client_arc.lock().await;
            {
                let Some(s) = c.session_ref(loc) else {
                    break;
                };
                if s.state != target.expected_state() {
                    break;
                }
            }
            // Clone before mutable borrow to satisfy borrow checker
            let my_call = c.callsign.clone();
            let my_ssid = c.ssid;
            let frame = {
                let Some(s) = c.session_mut(loc) else {
                    break;
                };
                let remote = s.remote.clone();
                let remote_ssid = s.remote_ssid;
                let digis = s.digis.clone();
                let frame = target.build_frame(&remote, remote_ssid, &my_call, my_ssid, &digis);
                s.last_send_time = Some(std::time::Instant::now());
                frame
            };
            let _ = kiss::send_kiss(&mut c, &frame).await;
            crate::session::emit_tx_frame(app_handle.as_ref(), &frame);
            if force_after_send {
                // Give up locally right after the single backup DISC.
                let Some(s) = c.session_mut(loc) else {
                    break;
                };
                s.state = session::Ax25ConnState::Disconnected;
                s.pending_ns = None;
                if !s.invisible {
                    let _ = app_handle.emit(
                        "session-disconnected",
                        serde_json::json!(
                        {"tab": loc.index(), "reason": target.timeout_reason()}),
                    );
                }
                break;
            }
            if retry_num >= n2_max {
                // Kernel: n2count == N2 → disconnect ETIMEDOUT, no further retries
                let Some(s) = c.session_mut(loc) else {
                    break;
                };
                s.state = session::Ax25ConnState::Disconnected;
                s.pending_ns = None;
                if !s.invisible {
                    let _ = app_handle.emit(
                        "session-disconnected",
                        serde_json::json!(
                        {"tab": loc.index(), "reason": target.timeout_reason()}),
                    );
                }
                break;
            }
        }
        let mut c = client_arc.lock().await;
        if let Some(s) = c.session_mut(loc) {
            target.clear_handle(s);
        }
    })
}

// ── Unified Protocol Commands ──────────────────────────────
//
// These unified commands dispatch to KISS or AGW implementations
// based on the protocol stored in TcpClient. New frontend code
// can call these instead of the protocol-specific variants.

/// Connect to a KISS or AGWPE server. Set `protocol` to your desired mode.
/// Call this instead of `kiss_connect` or `agw_connect` for new code.
#[tauri::command]
pub async fn connect_to_server(
    id: String,
    label: String,
    host: String,
    port: u16,
    callsign: String,
    ssid: u8,
    protocol: session::Protocol,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    match protocol {
        session::Protocol::KISS => {
            kiss_connect(id, label, host, port, callsign, ssid, state, app_handle).await
        }
        session::Protocol::AGW => {
            agw_connect(id, label, host, port, callsign, ssid, state, app_handle).await
        }
    }
}

/// Initiate an AX.25 connected-mode session. Dispatches to KISS or AGW based on the connection's protocol.
#[tauri::command]
pub async fn connect_session(
    conn_id: String,
    tab: usize,
    remote: String,
    remote_ssid: u8,
    via: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // Check protocol from the stored connection
    let is_agw = connection_is_agw(&state, &conn_id).await;
    if is_agw {
        agw_ax25_connect(conn_id, tab, remote, remote_ssid, via, state, app_handle).await
    } else {
        ax25_connect(conn_id, tab, remote, remote_ssid, via, state, app_handle).await
    }
}

/// Send connected-mode data. Dispatches to I-frame or AGW data based on protocol.
#[tauri::command]
pub async fn send_connected_data(
    conn_id: String,
    tab: usize,
    text: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let is_agw = connection_is_agw(&state, &conn_id).await;
    if is_agw {
        agw_send_data(conn_id, tab, text, state, app_handle).await
    } else {
        ax25_send_iframe(conn_id, tab, text, state, app_handle).await
    }
}

/// Send a UI (unproto) frame. Dispatches to KISS or AGW based on protocol.
#[tauri::command]
pub async fn send_ui_frame(
    conn_id: String,
    dest: String,
    dest_ssid: u8,
    text: String,
    via: String,
    state: tauri::State<'_, session::AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let is_agw = connection_is_agw(&state, &conn_id).await;
    if is_agw {
        agw_send_ui(conn_id, dest, dest_ssid, text, via, state, app_handle).await
    } else {
        ax25_send_ui(conn_id, dest, dest_ssid, text, via, state, app_handle).await
    }
}

// ── Tests: beacon scheduler ──────────────────────────────

#[cfg(test)]
mod beacon_tests {
    use super::*;

    fn cfg(t: &str, d: &str, v: &str) -> (String, String, String) {
        (t.to_string(), d.to_string(), v.to_string())
    }

    fn sched(next: Option<u64>, c: Option<(String, String, String)>, iv: u64) -> BeaconSched {
        BeaconSched {
            next_send_at_ms: next,
            last_cfg: c,
            last_interval_secs: iv,
        }
    }

    #[test]
    fn disabled_is_never_scheduled() {
        let st = sched(Some(1_000_000), Some(cfg("hi", "BEACON", "")), 900);
        let (s, send) = beacon_tick(2_000_000, false, "hi", "BEACON", "", 900, &st);
        assert!(!send);
        assert_eq!(s, BeaconSched::default());
    }

    #[test]
    fn fresh_enable_schedules_verification_beacon_soon() {
        let st = BeaconSched::default();
        let (s, send) = beacon_tick(100_000, true, "hi", "BEACON", "", 900, &st);
        assert!(!send);
        assert_eq!(s.next_send_at_ms, Some(105_000));
        assert_eq!(s.last_cfg, Some(cfg("hi", "BEACON", "")));
        assert_eq!(s.last_interval_secs, 900);
    }

    #[test]
    fn unrelated_save_never_touches_schedule() {
        let st = sched(Some(500_000), Some(cfg("hi", "BEACON", "KBANN")), 900);
        let (s, send) = beacon_tick(400_000, true, "hi", "BEACON", "KBANN", 900, &st);
        assert!(!send);
        assert_eq!(
            s.next_send_at_ms,
            Some(500_000),
            "schedule must survive unrelated saves"
        );
    }

    #[test]
    fn text_change_triggers_single_verification() {
        let st = sched(Some(500_000), Some(cfg("old", "BEACON", "")), 900);
        let (s, send) = beacon_tick(400_000, true, "new", "BEACON", "", 900, &st);
        assert!(!send);
        assert_eq!(s.next_send_at_ms, Some(405_000));
        assert_eq!(s.last_cfg, Some(cfg("new", "BEACON", "")));
    }

    #[test]
    fn digi_string_change_triggers_verification_not_spam() {
        // One change → one verification. A second identical save does nothing.
        let st = sched(Some(500_000), Some(cfg("hi", "BEACON", "DIGI1")), 900);
        let (s1, _) = beacon_tick(400_000, true, "hi", "BEACON", "DIGI1 DIGI2", 900, &st);
        assert_eq!(s1.next_send_at_ms, Some(405_000));
        // Same fields again (e.g. a repeat auto-save): schedule untouched.
        let (s2, send2) = beacon_tick(401_000, true, "hi", "BEACON", "DIGI1 DIGI2", 900, &s1);
        assert!(!send2);
        assert_eq!(s2.next_send_at_ms, Some(405_000));
    }

    #[test]
    fn interval_change_resets_countdown_no_early_beacon() {
        let st = sched(Some(500_000), Some(cfg("hi", "BEACON", "")), 900);
        let (s, send) = beacon_tick(400_000, true, "hi", "BEACON", "", 300, &st);
        assert!(!send);
        assert_eq!(
            s.next_send_at_ms,
            Some(700_000),
            "countdown resets to new interval"
        );
        assert_eq!(s.last_interval_secs, 300);
    }

    #[test]
    fn interval_change_is_not_sent_even_if_old_schedule_was_due() {
        let st = sched(Some(390_000), Some(cfg("hi", "BEACON", "")), 900);
        let (s, send) = beacon_tick(405_000, true, "hi", "BEACON", "", 300, &st);
        assert!(!send);
        assert_eq!(s.next_send_at_ms, Some(705_000));
    }

    #[test]
    fn due_beacon_sends_and_reschedules_at_interval() {
        let st = sched(Some(400_000), Some(cfg("hi", "BEACON", "")), 900);
        let (s, send) = beacon_tick(405_000, true, "hi", "BEACON", "", 900, &st);
        assert!(send);
        assert_eq!(s.next_send_at_ms, Some(1_305_000));
    }

    #[test]
    fn text_and_interval_changed_together_verify_text_then_new_cadence() {
        let st = sched(Some(500_000), Some(cfg("old", "BEACON", "")), 900);
        let (s, send) = beacon_tick(400_000, true, "new", "BEACON", "", 300, &st);
        assert!(!send);
        assert_eq!(
            s.next_send_at_ms,
            Some(405_000),
            "text change wins for verification"
        );
        assert_eq!(
            s.last_interval_secs, 300,
            "cadence updates for after the verification"
        );
    }
}
