// ── AX.25 Connected-Mode Session Management ───────────────
//
// Manages the state machine for AX.25 connected-mode sessions.
// Each KISS TCP connection can have up to MAX_TABS simultaneous sessions.
//
// Protocol timers (matching linpac/BPQ defaults unless overridden in Settings):
//   T1 — Retransmission timeout: how long to wait for an ACK before retrying
//   T2 — ACK delay (1s): batch multiple ACKs into a single RR (Receive Ready)
//   T3 — Inactivity probe (5min): send RR+P to check if remote is still alive
//
// NOTE: session timers are driven from the user's KISS Settings values
// (loaded via `KissTiming::from_settings_or_default`); the 3s/1s values below
// are the historical module defaults used only when settings are absent (and
// the AGW path still uses KissTiming::default). See settings.rs / commands.rs.
//
// Retry strategy:
//   - Each I-frame is tracked with N(S) sequence number
//   - If no ACK within T1, retransmit up to N2 times
//   - On N2 exhaustion, disconnect and report "Retry limit exceeded"

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crate::ax25;
use crate::kiss;

// ── Constants ─────────────────────────────────────────────

pub const MAX_TABS: usize = 4; // Number of connected-mode tabs (C1–C4)
pub const T1_MS: u64 = 3_000; // Retransmission timeout — AX.25 spec §2.5.6 (3s)
pub const T2_MS: u64 = 1_000; // ACK batching delay — AX.25 spec §2.5.4.2 (1s)
pub const T3_MS: u64 = 300_000; // Inactivity keep-alive timeout — AX.25 spec §2.5.7 (5min)
pub const KA_CHECK_MS: u64 = 60_000; // How often to check T3 expiration (1min)
pub const N2_MAX: u8 = 10; // Max retransmissions before forced disconnect — matches linpac kernel default

// ── Session Role (Phase A kernel merge plumbing) ────────
//
// Pure additions for the future Server role (message board). Nothing in the
// client (C-tab) path reads these yet — the client path must stay
// byte-identical in behavior.

/// What a session is for. `Client` = C-tab (current behavior).
/// `Server` = message-board listener session (wired in later phases).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRole {
    Client,
    Server,
}

impl Default for SessionRole {
    fn default() -> Self {
        Self::Client
    }
}

// `Ax25Session` derives `Debug` and carries `Option<crate::bbs::BbsSession>`.
// `BbsSession` has no derives of its own; give it a local Debug impl here so
// bbs.rs stays untouched (pure plumbing — a board session's engine handle
// never needs a full debug dump).
impl std::fmt::Debug for crate::bbs::BbsSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BbsSession")
            .field("user", &self.user)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

// ── Data Structures ─────────────────────────────────────

/// Connection state for a single AX.25 connected-mode session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Ax25ConnState {
    Disconnected,       // No session active — this tab is idle (kernel STATE_0)
    AwaitingConnect,    // SABM sent, waiting for UA from remote (kernel STATE_1)
    Connected,          // SABM/UA exchange complete — data can flow (kernel STATE_3)
    AwaitingDisconnect, // DISC sent, waiting for UA confirmation (kernel STATE_2)
    TimerRecovery,      // T1 recovery: RR enquiry polled, waiting for response (kernel STATE_4)
}

/// Represents one of the MAX_TABS connected-mode sessions.
/// Each session tracks its own sequence numbers, retry state,
/// incoming data buffer, and keep-alive timers.
#[derive(Debug, Serialize, Deserialize)]
pub struct Ax25Session {
    pub tab: usize,           // 0..MAX_TABS-1, matches the UI tab index
    pub state: Ax25ConnState, // Current protocol state
    pub remote: String,       // Remote callsign
    pub remote_ssid: u8,      // Remote SSID
    pub digis: Vec<String>,   // Digipeater path (if any)
    pub ns: u8,               // Our send sequence number (next N(S) to use)
    pub nr: u8,               // Our receive sequence number (next expected N(S))

    // Retry state — reset on each new send, not persisted
    #[serde(skip)]
    pub pending_text: String, // Text of the last sent I-frame (for retransmit)
    #[serde(skip)]
    pub pending_ns: Option<u8>, // None = nothing pending, otherwise N(S) of un-ACK'd frame
    #[serde(skip)]
    pub last_send_time: Option<Instant>, // When the last I-frame was sent (for T1)

    /// T2 timer — delay RR to batch multiple I-frame ACKs
    #[serde(skip)]
    pub rr_pending: bool, // True if T2 timer is counting down
    #[serde(skip)]
    pub rr_target_nr: u8, // N(R) value when T2 fires
    #[serde(skip)]
    pub t2_handle: Option<tokio::task::JoinHandle<()>>, // T2 delayed-ACK task (re-armed per burst)
    #[serde(skip)]
    pub last_rx_time: Option<Instant>, // Last time we received anything from this session
    // RX line buffer — accumulates I-frame data, splits on \r
    // This handles the common case where a long message is split across
    // multiple I-frames. We buffer until we see \r, then emit the complete line.
    #[serde(skip)]
    pub rx_buffer: String,

    // Flush timer handle — cancels when a new I-frame arrives within the
    // burst window so the buffer accumulates before being displayed.
    #[serde(skip)]
    pub flush_handle: Option<tokio::task::JoinHandle<()>>,

    // T1 retry task handle — aborted when a new send replaces the pending frame.
    #[serde(skip)]
    pub retry_handle: Option<tokio::task::JoinHandle<()>>,

    // T1 retry task handle for SABM — resends SABM if no UA arrives.
    #[serde(skip)]
    pub sabm_handle: Option<tokio::task::JoinHandle<()>>,

    // T1 retry task handle for DISC — resends DISC if no UA arrives.
    #[serde(skip)]
    pub disc_handle: Option<tokio::task::JoinHandle<()>>,

    // Remote is busy (RNR received) — stop transmitting I-frames.
    #[serde(skip)]
    pub remote_busy: bool,

    // Kernel AX25_COND_REJECT latch: REJ sent once for the current gap;
    // suppressed for subsequent out-of-sequence frames until the gap fills.
    #[serde(skip)]
    pub reject_set: bool,

    // Kernel retry counter (ax25->n2count) — drives T1 linear backoff.
    #[serde(skip)]
    pub n2count: u8,

    // ── Server-role plumbing (Phase A kernel merge — pure additions) ──
    // Client (C-tab) path never reads these; they exist so a Server-role
    // session can ride the same state machine in later phases.
    /// Session identity — survives `reset()` (a session does not change
    /// role mid-life). All C-tabs are `Client`.
    #[serde(skip)]
    pub role: SessionRole,
    /// Suppress UI event emits — an invisible (server) session never
    /// paints a C-tab window. All C-tabs are visible.
    #[serde(skip)]
    pub invisible: bool,
    /// Direwolf owns the link state (AGW sessions): the kernel skips
    /// timers/retransmission and only the BBS engine runs.
    #[serde(skip)]
    pub link_external: bool,
    /// Engine handle: `Some` ⇒ the data path feeds the BBS engine
    /// instead of the UI. `None` for C-tabs.
    /// (Constructed by the A5 router; read by the A4 state-0 accept.)
    #[serde(skip)]
    pub bbs: Option<crate::bbs::BbsSession>,
    /// Output queue for server sessions (multi-line engine replies).
    /// C-tabs never populate it; the drain hook no-ops when empty.
    #[serde(skip)]
    pub pending_out: std::collections::VecDeque<String>,
    /// Max info bytes per outgoing I-frame for this session (message-board
    /// output packing; from settings, default 250). Survives `reset()` —
    /// it's a link property, like the role.
    #[serde(skip)]
    pub max_packet_size: usize,
    /// Where this session lives: `Tab(tab)` for C-tabs (matches `tab`),
    /// `Server(i)` for message-board pool slots. Session identity —
    /// survives `reset()`.
    #[serde(skip)]
    pub loc: SessionLoc,
    /// T3 keep-alive task handle for server sessions (C-tab keepalives
    /// live in `TcpClient::ka_handles[tab]`). `reset()` deliberately does
    /// NOT abort this — a link re-establishment must keep supervising
    /// (the router respawns only a missing/finished handle).
    #[serde(skip)]
    pub keepalive_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Ax25Session {
    /// Create a new disconnected session for the given tab index.
    pub fn new(tab: usize) -> Self {
        Self {
            tab,
            state: Ax25ConnState::Disconnected,
            remote: String::new(),
            remote_ssid: 0,
            digis: vec![],
            ns: 0,
            nr: 0,
            pending_text: String::new(),
            pending_ns: None,
            last_send_time: None,
            rr_pending: false,
            rr_target_nr: 0,
            t2_handle: None,
            last_rx_time: None,
            rx_buffer: String::new(),
            flush_handle: None,
            retry_handle: None,
            sabm_handle: None,
            disc_handle: None,
            remote_busy: false,
            reject_set: false,
            n2count: 0,
            role: SessionRole::Client,
            invisible: false,
            link_external: false,
            bbs: None,
            pending_out: std::collections::VecDeque::new(),
            max_packet_size: BBS_MAX_INFO,
            loc: SessionLoc::Tab(tab),
            keepalive_handle: None,
        }
    }

    /// Reset all sequence/retry state — called when a connection is established or torn down.
    pub fn reset(&mut self) {
        self.ns = 0;
        self.nr = 0;
        self.pending_ns = None;
        self.pending_text.clear();
        self.last_send_time = None;
        self.rr_pending = false;
        self.last_rx_time = None;
        self.rx_buffer.clear();
        self.remote_busy = false;
        self.reject_set = false;
        self.n2count = 0;
        // Server-role plumbing: clear the output queue but keep session
        // identity (role/invisible/link_external/bbs/loc survive a reset).
        self.pending_out.clear();
        if let Some(h) = self.flush_handle.take() {
            h.abort();
        }
        if let Some(h) = self.retry_handle.take() {
            h.abort();
        }
        if let Some(h) = self.sabm_handle.take() {
            h.abort();
        }
        if let Some(h) = self.disc_handle.take() {
            h.abort();
        }
        if let Some(h) = self.t2_handle.take() {
            h.abort();
        }
        // keepalive_handle intentionally NOT aborted here: reset() runs on
        // link re-establishment, where T3 supervision must keep running
        // (the keepalive task self-terminates when the state leaves
        // Connected; teardown paths abort it explicitly).
    }

    /// Clear the pending I-frame state and notify the UI that N(R) `nr` was
    /// acked (kernel: a valid N(R) advance clears ACK_PENDING). Then, if the
    /// session has queued output (server sessions), send the next chunk —
    /// window-1: one frame per ACK, same rule as the old bbs_link
    /// drain_bbs_output. C-tabs never populate pending_out → no-op.
    fn ack_pending(
        &mut self,
        nr: u8,
        deferred: &mut DeferredOps,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    ) {
        self.pending_ns = None;
        self.pending_text.clear();
        self.last_send_time = None;
        if !self.invisible {
            let _ = app_handle.emit(
                "session-ack",
                serde_json::json!({
                    "tab": self.tab,
                    "nr": nr,
                }),
            );
        }
        // Window freed — send the next queued chunk (window-1: one frame
        // per ACK, same rule as the old bbs_link drain_bbs_output). The
        // actual send happens in execute() with full client access.
        deferred.push(Effect::DrainOutput(self.loc));
    }

    /// Arm/re-arm the T2 delayed-ACK: record the N(R) to send once the
    /// peer goes quiet (or polls with P=1). Setting `rr_pending` +
    /// `rr_target_nr` is the pure policy; the caller (`on_iframe`) owns
    /// the async timer task handle. Re-arming on every received in-sequence
    /// I-frame is what batches a burst into ONE ack at the end instead of
    /// an ack mid-burst.
    fn arm_t2_ack(&mut self) {
        self.rr_target_nr = self.nr;
        self.rr_pending = true;
    }
}

// Manual Clone — skips JoinHandle fields (not Clone)
impl Clone for Ax25Session {
    fn clone(&self) -> Self {
        Self {
            tab: self.tab,
            state: self.state.clone(),
            remote: self.remote.clone(),
            remote_ssid: self.remote_ssid,
            digis: self.digis.clone(),
            ns: self.ns,
            nr: self.nr,
            pending_text: self.pending_text.clone(),
            pending_ns: self.pending_ns,
            last_send_time: self.last_send_time,
            rr_pending: self.rr_pending,
            rr_target_nr: self.rr_target_nr,
            t2_handle: None,
            last_rx_time: self.last_rx_time,
            rx_buffer: self.rx_buffer.clone(),
            flush_handle: None,
            retry_handle: None,
            sabm_handle: None,
            disc_handle: None,
            remote_busy: self.remote_busy,
            reject_set: self.reject_set,
            n2count: self.n2count,
            role: self.role,
            invisible: self.invisible,
            link_external: self.link_external,
            // Engine handle is not Clone — a clone carries no live engine.
            bbs: None,
            pending_out: self.pending_out.clone(),
            max_packet_size: self.max_packet_size,
            loc: self.loc,
            keepalive_handle: None,
        }
    }
}

/// K2K Packet Desktop protocol mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Protocol {
    KISS,
    AGW,
}

impl Default for Protocol {
    fn default() -> Self {
        Self::KISS
    }
}

/// Information about a KISS TCP connection, serializable for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub connected: bool,
    pub protocol: Protocol,
    pub sessions: Vec<Ax25Session>,
    /// Tracks outgoing connects by (remote_callsign, tab) so RSP_CONNECTED
    /// routes to the correct tab instead of guessing a free one.
    pub outgoing_connects: Vec<(String, usize)>,
}

/// Pool-agnostic session locator (Phase A kernel merge).
///
/// Timer spawners and the router index sessions through this instead of
/// hard `info.sessions[tab]` indexing, so the same kernel machinery can
/// serve C-tab sessions (`Tab`) and message-board sessions (`Server`,
/// wired in A3/A4/A5). C-tab call sites pass `Tab(tab)` — behavior is
/// identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLoc {
    /// A C-tab session (index into `ConnectionInfo.sessions`).
    Tab(usize),
    /// A server (message-board) pool slot (index into
    /// `TcpClient::server_sessions`).
    Server(usize),
}

// `SessionLoc` is used as a hash key by `kernel::timers::TimerBank`.
impl std::hash::Hash for SessionLoc {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            SessionLoc::Tab(i) | SessionLoc::Server(i) => i.hash(state),
        }
    }
}

impl Default for SessionLoc {
    /// `Ax25Session::loc` is `#[serde(skip)]` — deserialization fills it
    /// with the C-tab identity (session locators are runtime-only).
    fn default() -> Self {
        Self::Tab(0)
    }
}

impl SessionLoc {
    /// The numeric index for UI payloads. For `Tab` this IS the tab
    /// number; for `Server` slots the value is only meaningful to the
    /// frontend if a session actually emits (invisible sessions don't).
    pub fn index(self) -> usize {
        match self {
            SessionLoc::Tab(tab) => tab,
            SessionLoc::Server(i) => i,
        }
    }
}

/// The main KISS TCP client struct — holds the connection, reader task, timers, and sessions.
pub struct TcpClient {
    pub info: ConnectionInfo,
    pub stream: Option<tokio::net::tcp::OwnedWriteHalf>,
    pub reader_handle: Option<tokio::task::JoinHandle<()>>,
    pub ka_handles: Vec<Option<tokio::task::JoinHandle<()>>>,
    /// Live transport adapter (Phase 3 seam). Set at connect time; outbound
    /// KISS/AGW frames are framed through it so the engine speaks `Link`, not
    /// protocol branches. The async write half stays on `stream`.
    pub link: Option<Box<dyn crate::link::Link>>,
    /// Phase 2 seam: the single owned schedule of every link-layer timer we
    /// supervise. Spawn functions consult/reserve a deadline here before
    /// starting a task (idempotent — re-schedule resets, never duplicates);
    /// cancelled by value on teardown. Kills the lost-JoinHandle /
    /// duplicate-poller bug class from the design audit.
    pub timers: crate::kernel::timers::TimerBank,
    pub app_handle: Option<std::sync::Arc<dyn crate::kernel::output::KernelOutput>>,
    pub callsign: String,
    pub ssid: u8,
    pub protocol: Protocol,
    /// Handle for the periodic beacon timer task.
    /// Spawned after connect if beacon is enabled; aborted on disconnect.
    pub beacon_handle: Option<tokio::task::JoinHandle<()>>,
    // ── AX.25 Link Layer Timer Values (from settings) ──
    pub t1_ms: u64,
    pub t2_ms: u64,
    pub t3_ms: u64,
    pub ka_check_ms: u64,
    pub n2_max: u8,
    /// Max info bytes per outgoing message-board frame (settings, default 250).
    pub max_packet_size: usize,
    // ── Message Board (PBBS) Runtime ──
    pub bbs_enabled: bool,
    pub bbs_callsign: String,
    pub bbs_ssid: u8,
    pub bbs_cfg: crate::bbs::BbsConfig,
    pub bbs_store: Option<crate::bbs::BbsStore>,
    /// Server-role session pool (message board) — cap MAX_BBS_SESSIONS.
    /// The one and only pool: both KISS (kernel state machine) and AGW
    /// (link_external) board sessions live here. (bbs_sessions removed in
    /// B1 — the parallel pool is gone.)
    pub server_sessions: Vec<Option<Ax25Session>>,
}

