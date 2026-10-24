// ── K2K → kiss-sim live integration / regression suite ─────────────────────
//
// Proves K2K's OWN byte path interoperates with the kiss-sim TNC, using
// exactly the code the app sends: crate::kiss framing and
// crate::kernel::ax25 builders/parser over a real TCP socket to the running
// simulator (default 127.0.0.1:8181). These are the "point K2K at host:8181"
// wire-compat checks — run them against every future K2K version to catch a
// refactor that breaks the wire contract (framer change, seq-number bug,
// builder drift) without needing real RF.
//
// The control panel (127.0.0.1:8182) is driven over plain HTTP for
// error-injection, multicast, and replay scenarios — the same endpoints K2K's
// developer test workflow and the sim's own UI use.
//
// SKIP guard: every #[test] skips (does not fail) when the sim service is not
// reachable on :8181, so plain `cargo test` in CI with no sim stays green.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::kernel::ax25::{build_ax25_frame, build_iframe, build_sabm, parse_ax25_frame};
use crate::kiss::{build_kiss_frame, kiss_unescape};

const SIM: &str = "127.0.0.1:8181";
const PANEL: &str = "127.0.0.1:8182";
const MY_CALL: &str = "K2KTEST";
const MY_SSID: u8 = 0;

// ── tiny HTTP client for the control panel (raw TCP; no external deps) ─────

fn panel_raw(method: &str, path: &str, body: Option<&str>) -> Option<String> {
    let addr = PANEL.to_socket_addrs().ok()?.next()?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {PANEL}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(req.as_bytes()).ok()?;
    let mut resp = String::new();
    let _ = sock.read_to_string(&mut resp);
    Some(resp)
}

fn panel_post(path: &str, body: &str) -> Option<String> {
    panel_raw("POST", path, Some(body))
}

/// True when the panel response JSON carries a success.
fn panel_ok(resp: Option<String>) -> bool {
    resp.map(|r| r.contains("\"ok\": true") || r.contains("\"ok\":true"))
        .unwrap_or(false)
}

fn sim_up() -> bool {
    panel_raw("GET", "/api/status", None).is_some()
}

/// These tests all drive the SAME live sim (it mutates shared panel/session
/// state — injection rules, sessions, replay). Cargo runs #[test]s in
/// parallel by default, so we serialize them with a process-global lock to
/// avoid cross-test interference (e.g. a drop rule killing another test's
/// connect). Grab this at the top of every live test.
fn live_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
}

// ── KISS frame reader/writer (what the app speaks over the wire) ───────────

struct SimLink {
    sock: TcpStream,
    buf: Vec<u8>,
}

impl SimLink {
    fn connect() -> Option<Self> {
        let stream = TcpStream::connect_timeout(
            &SIM.to_socket_addrs().ok()?.next()?,
            Duration::from_secs(2),
        )
        .ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        stream.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
        Some(Self {
            sock: stream,
            buf: Vec::new(),
        })
    }

    fn tx(&mut self, raw_ax25: &[u8]) {
        let framed = build_kiss_frame(0, raw_ax25);
        let _ = self.sock.write_all(&framed);
    }

