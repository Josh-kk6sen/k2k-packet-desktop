// ── Message Board (PBBS) Gate & Setup ────────────────────
//
// B1: the parallel AX.25 link layer (BbsLinkSession + handle_bbs_frame +
// all send_bbs_*/spawn_bbs_* machinery) has been DELETED — the message
// board now rides the kernel state machine in session.rs (Server-role
// sessions in the server_sessions pool). This file keeps only what the
// kernel needs:
//
//   - MAX_BBS_SESSIONS   (pool cap)
//   - BbsSetup           (settings snapshot: enabled/callsign/ssid/cfg/store)
//   - BbsGate            (pure gate decision from an AppSettings snapshot)
//
// PURE: no tauri. Settings are loaded through the `KernelOutput` seam, and
// the gate is computed purely from an `AppSettings`.

use crate::settings::AppSettings;

/// Max simultaneous message board users (Python original: one).
pub const MAX_BBS_SESSIONS: usize = 4;

/// BBS runtime snapshot built at connect time from settings (parity with
/// KissTiming — setting changes apply on reconnect).
pub struct BbsSetup {
    pub enabled: bool,
    pub callsign: String,
    pub ssid: u8,
    pub cfg: crate::bbs::BbsConfig,
    pub store: Option<crate::bbs::BbsStore>,
}

impl BbsSetup {
    /// Build a BBS runtime snapshot from an `AppSettings` snapshot.
    /// Pure (no tauri): the caller supplies the settings + data dir.
    pub fn from_settings(s: &AppSettings, data_dir: &std::path::Path) -> Self {
        let enabled = s.message_board_enabled && !s.message_board_callsign.trim().is_empty();
        let callsign = s.message_board_callsign.trim().to_uppercase();
        let cfg = crate::bbs::BbsConfig::from_app_settings(s);
        let store = if enabled {
            let dir = data_dir.to_path_buf();
            let _ = std::fs::create_dir_all(&dir);
            Some(crate::bbs::BbsStore::load(&dir))
        } else {
            None
        };
        BbsSetup {
            enabled,
            callsign,
            ssid: s.message_board_ssid.clamp(0, 15),
            cfg,
            store,
        }
    }
}

/// Gate decision for frames addressed to a board callsign.
#[derive(Debug, Clone, PartialEq)]
pub enum BbsGate {
    /// No board callsign configured in settings.
    NotConfigured,
    /// Callsign set but the board is disabled (Enable checkbox off).
    Disabled(String, u8),
    /// Board enabled — this is the live callsign to answer.
    Enabled(String, u8),
}

/// Pure gate decision from an `AppSettings` snapshot. Exposed so the
/// `KernelOutput`-driven `bbs_gate()` wrapper can compute it without tauri.
pub fn compute_bbs_gate(s: &AppSettings) -> BbsGate {
    if s.message_board_callsign.trim().is_empty() {
        return BbsGate::NotConfigured;
    }
    let call = s.message_board_callsign.trim().to_uppercase();
    let ssid = s.message_board_ssid.clamp(0, 15);
    if s.message_board_enabled {
        BbsGate::Enabled(call, ssid)
    } else {
        BbsGate::Disabled(call, ssid)
    }
}