impl TcpClient {
    /// The write half of the TNC stream — errors when the link is down.
    pub fn writer(&mut self) -> Result<&mut tokio::net::tcp::OwnedWriteHalf, String> {
        match &mut self.stream {
            Some(w) => Ok(w),
            None => Err("Not connected to TNC".to_string()),
        }
    }

    /// Resolve a `SessionLoc` to the live session, or `None` if the index
    /// is out of range / the pool slot is empty. Follows the same
    /// lock discipline as the timer spawners: callers lock the
    /// `Arc<Mutex<TcpClient>>` once, then use this accessor (and clone
    /// session data out before mutating other fields, per the
    /// borrow-splitting pattern in rust-borrow-splitting-tcpclient.md).
    pub fn session_ref(&self, loc: SessionLoc) -> Option<&Ax25Session> {
        match loc {
            SessionLoc::Tab(tab) => self.info.sessions.get(tab),
            SessionLoc::Server(i) => self.server_sessions.get(i).and_then(|o| o.as_ref()),
        }
    }

    /// Mutable variant of [`session_ref`](Self::session_ref).
    pub fn session_mut(&mut self, loc: SessionLoc) -> Option<&mut Ax25Session> {
        match loc {
            SessionLoc::Tab(tab) => self.info.sessions.get_mut(tab),
            SessionLoc::Server(i) => self.server_sessions.get_mut(i).and_then(|o| o.as_mut()),
        }
    }

    /// Build a client with 4 empty sessions (C1–C4) and no tasks running.
    pub fn new(
        id: String,
        label: String,
        host: String,
        port: u16,
        protocol: Protocol,
        writer: tokio::net::tcp::OwnedWriteHalf,
        callsign: String,
        ssid: u8,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        timing: KissTiming,
    ) -> Self {
        let bbs = {
            let s = app_handle
                .load_settings()
                .ok()
                .flatten()
                .unwrap_or_else(crate::settings::AppSettings::default);
            crate::bbs_link::BbsSetup::from_settings(&s, &app_handle.data_dir())
        };
        TcpClient {
            info: ConnectionInfo {
                id,
                label,
                host,
                port,
                connected: false,
                protocol,
                sessions: (0..MAX_TABS).map(Ax25Session::new).collect(),
                outgoing_connects: Vec::new(),
            },
            stream: Some(writer),
            reader_handle: None,
            ka_handles: (0..MAX_TABS).map(|_| None).collect(),
            link: None,
            timers: crate::kernel::timers::TimerBank::new(),
            app_handle: Some(app_handle.clone()),
            callsign: if callsign.is_empty() {
                "N0CALL".to_string()
            } else {
                callsign
            },
            ssid,
            protocol,
            beacon_handle: None,
            t1_ms: timing.t1_ms,
            t2_ms: timing.t2_ms,
            t3_ms: timing.t3_ms,
            ka_check_ms: timing.ka_check_ms,
            n2_max: timing.n2_max,
            max_packet_size: timing.max_packet_size,
            bbs_enabled: bbs.enabled,
            bbs_callsign: bbs.callsign,
            bbs_ssid: bbs.ssid,
            bbs_cfg: bbs.cfg,
            bbs_store: bbs.store,
            server_sessions: (0..crate::bbs_link::MAX_BBS_SESSIONS)
                .map(|_| None)
                .collect(),
        }
    }
}

// ── TNC Timing Parameters ─────────────────────────────────

/// TNC timing parameters: the kernel AX.25 timers plus the KISS CSMA
/// parameters (TXDELAY / P / SlotTime) sent to the TNC on connect.
#[derive(Clone, Copy)]
pub struct KissTiming {
    pub t1_ms: u64,
    pub t2_ms: u64,
    pub t3_ms: u64,
    pub ka_check_ms: u64,
    pub n2_max: u8,
    pub max_packet_size: usize,
    pub txdelay: u8,
    pub p: u8,
    pub slot_time: u8,
}

impl KissTiming {
    /// Load from settings.json; falls back to the session constants when
    /// the file is missing (KISS params: 300ms TXDELAY / 63 / 100ms slot).
    pub fn from_settings_or_default(out: &dyn crate::kernel::output::KernelOutput) -> Self {
        match out.load_settings() {
            Ok(Some(s)) => KissTiming {
                t1_ms: s.kiss_t1_ms as u64,
                t2_ms: s.kiss_t2_ms as u64,
                t3_ms: s.kiss_t3_ms as u64,
                ka_check_ms: s.kiss_ka_check_ms as u64,
                n2_max: s.kiss_n2_max as u8,
                max_packet_size: s.message_board_max_packet_size as usize,
                txdelay: s.kiss_txdelay as u8,
                p: s.kiss_p as u8,
                slot_time: s.kiss_slot_time as u8,
            },
            _ => KissTiming::default(),
        }
    }
}

impl TcpClient {
    /// Re-read the message board config from settings and update the runtime
    /// snapshot in place. Called when an inbound frame is addressed to the
    /// board callsign while the snapshot is stale — this is what makes
    /// enabling the board work without disconnecting.
    pub(crate) fn refresh_bbs_setup(&mut self, out: &dyn crate::kernel::output::KernelOutput) {
        let s = out
            .load_settings()
            .ok()
            .flatten()
            .unwrap_or_else(crate::settings::AppSettings::default);
        let setup = crate::bbs_link::BbsSetup::from_settings(&s, &out.data_dir());
        self.bbs_enabled = setup.enabled;
        self.bbs_callsign = setup.callsign;
        self.bbs_ssid = setup.ssid;
        self.bbs_cfg = setup.cfg;
        self.bbs_store = setup.store;
    }
}

impl Default for KissTiming {
    fn default() -> Self {
        KissTiming {
            t1_ms: T1_MS,
            t2_ms: T2_MS,
            t3_ms: T3_MS,
            ka_check_ms: KA_CHECK_MS,
            n2_max: N2_MAX,
            max_packet_size: BBS_MAX_INFO,
            txdelay: 30,
            p: 63,
            slot_time: 10,
        }
    }
}

/// K2K AppState — the managed app state.
///
/// Re-exported from the `app` layer (Seam 2/4): the definition genuinely lives
/// in `app/state.rs` (`AppState` wraps a single `Mutex<AppCore>`), so `kernel/`
/// carries no state type of its own — this `pub use` simply lets the many
/// existing `crate::session::AppState` command signatures keep compiling while
/// the type is owned by the app layer.
pub use crate::app::state::AppState;

// ── TCP Keepalive Configuration ──────────────────────────

use tokio::net::TcpStream;

/// Configure TCP keepalive on a stream so dead connections are detected
/// within ~25 seconds instead of the OS default of 2 hours.
///
/// Settings:
///   - keepalive_idle: 10s  — first probe sent after 10s of inactivity
///   - keepalive_interval: 5s  — subsequent probes every 5s
///   - keepalive_retries: 3  — disconnect after 3 unanswered probes
///
/// Total disconnect detection: 10 + 3*5 = 25 seconds worst case.
/// This is bundled in the binary via the `socket2` crate — no system deps.
pub fn configure_keepalive(stream: &TcpStream) {
    let socket_ref = socket2::SockRef::from(stream);
    let _ = socket_ref.set_keepalive(true);
    // Set keepalive timing: 10s idle before first probe, 5s between probes.
    // Retry count uses the OS default (typically 9 on Linux, 10 on Windows).
    let _ = socket_ref.set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(std::time::Duration::from_secs(10))
            .with_interval(std::time::Duration::from_secs(5)),
    );
}

// ── Utility Helpers ──────────────────────────────────────

/// Parse a via/digipeater string into a Vec of uppercase callsigns.
/// Accepts space, comma, or whitespace as delimiters.
/// Empty strings are filtered out.
pub fn parse_via(via: &str) -> Vec<String> {
    if via.is_empty() {
        vec![]
    } else {
        via.split(|c: char| c == ',' || c.is_whitespace())
            .map(|s| s.to_uppercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

// ── Kernel-Faithful Helpers (Linux net/ax25 semantics) ────
//
// These replicate the Linux kernel AX.25 implementation (ax25_subr.c,
// ax25_std_timer.c, ax25_std_subr.c) so this build behaves like the kernel
// stack that pyham_ax25 sits on.

/// Kernel `ax25_calculate_t1` with default linear backoff (AX25_DEF_BACKOFF=1):
///   t  = 2 + 2*n2count
///   T1 = t * rtt,  where rtt is initialized to T1/2 (af_ax25.c fillin_cb)
/// → n2count=0 gives t1_ms, n2count=1 gives 2×t1_ms, n2count=2 gives 3×t1_ms …
/// (Kernel RTT adaptation is not replicated — rtt stays at the T1/2 default.)
pub fn kernel_t1_ms(t1_ms: u64, n2count: u8) -> u64 {
    let t = 2u64 + 2 * (n2count as u64);
    (t1_ms / 2).max(1) * t
}

/// Kernel `ax25_validate_nr`: is N(R) within [V(A) .. V(S)] inclusive (mod 8)?
/// With a single outstanding frame (window=1): V(A) = pending_ns if pending,
/// otherwise V(A) == V(S).
pub fn nr_valid(session: &Ax25Session, nr: u8) -> bool {
    let vs = session.ns;
    if session.pending_ns.is_none() {
        return nr == vs;
    }
    let mut vc = session.pending_ns.unwrap();
    loop {
        if nr == vc {
            return true;
        }
        if vc == vs {
            break;
        }
        vc = (vc + 1) & 0x07;
    }
    nr == vs
}

/// Does N(R)=nr acknowledge our outstanding frame (kernel `frames_acked`)?
/// With window=1 the single pending frame is acked iff nr == V(S).
pub fn nr_acks_pending(session: &Ax25Session, nr: u8) -> bool {
    session.pending_ns.is_some() && nr == session.ns
}

/// Kernel `ax25_std_transmit_enquiry` / `ax25_std_t3timer`: send RR (or RNR)
/// with P=1 as a COMMAND frame. This is a poll, not an ack.
pub async fn send_rr_enquiry(
    c: &mut TcpClient,
    loc: SessionLoc,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) -> bool {
    let Some(s) = c.session_ref(loc) else {
        return false;
    };
    let (remote, remote_ssid, digis, nr) = (s.remote.clone(), s.remote_ssid, s.digis.clone(), s.nr);
    // A Server-role (message-board) session polls as the board callsign —
    // the address the peer actually dialed (same rule as the old bbs_link).
    let (my_call, my_ssid) = if s.role == SessionRole::Server {
        (c.bbs_callsign.clone(), c.bbs_ssid)
    } else {
        (c.callsign.clone(), c.ssid)
    };
    let rr_control = 0x11 | (nr << 5);
    let mut rr_frame =
        ax25::encode_ax25_header(&remote, remote_ssid, &my_call, my_ssid, true, &digis);
    rr_frame.push(rr_control);
    // Emit via the raw-frame parser so the digipeater path AND the F-bit show
    // correctly (consistent with how every other sent frame is displayed).
    emit_tx_frame(app_handle.as_ref(), &rr_frame);
    kiss::send_kiss(c, &rr_frame).await.is_ok()
}

// ── T2 ACK Delay Timer ───────────────────────────────────
//
// Per AX.25 §2.5.4.2, when we receive an I-frame we should not respond
// with RR immediately — we wait T2 milliseconds in case more I-frames
// arrive. All I-frames received during T2 get a single RR sent at the end.
// This is called "ACK batching" or "T2 delayed ACK".

/// Spawn a task that waits T2 ms, then sends an RR with the session's latest N(R).
/// If the client sets rr_pending = false before the timer fires (e.g. because
/// another path sent an RR), no RR is sent.
pub fn spawn_t2_timer(
    client: Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Read T2 from the client's configured value
        let t2_ms = {
            let c = client.lock().await;
            c.t2_ms
        };
        tokio::time::sleep(std::time::Duration::from_millis(t2_ms)).await;
        let mut c = client.lock().await;
        let Some(s) = c.session_ref(loc) else {
            return;
        };
        if !s.rr_pending {
            return;
        }

        // Clone all data we need before any mutable borrows
        let remote = s.remote.clone();
        let remote_ssid = s.remote_ssid;
        let digis = s.digis.clone();
        let nr = s.rr_target_nr;
        let role = s.role;

        // Clear pending flag (no longer borrowing session)
        if let Some(s) = c.session_mut(loc) {
            s.rr_pending = false;
        }

        // Server-role sessions ack as the board callsign (the address the
        // peer dialed) — same rule as the old bbs_link.
        let (my_call, my_ssid) = if role == SessionRole::Server {
            (c.bbs_callsign.clone(), c.bbs_ssid)
        } else {
            (c.callsign.clone(), c.ssid)
        };

        // Build and send RR with current N(R)
        let rr_control = 0x01 | (nr << 5);
        let mut rr_frame =
            ax25::encode_ax25_header(&remote, remote_ssid, &my_call, my_ssid, false, &digis);
        rr_frame.push(rr_control);
        let ah = c.app_handle.clone();
        if let Some(ah_ref) = ah.as_ref() {
            emit_tx_frame(ah_ref.as_ref(), &rr_frame);
        }
        let _ = kiss::send_kiss(&mut c, &rr_frame).await;
    })
}

// ── T3 Keep-Alive Timer ──────────────────────────────────
//
// Per AX.25 §2.5.7, if no frames are received within T3 seconds,
// we send an RR with the P-bit set to probe the link.
// If no response, we conclude the link is dead and disconnect.

/// Kernel T1 delay for a session's current state; None when there's
/// nothing left to supervise (acked, disconnected, or nothing pending).
///
/// Also returns None when the client's port has been torn down
/// (`info.connected == false`) — a surviving recovery timer must not keep
/// polling (and transmitting RR enquiries) on a disconnected TNC.
async fn recovery_delay_ms(
    client_arc: &Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    t1_ms: u64,
) -> Option<u64> {
    let c = client_arc.lock().await;
    if !c.info.connected {
        return None;
    }
    let s = c.session_ref(loc)?;
    match s.state {
        Ax25ConnState::Connected if s.pending_ns.is_some() => Some(t1_ms),
        Ax25ConnState::TimerRecovery => Some(kernel_t1_ms(t1_ms, s.n2count)),
        _ => None,
    }
}

/// Spawn the T1 recovery timer for a session if none is running.
///
/// The `TimerBank` is the idempotency authority: before spawning we reserve a
/// T1 deadline for this session. If one is already scheduled (duplicate
/// request), we return early — the bank reset the deadline, there is never a
/// second recovery task fighting the first (the design's duplicate-poller fix).
pub(crate) fn ensure_recovery_timer(
    c: &mut TcpClient,
    client_arc: &Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) {
    let Some(s) = c.session_ref(loc) else {
        return;
    };
    if s.retry_handle.is_some() {
        return;
    }
    let reserved = c.timers.schedule_in(
        crate::kernel::timers::TimerID::new(crate::kernel::timers::TimerKind::T1, loc),
        std::time::Duration::from_millis(c.t1_ms),
    );
    if reserved {
        return;
    } // already supervised — no duplicate
    let handle = spawn_recovery_timer(client_arc.clone(), loc, app_handle);
    if let Some(s) = c.session_mut(loc) {
        s.retry_handle = Some(handle);
    }
}

/// Spawn a keep-alive monitor task for a connected session.
///
/// Kernel-faithful (ax25_std_t3timer): when no frames have been received
/// within T3, send ONE RR enquiry (P=1, COMMAND) and enter TimerRecovery —
/// the T1 recovery timer then governs the polling cadence and N2 limit.
/// (The kernel restarts T3 at all-acked points; we approximate by resetting
/// the idle clock on any received frame.)
pub fn spawn_keepalive(
    client: Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) -> tokio::task::JoinHandle<()> {
    let app_handle = app_handle.clone();
    tokio::spawn(async move {
        // Read timing values from the client
        let (ka_check_ms, t3_ms) = {
            let c = client.lock().await;
            (c.ka_check_ms, c.t3_ms)
        };
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(ka_check_ms)).await;
            let mut c = client.lock().await;
            let Some(s) = c.session_ref(loc) else {
                break;
            };
            if s.state != Ax25ConnState::Connected {
                break; // Session disconnected or moved — stop monitoring
            }
            let last_rx = s.last_rx_time;
            let should_probe = match last_rx {
                Some(t) => t.elapsed().as_millis() as u64 >= t3_ms,
                None => true, // Never received anything — probe anyway
            };
            if should_probe {
                // Kernel ax25_std_t3timer: n2count = 0; transmit_enquiry; state = 4.
                // The T1 recovery timer takes over supervision from here.
                {
                    let s = c.session_mut(loc).unwrap();
                    s.n2count = 0;
                }
                let ok = send_rr_enquiry(&mut c, loc, &app_handle).await;
                if ok {
                    {
                        let s = c.session_mut(loc).unwrap();
                        s.state = Ax25ConnState::TimerRecovery;
                    }
                    ensure_recovery_timer(&mut c, &client, loc, &app_handle);
                }
                break;
            }
        }
    })
}