    /// Read one raw AX.25 frame (unframed). None on timeout/EOF.
    fn rx(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(idx) = self.buf.iter().position(|&b| b == 0xC0) {
                let seg: Vec<u8> = self.buf.drain(..=idx).collect();
                if seg.len() >= 2 {
                    let frame = kiss_unescape(&seg[1..seg.len() - 1]);
                    if !frame.is_empty() {
                        return Some(frame);
                    }
                }
                continue;
            }
            let mut chunk = [0u8; 4096];
            match self.sock.read(&mut chunk) {
                Ok(n) if n > 0 => self.buf.extend_from_slice(&chunk[..n]),
                _ => {
                    // Flush a partial frame before giving up.
                    if let Some(idx) = self.buf.iter().position(|&b| b == 0xC0) {
                        let seg: Vec<u8> = self.buf.drain(..=idx).collect();
                        if seg.len() >= 2 {
                            return Some(kiss_unescape(&seg[1..seg.len() - 1]));
                        }
                    }
                    self.buf.clear();
                    return None;
                }
            }
        }
    }

    /// Read frames for a duration and return parsed frames verbatim (for
    /// asserting path / control bytes), unpolluted by ACKing.
    fn collect_frames(&mut self, dur: Duration) -> Vec<crate::kernel::ax25::ParsedAx25Frame> {
        let deadline = std::time::Instant::now() + dur;
        let mut out = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Some(f) = self.rx() {
                if let Some(p) = parse_ax25_frame(&f) {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// RR acknowledgement for the given N(R), addressed from our call to `to`.
fn rr_frame(to: &str, nr: u8) -> Vec<u8> {
    let ctl = (nr << 5) | 0x01;
    build_ax25_frame(to, 0, MY_CALL, MY_SSID, ctl, 0xF0, &[], &[], true)
}

fn rr_to(link: &mut SimLink, to: &str, nr: u8) {
    link.tx(&rr_frame(to, nr));
}

/// True if a frame's control is UA (mod P/F).
fn is_ua(ctl: u8) -> bool {
    ctl & 0xEF == crate::kernel::ax25::UA_CTL
}

/// True if a frame's control is DM (mod P/F).
fn is_dm(ctl: u8) -> bool {
    ctl & 0xEF == crate::kernel::ax25::DM_CTL
}

// ── Scenario 1: BBS0 direct connect → banner → L list → DISC ───────────────

#[test]
fn live_bbs0_connect_receive_and_disconnect() {
    let _lock = live_lock();
    let mut link = match SimLink::connect() {
        Some(l) => l,
        None => {
            eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
            return;
        }
    };

    // 1) SABM to BBS0 — the app's own connect frame.
    link.tx(&build_sabm("BBS0", 0, MY_CALL, MY_SSID, &[]));

    // 2) Await UA, then collect banner I-frames (ACKing to keep window open).
    let mut saw_ua = false;
    let mut banner = String::new();
    let mut nr: u8 = 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        if let Some(f) = link.rx() {
            if let Some(p) = parse_ax25_frame(&f) {
                if is_ua(p.control) {
                    saw_ua = true;
                } else if crate::kernel::ax25::is_iframe_ctl(p.control) {
                    banner.push_str(&String::from_utf8_lossy(&p.info));
                    // ACK whatever we just consumed so the window keeps flowing.
                    rr_to(&mut link, "BBS0", nr);
                    nr = nr.wrapping_add(1) & 0x07;
                }
            }
        }
    }
    assert!(saw_ua, "BBS0 never sent UA in reply to SABM");
    assert!(
        banner.contains("Welcome") || banner.contains("AVAILABLE"),
        "expected BBS banner text, got {banner:?}"
    );

    // 3) Send "L\r" and confirm a message list comes back.
    link.tx(&build_iframe("BBS0", 0, MY_CALL, MY_SSID, 0, nr, b"L\r", &[]));
    let mut got_list = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        if let Some(f) = link.rx() {
            if let Some(p) = parse_ax25_frame(&f) {
                if crate::kernel::ax25::is_iframe_ctl(p.control) {
                    let text = String::from_utf8_lossy(&p.info).into_owned();
                    if text.contains('#') || text.contains("SUBJECT") || text.contains("ST") {
                        got_list = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(got_list, "BBS0 did not return a message list to 'L'");

    // 4) DISC teardown; expect UA or DM back.
    link.tx(&build_ax25_frame("BBS0", 0, MY_CALL, MY_SSID, 0x53, 0xF0, &[], &[], true));
    let mut got_ack = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if let Some(f) = link.rx() {
            if let Some(p) = parse_ax25_frame(&f) {
                if !crate::kernel::ax25::is_iframe_ctl(p.control) {
                    got_ack = true;
                    break;
                }
            }
        }
    }
    assert!(got_ack, "BBS0 did not acknowledge DISC teardown");
    eprintln!("[k2k-live] BBS0 connect→UA→banner→'L'→DISC: OK");
}

// ── Scenario 2: immediate DMs when not connected ───────────────────────────

#[test]
fn live_unconnected_gets_dm() {
    let _lock = live_lock();
    let mut link = match SimLink::connect() {
        Some(l) => l,
        None => {
            eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
            return;
        }
    };
    // Send an I-frame without first SABM-ing → sim should reject with DM.
    link.tx(&build_iframe("BBS0", 0, MY_CALL, MY_SSID, 0, 0, b"hi\r", &[]));
    let mut got_dm = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if let Some(f) = link.rx() {
            if let Some(p) = parse_ax25_frame(&f) {
                if is_dm(p.control) {
                    got_dm = true;
                    break;
                }
            }
        }
    }
    assert!(got_dm, "sim should DM a session-less I-frame");
    eprintln!("[k2k-live] unconnected I-frame → DM: OK");
}

// ── Scenario 3: direct node reply (NODE-A) ─────────────────────────────────

#[test]
fn live_direct_node_reply() {
    let _lock = live_lock();
    let mut link = match SimLink::connect() {
        Some(l) => l,
        None => {
            eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
            return;
        }
    };
    link.tx(&build_sabm("NODE-A", 4, MY_CALL, MY_SSID, &[]));
    let mut saw_ua = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Some(f) = link.rx() {
            if let Some(p) = parse_ax25_frame(&f) {
                if is_ua(p.control) {
                    saw_ua = true;
                    break;
                }
            }
        }
    }
    assert!(saw_ua, "NODE-A did not UA the SABM");
    eprintln!("[k2k-live] NODE-A direct SABM→UA: OK");
}

// ── Scenario 4: never-responds node stays silent ───────────────────────────

#[test]
fn live_never_responds_stays_silent() {
    let _lock = live_lock();
    let mut link = match SimLink::connect() {
        Some(l) => l,
        None => {
            eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
            return;
        }
    };
    link.tx(&build_sabm("NODE-D", 7, MY_CALL, MY_SSID, &[]));
    let frames = link.collect_frames(Duration::from_secs(2));
    assert!(frames.is_empty(), "NODE-D (never_responds) should send nothing");
    eprintln!("[k2k-live] NODE-D (never_responds) silence: OK");
}

// ── Scenario 5: digipeater path — NODE-B via RELAY carries path back ───────

#[test]
fn live_digipeater_path_carries_back() {
    let _lock = live_lock();
    let mut link = match SimLink::connect() {
        Some(l) => l,
        None => {
            eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
            return;
        }
    };
    // NODE-B is only reachable via RELAY digipeater.
    link.tx(&build_sabm("NODE-B", 5, MY_CALL, MY_SSID, &["RELAY".to_string()]));
    let mut saw_ua = false;
    let mut framed_ok = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Some(f) = link.rx() {
            if let Some(p) = parse_ax25_frame(&f) {
                if is_ua(p.control) {
                    saw_ua = true;
                    // The sim puts the via path on the response's digis.
                    framed_ok = !p.digis.is_empty();
                    break;
                }
            }
        }
    }
    assert!(saw_ua, "NODE-B via RELAY did not respond");
    assert!(framed_ok, "digipeater response should carry the via path back");
    eprintln!("[k2k-live] NODE-B via RELAY (path carried back): OK");
}

// ── Scenario 6: multicast UI broadcast reaches 2 clients ───────────────────

#[test]
fn live_multicast_ui_reaches_all_clients() {
    let _lock = live_lock();
    if !sim_up() {
        eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
        return;
    }
    let mut c1 = SimLink::connect().expect("client 1");
    let mut c2 = SimLink::connect().expect("client 2");

    // Give the sim's per-connection server threads a moment to register both
    // sockets before we broadcast (the panel only delivers to sockets it has
    // seen). Mirrors the sleep in the sim's own multi_client_test.py.
    std::thread::sleep(Duration::from_millis(300));

    // Broadcast a UI frame via the panel to all connected sockets.
    let resp = panel_post(
        "/api/send",
        "{\"from\":\"NODE-A\",\"to\":\"ALL\",\"control\":\"UI\",\"info\":\"regression multicast ping\"}",
    );
    assert!(panel_ok(resp), "api/send failed");

    let mut hit1 = false;
    let mut hit2 = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        for (lab, link, hit) in [
            ("c1", &mut c1, &mut hit1),
            ("c2", &mut c2, &mut hit2),
        ] {
            if let Some(f) = link.rx() {
                if let Some(p) = parse_ax25_frame(&f) {
                    if String::from_utf8_lossy(&p.info).contains("regression multicast ping") {
                        *hit = true;
                        eprintln!("[k2k-live] multicast {lab} received");
                    }
                }
            }
        }
    }
    assert!(hit1 && hit2, "both clients should receive the multicast UI frame");
    eprintln!("[k2k-live] multicast UI to 2 clients: OK");
}

// ── Scenario 7: error injection — drop rule kills frames, clear restores ───

#[test]
fn live_error_injection_drop_clears() {
    let _lock = live_lock();
    if !sim_up() {
        eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
        return;
    }
    // Add a drop rule on outbound (node→app) frames to BBS0.
    let add = panel_post(
        "/api/inject",
        "{\"action\":\"drop\",\"targets\":[\"BBS0\"],\"every\":1}",
    );
    assert!(panel_ok(add), "rule accepted");

    // Connect → sim sends UA then banner, but the drop rule should suppress them.
    let mut link = SimLink::connect().unwrap();
    link.tx(&build_sabm("BBS0", 0, "K2KDROP", 9, &[]));
    let frames = link.collect_frames(Duration::from_secs(2));
    assert!(
        frames.is_empty(),
        "drop rule should have suppressed BBS0's response frames"
    );

    // Clear live rules → a fresh connect gets through.
    panel_post("/api/inject/clear", "");
    let mut link2 = SimLink::connect().unwrap();
    link2.tx(&build_sabm("BBS0", 0, "K2KDROP", 9, &[]));
    let frames2 = link2.collect_frames(Duration::from_secs(3));
    assert!(
        !frames2.is_empty(),
        "after clearing drop rule, BBS0 should respond again"
    );
    eprintln!("[k2k-live] error injection drop→clear: OK");
}

// ── Scenario 8: replay load/step ───────────────────────────────────────────

#[test]
fn live_replay_load_and_step() {
    let _lock = live_lock();
    if !sim_up() {
        eprintln!("[k2k-live] SKIPPED: kiss-sim not reachable on {SIM}");
        return;
    }
    // replay.json ships with the sim repo.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let path = format!("{home}/projects/kiss-sim/replay.json");
    let load = panel_post(
        "/api/replay",
        &format!("{{\"action\":\"load\",\"path\":\"{path}\"}}"),
    );
    let lresp = load.unwrap_or_default();
    assert!(lresp.contains("\"ok\": true"), "replay load failed: {lresp}");

    let step = panel_post("/api/replay", "{\"action\":\"step\"}");
    assert!(panel_ok(step), "replay step failed");

    panel_post("/api/replay", "{\"action\":\"stop\"}");
    eprintln!("[k2k-live] replay load+step: OK");
}