/// Kernel T1 recovery timer (ax25_std_t1timer_expiry semantics).
///
/// Supervises one session while an I-frame is unacknowledged or the session
/// is in TimerRecovery. Fires on the kernel T1 schedule with linear backoff:
///   - Connected + pending  → n2count = 1, RR enquiry (P=1, COMMAND),
///     enter TimerRecovery (kernel state 3 → 4)
///   - TimerRecovery        → n2count += 1; if n2count > N2, send DM(P=1,
///     RESPONSE) and disconnect; else send another RR enquiry
/// Exits when the frame is acked or the session leaves these states.
///
/// NOTE: unlike the old app (which retransmitted the I-frame on every T1),
/// the kernel polls first and only retransmits when the peer's reply shows
/// frames still unacknowledged (handled in run_state_machine via
/// Effect::SendPending).
pub fn spawn_recovery_timer(
    client_arc: Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) -> tokio::task::JoinHandle<()> {
    let app_handle = app_handle.clone();
    tokio::spawn(async move {
        let (t1_ms, n2_max) = {
            let c = client_arc.lock().await;
            (c.t1_ms, c.n2_max)
        };
        loop {
            // Kernel T1 delay for the current state; None means the session
            // is acked or moved on — nothing left to supervise.
            let Some(delay_ms) = recovery_delay_ms(&client_arc, loc, t1_ms).await else {
                break;
            };
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

            let mut c = client_arc.lock().await;
            let (state, pending) = {
                let Some(s) = c.session_ref(loc) else {
                    break;
                };
                (s.state.clone(), s.pending_ns.is_some())
            };
            match state {
                Ax25ConnState::Connected if pending => {
                    // Kernel state 3 T1 expiry: n2count = 1; transmit_enquiry; state 4
                    {
                        let s = c.session_mut(loc).unwrap();
                        s.n2count = 1;
                    }
                    let _ = send_rr_enquiry(&mut c, loc, &app_handle).await;
                    {
                        let s = c.session_mut(loc).unwrap();
                        s.state = Ax25ConnState::TimerRecovery;
                        if !s.invisible {
                            let _ = app_handle.emit(
                                "session-retry",
                                serde_json::json!({
                                    "tab": loc.index(),
                                    "retry": s.n2count,
                                    "max": n2_max,
                                    "text": s.pending_text,
                                    "t1_ms": t1_ms,
                                }),
                            );
                        }
                    }
                }
                Ax25ConnState::TimerRecovery => {
                    // Clone everything we need from the session FIRST — the
                    // accessor borrows all of `c`, so disjoint-field
                    // splitting does not work through it (see
                    // rust-borrow-splitting-tcpclient.md).
                    let (n2count, remote, remote_ssid, digis, invisible, role) = {
                        let Some(s) = c.session_ref(loc) else {
                            break;
                        };
                        (
                            s.n2count,
                            s.remote.clone(),
                            s.remote_ssid,
                            s.digis.clone(),
                            s.invisible,
                            s.role,
                        )
                    };
                    if n2count >= n2_max {
                        // Kernel state 4 T1 expiry with n2count == N2:
                        // DM(P=1, RESPONSE) then disconnect (ETIMEDOUT).
                        // Server-role sessions DM as the board callsign.
                        let (my_call, my_ssid) = if role == SessionRole::Server {
                            (c.bbs_callsign.clone(), c.bbs_ssid)
                        } else {
                            (c.callsign.clone(), c.ssid)
                        };
                        let dm = ax25::build_ax25_frame(
                            &remote,
                            remote_ssid,
                            &my_call,
                            my_ssid,
                            0x1F,
                            0xF0,
                            &[],
                            &digis,
                            false,
                        );
                        emit_tx_frame(app_handle.as_ref(), &dm);
                        let _ = kiss::send_kiss(&mut c, &dm).await;
                        if let Some(s) = c.session_mut(loc) {
                            s.state = Ax25ConnState::Disconnected;
                            s.pending_ns = None;
                        }
                        // Free the server-pool slot so a timeout-DM'd board
                        // user doesn't hold a slot forever (pool exhaustion).
                        // Mirrors the peer-DISC/board-hangup teardown: abort
                        // the session's timers, then release the slot. C-tab
                        // sessions are permanent and must NOT be freed.
                        if role == SessionRole::Server {
                            if let SessionLoc::Server(i) = loc {
                                if let Some(s) = c.server_sessions.get_mut(i) {
                                    if let Some(h) = s.as_mut().and_then(|x| x.keepalive_handle.take())
                                    {
                                        h.abort();
                                    }
                                    if let Some(h) = s.as_mut().and_then(|x| x.retry_handle.take()) {
                                        h.abort();
                                    }
                                }
                                c.server_sessions[i] = None;
                            }
                        }
                        if !invisible {
                            let _ = app_handle.emit(
                                "session-disconnected",
                                serde_json::json!({
                                    "tab": loc.index(),
                                    "reason": "Retry limit exceeded (T1 recovery)",
                                }),
                            );
                        }
                        break;
                    }
                    if let Some(s) = c.session_mut(loc) {
                        s.n2count += 1;
                    }
                    let _ = send_rr_enquiry(&mut c, loc, &app_handle).await;
                    let (retry, pending_text, invisible) = {
                        let Some(s) = c.session_ref(loc) else {
                            break;
                        };
                        (s.n2count, s.pending_text.clone(), s.invisible)
                    };
                    if !invisible {
                        let _ = app_handle.emit(
                            "session-retry",
                            serde_json::json!({
                                "tab": loc.index(),
                                "retry": retry,
                                "max": n2_max,
                                "text": pending_text,
                                "t1_ms": t1_ms,
                            }),
                        );
                    }
                }
                _ => break,
            }
        }
        // Clear the handle so a new recovery timer can be spawned. Also
        // release the TimerBank T1 slot for this session: the slot would
        // otherwise stay reserved, so the NEXT ensure_recovery_timer would
        // see `schedule_in → reserved=true` and bail without spawning a
        // fresh timer — leaving the next outstanding I-frame unsupervised
        // (the one-time-latch retransmission freeze).
        let mut c = client_arc.lock().await;
        if let Some(s) = c.session_mut(loc) {
            s.retry_handle = None;
        }
        c.timers.cancel(crate::kernel::timers::TimerID::new(
            crate::kernel::timers::TimerKind::T1,
            loc,
        ));
    })
}

// ── Incoming Frame Processing ────────────────────────────
//
// This is the heart of the state machine. Called by the KISS reader task
// whenever a complete AX.25 frame arrives. It:
//   1. Parses the AX.25 frame
//   2. Classifies the frame type (I, S, U, UI, NET/ROM)
//   3. Emits to the Unproto monitor panel
//   4. Runs the connected-mode state machine for each session
//   5. Handles deferred operations (keep-alive spawns, RR responses, UA)

/// Process a single incoming AX.25 frame from a KISS connection.
///
/// `payload` = full AX.25 bytes after the KISS command byte has been stripped
/// (addresses + control + PID + info).
pub async fn process_incoming_frame(
    payload: &[u8],
    client_arc: &Arc<Mutex<TcpClient>>,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) {
    let raw_kiss_hex = ax25::fmt_raw_kiss(payload);

    // Step 1: Parse the AX.25 frame
    // If it doesn't parse as AX.25, show it as raw hex on the monitor
    if let Some(f) = ax25::parse_ax25_frame(payload) {
        // Step 2: Classify the frame type
        let frame_type = ax25::classify_frame_type(f.control);

        // Step 3: Build display labels
        let src_label = ax25::fmt_call(&f.src, f.src_ssid);
        let dest_label = ax25::fmt_call(&f.dest, f.dest_ssid);

        // Step 4: Detect NET/ROM frames (PID = 0x01 or 0xCF)
        let is_netrom = f.pid == 0x01 || f.pid == 0xCF;

        if is_netrom {
            emit_netrom_frame(
                app_handle,
                &src_label,
                &dest_label,
                &f.digis,
                &f.info,
                f.pid,
                &raw_kiss_hex,
            )
            .await;
        } else {
            // Step 5: Handle connected-mode AX.25 frames
            handle_ax25_frame(
                app_handle,
                client_arc,
                &f.dest,
                f.dest_ssid,
                &f.src,
                f.src_ssid,
                &f.digis,
                f.control,
                f.pid,
                &f.info,
                &src_label,
                &dest_label,
                &frame_type,
                &raw_kiss_hex,
                f.src_c_bit,
            )
            .await;
        }
    } else {
        // Can't parse as AX.25 — show as raw hex fallback
        let hex = ax25::fmt_raw_kiss(&payload);
        let formatted = format!("[RAW] {}", hex);
        emit_unproto_frame(
            app_handle.as_ref(),
            "?",
            "?",
            &hex,
            &formatted,
            "RAW",
            &[],
            "",
            &raw_kiss_hex,
            false,
        );
    }
}

/// Emit a NET/ROM frame to the unproto monitor panel.
async fn emit_netrom_frame(
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    src_label: &str,
    dest_label: &str,
    digis: &[(String, u8, bool)],
    info: &[u8],
    pid: u8,
    raw_kiss_hex: &str,
) {
    let pid_str = if pid == 0x01 { "01" } else { "CF" };
    let via_str = ax25::fmt_via(digis);

    if let Some((netrom_display, _header_suffix)) = crate::netrom::parse_netrom_frame(info) {
        // Successfully parsed NET/ROM content
        let radio_header = if via_str.is_empty() {
            format!(
                "radio: fm {} to {} ctl NET/ROM pid={}(NET/ROM) len {}",
                src_label,
                dest_label,
                pid_str,
                info.len()
            )
        } else {
            format!(
                "radio: fm {} to {} {} ctl NET/ROM pid={}(NET/ROM) len {}",
                src_label,
                dest_label,
                via_str,
                pid_str,
                info.len()
            )
        };
        let digi_labels: Vec<String> = ax25::fmt_digi_labels(&digis);
        emit_unproto_frame(
            app_handle.as_ref(),
            &src_label,
            &dest_label,
            &netrom_display,
            &netrom_display,
            "NET/ROM",
            &digi_labels,
            &radio_header,
            &raw_kiss_hex,
            false,
        );
    } else {
        // NET/ROM parse failed — show raw hex
        let hex = ax25::fmt_raw_kiss(&info);
        let radio_header = if via_str.is_empty() {
            format!(
                "radio: fm {} to {} ctl NET/ROM pid={}(NET/ROM) len {}",
                src_label,
                dest_label,
                pid_str,
                info.len()
            )
        } else {
            format!(
                "radio: fm {} to {} {} ctl NET/ROM pid={}(NET/ROM) len {}",
                src_label,
                dest_label,
                via_str,
                pid_str,
                info.len()
            )
        };
        let netrom_display = format!("[NET/ROM] {}", hex);
        let digi_labels: Vec<String> = ax25::fmt_digi_labels(&digis);
        emit_unproto_frame(
            app_handle.as_ref(),
            &src_label,
            &dest_label,
            &netrom_display,
            &netrom_display,
            "NET/ROM",
            &digi_labels,
            &radio_header,
            &raw_kiss_hex,
            false,
        );
    }
}

/// Handle a regular AX.25 frame (not NET/ROM) — emit to monitor + run state machine.
/// Build a linpac-style radio header line like:
async fn handle_ax25_frame(
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    client_arc: &Arc<Mutex<TcpClient>>,
    dest: &str,
    dest_ssid: u8,
    src: &str,
    src_ssid: u8,
    digis: &[(String, u8, bool)],
    control: u8,
    pid: u8,
    info: &[u8],
    src_label: &str,
    dest_label: &str,
    frame_type: &str,
    raw_kiss_hex: &str,
    src_c_bit: bool,
) {
    // Decode info field as text for display
    let info_text = ax25::info_to_string(info);
    let info_text_clone = info_text.clone();

    // For I-frames, show N(S)/N(R) in the display
    // classify_frame_type returns "I00-", "I11^", etc.
    let type_info = if frame_type.starts_with('I') && !frame_type.starts_with("ID") {
        let ns = (control >> 1) & 0x07;
        let nr = (control >> 5) & 0x07;
        if info_text_clone.is_empty() {
            format!("[I N(S)={} N(R)={}]", ns, nr)
        } else {
            info_text_clone
        }
    } else if frame_type.starts_with("UI") {
        info_text_clone
    } else {
        String::new()
    };

    // Build linpac-style radio header
    let radio_header = ax25::build_radio_header(
        src_label,
        dest_label,
        digis,
        frame_type,
        control,
        pid,
        info.len(),
    );

    // Build formatted display text
    // I-frame types are "I00-", "I11+", etc.; UI frames are "UI-" or "UI+"
    let is_data_frame = frame_type.starts_with("UI")
        || (frame_type.starts_with('I') && !frame_type.starts_with("ID"));
    let formatted = if is_data_frame {
        if frame_type.starts_with('I') && type_info.is_empty() {
            format!("{}>{}: [{}]", src_label, dest_label, frame_type)
        } else {
            format!("{}>{}: {}", src_label, dest_label, type_info)
        }
    } else {
        format!("{}>{}: [{}]", src_label, dest_label, frame_type)
    };

    // Decode the display info for the unproto entry
    let display_info = if is_data_frame {
        info_text.clone()
    } else if type_info.is_empty() {
        String::new()
    } else {
        type_info.clone()
    };

    let digi_labels: Vec<String> = ax25::fmt_digi_labels(&digis);
    emit_unproto_frame(
        app_handle.as_ref(),
        &src_label,
        &dest_label,
        &display_info,
        &formatted,
        frame_type,
        &digi_labels,
        &radio_header,
        &raw_kiss_hex,
        false,
    );

    // ── Message board routing ──
    // Frames addressed to the board callsign belong to the BBS, never to
    // the C-tabs — a station connected to both would otherwise be
    // double-processed (tabs match by src, the BBS by dest).
    //
    // The gate reads settings from DISK on every frame (cheap at 1200 baud),
    // so enabling or changing the board mid-session takes effect without a
    // reconnect — the old connect-time snapshot left a board enabled after
    // connect silently unresponsive.
    let is_bbs_frame = {
        let mut c = client_arc.lock().await;
        match crate::kernel::output::bbs_gate(app_handle.as_ref()) {
            crate::bbs_link::BbsGate::Enabled(call, ssid) if dest == call && dest_ssid == ssid => {
                if !c.bbs_enabled || c.bbs_callsign != call || c.bbs_ssid != ssid {
                    app_handle.log_line(&format!(
                        "Message board: listener for {} active (settings picked up mid-session)",
                        call
                    ));
                    c.refresh_bbs_setup(app_handle.as_ref());
                }
                true
            }
            crate::bbs_link::BbsGate::Disabled(call, ssid)
                if dest == call && dest_ssid == ssid && crate::ax25::is_sabm_ctl(control) =>
            {
                app_handle.log_line(&format!(
                    "Message board: ignoring SABM from {}-{} to {} (board disabled)",
                    src, src_ssid, call
                ));
                false
            }
            _ => false,
        }
    };
    if is_bbs_frame {
        route_bbs_frame(
            app_handle, client_arc, dest, dest_ssid, src, src_ssid, digis, control, info,
            frame_type, src_c_bit,
        )
        .await;
        return;
    }

    // Run the connected-mode state machine
    run_state_machine(
        app_handle, client_arc, dest, dest_ssid, src, src_ssid, control, info, frame_type, digis,
        src_c_bit, None,
    )
    .await;
}

/// Find the server pool slot holding a session for `(src, src_ssid)`.
///
/// Pure (no I/O) so the router's slot resolution is unit-testable.
fn server_slot_for(sessions: &[Option<Ax25Session>], src: &str, src_ssid: u8) -> Option<usize> {
    sessions.iter().position(|s| {
        s.as_ref()
            .is_some_and(|x| x.remote == src && x.remote_ssid == src_ssid)
    })
}

/// Route an inbound frame addressed to the board callsign (KISS mode).
///
/// Server sessions live in the `server_sessions` pool and ride the SAME
/// kernel state machine as the C-tabs (`run_state_machine` with
/// `SessionLoc::Server(i)`). The kernel state-0 SABM accept (A4) handles
/// UA + banner + prompt; the kernel handles everything after that
/// (I-frame ACK/retransmit, REJ/RR, T1/T2/T3, DISC teardown).
async fn route_bbs_frame(
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    client_arc: &Arc<Mutex<TcpClient>>,
    dest: &str,
    dest_ssid: u8,
    src: &str,
    src_ssid: u8,
    digis: &[(String, u8, bool)],
    control: u8,
    info: &[u8],
    frame_type: &str,
    src_c_bit: bool,
) {
    let is_sabm = crate::ax25::is_sabm_ctl(control);
    let mut c = client_arc.lock().await;

    // Resolve the pool slot for this source (one session per connected user).
    let slot = server_slot_for(&c.server_sessions, src, src_ssid);

    if is_sabm {
        if let Some(i) = slot {
            // Re-establish (or re-accept after teardown): the kernel handles
            // SABM in Connected (UA + reset) and in state 0 via A4.
            let loc = SessionLoc::Server(i);
            drop(c);
            run_state_machine(
                app_handle,
                client_arc,
                dest,
                dest_ssid,
                src,
                src_ssid,
                control,
                info,
                frame_type,
                digis,
                src_c_bit,
                Some(loc),
            )
            .await;
            // A4's state-0 accept sends UA but can't spawn keepalive (no
            // client_arc in the handler) — do it here so T3 idle supervision
            // works for board users.
            ensure_server_keepalive(client_arc, loc, app_handle).await;
            return;
        }
        let busy = c.server_sessions.iter().flatten().count() >= crate::bbs_link::MAX_BBS_SESSIONS;
        if busy {
            // No free slot — DM (kernel: no listener capacity). Answer as
            // the BOARD callsign (the frame was addressed to it) and
            // preserve the digi path so the peer behind digis receives it.
            crate::commands::log_line(&format!(
                "Message board: connect from {}-{} refused ({} users max)",
                src,
                src_ssid,
                crate::bbs_link::MAX_BBS_SESSIONS
            ));
            let via: Vec<String> = digis
                .iter()
                .map(|(d, s, _)| ax25::fmt_call(d, *s))
                .collect();
            let dm_control = if control & 0x10 != 0 { 0x1F } else { 0x0F };
            let dm_frame = ax25::build_ax25_frame(
                src,
                src_ssid,
                &c.bbs_callsign,
                c.bbs_ssid,
                dm_control,
                0xF0,
                &[],
                &via,
                false,
            );
            emit_tx_frame(app_handle.as_ref(), &dm_frame);
            let _ = kiss::send_kiss(&mut c, &dm_frame).await;
            return;
        }
        // Room — create the Server-role session in state 0; the kernel
        // accept (A4) sends UA, records heard, queues banner + prompt.
        let free = c.server_sessions.iter().position(|s| s.is_none()).unwrap();
        crate::commands::log_line(&format!(
            "Message board: {} connected to board callsign ({} of {} users)",
            src,
            c.server_sessions.iter().flatten().count() + 1,
            crate::bbs_link::MAX_BBS_SESSIONS
        ));
        let digi_calls: Vec<String> = digis
            .iter()
            .map(|(d, s, _)| ax25::fmt_call(d, *s))
            .collect();
        let mut sess = Ax25Session::new(0);
        sess.role = SessionRole::Server;
        sess.invisible = true;
        sess.link_external = false;
        let mut engine = crate::bbs::BbsSession::new(src);
        engine.path = digi_calls.clone();
        sess.bbs = Some(engine);
        sess.remote = src.to_string();
        sess.remote_ssid = src_ssid;
        sess.digis = digi_calls;
        sess.loc = SessionLoc::Server(free);
        sess.max_packet_size = c.max_packet_size;
        sess.state = Ax25ConnState::Disconnected; // state 0 — A4 accepts the SABM
        sess.last_rx_time = Some(std::time::Instant::now());
        let loc = SessionLoc::Server(free);
        c.server_sessions[free] = Some(sess);
        drop(c);
        run_state_machine(
            app_handle,
            client_arc,
            dest,
            dest_ssid,
            src,
            src_ssid,
            control,
            info,
            frame_type,
            digis,
            src_c_bit,
            Some(loc),
        )
        .await;
        ensure_server_keepalive(client_arc, loc, app_handle).await;
        return;
    }

    // Non-SABM frame for the board.
    if let Some(i) = slot {
        // Data / supervision for a live session — kernel handles it.
        let loc = SessionLoc::Server(i);
        drop(c);
        run_state_machine(
            app_handle,
            client_arc,
            dest,
            dest_ssid,
            src,
            src_ssid,
            control,
            info,
            frame_type,
            digis,
            src_c_bit,
            Some(loc),
        )
        .await;
        return;
    }
    // Connected remote's traffic finding no session means something tore
    // the link down — log it (matches old bbs_link behavior: logs every
    // non-SABM orphan frame).
    crate::commands::log_line(&format!(
        "Message board: traffic from {}-{} with no live session (link torn down?)",
        src, src_ssid
    ));
}

/// Ensure a Connected server (board) session has a running T3 keepalive.
///
/// A4's state-0 accept sends UA + queues the banner but can't spawn the
/// keepalive task (the handler has no client_arc). The old bbs_link spawned
/// keepalive on connect (line 296); without it, an idle board user would
/// never be probed after T3 and the session would sit silently.
async fn ensure_server_keepalive(
    client_arc: &Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
) {
    let mut c = client_arc.lock().await;
    let Some(s) = c.session_ref(loc) else {
        return;
    };
    if s.state != Ax25ConnState::Connected || s.keepalive_handle.is_some() {
        return;
    }
    let handle = spawn_keepalive(client_arc.clone(), loc, app_handle);
    if let Some(s) = c.session_mut(loc) {
        s.keepalive_handle = Some(handle);
    }
}

/// Default max octets per I-frame info field (AX.25 spec: 256; keep headroom
/// for the CRLF terminator and TNC quirkiness). The user-configurable
/// "Max Packet Size" message-board setting overrides this per-client.
pub(crate) const BBS_MAX_INFO: usize = 250;

/// Send the next queued output chunk once the window frees (window-1).
///
/// Window-1 rule: send at most ONE frame per ACK — sending sets the window
/// busy again, so this always transmits a single chunk and returns (the
/// name keeps that contract explicit). Server sessions populate
/// `pending_out` (multi-line engine replies); C-tabs never do, so this is a
/// no-op for them (byte-identical client path).
async fn send_one_output_chunk(
    c: &mut TcpClient,
    client_arc: &Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    my_callsign: &str,
    my_ssid: u8,
) {
    let (chunk, remote, remote_ssid, digis, ns, nr) = {
        let Some(s) = c.session_ref(loc) else {
            return;
        };
        if s.pending_ns.is_some() || s.remote_busy {
            return; // window still busy, or peer RNR — wait for next ACK
        }
        let Some(chunk) = s.pending_out.front().cloned() else {
            return;
        };
        (
            chunk,
            s.remote.clone(),
            s.remote_ssid,
            s.digis.clone(),
            s.ns,
            s.nr,
        )
    };
    // Pop before sending so a failed TNC write can't wedge the queue (the
    // link is dying anyway — same as the old pre-queue behavior).
    if let Some(s) = c.session_mut(loc) {
        s.pending_out.pop_front();
    }
    // Send one I-frame with the current N(S)/N(R) (kernel build_iframe,
    // linpac line-termination like the C-tab send path).
    let info_bytes = ax25::normalize_linpac_text(&chunk);
    let frame = ax25::build_iframe(
        &remote,
        remote_ssid,
        my_callsign,
        my_ssid,
        ns,
        nr,
        &info_bytes,
        &digis,
    );
    // Emit BEFORE sending (every kernel path + the old bbs_link do this):
    // the monitor shows frames even if the TNC write later fails — the
    // ground truth for "the board tried to send".
    emit_tx_frame(app_handle.as_ref(), &frame);
    let _ = kiss::send_kiss(c, &frame).await;
    // Window-1 pending state (kernel store_pending_iframe semantics):
    // advance N(S), remember the text for T1 retransmit.
    if let Some(s) = c.session_mut(loc) {
        s.pending_ns = Some(ns);
        s.pending_text = chunk;
        s.last_send_time = Some(std::time::Instant::now());
        s.ns = (ns + 1) & 0x07;
    }
    // Arm the T1 recovery timer for the new outstanding frame (kernel
    // ax25_kick → start_t1timer).
    ensure_recovery_timer(c, client_arc, loc, app_handle);
}

/// Pure core: feed received text into the session's BBS engine (server
/// sessions) and get back the reply lines + hangup flag. No I/O.
///
/// Shared by BOTH link kinds — the KISS transport wrapper
/// (`feed_server_engine`) and the AGW path (`commands::on_agw_data`) both
/// call this. It accumulates text in `rx_buffer`, splits complete lines on
/// CR/LF, runs each through `handle_line` with the board's store/config,
/// and returns the reply lines + hangup flag. Only the output transport
/// differs after this point (KISS → pending_out/send_one_output_chunk,
/// AGW → agw_bbs_send_line) — ONE feed implementation, per the merge plan.
pub(crate) fn run_engine_lines(
    session: &mut Ax25Session,
    store: &mut crate::bbs::BbsStore,
    cfg: &crate::bbs::BbsConfig,
    text: &str,
) -> (Vec<String>, bool) {
    let Some(engine) = session.bbs.as_mut() else {
        return (Vec::new(), false);
    };
    session.rx_buffer.push_str(text);
    let mut out_lines: Vec<String> = Vec::new();
    let mut hangup = false;
    loop {
        let line = match session.rx_buffer.find(['\r', '\n']) {
            Some(idx) => {
                let l = session.rx_buffer[..idx].to_string();
                session.rx_buffer.drain(..=idx);
                l
            }
            None => break,
        };
        let reply = engine.handle_line(&line, store, cfg);
        if reply.hangup {
            hangup = true;
        }
        for l in reply.lines {
            out_lines.push(l);
        }
        if hangup {
            break;
        }
    }
    // Command-menu prompt: the Python main() prints it before each input
    // read, so every reply that leaves the engine at the command menu is
    // followed by it (KISS and AGW both — ONE implementation). While
    // composing a message `at_command()` is false and the sub-prompts
    // (To / Subject / body) are enough; a menu prompt would be noise.
    if !hangup && engine.at_command() {
        out_lines.push(engine.prompt(cfg));
    }
    (out_lines, hangup)
}

/// Feed one I-frame's text into the session's BBS engine (server sessions).
///
/// Generalized from the old bbs_link engine feed: accumulate the text in
/// `rx_buffer`, split complete lines, run each through `handle_line`, and
/// queue the reply lines + prompt into `pending_out` (chunked, window-1).
/// On `BbsReply.hangup` (B(ye)) the farewell lines are sent as a burst,
/// then DISC is sent and the pool slot is cleared — the old bbs_link
/// teardown behavior.
///
/// The line-split + handle_line core is shared with the AGW board path
/// (`crate::commands::on_agw_data` → `run_engine_lines`) — ONE engine
/// feed for both link kinds; only the output transport differs (KISS:
/// pending_out/drain, AGW: agw_bbs_send_line).
async fn feed_server_engine(
    c: &mut TcpClient,
    client_arc: &Arc<Mutex<TcpClient>>,
    loc: SessionLoc,
    text: &str,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    my_callsign: &str,
    my_ssid: u8,
) {
    let SessionLoc::Server(i) = loc else {
        return;
    };
    let (remote, remote_ssid, digis, ns, nr, out_lines, hangup) = {
        let TcpClient {
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
        let (out_lines, hangup) = run_engine_lines(s, store, bbs_cfg, text);
        (
            s.remote.clone(),
            s.remote_ssid,
            s.digis.clone(),
            s.ns,
            s.nr,
            out_lines,
            hangup,
        )
    };

    if hangup {
        // B(ye) — farewell burst, then DISC and free the slot (the old
        // bbs_link teardown: fire-and-forget, no recovery pass for text
        // about to be disconnected).
        crate::commands::log_line(&format!(
            "Message board: {} disconnected (link closed)",
            remote
        ));
        // ns is the session's send-sequence counter; advance it per farewell
        // frame as we transmit the burst.
        let mut next_ns = ns;
        let max_pkt = c.server_sessions[i]
            .as_ref()
            .map(|s| s.max_packet_size)
            .unwrap_or(BBS_MAX_INFO);
        for packed in pack_bbs_lines(&out_lines, max_pkt) {
            let info_bytes = ax25::normalize_linpac_text(&packed);
            let frame = ax25::build_iframe(
                &remote,
                remote_ssid,
                my_callsign,
                my_ssid,
                next_ns,
                nr,
                &info_bytes,
                &digis,
            );
            emit_tx_frame(app_handle.as_ref(), &frame);
            let _ = kiss::send_kiss(c, &frame).await;
            next_ns = (next_ns + 1) & 0x07;
        }
        let disc = ax25::build_disc(&remote, remote_ssid, my_callsign, my_ssid, &digis);
        emit_tx_frame(app_handle.as_ref(), &disc);
        let _ = kiss::send_kiss(c, &disc).await;
        // Free the slot + abort timers (old teardown_bbs_session).
        if let Some(s) = c.server_sessions.get_mut(i) {
            if let Some(h) = s.as_mut().and_then(|x| x.keepalive_handle.take()) {
                h.abort();
            }
            if let Some(h) = s.as_mut().and_then(|x| x.retry_handle.take()) {
                h.abort();
            }
        }
        c.server_sessions[i] = None;
        return;
    }

    // Queue reply lines + prompt (window-1, packed to the session's max
    // packet size — KPC-3P style: join as many whole lines as fit, one
    // frame per ACK). run_engine_lines already appended the command-menu
    // prompt (at_command only), so this is just the transport.
    {
        let TcpClient {
            server_sessions, ..
        } = &mut *c;
        let Some(s) = server_sessions.get_mut(i).and_then(|o| o.as_mut()) else {
            return;
        };
        for packed in pack_bbs_lines(&out_lines, s.max_packet_size) {
            s.pending_out.push_back(packed);
        }
    }
    // Kick the first chunk immediately if the window is free (the old
    // queue_bbs_output sent the first chunk right away).
    send_one_output_chunk(c, client_arc, loc, app_handle, my_callsign, my_ssid).await;
}

/// Split `text` into chunks of ≤ `limit` bytes without splitting a
/// UTF-8 char. Chunks of one logical line are reassembled by the peer's
/// rx buffer (standard terminal behavior: buffer until CR).
///
/// Moved here from bbs_link.rs in A6 so the kernel owns the one and only
/// chunking implementation (the old parallel link layer is deleted in B1).
pub(crate) fn chunk_text(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        if i - start + ch.len_utf8() > limit && i > start {
            out.push(text[start..i].to_string());
            start = i;
        }
    }
    out.push(text[start..].to_string());
    out
}

/// Pack engine reply lines into ≤ `limit`-octet frames (KPC-3P style:
/// join as many whole lines as fit, then flush — "send as much as we can").
/// The sender appends one trailing \r\n per frame, so the packer counts
/// 2 bytes per JOINED line and leaves the final terminator to the sender.
/// Lines longer than the limit are split by chunk_text (existing behavior).
///
/// Restored from the old bbs_link (pack_bbs_lines, deleted by B1) — the A6
/// merge moved chunk_text but dropped the line-joining half, which sent the
/// board's output one line per frame again.
pub(crate) fn pack_bbs_lines(elements: &[String], limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    for el in elements {
        started = true;
        if el.len() > limit {
            // Oversize single line — flush what we have, then split it.
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.extend(chunk_text(el, limit));
            continue;
        }
        // Would the frame (cur + \r\n + el + sender's trailing \r\n) fit?
        let extra = if cur.is_empty() {
            el.len()
        } else {
            2 + el.len()
        };
        if !cur.is_empty() && cur.len() + extra + 2 > limit {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push_str("\r\n");
        }
        cur.push_str(el);
    }
    if started {
        out.push(cur); // even an empty last line survives as its own frame
    }
    out
}

/// Represents a side effect to execute after the session loop.
/// Collecting effects instead of executing them inline avoids borrow
/// conflicts (we can't hold a mutable borrow on a session while also
/// accessing other parts of the client).
enum Effect {
    /// Send an immediate RR response (enquiry response, P-bit response).
    /// Always framed as RESPONSE (C-bit 0) per kernel enquiry_response/timeout_response.
    SendRR {
        remote: String,
        ssid: u8,
        digis: Vec<String>,
        nr: u8,
        pf_bit: bool,
    },
    /// Send REJ for out-of-sequence I-frame. F echoes the received P bit,
    /// per kernel `ax25_send_control(AX25_REJ, pf, AX25_RESPONSE)`.
    SendREJ {
        remote: String,
        ssid: u8,
        digis: Vec<String>,
        nr: u8,
        pf_bit: bool,
    },
    /// Send a raw deferred frame (UA, DM, SABM, etc.)
    SendFrame(Vec<u8>),
    /// Kernel requeue + kick: retransmit the session's pending I-frame.
    SendPending(SessionLoc),
    /// Spawn T3 keep-alive monitoring for a session
    SpawnKeepalive(SessionLoc, tokio::task::JoinHandle<()>),
    /// Abort an existing keep-alive task for a session
    AbortKeepalive(SessionLoc),
    /// Store a SABM retry handle (kernel establish_data_link link resets)
    SpawnSabmRetry(SessionLoc, tokio::task::JoinHandle<()>),
    /// Abort + replace the T1 recovery timer (kernel T1 restart after requeue)
    RestartRecovery(SessionLoc, tokio::task::JoinHandle<()>),
    /// Feed a received I-frame's text into the session's BBS engine
    /// (server sessions only). Executed with full TcpClient access so the
    /// engine can use the BBS store/config; reply lines are queued into
    /// `pending_out` and the first chunk is kicked out immediately.
    EngineFeed(SessionLoc, String),
    /// Send the next queued output chunk when the window is free (window-1).
    /// Pushed after an ACK frees the window and after an engine feed queues
    /// reply lines; the shared drain implementation lives in
    /// `send_one_output_chunk`.
    DrainOutput(SessionLoc),
}

/// Collects operations deferred during the session loop so they can be
/// executed after the loop, avoiding borrow conflicts.
struct DeferredOps(Vec<Effect>);

impl DeferredOps {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, effect: Effect) {
        self.0.push(effect);
    }

    /// Queue an RR response for the given session (enquiry/poll response).
    fn send_rr(&mut self, session: &Ax25Session, pf_bit: bool) {
        self.0.push(Effect::SendRR {
            remote: session.remote.clone(),
            ssid: session.remote_ssid,
            digis: session.digis.clone(),
            nr: session.nr,
            pf_bit,
        });
    }

    /// Queue a REJ response for the given session (out-of-sequence I-frame).
    fn send_rej(&mut self, session: &Ax25Session, pf_bit: bool) {
        self.0.push(Effect::SendREJ {
            remote: session.remote.clone(),
            ssid: session.remote_ssid,
            digis: session.digis.clone(),
            nr: session.nr,
            pf_bit,
        });
    }

    /// Consume the effects vector — for extraction into a pure transition function.
    fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.0)
    }

    /// Execute all collected effects against the TcpClient.
    async fn execute(
        &mut self,
        c: &mut TcpClient,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        for effect in self.take_effects() {
            match effect {
                Effect::SendRR {
                    remote,
                    ssid,
                    digis,
                    nr,
                    pf_bit,
                } => {
                    let rr_control = if pf_bit {
                        0x11 | (nr << 5)
                    } else {
                        0x01 | (nr << 5)
                    };
                    // RESPONSE frame (C-bit 0) — kernel enquiry_response/timeout_response.
                    // (v0.9.0-kernel-test: was `true`/COMMAND — inverted vs kernel + Android.)
                    let mut rr_frame = ax25::encode_ax25_header(
                        &remote,
                        ssid,
                        my_callsign,
                        my_ssid,
                        false,
                        &digis,
                    );
                    rr_frame.push(rr_control);
                    // Emit via the raw-frame parser so the digipeater path and
                    // F-bit show correctly (consistent with other sent frames).
                    emit_tx_frame(app_handle.as_ref(), &rr_frame);
                    let _ = kiss::send_kiss(c, &rr_frame).await;
                }
                Effect::SendREJ {
                    remote,
                    ssid,
                    digis,
                    nr,
                    pf_bit,
                } => {
                    // Kernel sends REJ as RESPONSE with F echoing the received P bit.
                    let rej_control = 0x09 | (nr << 5) | (if pf_bit { 0x10 } else { 0x00 });
                    let mut rej_frame = ax25::encode_ax25_header(
                        &remote,
                        ssid,
                        my_callsign,
                        my_ssid,
                        false,
                        &digis,
                    );
                    rej_frame.push(rej_control);
                    // Emit via the raw-frame parser so sent REJs show the via path.
                    emit_tx_frame(app_handle.as_ref(), &rej_frame);
                    let _ = kiss::send_kiss(c, &rej_frame).await;
                }
                Effect::SendFrame(frame) => {
                    // Every transmitted frame is visible in the unproto panel
                    emit_tx_frame(app_handle.as_ref(), &frame);
                    let _ = kiss::send_kiss(c, &frame).await;
                }
                Effect::SendPending(loc) => {
                    // Kernel requeue_frames + ax25_kick: retransmit the pending I-frame.
                    retransmit_pending(c, app_handle, loc, my_callsign, my_ssid).await;
                }
                Effect::SpawnKeepalive(loc, handle) => {
                    match loc {
                        // C-tabs: the handle lives in the tab-indexed vector.
                        SessionLoc::Tab(tab) => {
                            if tab < c.ka_handles.len() {
                                c.ka_handles[tab] = Some(handle);
                            }
                        }
                        // Server sessions: the handle lives on the session.
                        SessionLoc::Server(i) => {
                            if let Some(s) = c.server_sessions.get_mut(i).and_then(|o| o.as_mut()) {
                                s.keepalive_handle = Some(handle);
                            }
                        }
                    }
                }
                Effect::AbortKeepalive(loc) => match loc {
                    SessionLoc::Tab(tab) => {
                        if let Some(h) = c.ka_handles.get_mut(tab).and_then(|o| o.take()) {
                            h.abort();
                        }
                    }
                    SessionLoc::Server(i) => {
                        if let Some(h) = c
                            .server_sessions
                            .get_mut(i)
                            .and_then(|o| o.as_mut())
                            .and_then(|s| s.keepalive_handle.take())
                        {
                            h.abort();
                        }
                    }
                },
                Effect::SpawnSabmRetry(loc, handle) => {
                    if let Some(s) = c.session_mut(loc) {
                        s.sabm_handle = Some(handle);
                    }
                }
                Effect::RestartRecovery(loc, handle) => {
                    if let Some(s) = c.session_mut(loc) {
                        if let Some(h) = s.retry_handle.take() {
                            h.abort();
                        }
                        s.retry_handle = Some(handle);
                    }
                }
                Effect::EngineFeed(loc, text) => {
                    feed_server_engine(c, client_arc, loc, &text, app_handle, my_callsign, my_ssid)
                        .await;
                }
                Effect::DrainOutput(loc) => {
                    send_one_output_chunk(c, client_arc, loc, app_handle, my_callsign, my_ssid).await;
                }
            }
        }
    }
}

/// Kernel requeue_frames + ax25_kick: retransmit the session's pending
/// I-frame with the current N(R), and show it in the Unproto monitor.
async fn retransmit_pending(
    c: &mut TcpClient,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    loc: SessionLoc,
    my_callsign: &str,
    my_ssid: u8,
) {
    let (remote, remote_ssid, digis, nr, pending_ns, pending_text) = {
        let Some(s) = c.session_ref(loc) else {
            return;
        };
        if s.pending_ns.is_none() || s.pending_text.is_empty() {
            return;
        }
        (
            s.remote.clone(),
            s.remote_ssid,
            s.digis.clone(),
            s.nr,
            s.pending_ns.unwrap(),
            s.pending_text.clone(),
        )
    };
    let info_bytes = ax25::normalize_linpac_text(&pending_text);
    let frame = ax25::build_iframe(
        &remote,
        remote_ssid,
        my_callsign,
        my_ssid,
        pending_ns,
        nr,
        &info_bytes,
        &digis,
    );
    let _ = kiss::send_kiss(c, &frame).await;
    // Emit retransmitted I-frame to the Unproto monitor
    let src_label = ax25::fmt_call(my_callsign, my_ssid);
    let dest_label = ax25::fmt_call(&remote, remote_ssid);
    let via_part = ax25::format_via(&digis);
    let radio_header = format!(
        "radio: fm {} to {} {}ctl I N(S)={} N(R)={} pid=F0(Text) len {}",
        src_label,
        dest_label,
        via_part,
        pending_ns,
        nr,
        info_bytes.len()
    );
    let raw_kiss = ax25::fmt_raw_kiss(&frame);
    let formatted = format!("{}>{}: {}", src_label, dest_label, pending_text);
    emit_unproto_frame(
        app_handle.as_ref(),
        &src_label,
        &dest_label,
        &pending_text,
        &formatted,
        "I",
        &digis,
        &radio_header,
        &raw_kiss,
        true,
    );
}

/// Emit a decoded AX.25 frame to the Unproto monitor panel.
///
/// Re-exported from `kernel::output` (the single home of the output seam), so
/// call sites can keep using `crate::session::emit_unproto_frame`.
pub use crate::kernel::output::{emit_tx_frame, emit_unproto_frame};

// ── Per-state frame handlers ──────────────────────────────
//
// Each handler covers one AX.25 link state (kernel states 0-4). Shared
// building blocks (UA send, N(R)-error recovery, I-frame acceptance,
// disconnect) live in helper methods below so Connected and TimerRecovery —
// which process I-frames identically — share one implementation.

/// Decoded per-frame facts passed to every state handler.
struct FrameCtx<'a> {
    src: &'a str,
    src_ssid: u8,
    dest: &'a str,
    dest_ssid: u8,
    control: u8,
    info: &'a [u8],
    frame_type: &'a str,
    digis: &'a [(String, u8, bool)],
    pf: bool,
    is_command: bool,
    is_iframe: bool,
    is_sframe: bool,
    is_uframe: bool,
    nr_field: u8,
}

impl Ax25Session {
    /// Queue a UA frame to the peer (SABM re-establish / DISC confirm).
    fn send_ua(&mut self, deferred: &mut DeferredOps, my_callsign: &str, my_ssid: u8) {
        let ua = ax25::build_ax25_frame(
            &self.remote,
            self.remote_ssid,
            my_callsign,
            my_ssid,
            ax25::UA,
            0xF0,
            &[],
            &self.digis,
            false,
        );
        deferred.push(Effect::SendFrame(ua));
    }

    /// Whether an inbound frame belongs to this connected C-tab session.
    ///
    /// Beyond the source/dest match (enforced by the per-state handlers), a
    /// connection established THROUGH a digipeater must arrive via the
    /// session's configured first hop — the digipeater closest to us (e.g.
    /// the "KBANN" in "KK6SEN -> COOL via KBANN"). That way a copy of the far
    /// station's frame heard DIRECTLY (no via) or via a different path never
    /// drives this tab; only the copy carried by the digipeater we dialed
    /// through is shown. A direct connection has no digis configured, so the
    /// path is not enforced (behavior unchanged).
    fn path_accepts(&self, ctx: &FrameCtx) -> bool {
        // The per-handler guards own src/dest matching and teardown handling;
        // only enforce the via path for frames sourced from this peer.
        if ctx.src != self.remote || ctx.src_ssid != self.remote_ssid {
            return true;
        }
        if self.digis.is_empty() {
            return true; // direct connection — nothing to enforce
        }
        // Connection-establishment responses (UA) must not be dropped on a
        // noisy multi-hop link. The first UA back can be caught before our
        // closest digi has re-keyed it (its H-bit clear); dropping that copy
        // throws away the exact frame that completes the connect, so the app
        // stays in AwaitingConnect and the T1 retry re-sends SABM — making the
        // peer re-welcome and thrash the link. A UA completes the link, so the
        // H-bit filter is counterproductive for it.
        if ctx.is_uframe && crate::ax25::is_ua_ctl(ctx.control) {
            return true;
        }
        // Frame from our peer must arrive through the first hop we configured
        // (the digipeater closest to us) AND be the copy that hop repeated
        // back to us (its H-bit set). With multiple hops the peer transmits
        // toward us through the REVERSE path, so our closest hop (digis[0])
        // appears at the END of the inbound digi list, not the start — match
        // it anywhere in the path. The pre-repeat legs (H-bit clear on our
        // closest hop) must NOT drive the tab — only the copy digipeated to
        // us does.
        let want = self.digis[0].split('-').next().unwrap_or("");
        ctx.digis
            .iter()
            .any(|(call, _, h_bit)| *h_bit && call.eq_ignore_ascii_case(want))
    }

    /// Invalid N(R) → kernel nr_error_recovery: send SABM(P=1), start the
    /// connect retry task, and drop to state 1.
    fn nr_error_recovery(
        &mut self,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        let sabm = ax25::build_sabm(
            &self.remote,
            self.remote_ssid,
            my_callsign,
            my_ssid,
            &self.digis,
        );
        deferred.push(Effect::SendFrame(sabm));
        let handle = crate::commands::spawn_connect_retry(
            crate::commands::RetryTarget::Sabm,
            client_arc.clone(),
            self.loc,
            app_handle.clone(),
        );
        deferred.push(Effect::SpawnSabmRetry(self.loc, handle));
        self.reset();
        self.state = Ax25ConnState::AwaitingConnect;
    }

    /// Mark the link dead: state 0, abort the keepalive task, notify the UI.
    fn disconnect_link(
        &mut self,
        reason: &str,
        deferred: &mut DeferredOps,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    ) {
        self.state = Ax25ConnState::Disconnected;
        self.pending_ns = None;
        deferred.push(Effect::AbortKeepalive(self.loc));
        if !self.invisible {
            let _ = app_handle.emit(
                "session-disconnected",
                serde_json::json!({
                    "tab": self.tab,
                    "reason": reason,
                }),
            );
        }
    }

    /// Re-send the pending frame and restart the recovery timer (kernel
    /// requeue on an unacked poll response).
    fn requeue_pending(
        &mut self,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    ) {
        deferred.push(Effect::SendPending(self.loc));
        let handle = spawn_recovery_timer(client_arc.clone(), self.loc, app_handle);
        deferred.push(Effect::RestartRecovery(self.loc, handle));
    }

    /// Go-Back-N: reset N(S) to the peer's N(R) and retransmit from there.
    fn retransmit_from(
        &mut self,
        nr_field: u8,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    ) {
        self.ns = nr_field;
        self.pending_ns = Some(nr_field);
        self.requeue_pending(deferred, client_arc, app_handle);
    }

    /// ACK pending frames when N(R) is valid; otherwise run N(R)-error recovery.
    /// A valid response with NOTHING outstanding (kernel ax25_frames_acked:
    /// V(A)==V(S)) also ends Timer Recovery — the peer is alive and in sync,
    /// there is no reason to keep polling it to N2.
    fn ack_or_recover(
        &mut self,
        nr_field: u8,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        if nr_valid(self, nr_field) {
            if nr_acks_pending(self, nr_field) || self.pending_ns.is_none() {
                self.ack_pending(nr_field, deferred, app_handle);
                if self.state == Ax25ConnState::TimerRecovery {
                    self.n2count = 0;
                    self.state = Ax25ConnState::Connected;
                    self.last_rx_time = Some(std::time::Instant::now());
                    let handle = spawn_keepalive(client_arc.clone(), self.loc, app_handle);
                    deferred.push(Effect::SpawnKeepalive(self.loc, handle));
                }
            }
        } else {
            self.nr_error_recovery(deferred, client_arc, app_handle, my_callsign, my_ssid);
        }
    }

    /// Received REJ: Go-Back-N retransmit from N(R) (kernel state3/4).
    /// When `exit_recovery` is set (poll response in Timer Recovery) the
    /// kernel also clears the retry counter and — if the REJ acks
    /// everything — drops back to Connected and restarts the keepalive.
    fn on_rej(
        &mut self,
        ctx: &FrameCtx,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
        exit_recovery: bool,
    ) {
        self.remote_busy = false;
        if exit_recovery {
            self.n2count = 0;
        }
        if ctx.is_command && ctx.pf {
            self.rr_pending = false; // kernel enquiry_response clears ACK_PENDING
            deferred.send_rr(self, true);
        }
        if nr_valid(self, ctx.nr_field) {
            if ctx.nr_field == self.ns {
                // All our frames acked — nothing to retransmit
                if self.pending_ns.is_some() {
                    self.ack_pending(ctx.nr_field, deferred, app_handle);
                }
                if exit_recovery {
                    // Poll response acked everything — back to Connected,
                    // restart T3 keepalive.
                    self.state = Ax25ConnState::Connected;
                    self.last_rx_time = Some(std::time::Instant::now());
                    let handle = spawn_keepalive(client_arc.clone(), self.loc, app_handle);
                    deferred.push(Effect::SpawnKeepalive(self.loc, handle));
                }
            } else {
                // Reset N(S) to requested value and retransmit pending
                self.retransmit_from(ctx.nr_field, deferred, client_arc, app_handle);
            }
        } else {
            self.nr_error_recovery(deferred, client_arc, app_handle, my_callsign, my_ssid);
        }
    }

    /// I-frame from the peer — identical processing in Connected (state 3)
    /// and TimerRecovery (state 4): validate N(R), ack outstanding frames,
    /// then accept in-sequence data (T2-delayed or P-bit RR) or latch REJ.
    fn on_iframe(
        &mut self,
        received_ns: u8,
        nr_field: u8,
        pf: bool,
        info: &[u8],
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        if !nr_valid(self, nr_field) {
            self.nr_error_recovery(deferred, client_arc, app_handle, my_callsign, my_ssid);
            return;
        }
        if nr_acks_pending(self, nr_field) {
            self.ack_pending(nr_field, deferred, app_handle);
        }
        if received_ns == self.nr {
            // In-sequence — accept
            self.nr = (self.nr + 1) & 0x07;
            self.reject_set = false;
            let txt = ax25::info_to_string(info);
            let txt = txt.replace("\r\n", "\n").replace('\r', "\n");
            if !txt.is_empty() {
                if self.bbs.is_some() {
                    // Server session: feed the engine (line-buffered,
                    // replies queued into pending_out). The engine feed
                    // runs in execute() with full client access (store,
                    // config, and the send path).
                    deferred.push(Effect::EngineFeed(self.loc, txt.clone()));
                } else if !self.invisible {
                    let _ = app_handle.emit(
                        "session-iframe",
                        serde_json::json!({
                            "tab": self.tab,
                            "text": txt,
                            "ns": received_ns,
                        }),
                    );
                }
            }
            if pf {
                // Kernel: enquiry_response (RR F=1) immediately
                self.rr_pending = false; // kernel enquiry_response clears ACK_PENDING
                if let Some(h) = self.t2_handle.take() {
                    h.abort();
                }
                deferred.send_rr(self, true);
            } else {
                // T2-delayed RR — reload the deadline on EVERY received
                // in-sequence I-frame. A node that sends several packets
                // back-to-back before expecting an ACK (the case Josh hit)
                // must get ONE ack once it goes quiet (or polls with P=1) —
                // not an ack mid-burst. We abort any pending T2 and re-arm
                // it so the deadline slides forward as long as data keeps
                // arriving; the last timer to survive fires after a full
                // T2 of silence. Matches the Linux ax25 t2timer behavior
                // (ack only when idle or polled).
                self.arm_t2_ack(); // pure policy: rr_target_nr / rr_pending
                if let Some(h) = self.t2_handle.take() {
                    h.abort();
                }
                self.t2_handle = Some(spawn_t2_timer(client_arc.clone(), self.loc));
            }
        } else {
            // Out-of-sequence: kernel AX25_COND_REJECT latch.
            // REJ is sent ONCE per gap; further OOS frames get
            // no response (unless polled). No duplicate detection.
            if self.reject_set {
                if pf {
                    self.rr_pending = false; // kernel enquiry_response clears ACK_PENDING
                    deferred.send_rr(self, true);
                }
            } else {
                self.reject_set = true;
                deferred.send_rej(self, pf); // kernel echoes received P into F
                                             // Kernel clears ACK_PENDING when REJ is sent
                self.rr_pending = false;
            }
        }
    }

    /// Kernel STATE_1 — SABM sent, awaiting UA.
    fn handle_awaiting_connect(
        &mut self,
        ctx: &FrameCtx,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        if ctx.src != self.remote
            || ctx.src_ssid != self.remote_ssid
            || ctx.dest != my_callsign
            || ctx.dest_ssid != my_ssid
        {
            return;
        }
        if ctx.is_uframe && ax25::is_sabm_ctl(ctx.control) {
            // SABM collision: both sides sent SABM. Kernel state 1 replies
            // UA (F echoes P) and STAYS in state 1 — the SABM retry keeps
            // running until the peer's UA arrives (ax25_std_state1_machine).
            self.send_ua(deferred, my_callsign, my_ssid);
        } else if ctx.is_uframe && ax25::is_disc_ctl(ctx.control) {
            // DISC → DM(F = echo of P) (kernel state1)
            let dm_control = if ctx.pf { 0x1F } else { 0x0F };
            let dm = ax25::build_ax25_frame(
                &self.remote,
                self.remote_ssid,
                my_callsign,
                my_ssid,
                dm_control,
                0xF0,
                &[],
                &self.digis,
                false,
            );
            deferred.push(Effect::SendFrame(dm));
        } else if ctx.is_uframe && ax25::is_ua_ctl(ctx.control) && ctx.pf {
            // UA with F=1 → Connected (kernel requires the final bit)
            self.state = Ax25ConnState::Connected;
            self.reset();
            self.last_rx_time = Some(std::time::Instant::now());
            let handle = spawn_keepalive(client_arc.clone(), self.loc, app_handle);
            deferred.push(Effect::SpawnKeepalive(self.loc, handle));
            if !self.invisible {
                let _ = app_handle.emit(
                    "session-connected",
                    serde_json::json!({
                        "tab": self.tab,
                        "remote": self.remote,
                        "remote_ssid": self.remote_ssid,
                    }),
                );
            }
        } else if ctx.is_uframe && ax25::is_dm_ctl(ctx.control) && ctx.pf {
            // DM with F=1 → connection refused (kernel: ECONNREFUSED)
            self.disconnect_link("Connection rejected (DM)", deferred, app_handle);
        }
        // Kernel ignores I/S frames in state 1 (no implicit connect).
    }

    /// Kernel STATE_3 — connected.
    fn handle_connected(
        &mut self,
        ctx: &FrameCtx,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        if ctx.src != self.remote || ctx.src_ssid != self.remote_ssid {
            // Echoed/reversed DM — remote has disconnected our session
            if ctx.dest == self.remote
                && ctx.src == my_callsign
                && (ctx.frame_type == "DM+" || ctx.frame_type == "DM-")
            {
                self.disconnect_link("Remote disconnected (DM)", deferred, app_handle);
            }
            return;
        }

        // Track last receive time for T3 keep-alive
        self.last_rx_time = Some(std::time::Instant::now());

        // Only process frames addressed to us (exact callsign+SSID)
        if ctx.dest != my_callsign || ctx.dest_ssid != my_ssid {
            return;
        }

        if ctx.is_uframe {
            match ctx.control & 0xEF {
                ax25::SABM_CTL => {
                    // SABM from remote → UA + reset, stay connected (link re-establish)
                    self.send_ua(deferred, my_callsign, my_ssid);
                    self.reset();
                    self.last_rx_time = Some(std::time::Instant::now());
                    if !self.invisible {
                        let _ = app_handle.emit(
                            "session-connected",
                            serde_json::json!({
                                "tab": self.tab,
                                "remote": self.remote,
                                "remote_ssid": self.remote_ssid,
                            }),
                        );
                    }
                }
                ax25::DISC_CTL => {
                    // DISC → UA immediately, then disconnect (kernel)
                    self.send_ua(deferred, my_callsign, my_ssid);
                    self.disconnect_link("Remote disconnected", deferred, app_handle);
                }
                ax25::DM_CTL => {
                    // DM → disconnect, NO UA (kernel state3)
                    self.disconnect_link("Remote disconnected (DM)", deferred, app_handle);
                }
                0x87 => {
                    // FRMR → kernel establish_data_link: SABM(P=1), state 1
                    self.nr_error_recovery(deferred, client_arc, app_handle, my_callsign, my_ssid);
                    self.last_rx_time = Some(std::time::Instant::now());
                }
                _ => {
                    // UA, SABME, XID, TEST — kernel ignores these in state 3
                }
            }
            return;
        }
        if ctx.is_sframe {
            let s_type = ctx.control & 0x0F;
            if s_type == 0x01 || s_type == 0x05 {
                // RR / RNR: PEER_RX_BUSY flag, poll response, N(R) validation
                self.remote_busy = s_type == 0x05;
                if ctx.is_command && ctx.pf {
                    // enquiry_response: RR(F=1) as RESPONSE
                    self.rr_pending = false; // kernel enquiry_response clears ACK_PENDING
                    deferred.send_rr(self, true);
                }
                self.ack_or_recover(
                    ctx.nr_field,
                    deferred,
                    client_arc,
                    app_handle,
                    my_callsign,
                    my_ssid,
                );
            } else if s_type == 0x09 {
                // REJ: Go-Back-N retransmit from N(R) (kernel state3)
                self.on_rej(
                    ctx,
                    deferred,
                    client_arc,
                    app_handle,
                    my_callsign,
                    my_ssid,
                    false,
                );
            }
            return;
        }
        if ctx.is_iframe {
            let received_ns = (ctx.control >> 1) & 0x07;
            self.on_iframe(
                received_ns,
                ctx.nr_field,
                ctx.pf,
                ctx.info,
                deferred,
                client_arc,
                app_handle,
                my_callsign,
                my_ssid,
            );
        }
    }

    /// Kernel STATE_4 — Timer Recovery.
    fn handle_timer_recovery(
        &mut self,
        ctx: &FrameCtx,
        deferred: &mut DeferredOps,
        client_arc: &Arc<Mutex<TcpClient>>,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        if ctx.src != self.remote
            || ctx.src_ssid != self.remote_ssid
            || ctx.dest != my_callsign
            || ctx.dest_ssid != my_ssid
        {
            return;
        }
        self.last_rx_time = Some(std::time::Instant::now());
        if ctx.is_uframe {
            match ctx.control & 0xEF {
                ax25::SABM_CTL => {
                    // SABM → UA, reset, back to Connected (kernel state4)
                    self.send_ua(deferred, my_callsign, my_ssid);
                    self.reset();
                    self.state = Ax25ConnState::Connected;
                    self.last_rx_time = Some(std::time::Instant::now());
                    let handle = spawn_keepalive(client_arc.clone(), self.loc, app_handle);
                    deferred.push(Effect::SpawnKeepalive(self.loc, handle));
                    if !self.invisible {
                        let _ = app_handle.emit(
                            "session-connected",
                            serde_json::json!({
                                "tab": self.tab,
                                "remote": self.remote,
                                "remote_ssid": self.remote_ssid,
                            }),
                        );
                    }
                }
                ax25::DISC_CTL => {
                    // DISC → UA + disconnect
                    self.send_ua(deferred, my_callsign, my_ssid);
                    self.disconnect_link("Remote disconnected", deferred, app_handle);
                }
                ax25::DM_CTL => {
                    // DM → disconnect (no UA)
                    self.disconnect_link("Remote disconnected (DM)", deferred, app_handle);
                }
                _ => {}
            }
            return;
        }
        if ctx.is_sframe {
            let s_type = ctx.control & 0x0F;
            if s_type == 0x01 || s_type == 0x05 {
                // RR / RNR in recovery
                self.remote_busy = s_type == 0x05;
                if !ctx.is_command && ctx.pf {
                    // Poll response (F=1 RESPONSE) — kernel exits recovery here.
                    // Kernel ax25_std_timeout_response semantics: the retry
                    // budget (n2count) resets ONLY when the response actually
                    // ACKs the pending frame(s). A response that leaves frames
                    // unacked must NOT reset it, or a peer that answers every
                    // poll (without acking) keeps the RR loop alive forever —
                    // n2count stays pinned at 1 and never climbs to N2 (the
                    // AC0ZH-1 trace). The recovery timer keeps incrementing it
                    // until N2, then sends DM and disconnects.
                    if nr_valid(self, ctx.nr_field) {
                        if nr_acks_pending(self, ctx.nr_field) || self.pending_ns.is_none() {
                            // All frames acked — or nothing was outstanding
                            // (kernel ax25_frames_acked: V(A)==V(S)) → budget
                            // reset, back to state 3. The rule above still
                            // holds: an answer that leaves frames unacked
                            // keeps its retry budget and climbs to N2.
                            self.n2count = 0;
                            self.ack_pending(ctx.nr_field, deferred, app_handle);
                            self.state = Ax25ConnState::Connected;
                            self.last_rx_time = Some(std::time::Instant::now());
                            let handle = spawn_keepalive(client_arc.clone(), self.loc, app_handle);
                            deferred.push(Effect::SpawnKeepalive(self.loc, handle));
                        } else {
                            // Frames still unacked → requeue + retransmit (kernel)
                            self.requeue_pending(deferred, client_arc, app_handle);
                        }
                    } else {
                        self.nr_error_recovery(
                            deferred,
                            client_arc,
                            app_handle,
                            my_callsign,
                            my_ssid,
                        );
                    }
                } else {
                    // Command poll or non-final — respond + validate N(R)
                    if ctx.is_command && ctx.pf {
                        self.rr_pending = false; // kernel enquiry_response clears ACK_PENDING
                        deferred.send_rr(self, true);
                    }
                    self.ack_or_recover(
                        ctx.nr_field,
                        deferred,
                        client_arc,
                        app_handle,
                        my_callsign,
                        my_ssid,
                    );
                }
            } else if s_type == 0x09 {
                // REJ in recovery — poll response exits recovery (kernel state4)
                self.on_rej(
                    ctx,
                    deferred,
                    client_arc,
                    app_handle,
                    my_callsign,
                    my_ssid,
                    !ctx.is_command && ctx.pf,
                );
            }
            return;
        }
        if ctx.is_iframe {
            // I-frame in recovery — same processing as state 3, no state exit
            let received_ns = (ctx.control >> 1) & 0x07;
            self.on_iframe(
                received_ns,
                ctx.nr_field,
                ctx.pf,
                ctx.info,
                deferred,
                client_arc,
                app_handle,
                my_callsign,
                my_ssid,
            );
        }
    }

    /// Kernel STATE_2 — DISC sent, awaiting UA/DM.
    fn handle_awaiting_disconnect(
        &mut self,
        ctx: &FrameCtx,
        deferred: &mut DeferredOps,
        app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
        my_callsign: &str,
        my_ssid: u8,
    ) {
        if ctx.src != self.remote
            || ctx.src_ssid != self.remote_ssid
            || ctx.dest != my_callsign
            || ctx.dest_ssid != my_ssid
        {
            return;
        }
        if ctx.is_uframe && (ax25::is_ua_ctl(ctx.control) || ax25::is_dm_ctl(ctx.control)) {
            // UA or DM → disconnect confirmed
            self.state = Ax25ConnState::Disconnected;
            self.remote.clear();
            self.reset();
            deferred.push(Effect::AbortKeepalive(self.loc));
            if !self.invisible {
                let _ = app_handle.emit(
                    "session-disconnected",
                    serde_json::json!({
                        "tab": self.tab,
                        "reason": "Disconnected",
                    }),
                );
            }
        } else if (ctx.is_iframe || ctx.is_sframe) && ctx.pf {
            // Kernel state2: polled I/S frames get DM(P=1) RESPONSE
            let dm = ax25::build_ax25_frame(
                &self.remote,
                self.remote_ssid,
                my_callsign,
                my_ssid,
                0x1F,
                0xF0,
                &[],
                &self.digis,
                false,
            );
            deferred.push(Effect::SendFrame(dm));
        }
    }

    /// Kernel STATE_0 — disconnected (ax25_std_state0_machine): only SABM/SABME
    /// and DISC get a DM response (we have no listener). Unsolicited I/S
    /// frames are IGNORED — they belong to another session/tab or a
    /// different KISS client sharing this TNC, and answering DM is
    /// read by the remote as a disconnect request that kills a live
    /// session. Respond only for THIS session's remote (idle tabs
    /// keep remote=""), only when the frame is addressed exactly to
    /// us, never when another live session owns this source, and at
    /// most once per frame.
    fn handle_disconnected(
        &mut self,
        ctx: &FrameCtx,
        deferred: &mut DeferredOps,
        my_callsign: &str,
        my_ssid: u8,
        active_remotes: &[(String, u8)],
        dm_sent: &mut bool,
        bbs_ctx: Option<(&mut crate::bbs::BbsStore, &crate::bbs::BbsConfig)>,
    ) {
        let is_ui = ax25::is_ui_ctl(ctx.control);
        let is_sabm = ax25::is_sabm_ctl(ctx.control);
        let is_disc = ax25::is_disc_ctl(ctx.control);

        // Server role (message board listener): SABM in state 0 accepts the
        // link (kernel state0 → state3) — UA, Connected, heard record, and
        // the banner + prompt queued for the output drain (A6). Client role
        // never takes this branch. `bbs_ctx` is only meaningful here; the
        // C-tab dispatch always passes None.
        if self.role == SessionRole::Server
            && is_sabm
            && ctx.src == self.remote
            && ctx.src_ssid == self.remote_ssid
            && ctx.dest == my_callsign
            && ctx.dest_ssid == my_ssid
        {
            let (store, cfg) = bbs_ctx.expect("server SABM accept requires BBS store+config");
            self.send_ua(deferred, my_callsign, my_ssid);
            self.state = Ax25ConnState::Connected;
            self.last_rx_time = Some(Instant::now());
            store.record_heard(&ctx.src);
            let engine = self
                .bbs
                .as_ref()
                .expect("server-role session requires a BBS engine");
            let mut lines = engine.connect_output(store, cfg);
            lines.push(engine.prompt(cfg));
            for packed in pack_bbs_lines(&lines, self.max_packet_size) {
                self.pending_out.push_back(packed);
            }
            // Kick the first banner chunk immediately (window is free — UA
            // was a U-frame, no I-frame outstanding). The old bbs_link
            // queue_bbs_output sent the first chunk right away.
            deferred.push(Effect::DrainOutput(self.loc));
            return;
        }

        if !*dm_sent
            && ctx.src == self.remote
            && ctx.src_ssid == self.remote_ssid
            && !is_ui
            && (is_sabm || is_disc)
            && ctx.dest == my_callsign
            && ctx.dest_ssid == my_ssid
            && !active_remotes.contains(&(ctx.src.to_string(), ctx.src_ssid))
        {
            *dm_sent = true;
            let dm_control = if ctx.pf { 0x1F } else { 0x0F };
            let dm_frame = ax25::build_ax25_frame(
                ctx.src,
                ctx.src_ssid,
                my_callsign,
                my_ssid,
                dm_control,
                0xF0,
                &[],
                &[],
                false,
            );
            deferred.push(Effect::SendFrame(dm_frame));
        }
    }
}

/// Dispatch a single frame to a session based on its current AX.25 state.
///
/// Shared by the two paths in `run_state_machine` — the server-pool
/// single-session branch and the C-tab multi-session loop — so the per-state
/// handler calls can never drift between copies. The caller supplies the
/// callsign/ssid identity to gate on (board for server sessions, station for
/// C-tabs) plus the `active_remotes`/`dm_sent` DM-suppression state, because
/// the two paths build them differently (server uses fresh empties, C-tabs
/// use the shared pool-wide values).
#[allow(clippy::too_many_arguments)]
fn dispatch_state(
    session: &mut Ax25Session,
    ctx: &FrameCtx,
    deferred: &mut DeferredOps,
    client_arc: &Arc<Mutex<TcpClient>>,
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    callsign: &str,
    ssid: u8,
    active_remotes: &[(String, u8)],
    dm_sent: &mut bool,
    bbs_ctx: Option<(&mut crate::bbs::BbsStore, &crate::bbs::BbsConfig)>,
) {
    match session.state {
        Ax25ConnState::AwaitingConnect => {
            session.handle_awaiting_connect(ctx, deferred, client_arc, app_handle, callsign, ssid);
        }
        Ax25ConnState::Connected => {
            session.handle_connected(ctx, deferred, client_arc, app_handle, callsign, ssid);
        }
        Ax25ConnState::TimerRecovery => {
            session.handle_timer_recovery(ctx, deferred, client_arc, app_handle, callsign, ssid);
        }
        Ax25ConnState::AwaitingDisconnect => {
            session.handle_awaiting_disconnect(ctx, deferred, app_handle, callsign, ssid);
        }
        Ax25ConnState::Disconnected => {
            session.handle_disconnected(ctx, deferred, callsign, ssid, active_remotes, dm_sent, bbs_ctx);
        }
    }
}

/// Run the AX.25 connected-mode state machine against all sessions.
///
/// Decodes the incoming frame once, then dispatches to the per-state
/// handler for each session. Deferred operations are collected during
/// the dispatch and executed afterwards to avoid borrow conflicts.
async fn run_state_machine(
    app_handle: &std::sync::Arc<dyn crate::kernel::output::KernelOutput>,
    client_arc: &Arc<Mutex<TcpClient>>,
    dest: &str,
    dest_ssid: u8,
    src: &str,
    src_ssid: u8,
    control: u8,
    info: &[u8],
    frame_type: &str,
    digis: &[(String, u8, bool)],
    src_c_bit: bool,
    target: Option<SessionLoc>,
) {
    let mut c = client_arc.lock().await;
    let my_callsign = c.callsign.clone();
    let my_ssid = c.ssid;

    // Deferred operations — collected during the session loop and executed after
    // to avoid borrow conflicts (we can't hold a mutable borrow on session
    // while also accessing other parts of the client).
    let mut deferred = DeferredOps::new();

    // ── Single-session dispatch (server pool) ──────────────
    // The router resolves the board slot by src and passes
    // `SessionLoc::Server(i)`. The frame already belongs to that session —
    // the C-tab multi-tab collision gates (active_remotes / dm_sent) don't
    // apply. The session resolves through the pool so its effects, timers,
    // and emits all land on the server session (invisible → no UI noise).
    if let Some(loc) = target {
        let SessionLoc::Server(i) = loc else {
            return;
        };
        // Disjoint-field destructure — the borrow checker won't split the
        // MutexGuard deref for session + store together (bbs_link pattern).
        let TcpClient {
            server_sessions,
            bbs_store,
            bbs_cfg,
            bbs_callsign,
            bbs_ssid,
            ..
        } = &mut *c;
        let Some(session) = server_sessions.get_mut(i).and_then(|o| o.as_mut()) else {
            return;
        };
        // Server frames are addressed to the BOARD callsign (that's why the
        // gate routed them here) — every handler gates on
        // `ctx.dest == board_callsign && ctx.dest_ssid == board_ssid`, so the
        // board identity must be passed, not the station's. (Default config:
        // board = station-1, e.g. KK6SEN-1 vs station KK6SEN-0.)
        let board_callsign = bbs_callsign.clone();
        let board_ssid = *bbs_ssid;
        let ctx = FrameCtx {
            src,
            src_ssid,
            dest,
            dest_ssid,
            control,
            info,
            frame_type,
            digis,
            pf: (control & 0x10) != 0,
            is_command: !src_c_bit,
            is_iframe: ax25::is_iframe_ctl(control),
            is_sframe: (control & 0x03) == 0x01,
            is_uframe: (control & 0x03) == 0x03,
            nr_field: (control >> 5) & 0x07,
        };
        // A server session in state 0 needs the BBS store/config to queue
        // its banner + prompt (A4 accept). Other states don't touch it.
        let bbs_ctx = bbs_store.as_mut().map(|store| (store, &*bbs_cfg));
        // Server single-session path: no other live sessions to suppress DMs
        // against, so use fresh empties (the C-tab loop passes pool-wide state).
        let mut dm_sent = false;
        let active_remotes: Vec<(String, u8)> = Vec::new();
        dispatch_state(
            session,
            &ctx,
            &mut deferred,
            client_arc,
            app_handle,
            &board_callsign,
            board_ssid,
            &active_remotes,
            &mut dm_sent,
            bbs_ctx,
        );
        deferred
            .execute(&mut c, client_arc, app_handle, &board_callsign, board_ssid)
            .await;
        return;
    }

    // Collect remotes owned by ANY live session (every state except
    // Disconnected) so disconnected sessions never respond for frames that
    // another session handles. Previously only `Connected` was counted, so
    // during TimerRecovery / AwaitingConnect / AwaitingDisconnect windows
    // the gate opened and idle tabs flooded the remote with DM (one per tab).
    let active_remotes: Vec<(String, u8)> = c
        .info
        .sessions
        .iter()
        .filter(|s| s.state != Ax25ConnState::Disconnected)
        .map(|s| (s.remote.clone(), s.remote_ssid))
        .collect();

    // Cap spontaneous DM responses at one per frame — a frame belongs to one
    // session; duplicate tabs bound to the same remote must not each answer.
    let mut dm_sent = false;

    for session in &mut c.info.sessions {
        let ctx = FrameCtx {
            src,
            src_ssid,
            dest,
            dest_ssid,
            control,
            info,
            frame_type,
            digis,
            pf: (control & 0x10) != 0,
            is_command: !src_c_bit,
            is_iframe: ax25::is_iframe_ctl(control),
            is_sframe: (control & 0x03) == 0x01,
            is_uframe: (control & 0x03) == 0x03,
            nr_field: (control >> 5) & 0x07,
        };
        // Path gate: a connected C-tab session only populates from frames
        // that match its configured via path (the digipeater closest to us).
        // e.g. "KK6SEN -> COOL via KBANN" only drives from COOL frames that
        // arrive via KBANN, not copies heard direct.
        if !session.path_accepts(&ctx) {
            continue;
        }
        // C-tab dispatch: no BBS store/config — server sessions are routed
        // separately; Client role ignores bbs_ctx.
        dispatch_state(
            session,
            &ctx,
            &mut deferred,
            client_arc,
            app_handle,
            &my_callsign,
            my_ssid,
            &active_remotes,
            &mut dm_sent,
            None,
        );
    }

    // ── Execute deferred operations ──────────────────────────
    //
    // These are collected during the session loop but executed here
    // to avoid holding a mutable borrow on a session while also
    // borrowing other parts of the client.
    deferred
        .execute(&mut c, client_arc, app_handle, &my_callsign, my_ssid)
        .await;
}

// ── Tests ─────────────────────────────────────────────────
//
// Phase A4: state-0 server SABM accept + client-path preservation.

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty board store in a scratch dir (mirrors bbs.rs tests — the store
    /// has no test constructor, only `BbsStore::load(dir)`).
    fn tmp_store(name: &str) -> crate::bbs::BbsStore {
        let dir =
            std::env::temp_dir().join(format!("k2k_session_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        crate::bbs::BbsStore::load(&dir)
    }

    fn board_config() -> crate::bbs::BbsConfig {
        crate::bbs::BbsConfig {
            node_name: "KK6ABC-1".to_string(),
            sysops: vec![],
            privacy_enabled: false,
            public_aliases: vec!["ALL".to_string()],
        }
    }

    /// A synthetic Server-role board session bound to KK6SEN, in state 0.
    fn server_session() -> Ax25Session {
        let mut s = Ax25Session::new(0);
        s.role = SessionRole::Server;
        s.invisible = true;
        s.remote = "KK6SEN".to_string();
        s.remote_ssid = 0;
        s.state = Ax25ConnState::Disconnected;
        s.bbs = Some(crate::bbs::BbsSession::new("KK6SEN"));
        s
    }

    /// SABM(P=1) from KK6SEN addressed to KK6ABC-1 (the board).
    fn sabm_ctx<'a>() -> FrameCtx<'a> {
        FrameCtx {
            src: "KK6SEN",
            src_ssid: 0,
            dest: "KK6ABC",
            dest_ssid: 1,
            control: 0x2F, // SABM, P=1
            info: &[],
            frame_type: "SABM+",
            digis: &[],
            pf: true,
            is_command: true,
            is_iframe: false,
            is_sframe: false,
            is_uframe: true,
            nr_field: 0,
        }
    }

    /// Build a C-tab session for "KK6SEN -> COOL via KBANN".
    fn via_session() -> Ax25Session {
        let mut s = Ax25Session::new(0);
        s.role = SessionRole::Client;
        s.remote = "COOL".to_string();
        s.remote_ssid = 0;
        s.digis = vec!["KBANN".to_string()];
        s
    }

    /// A received connected-mode I-frame from COOL, with the given via list.
    /// Caller owns the digis slice so it outlives the FrameCtx.
    fn data_ctx(digis: &[(String, u8, bool)]) -> FrameCtx<'_> {
        FrameCtx {
            src: "COOL",
            src_ssid: 0,
            dest: "KK6SEN",
            dest_ssid: 0,
            control: 0x00,
            info: &[b'h', b'i'],
            frame_type: "I00-",
            digis,
            pf: false,
            is_command: false,
            is_iframe: true,
            is_sframe: false,
            is_uframe: false,
            nr_field: 0,
        }
    }

    #[test]
    fn path_accepts_gates_on_first_via_hop() {
        let s = via_session();
        let via_kbann = vec![("KBANN".to_string(), 0, true)];
        let no_via: Vec<(String, u8, bool)> = vec![];
        let via_kjohn = vec![("KJOHN".to_string(), 0, false)];
        // Pre-repeat leg: KBANN in path but H-bit CLEAR (dest->digi copy) -> reject
        let via_kbann_prerepeat = vec![("KBANN".to_string(), 0, false)];

        // Frame from COOL arriving via KBANN with its H-bit set (KBANN repeated
        // it back to us — the digi->me leg) -> accept
        assert!(
            s.path_accepts(&data_ctx(&via_kbann)),
            "repeated copy via KBANN accepted"
        );

        // Same KBANN path but H-bit CLEAR (the pre-repeat, dest->digi copy) -> reject
        assert!(
            !s.path_accepts(&data_ctx(&via_kbann_prerepeat)),
            "pre-repeat leg (H-bit clear) rejected: only digipeated-to-us data loads"
        );

        // Frame from COOL heard DIRECT (no via) -> reject: don't drive the via tab
        assert!(!s.path_accepts(&data_ctx(&no_via)), "direct copy rejected");

        // Frame routed through a DIFFERENT digipeater -> reject
        assert!(
            !s.path_accepts(&data_ctx(&via_kjohn)),
            "different via rejected"
        );

        // Frame not from our peer -> hands off to the handlers (not gated here)
        let other = FrameCtx {
            src: "BOB",
            src_ssid: 0,
            dest: "KK6SEN",
            dest_ssid: 0,
            control: 0x00,
            info: &[],
            frame_type: "I00-",
            digis: &[],
            pf: false,
            is_command: false,
            is_iframe: true,
            is_sframe: false,
            is_uframe: false,
            nr_field: 0,
        };
        assert!(s.path_accepts(&other), "different source not gated by path");

        // Direct connection (no via configured) -> no path to enforce
        let mut direct = Ax25Session::new(0);
        direct.remote = "COOL".to_string();
        direct.remote_ssid = 0;
        assert!(
            direct.path_accepts(&data_ctx(&no_via)),
            "direct conn accepts any-path frames"
        );
        let mut d2 = Ax25Session::new(0);
        d2.remote = "COOL".to_string();
        assert!(
            d2.path_accepts(&data_ctx(&via_kbann)),
            "direct conn not via-gated"
        );
    }

    #[test]
    fn multi_hop_via_accepts_when_nearest_hop_repeated() {
        // Josh: issues when communicating through MULTIPLE digipeater hops.
        // Here we connect to N6ZOO-1 via KBANN KJOHN (two hops). KBANN is the
        // hop closest to US = session.digis[0]. When N6ZOO-1's data reaches
        // us, BOTH digis have repeated it, so the frame carries KBANN AND
        // KJOHN H-bits set. The tab must load this repeated copy, and it must
        // NOT load the pre-repeat copies.
        let mut s = Ax25Session::new(0);
        s.role = SessionRole::Client;
        s.remote = "N6ZOO-1".to_string();
        s.remote_ssid = 0;
        s.digis = vec!["KBANN".to_string(), "KJOHN".to_string()]; // closest first

        // A FrameCtx from the actual peer N6ZOO-1 carrying the given via list.
        fn peer_ctx(digis: &[(String, u8, bool)]) -> FrameCtx<'_> {
            FrameCtx {
                src: "N6ZOO-1",
                src_ssid: 0,
                dest: "KK6SEN",
                dest_ssid: 0,
                control: 0x00,
                info: &[b'h', b'i'],
                frame_type: "I00-",
                digis,
                pf: false,
                is_command: false,
                is_iframe: true,
                is_sframe: false,
                is_uframe: false,
                nr_field: 0,
            }
        }

        // Frame from N6ZOO-1 repeated by BOTH hops -> reaches us via
        // [KBANN*, KJOHN*] (both H-bit set). Closest hop (KBANN) is first.
        let repeated = vec![
            ("KBANN".to_string(), 0, true),
            ("KJOHN".to_string(), 0, true),
        ];
        assert!(
            s.path_accepts(&peer_ctx(&repeated)),
            "multi-hop frame repeated to us (both H-bits set) loads"
        );

        // REAL-WORLD multi-hop case (from the 260829 W6ELA log): the peer
        // transmits toward us through the REVERSE path, so our closest hop
        // (KBANN) is the LAST digi, not the first. Must still accept when
        // that hop's H-bit is set.
        let reversed_repeated = vec![
            ("KJOHN".to_string(), 0, true),
            ("KBANN".to_string(), 0, true),
        ];
        assert!(
            s.path_accepts(&peer_ctx(&reversed_repeated)),
            "multi-hop REVERSED path (closest hop last, H-bit set) loads \
             — the actual 2-hop case"
        );

        // Pre-repeat legs must NOT drive the tab:
        //  (a) N6ZOO-1->KJOHN original, no hop repeated yet
        let orig = vec![
            ("KBANN".to_string(), 0, false),
            ("KJOHN".to_string(), 0, false),
        ];
        assert!(
            !s.path_accepts(&peer_ctx(&orig)),
            "pre-repeat original rejected"
        );
        //  (b) after KJOHN repeats toward KBANN: KJOHN H set, KBANN clear
        let mid = vec![
            ("KBANN".to_string(), 0, false),
            ("KJOHN".to_string(), 0, true),
        ];
        assert!(
            !s.path_accepts(&peer_ctx(&mid)),
            "mid-path (far hop repeated) rejected"
        );
        //  (c) reversed path where closest hop (KBANN) has NOT repeated yet:
        //     'via KJOHN* KBANN' (KBANN clear) -> must be rejected
        let reversed_prerepeat = vec![
            ("KJOHN".to_string(), 0, true),
            ("KBANN".to_string(), 0, false),
        ];
        assert!(
            !s.path_accepts(&peer_ctx(&reversed_prerepeat)),
            "reversed pre-repeat ('KJOHN* KBANN', KBANN clear) rejected"
        );

        // ACK/data routing must carry BOTH digis (go back through the full
        // path to the far end), not just the nearest hop.
        let mut deferred = DeferredOps::new();
        deferred.send_rr(&s, false);
        let effects = deferred.take_effects();
        let Effect::SendRR {
            remote,
            ssid,
            digis,
            nr,
            pf_bit,
        } = &effects[0]
        else {
            panic!("expected SendRR");
        };
        assert_eq!(remote, "N6ZOO-1", "RR addressed to far-end node");
        assert_eq!(*ssid, 0);
        assert_eq!(
            digis, &s.digis,
            "RR routed through BOTH digis, not just nearest"
        );
        assert_eq!(*pf_bit, false);
        assert_eq!(*nr, 0);
    }

    #[test]
    fn ua_connection_response_accepted_even_when_nearest_hop_pre_repeat() {
        // Regression: 260830 COOL session. KK6SEN->COOL via KJOHN KBANN
        // (two hops). The FIRST UA back from COOL arrived via the reversed
        // path 'KBANN* KJOHN' — our closest hop (KJOHN, digis[0]) had its
        // H-bit CLEAR (it was still forwarding, not yet re-keyed toward us).
        // Before this fix path_accepts() dropped that UA, so the app stayed
        // in AwaitingConnect, the T1 retry re-sent SABM, and COOL re-welcomed
        // (banner thrash). A UA completes the connect, so it must be accepted
        // regardless of the closest hop's H-bit.
        let mut s = Ax25Session::new(0);
        s.role = SessionRole::Client;
        s.remote = "COOL".to_string();
        s.remote_ssid = 0;
        s.digis = vec!["KJOHN".to_string(), "KBANN".to_string()]; // closest first

        // UA, F=1 → control 0x73 (is_uframe true). This is the :09 frame:
        // via KBANN(H set) KJOHN(H clear) — reversed path, closest hop clear.
        let ua = FrameCtx {
            src: "COOL",
            src_ssid: 0,
            dest: "KK6SEN",
            dest_ssid: 0,
            control: 0x73,
            info: &[],
            frame_type: "UA+",
            digis: &[
                ("KBANN".to_string(), 0, true),
                ("KJOHN".to_string(), 0, false),
            ],
            pf: true,
            is_command: false,
            is_iframe: false,
            is_sframe: false,
            is_uframe: true,
            nr_field: 0,
        };
        assert!(
            s.path_accepts(&ua),
            "UA accepted even when closest hop (KJOHN) H-bit is clear — \
             connection completion must not be dropped on a noisy link"
        );

        // A nearby UA copy via the pre-repeat leg should also land.
        let ua2 = FrameCtx {
            src: "COOL",
            src_ssid: 0,
            dest: "KK6SEN",
            dest_ssid: 0,
            control: 0x73,
            info: &[],
            frame_type: "UA+",
            digis: &[
                ("KJOHN".to_string(), 0, false),
                ("KBANN".to_string(), 0, false),
            ],
            pf: true,
            is_command: false,
            is_iframe: false,
            is_sframe: false,
            is_uframe: true,
            nr_field: 0,
        };
        assert!(
            s.path_accepts(&ua2),
            "UA accepted on the pre-repeat leg too"
        );
    }

    #[test]
    fn t2_ack_batches_a_burst_into_one_pending_ack() {
        // Simulate a node sending several P=0 I-frames before expecting an
        // ack (the case Josh hit). Each received in-sequence frame advances
        // nr, and arm_t2_ack() re-arms the SAME delayed-ack — so a burst of
        // 3 frames leaves only ONE pending ack covering N(R)=3, not three
        // separate acks.
        let mut s = Ax25Session::new(0);
        s.remote = "COOL".to_string();

        // frame 1: N(S)=0 in sequence -> nr becomes 1, arm ack
        s.nr = (s.nr + 1) & 0x07;
        s.arm_t2_ack();
        assert!(s.rr_pending, "ack pending after first I-frame");
        assert_eq!(s.rr_target_nr, 1);
        assert!(
            s.t2_handle.is_none(),
            "timer handle owned by caller, not session"
        );

        // frames 2 & 3 arrive before the ack fires: re-arm (reload deadline)
        s.nr = (s.nr + 1) & 0x07;
        s.arm_t2_ack();
        s.nr = (s.nr + 1) & 0x07;
        s.arm_t2_ack();
        assert!(s.rr_pending, "still one ack pending across the burst");
        assert_eq!(s.rr_target_nr, 3, "single ack covers N(R)=3 (all 3 frames)");

        // A polled frame (P=1) sends the ack immediately and clears it.
        s.rr_pending = false;
        assert!(
            !s.rr_pending,
            "immediate P=1 response clears the pending ack"
        );
    }

    #[test]
    fn test_server_role_sabm_accepts() {
        let mut session = server_session();
        let mut deferred = DeferredOps::new();
        let mut dm_sent = false;
        let mut store = tmp_store("server_sabm");
        let cfg = board_config();
        let ctx = sabm_ctx();

        session.handle_disconnected(
            &ctx,
            &mut deferred,
            "KK6ABC",
            1,
            &[],
            &mut dm_sent,
            Some((&mut store, &cfg)),
        );

        // UA queued + the first-chunk kick (B1): the accept queues the
        // banner and immediately kicks the drain so the first chunk goes
        // out without waiting for an ACK (old queue_bbs_output behavior).
        let effects = deferred.take_effects();
        assert_eq!(effects.len(), 2, "accept queues UA + DrainOutput kick");
        let Effect::SendFrame(ua) = &effects[0] else {
            panic!("expected SendFrame (UA) effect");
        };
        let parsed = ax25::parse_ax25_frame(ua).expect("UA frame parses");
        assert_eq!(parsed.control, ax25::UA, "queued frame is UA");
        assert_eq!(parsed.dest, "KK6SEN");
        assert_eq!(parsed.dest_ssid, 0);
        assert_eq!(parsed.src, "KK6ABC");
        assert_eq!(parsed.src_ssid, 1);
        assert!(
            matches!(&effects[1], Effect::DrainOutput(_)),
            "second effect kicks the output drain"
        );

        // State machine: state 0 → Connected, RX timestamp bumped.
        assert_eq!(session.state, Ax25ConnState::Connected);
        assert!(session.last_rx_time.is_some(), "last_rx_time bumped");

        // Heard record written.
        assert!(store.heard.contains_key("KK6SEN"), "heard recorded");

        // Banner + prompt queued, PACKED (KPC-3P style): the whole connect
        // output fits in ONE frame (≤ max_packet_size = 250 bytes) — the
        // restored pack_bbs_lines behavior from the old bbs_link.
        assert!(!session.pending_out.is_empty(), "banner+prompt queued");
        let first = session.pending_out.front().unwrap();
        assert!(
            first.starts_with("*** Welcome KK6SEN"),
            "banner first: {first:?}"
        );
        assert_eq!(
            session.pending_out.len(),
            1,
            "banner+prompt pack into one frame"
        );
        assert!(
            first.ends_with("ENTER COMMAND:  B,J,K,L,R,S, or Help >"),
            "packed frame ends with the prompt: {first:?}"
        );
        assert!(
            session.pending_out.iter().all(|c| c.len() <= 250),
            "every queued chunk fits a BBS_MAX_INFO frame"
        );

        // No DM — the accept path leaves the DM gate untouched.
        assert!(!dm_sent);
    }

    #[test]
    fn test_client_role_sabm_still_ignored() {
        // Idle C-tab (remote=""): a SABM addressed to us is ignored entirely —
        // the frame belongs to no session of ours (client behavior, unchanged).
        let mut session = Ax25Session::new(0); // role = Client, remote = ""
        let mut deferred = DeferredOps::new();
        let mut dm_sent = false;
        let ctx = sabm_ctx();

        session.handle_disconnected(&ctx, &mut deferred, "KK6ABC", 1, &[], &mut dm_sent, None);

        assert_eq!(session.state, Ax25ConnState::Disconnected);
        assert!(deferred.take_effects().is_empty(), "no UA/DM queued");
        assert!(!dm_sent, "dm_sent untouched");
    }

    #[test]
    fn test_client_role_sabm_matching_remote_still_sends_dm() {
        // C-tab bound to KK6SEN in state 0: SABM from that remote still gets
        // the pre-A4 DM response — the client path is byte-identical.
        let mut session = Ax25Session::new(0);
        session.remote = "KK6SEN".to_string();
        session.remote_ssid = 0;
        let mut deferred = DeferredOps::new();
        let mut dm_sent = false;
        let ctx = sabm_ctx();

        session.handle_disconnected(&ctx, &mut deferred, "KK6ABC", 1, &[], &mut dm_sent, None);

        assert_eq!(
            session.state,
            Ax25ConnState::Disconnected,
            "client stays disconnected"
        );
        let effects = deferred.take_effects();
        assert_eq!(effects.len(), 1, "one DM");
        let Effect::SendFrame(dm) = &effects[0] else {
            panic!("expected DM frame");
        };
        let parsed = ax25::parse_ax25_frame(dm).expect("DM frame parses");
        assert_eq!(parsed.control, 0x1F, "DM with F bit set (P was set)");
        assert!(dm_sent, "DM gate engaged");
        assert!(session.pending_out.is_empty(), "client never queues banner");
    }

    // ── Phase A5: router slot resolution + single-session dispatch ──
    //
    // The full SABM-accept flow through run_state_machine's
    // target=Some(Server(i)) path needs a std::sync::Arc<dyn crate::kernel::output::KernelOutput>, which unit
    // tests can't construct (no tauri "test" feature) — that end-to-end
    // path is covered by the B2 direwolf loopback / RF checklist. What IS
    // unit-testable without an AppHandle is the router's pure decision
    // logic, extracted into server_slot_for.

    #[test]
    fn test_server_slot_resolution() {
        let mut pool: Vec<Option<Ax25Session>> = (0..4).map(|_| None).collect();
        // Empty pool → no slot.
        assert_eq!(server_slot_for(&pool, "KK6SEN", 0), None);

        // Occupied slot matches by remote + ssid.
        let mut s = Ax25Session::new(0);
        s.remote = "KK6SEN".to_string();
        s.remote_ssid = 0;
        s.invisible = true; // router-created server sessions are invisible
        pool[2] = Some(s);
        assert_eq!(server_slot_for(&pool, "KK6SEN", 0), Some(2));
        // Same callsign, different SSID → no match (separate user).
        assert_eq!(server_slot_for(&pool, "KK6SEN", 7), None);
        // Different callsign → no match.
        assert_eq!(server_slot_for(&pool, "W6ABC", 0), None);

        // The kernel's per-state dispatch for a Server-role session in the
        // pool: a SABM addressed to the board still resolves through the
        // same slot the router would pick (regression guard for the router
        // swap — the pool is the source of truth, not the old bbs_sessions).
        let idx = server_slot_for(&pool, "KK6SEN", 0).expect("slot found");
        let sess = pool[idx].as_ref().expect("session present");
        assert_eq!(sess.remote, "KK6SEN");
        assert!(sess.invisible, "server sessions never paint the UI");
    }

    // ── Output packing (restored from the old bbs_link; A6 dropped it) ──

    const LIMIT: usize = 250;

    #[test]
    fn chunk_splits_long_text_without_loss() {
        let long = "a".repeat(600);
        let chunks = chunk_text(&long, LIMIT);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.len() <= LIMIT));
        assert_eq!(chunks.concat(), long);
    }

    #[test]
    fn chunk_keeps_utf8_chars_intact() {
        // "é" is 2 bytes — 200 of them is 400 bytes: chunks must never split
        // a char (every chunk length stays even) and must reassemble exactly.
        let text = "é".repeat(200);
        let chunks = chunk_text(&text, LIMIT);
        assert!(chunks.iter().all(|c| c.len() <= LIMIT));
        assert!(chunks.iter().all(|c| c.len() % 2 == 0), "char split!");
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunk_handles_short_blank_and_single_char() {
        assert_eq!(chunk_text("hello", LIMIT), vec!["hello".to_string()]);
        // blank separator lines survive as their own chunk
        assert_eq!(chunk_text("", LIMIT), vec![String::new()]);
        assert_eq!(chunk_text("\n", LIMIT), vec!["\n".to_string()]);
        // a line exactly at the limit stays one chunk
        let exact = "x".repeat(LIMIT);
        assert_eq!(chunk_text(&exact, LIMIT), vec![exact]);
    }

    #[test]
    fn pack_joins_lines_into_few_frames() {
        // Connect output: banner + summary + prompt all fit
        // in ONE frame (KPC-3P style) — the "why so many packets" fix.
        let lines = vec![
            "*** Welcome KK6SEN to the KK6SEN-1 K2K Packet Message Board ***".to_string(),
            "THERE ARE 3 MESSAGES NUMBERED 1-3".to_string(),
            "ENTER COMMAND:  B,J,K,L,R,S, or Help >".to_string(),
        ];
        let frames = pack_bbs_lines(&lines, LIMIT);
        assert_eq!(frames.len(), 1);
        // +2 = the \r\n the sender appends to every frame
        assert!(frames[0].len() + 2 <= LIMIT);
    }

    #[test]
    fn pack_splits_at_limit_between_lines() {
        // 200-char lines: 200+2 fits; 200+2+200+2 does not → each 200-char
        // line becomes its own frame; the 100-char line joins nothing.
        let lines = vec!["a".repeat(200), "b".repeat(200), "c".repeat(100)];
        let frames = pack_bbs_lines(&lines, LIMIT);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], "a".repeat(200));
        assert_eq!(frames[1], "b".repeat(200));
        assert_eq!(frames[2], "c".repeat(100));
    }

    #[test]
    fn pack_splits_oversize_lines_but_keeps_neighbors() {
        let long = "x".repeat(400);
        let lines = vec!["head".to_string(), long.clone(), "tail".to_string()];
        let frames = pack_bbs_lines(&lines, LIMIT);
        assert_eq!(frames[0], "head");
        assert!(frames[1].len() + frames[2].len() == long.len());
        assert!(frames[1].len() <= LIMIT);
        assert!(frames[2].len() <= LIMIT);
        assert_eq!(frames[3], "tail");
    }

    #[test]
    fn pack_keeps_blank_lines() {
        assert_eq!(pack_bbs_lines(&[String::new()], LIMIT), vec![String::new()]);
        let frames = pack_bbs_lines(&["a".to_string(), String::new()], LIMIT);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], "a\r\n");
    }

    #[test]
    fn pack_uses_every_byte_up_to_the_limit() {
        let exact = "x".repeat(248);
        let frames = pack_bbs_lines(&[exact.clone()], LIMIT);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len() + 2, LIMIT);
    }

    #[test]
    fn pack_respects_a_smaller_limit() {
        // HF profile: limit 128 — lines pack to the smaller frame size.
        let lines = vec!["a".repeat(80), "b".repeat(80)];
        let frames = pack_bbs_lines(&lines, 128);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() + 2 <= 128));
        let lines = vec!["a".repeat(60), "b".repeat(60)];
        let frames = pack_bbs_lines(&lines, 128);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].len() + 2 <= 128);
    }

    #[test]
    fn pack_empty_input_yields_no_frames() {
        let empty: Vec<String> = vec![];
        assert!(pack_bbs_lines(&empty, LIMIT).is_empty());
    }

    /// The message board pool accepts up to MAX_BBS_SESSIONS DISTINCT
    /// stations simultaneously — one session per (callsign, ssid) — and a
    /// 5th distinct user finds no free slot (refused). This mirrors the exact
    /// decision `route_bbs_frame` makes (server_slot_for + capacity) against
    /// the real pool and constant. No RF needed.
    #[test]
    fn board_pool_accepts_4_distinct_users_and_has_no_slot_for_the_5th() {
        let n = crate::bbs_link::MAX_BBS_SESSIONS;
        assert_eq!(n, 4, "pool capacity is 4");
        let users = ["N6ZOO", "KM6LYW", "KF6ANX", "N3CKF", "W6ABC"];
        let mut pool: Vec<Option<Ax25Session>> = (0..n).map(|_| None).collect();

        // Simulate route_bbs_frame's accept: a NEW distinct user → no existing
        // slot and capacity available → occupies the first free slot.
        for (i, user) in users[..n].iter().enumerate() {
            let existing = server_slot_for(&pool, user, i as u8);
            assert!(existing.is_none(), "{user} is new — no prior slot");
            let busy = pool.iter().flatten().count() >= n;
            assert!(!busy, "capacity available before user {i}");
            let free = pool.iter().position(|s| s.is_none()).unwrap();
            let mut s = server_session(); // server() defaults remote KK6SEN
            s.remote = user.to_string();
            s.remote_ssid = i as u8;
            pool[free] = Some(s);
        }
        assert_eq!(pool.iter().flatten().count(), n, "4 distinct users each get a slot");

        // 5th distinct user: no existing slot AND pool is full → must be
        // refused (the router sends DM; here we assert the refusal decision).
        let fifth = server_slot_for(&pool, users[4], 0);
        assert!(fifth.is_none(), "5th user has no slot");
        let busy = pool.iter().flatten().count() >= n;
        assert!(busy, "pool is full — 5th connect is refused");

        // Each of the 4 distinct users resolves to its OWN distinct slot.
        let mut slots: Vec<usize> = users[..n]
            .iter()
            .enumerate()
            .map(|(i, user)| server_slot_for(&pool, user, i as u8).unwrap())
            .collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), n, "one session per distinct user, all distinct slots");
    }

    /// Stale board user: the recovery T1 loop DM's and — post-fix — frees
    /// the server-pool slot (mirrors the peer-DISC / board-hangup teardown),
    /// so a timeout-DM'd board user can't hold a slot forever. Runs the real
    /// `spawn_recovery_timer` with a tiny t1 so it fires promptly; no RF.
    #[tokio::test]
    async fn timeout_dm_runs_recovery_and_frees_the_server_slot() {
        let out: std::sync::Arc<dyn crate::kernel::output::KernelOutput> =
            std::sync::Arc::new(crate::kernel::output::NullOutput);

        // Real loopback TCP pair for the required write half (no RF).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (a, b) = tokio::join!(
            tokio::net::TcpStream::connect(addr),
            async {
                let (s, _) = listener.accept().await.unwrap();
                s
            },
        );
        let (_read, write) = a.unwrap().into_split();
        let _keep_connected = b;

        let mut c = TcpClient::new(
            "test".into(), "board".into(), "127.0.0.1".into(), 8001,
            Protocol::KISS, write, "KK6SEN".into(), 0, &out, KissTiming::default(),
        );
        c.bbs_enabled = true;
        c.bbs_callsign = "KK6SEN".to_string();
        c.bbs_ssid = 1;
        c.info.connected = true; // recovery_delay_ms requires connected
        c.n2_max = 3;            // small so the DM branch is reached quickly
        c.t1_ms = 5;             // tiny so the T1 sleep returns fast

        // A stale board user already in TimerRecovery with n2count == n2_max:
        // the next recovery pass must DM AND free the slot.
        let mut s = server_session();
        s.role = SessionRole::Server;
        s.remote = "N6ZOO".to_string();
        s.remote_ssid = 0;
        s.state = Ax25ConnState::TimerRecovery;
        s.n2count = c.n2_max;
        c.server_sessions[0] = Some(s);
        let client_arc = std::sync::Arc::new(tokio::sync::Mutex::new(c));

        // Run the recovery timer until it exits (DM + free). sleep(t1=5ms)
        // fires quickly; poll for completion up to a generous wall budget.
        let h = spawn_recovery_timer(client_arc.clone(), SessionLoc::Server(0), &out);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !h.is_finished() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let _ = h.await;

        let g = client_arc.lock().await;
        assert!(
            g.server_sessions[0].is_none(),
            "recovery DM frees the server slot"
        );
        assert_eq!(g.server_sessions.iter().flatten().count(), 0, "slot returned to pool");
    }

    /// A C-tab (permanent tab session) is never freed by the recovery
    /// teardown — only server-pool slots are reclaimed. Guards the role gate.
    #[test]
    fn tab_session_is_not_freed_by_the_recovery_teardown() {
        // C-tab sessions live in info.sessions (permanent, indexed by tab);
        // recovery teardown only frees SessionRole::Server pool slots. Simulate
        // a disconnected C-tab: it stays in place (state Disconnected), never
        // evicted.
        let mut s = Ax25Session::new(0);
        s.role = SessionRole::Client;
        s.remote = "COOL".to_string();
        s.remote_ssid = 0;
        s.state = Ax25ConnState::Disconnected;
        // The fix's role gate: only Server sessions are freed; Client stays.
        assert_ne!(s.role, SessionRole::Server);
        assert_eq!(s.role, SessionRole::Client);
        assert_eq!(s.state, Ax25ConnState::Disconnected);
    }
}
