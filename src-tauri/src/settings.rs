use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

/// Connection protocol as stored in settings.json. The stored value is
/// the display label "AGWPE" (Direwolf), which differs from the
/// session::Protocol enum (serializes as "AGW").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProtocolPref {
    #[default]
    #[serde(rename = "AGWPE")]
    Agwpe,
    #[serde(rename = "KISS")]
    Kiss,
}

/// Per-theme unproto monitor font color overrides. A `None` field (or a
/// missing theme entry) means "use this theme's built-in color". Only colors
/// the user actually changed are stored; values are hex like "#33ff33".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UnprotoThemeColors {
    pub time: Option<String>,
    pub call: Option<String>,
    pub via: Option<String>,
    pub meta: Option<String>,
    pub payload: Option<String>,
    pub connrx: Option<String>,   // connected-mode received payload text (--msg-rx)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub protocol: ProtocolPref,
    pub callsign: String,
    pub host: String,
    pub port: u16,
    pub auto_connect: bool,
    pub beacon_enabled: bool,
    pub beacon: String,
    pub beacon_interval: u32,                // minutes, 0 = disabled
    pub beacon_dest: String,                 // destination callsign for beacon
    pub beacon_via: String,                  // digipeater path for beacon
    pub font_size: u32,                      // UI font size in px (legacy)
    pub mon_font_size: u32,                  // Monitor font size in px (legacy)
    pub app_font_size: u32,                  // App base font size in px (10-25)
    pub conn_font_size: i32,                 // Connected mode OFFSET (-5..+5) from app base
    pub unproto_font_size: i32,              // Unproto OFFSET (-5..+5) from app base
    pub raw_kiss_display: bool,
    pub debug_tx_enabled: bool,
    pub deduplicate_packets: bool,
    pub simple_unproto: bool,
    pub only_unproto: bool,
    pub hide_via: bool,
    pub unproto_ignored_destinations: String,
    pub unproto_to: String,                   // last unproto To: (persisted, not in settings UI)
    pub unproto_via: String,                  // last unproto Via: (persisted, not in settings UI)
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    pub window_maximized: Option<bool>,
    pub theme: String,                      // "dark", "light", "blue", "sepia", "synthwave", or "vintage"
    // ── Per-theme unproto font colors (None / absent = theme built-in) ──
    pub unproto_colors: Option<HashMap<String, UnprotoThemeColors>>,
    // ── KISS TNC Parameters (only used when protocol is Kiss) ──
    pub kiss_p: u32,                         // Persistence: 0–255 (63 = p≈0.25)
    pub kiss_slot_time: u32,                 // Slot interval in 10ms units (10 = 100ms)
    pub kiss_txdelay: u32,                   // TX keyup delay in 10ms units (30 = 300ms)
    // ── AX.25 Link Layer Timers ──
    pub kiss_t1_ms: u32,                     // Retransmission timeout in ms (default 10s)
    pub kiss_t2_ms: u32,                     // ACK batching delay in ms (default 1s)
    pub kiss_t3_ms: u32,                     // Inactivity keep-alive in ms (default 5min)
    pub kiss_ka_check_ms: u32,              // Keep-alive check interval in ms (default 1min)
    pub kiss_n2_max: u32,                    // Max retransmissions before disconnect (default 5)
    // ── Message Board (PBBS) ──
    pub message_board_enabled: bool,         // Listen for connects to the board callsign
    pub message_board_callsign: String,      // Callsign others connect to (defaults to the user's)
    pub message_board_ssid: u8,              // SSID of the board callsign (default 1 → KK6SEN-1)
    pub message_board_sysops: String,        // Comma-separated callsigns with Delete/Pin rights
    pub message_board_privacy: bool,         // Users see only their own + public mail
    pub message_board_public_aliases: String, // Comma-separated public groups (ALL by default)
    pub message_board_max_packet_size: u32,  // Max info bytes per board output frame (default 250; 32–256)
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            protocol: ProtocolPref::Agwpe,
            callsign: String::new(),
            host: String::new(),
            port: 8000,
            auto_connect: true,
            beacon_enabled: false,
            beacon: String::new(),
            beacon_interval: 240,
            beacon_dest: "BEACON".to_string(),
            beacon_via: String::new(),
            font_size: 13,
            mon_font_size: 11,
            app_font_size: 18,
            conn_font_size: -2,
            unproto_font_size: -2,
            raw_kiss_display: false,
            debug_tx_enabled: false,
            deduplicate_packets: false,
            simple_unproto: false,
            only_unproto: false,
            hide_via: false,
            unproto_ignored_destinations: "NODES BEACON ID".to_string(),
            unproto_to: String::new(),
            unproto_via: String::new(),
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            window_maximized: None,
            theme: "dark".to_string(),
            unproto_colors: None,
            kiss_p: 63,              // p≈0.25 — conservative for shared channels
            kiss_slot_time: 10,      // 100ms — responsive without being aggressive
            kiss_txdelay: 30,        // 300ms — safe for most TNCs
            kiss_t1_ms: 10_000,      // 10s — allows digipeater round-trip time
            kiss_t2_ms: 1_000,       // 1s — standard ACK batching delay
            kiss_t3_ms: 300_000,     // 5min — inactivity keep-alive timeout
            kiss_ka_check_ms: 60_000,// 1min — keep-alive check interval
            kiss_n2_max: 5,          // 5 retries before forced disconnect
            message_board_enabled: false,
            message_board_callsign: String::new(),
            message_board_ssid: 1,
            message_board_sysops: String::new(),
            message_board_privacy: false,
            message_board_public_aliases: "ALL".to_string(),
            message_board_max_packet_size: 250,
        }
    }
}

/// Clamp AX.25 link-layer timers to safe ranges. Guards against malformed
/// or hand-edited settings.json: a zero/absent timer value (e.g. a missing
/// key deserializing to 0, or an old build writing 0) must never produce an
/// instant-fire retransmission storm — a T1 of 0 makes the recovery timer
/// fire RR enquiries back-to-back. The UI already bounds its inputs, so this
/// is the load-time safety net, same role as the font clamps.
fn clamp_ax25_timers(s: &mut AppSettings) {
    s.kiss_t1_ms = s.kiss_t1_ms.clamp(100, 120_000);              // 100ms..2min
    s.kiss_t2_ms = s.kiss_t2_ms.clamp(100, 10_000);               // 100ms..10s
    s.kiss_t3_ms = s.kiss_t3_ms.clamp(5_000, 3_600_000);          // 5s..1h
    s.kiss_ka_check_ms = s.kiss_ka_check_ms.clamp(1_000, 600_000); // 1s..10min
    s.kiss_n2_max = s.kiss_n2_max.clamp(1, 50);                   // 1..50 retries
}

/// A beacon interval of 0 (empty field in the UI, or a hand-edited file)
/// would silently mean "every minute" via the runtime max(1) — normalize the
/// stored value to 1 minute instead, so settings.json never holds a 0.
/// Disabling the beacon is the Enable checkbox, not the interval.
pub fn clamp_beacon_interval(s: &mut AppSettings) {
    s.beacon_interval = s.beacon_interval.max(1);
}

/// Load settings from disk (returns None if no usable file exists).
/// Self-healing: a missing or corrupt settings.json falls back to the
/// atomic-writer's orphaned `settings.json.tmp`, then to `settings.json.bak`
/// (written on every save), and a recovered copy is promoted back to the
/// main file. Clamps font sizes to safe ranges (8-25) and AX.25 timers to
/// safe minimums on load to guard against malformed settings.json or manual
/// editing mistakes.
pub fn load_settings_from_disk(app: &AppHandle) -> Result<Option<AppSettings>, String> {
    let path = crate::storage::app_data_path(app, "settings.json");
    load_settings_from_path(&path)
}

/// Read + sanitize ONE settings file. `Ok(None)` = file absent;
/// `Err` = present but unreadable/unparseable. No fallbacks, no side effects.
fn parse_settings_file(path: &std::path::Path) -> Result<Option<AppSettings>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(path).map_err(|e| format!("Failed to read settings: {}", e))?;
    let mut json: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse settings: {e}"))?;
    default_missing_unproto_destinations(&mut json);
    let mut settings: AppSettings =
        serde_json::from_value(json).map_err(|e| format!("Failed to parse settings: {e}"))?;
    // Clamp font sizes to safe range — guards against malformed settings.json.
    // app_font_size is an absolute px (10-25). conn/unproto are OFFSETS
    // (-5..+5) from the app base. Legacy settings.json files (≤0.11.17)
    // stored absolute px for all three (defaults 15/14/14) — convert those
    // to offsets (px 14 → offset −2) before clamping.
    let a = settings.app_font_size;
    settings.app_font_size = if a <= 5 { 18 } else { a.clamp(10, 25) };
    let base = (settings.app_font_size as i32); // anchor legacy px → offset
    settings.conn_font_size = if settings.conn_font_size > 5 {
        settings.conn_font_size - base
    } else {
        settings.conn_font_size
    }
    .clamp(-5, 5);
    settings.unproto_font_size = if settings.unproto_font_size > 5 {
        settings.unproto_font_size - base
    } else {
        settings.unproto_font_size
    }
    .clamp(-5, 5);
    clamp_ax25_timers(&mut settings);
    clamp_beacon_interval(&mut settings);
    // New-field upgrade guard: a 0.9.38 settings.json lacks the key, which
    // deserializes to 0 — map that to the default 250 rather than the 32
    // floor. Hand-edited garbage still gets clamped to 32..=256.
    let mp = settings.message_board_max_packet_size;
    settings.message_board_max_packet_size = if mp == 0 { 250 } else { mp.clamp(32, 256) };
    sanitize_unproto_colors(&mut settings.unproto_colors);
    sanitize_message_board(&mut settings);
    Ok(Some(settings))
}

/// Load settings from a concrete path, self-healing from the .tmp / .bak
/// siblings. `Ok(None)` only when nothing usable exists anywhere → fresh
/// install. `Err` only when a main file exists but every copy is corrupt.
fn load_settings_from_path(path: &std::path::Path) -> Result<Option<AppSettings>, String> {
    // Fast path: main file parses.
    if let Ok(Some(s)) = parse_settings_file(path) {
        return Ok(Some(s));
    }
    // Main missing/corrupt — the atomic writer's orphaned .tmp holds the
    // newest full content (crash between write and rename), else the .bak
    // from the previous save.
    let tmp = path.with_extension("json.tmp");
    let bak = PathBuf::from(format!("{}.bak", path.display()));
    for (label, cand) in [("tmp", &tmp), ("bak", &bak)] {
        let Ok(Some(s)) = parse_settings_file(cand) else { continue };
        crate::append_log(
            "settings",
            &format!(
                "self-heal: settings.json {} — restored from .{}",
                if path.exists() { "corrupt" } else { "missing" },
                label
            ),
        );
        // Promote the good copy back to the main file. Must NOT go through
        // save_settings_to_disk — its .bak-copy-first step would copy the
        // still-corrupt main over the only good backup (Python-parity rule).
        if let Err(e) = crate::storage::write_json_file(&path.to_path_buf(), &s, "settings") {
            crate::append_log("settings", &format!("self-heal promote failed: {e}"));
        }
        return Ok(Some(s));
    }
    if path.exists() {
        Err("Failed to parse settings: main, .tmp, and .bak all unreadable".to_string())
    } else {
        Ok(None)
    }
}

/// Missing-key default for `unprotoIgnoredDestinations`.
///
/// This struct's `#[serde(default)]` maps an ABSENT key to `""`
/// (`String::default`), which silently turns off simple-unproto's broadcast
/// filtering on settings.json files written before the field existed. We
/// distinguish "key absent" (old file / pre-upgrade — apply the default) from
/// "key present but empty" (the user deliberately cleared it in Settings —
/// keep it empty; only a factory reset restores the list). Read the raw JSON
/// first so we can tell the difference — the typed struct cannot.
fn default_missing_unproto_destinations(json: &mut serde_json::Value) {
    if json.get("unprotoIgnoredDestinations").is_none() {
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "unprotoIgnoredDestinations".to_string(),
                serde_json::Value::String("NODES BEACON ID".to_string()),
            );
        }
    }
}

/// Split a "CALL-N" string into (base callsign, SSID): the SSID is the
/// trailing number when the string looks like CALL-0..15. Used so a
/// callsign typed with an SSID (e.g. "KK6ABC-7") folds into the separate
/// callsign/ssid settings fields instead of being stored with a literal "-7".
fn split_callsign_ssid(call: &str) -> (&str, Option<u8>) {
    if let Some(dash) = call.rfind('-') {
        if let Ok(n) = call[dash + 1..].parse::<u8>() {
            if n <= 15 && call[..dash].len() >= 3 {
                return (&call[..dash], Some(n));
            }
        }
    }
    (call, None)
}

/// Sanitize message board settings on load/save: uppercase callsigns/aliases,
/// clamp SSID to 0–15, fold a "CALL-N" style callsign+ssid entry into the
/// separate fields (e.g. "KK6ABC-1" → callsign KK6ABC, ssid 1), and default
/// an empty board callsign to the user's own callsign with SSID 1 (any SSID
/// on the user's own callsign is dropped — "KK6SEN-7" → board KK6SEN-1).
fn sanitize_message_board(s: &mut AppSettings) {
    let mut call = s.message_board_callsign.trim().to_uppercase();
    if let (base, Some(n)) = split_callsign_ssid(&call) {
        s.message_board_ssid = n;
        call = base.to_string();
    }
    if call.is_empty() {
        // Board enabled with no callsign yet — default to the user's own
        // callsign with SSID 1 (user can change it later). Strip any SSID
        // the user's own callsign carries, or the board registers as
        // "KK6SEN-7-1" instead of "KK6SEN-1".
        let mine = s.callsign.trim().to_uppercase();
        if !mine.is_empty() {
            call = split_callsign_ssid(&mine).0.to_string();
            s.message_board_ssid = 1;
        }
    }
    // AX.25 callsigns are 1–6 alphanumeric. Anything else (trailing dash,
    // >6 chars, punctuation) can never match a decoded dest — the board
    // would silently never answer. Drop it and log instead of running dead.
    let valid = !call.is_empty()
        && call.len() <= 6
        && call.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid {
        if !call.is_empty() {
            crate::commands::log_line(&format!(
                "Message board: callsign \"{call}\" is invalid (use 1-6 letters/numbers) — board callsign cleared"
            ));
        }
        call = String::new();
        s.message_board_ssid = 1;
    }
    s.message_board_callsign = call;
    s.message_board_ssid = s.message_board_ssid.clamp(0, 15);
    // Sysops are compared against the session's BASE callsign (SSID is a
    // separate link-layer field) — fold any "CALL-N" entries so a sysop
    // list ported from a Python config with SSIDs doesn't silently never
    // match.
    s.message_board_sysops = s.message_board_sysops
        .split(',')
        .map(|c| {
            let t = c.trim().to_uppercase();
            split_callsign_ssid(&t).0.to_string()
        })
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    s.message_board_public_aliases = s.message_board_public_aliases.trim().to_uppercase();
}

/// Load settings from disk, falling back to defaults on missing file or parse error.
pub fn load_settings_or_default(app: &AppHandle) -> AppSettings {
    load_settings_from_disk(app).ok().flatten().unwrap_or_default()
}

/// True when the beacon is enabled AND has both text and a destination set.
pub fn beacon_configured(settings: &AppSettings) -> bool {
    settings.beacon_enabled
        && !settings.beacon.trim().is_empty()
        && !settings.beacon_dest.trim().is_empty()
}

/// Sync save window state (called synchronously on window close).
///
/// Safety rule: only ever save window state by *merging* onto settings that
/// actually loaded from disk. If settings.json is missing, locked, or
/// unparseable for ANY reason, skip the save entirely — writing defaults
/// here was the bug that wiped user settings: a transient read hiccup
/// degraded to `AppSettings::default()` and the save then overwrote the
/// real file with defaults + window state.
pub fn save_window_state_sync(
    app: &AppHandle,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    maximized: bool,
) -> Result<(), String> {
    let mut existing = match load_settings_from_disk(app) {
        Ok(Some(s)) => s,
        // Missing file on a fresh install, or unreadable file: do NOT
        // fabricate defaults and write them over disk. Window position is
        // not worth losing settings over.
        _ => return Ok(()),
    };
    existing.window_width = Some(width);
    existing.window_height = Some(height);
    existing.window_x = Some(x);
    existing.window_y = Some(y);
    existing.window_maximized = Some(maximized);
    save_settings_to_disk(app, &existing)
}

fn save_settings_to_disk(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    let path = crate::storage::app_data_path(app, "settings.json");
    save_settings_to_path(&path, settings)
}

/// Persist settings at a concrete path: rotate a valid main file to .bak,
/// then write atomically (tmp + rename).
fn save_settings_to_path(path: &std::path::Path, settings: &AppSettings) -> Result<(), String> {
    // .bak copy first (Python/BBS parity): the previous good settings survive
    // a crash/corruption mid-save, and load_settings_from_path self-heals
    // from it. Only rotate when the current main file is VALID — a corrupt
    // main (if we somehow reach a save without the load-path healing it)
    // must never clobber the only good backup.
    if path.exists() && parse_settings_file(path).is_ok_and(|v| v.is_some()) {
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        let _ = std::fs::copy(path, &bak);
    }
    crate::storage::write_json_file(&path.to_path_buf(), settings, "settings")
}

/// Setup-time restore: read settings and apply window geometry
pub fn restore_window_state(app: &AppHandle) -> Result<(), String> {
    let Ok(Some(settings)) = load_settings_from_disk(app) else { return Ok(()); };
    let Some(window) = app.get_webview_window("main") else { return Ok(()); };
    if !settings.window_maximized.unwrap_or(false) {
        if let (Some(w), Some(h)) = (settings.window_width, settings.window_height) {
            let _ = window.set_size(tauri::PhysicalSize {
                width: w,
                height: h,
            });
        }
    }
    if let (Some(x), Some(y)) = (settings.window_x, settings.window_y) {
        let _ = window.set_position(tauri::PhysicalPosition { x, y });
    }
    if settings.window_maximized.unwrap_or(false) {
        let _ = window.maximize();
    }
    Ok(())
}

// ── Tauri Commands ─────────────────────────────────────────

const THEME_NAMES: [&str; 6] = ["dark", "light", "blue", "sepia", "synthwave", "vintage"];

fn valid_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Drop unknown themes and malformed hex values from the color map, and
/// normalize stored hex to uppercase.
pub fn sanitize_unproto_colors(colors: &mut Option<HashMap<String, UnprotoThemeColors>>) {
    let Some(map) = colors.as_mut() else { return };
    for c in map.values_mut() {
        for slot in [&mut c.time, &mut c.call, &mut c.via, &mut c.meta, &mut c.payload, &mut c.connrx] {
            if let Some(v) = slot {
                if valid_hex(v) {
                    *slot = Some(v.to_uppercase());
                } else {
                    *slot = None;
                }
            }
        }
    }
    map.retain(|theme, _| THEME_NAMES.contains(&theme.as_str()));
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    mut settings: AppSettings,
) -> Result<(), String> {
    // The main settings dialog and the quick toggles (dedup, simple unproto,
    // last To/Via…) build their save payload without unproto_colors. Preserve
    // whatever is on disk so a normal save can't silently wipe custom colors.
    if settings.unproto_colors.is_none() {
        if let Ok(Some(existing)) = load_settings_from_disk(&app) {
            settings.unproto_colors = existing.unproto_colors;
        }
    }
    sanitize_unproto_colors(&mut settings.unproto_colors);
    clamp_beacon_interval(&mut settings);
    sanitize_message_board(&mut settings);
    // Daily settings-change log: which fields differ from what's on disk.
    {
        let old = load_settings_from_disk(&app).ok().flatten();
        let new_val = serde_json::to_value(&settings).unwrap_or_default();
        let changed = match &old {
            Some(o) => {
                let old_val = serde_json::to_value(o).unwrap_or_default();
                diff_keys(&old_val, &new_val)
            }
            None => "first save".to_string(),
        };
        crate::append_log(
            "settings",
            &format!(
                "[{}] settings saved: {}",
                chrono::Local::now().format("%H:%M:%S"),
                changed
            ),
        );
    }
    save_settings_to_disk(&app, &settings)?;
    // NOTE: no beacon restart here. The beacon task polls settings from disk
    // every 10s on its own; restarting it on every save was the cause of
    // beacon spam when unrelated settings (unproto To/Via auto-saves, UI
    // toggles…) saved. Beacon changes take effect within one poll tick.
    Ok(())
}

#[tauri::command]
pub async fn load_settings(app: AppHandle) -> Result<AppSettings, String> {
    let settings = load_settings_or_default(&app);
    // Write default settings.json if it didn't exist yet (fresh install)
    let path = crate::storage::app_data_path(&app, "settings.json");
    if !path.exists() {
        let _ = save_settings_to_disk(&app, &settings);
    }
    Ok(settings)
}

/// Field names that differ between two settings JSON objects (for the
/// daily settings-change log).
fn diff_keys(old: &serde_json::Value, new: &serde_json::Value) -> String {
    let mut out: Vec<String> = Vec::new();
    if let (serde_json::Value::Object(o), serde_json::Value::Object(n)) = (old, new) {
        for (k, v) in n {
            if o.get(k) != Some(v) {
                out.push(k.clone());
            }
        }
    }
    if out.is_empty() {
        "no changes".to_string()
    } else {
        out.join(", ")
    }
}

/// Reset settings to factory defaults (overwrites the file)
#[tauri::command]
pub async fn reset_settings(app: AppHandle) -> Result<(), String> {
    let defaults = AppSettings::default();
    save_settings_to_disk(&app, &defaults)
}

#[tauri::command]
pub async fn save_window_state(
    app: AppHandle,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    maximized: bool,
) -> Result<(), String> {
    save_window_state_sync(&app, width, height, x, y, maximized)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JS frontend sends camelCase keys (unprotoTo / unprotoVia) and expects
    /// them back on load. Guards the `rename_all = "camelCase"` mapping so a
    /// future field rename can't silently break To/Via persistence again.
    #[test]
    fn unproto_to_via_survive_json_round_trip() {
        let mut s = AppSettings::default();
        s.unproto_to = "W6ABC".to_string();
        s.unproto_via = "KBANN".to_string();

        // Serialize as save_settings_to_disk does
        let json = serde_json::to_string_pretty(&s).expect("serialize");
        assert!(json.contains("\"unprotoTo\": \"W6ABC\""), "json missing unprotoTo:\n{json}");
        assert!(json.contains("\"unprotoVia\": \"KBANN\""), "json missing unprotoVia:\n{json}");

        // Deserialize as load_settings_from_disk does
        let back: AppSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.unproto_to, "W6ABC");
        assert_eq!(back.unproto_via, "KBANN");

        // Simulate the exact JS payload shape: {settings: {unprotoTo: ...}}
        let payload = serde_json::json!({
            "settings": {"callsign": "KK6SEN", "unprotoTo": "W6ABC", "unprotoVia": "KBANN"}
        });
        let from_payload: AppSettings =
            serde_json::from_value(payload["settings"].clone()).expect("payload");
        assert_eq!(from_payload.unproto_to, "W6ABC");
        assert_eq!(from_payload.unproto_via, "KBANN");
    }

    /// The stored JSON labels must survive a save/load round-trip —
    /// existing settings.json files use "AGWPE"/"KISS" strings.
    #[test]
    fn protocol_pref_serde_roundtrip() {
        for (label, pref) in [("AGWPE", ProtocolPref::Agwpe), ("KISS", ProtocolPref::Kiss)] {
            let json = serde_json::to_string(&pref).unwrap();
            assert_eq!(json, format!("\"{}\"", label));
            let back: ProtocolPref = serde_json::from_str(&json).unwrap();
            assert_eq!(back, pref);
        }
        // Old-style settings.json value must still load
        let loaded: AppSettings = serde_json::from_str(r#"{"protocol":"AGWPE"}"#).unwrap();
        assert_eq!(loaded.protocol, ProtocolPref::Agwpe);
    }

    /// unprotoIgnoredDestinations must default to "NODES BEACON ID" when the
    /// key is ABSENT (old/pre-upgrade settings.json — otherwise simple-unproto
    /// silently stops hiding NODES/BEACON/ID), but MUST NOT resurrect when the
    /// user deliberately cleared the field (present-but-empty — that state is
    /// meant to persist until a factory reset).
    #[test]
    fn unproto_ignored_dest_missing_key_defaults_but_empty_is_respected() {
        let missing = r#"{"protocol":"AGWPE","simpleUnproto":true}"#;
        let mut mv: serde_json::Value = serde_json::from_str(missing).unwrap();
        default_missing_unproto_destinations(&mut mv);
        assert_eq!(
            mv["unprotoIgnoredDestinations"], "NODES BEACON ID",
            "absent key must default to the broadcast list"
        );

        let empty = r#"{"protocol":"AGWPE","simpleUnproto":true,"unprotoIgnoredDestinations":""}"#;
        let mut ev: serde_json::Value = serde_json::from_str(empty).unwrap();
        default_missing_unproto_destinations(&mut ev);
        assert_eq!(
            ev["unprotoIgnoredDestinations"], "",
            "a present-but-empty value (user cleared it) must stay empty"
        );

        let custom = r#"{"protocol":"AGWPE","unprotoIgnoredDestinations":"NODES"}"#;
        let mut cv: serde_json::Value = serde_json::from_str(custom).unwrap();
        default_missing_unproto_destinations(&mut cv);
        assert_eq!(cv["unprotoIgnoredDestinations"], "NODES");
    }

    /// unprotoColors survives the JSON round-trip (camelCase key), old
    /// settings files without the field stay None, and malformed values /
    /// unknown themes get dropped by sanitize.
    #[test]
    fn unproto_colors_roundtrip_and_sanitize() {
        let mut s = AppSettings::default();
        s.unproto_colors = Some(HashMap::from([(
            "dark".to_string(),
            UnprotoThemeColors {
                time: Some("#E6D080".into()),
                call: Some("#33ff33".into()),
                connrx: Some("#00cccc".into()),
                ..Default::default()
            },
        )]));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"unprotoColors\""), "missing camelCase key:\n{json}");

        let back: AppSettings = serde_json::from_str(&json).unwrap();
        let dark = &back.unproto_colors.as_ref().unwrap()["dark"];
        assert_eq!(dark.time.as_deref(), Some("#E6D080"));
        assert_eq!(dark.call.as_deref(), Some("#33ff33"));
        assert_eq!(dark.connrx.as_deref(), Some("#00cccc"));
        assert!(dark.via.is_none());

        // Old settings files (no unprotoColors) load fine and preserve None
        let old: AppSettings = serde_json::from_str(r#"{"theme":"sepia"}"#).unwrap();
        assert!(old.unproto_colors.is_none());

        // Malformed hex + unknown theme get dropped, hex normalized uppercase
        let mut bad = AppSettings::default();
        bad.unproto_colors = Some(HashMap::from([
            (
                "purple".to_string(),
                UnprotoThemeColors { time: Some("#12".into()), ..Default::default() },
            ),
            (
                "dark".to_string(),
                UnprotoThemeColors {
                    payload: Some("#zzzzzz".into()),
                    via: Some("#008888".into()),
                    ..Default::default()
                },
            ),
        ]));
        sanitize_unproto_colors(&mut bad.unproto_colors);
        let map = bad.unproto_colors.unwrap();
        assert_eq!(map.len(), 1, "unknown theme must be dropped");
        assert!(map["dark"].payload.is_none(), "malformed hex must be dropped");
        assert_eq!(map["dark"].via.as_deref(), Some("#008888"));
    }

    /// A "CALL-N" typed into the board callsign field folds into the
    /// separate callsign/ssid fields ("KK6ABC-7" → KK6ABC + ssid 7).
    #[test]
    fn message_board_callsign_with_ssid_folds_into_fields() {
        let mut s = AppSettings::default();
        s.message_board_callsign = "kk6abc-7".to_string();
        sanitize_message_board(&mut s);
        assert_eq!(s.message_board_callsign, "KK6ABC");
        assert_eq!(s.message_board_ssid, 7);
    }

    /// Empty board callsign defaults to the user's own callsign with SSID 1 —
    /// any SSID the user's callsign carries must be dropped ("KK6SEN-7" →
    /// board "KK6SEN-1", never "KK6SEN-7-1").
    #[test]
    fn message_board_defaults_strip_ssid_from_user_callsign() {
        let mut s = AppSettings::default();
        s.callsign = "KK6SEN-7".to_string();
        sanitize_message_board(&mut s);
        assert_eq!(s.message_board_callsign, "KK6SEN");
        assert_eq!(s.message_board_ssid, 1);
    }

    /// A malformed board callsign (trailing dash, too long) can never match
    /// a decoded AX.25 dest — it must be cleared, not left to run dead.
    #[test]
    fn message_board_invalid_callsign_is_cleared() {
        let mut s = AppSettings::default();
        s.message_board_callsign = "KK6ABC-".to_string();
        sanitize_message_board(&mut s);
        assert_eq!(s.message_board_callsign, "");
        s.message_board_callsign = "KK6ABCDEF".to_string(); // 9 chars
        sanitize_message_board(&mut s);
        assert_eq!(s.message_board_callsign, "");
    }

    /// Sysop entries with an SSID fold to the base callsign — the session
    /// user is the base, so "KK6ABC-7" in sysops would otherwise never match.
    #[test]
    fn message_board_sysops_ssids_fold_to_base() {
        let mut s = AppSettings::default();
        s.message_board_sysops = "KK6SEN-15, W6ABC, N7XYZ-0".to_string();
        sanitize_message_board(&mut s);
        assert_eq!(s.message_board_sysops, "KK6SEN,W6ABC,N7XYZ");
    }

    /// AX.25 link-layer timers must be clamped on load: a missing or zero
    /// timer value in settings.json (the AC0ZH-1 RR-loop incident: T1 of 0
    /// made the recovery timer fire RR enquiries back-to-back) must never
    /// survive into the running app, and absurd values get capped.
    #[test]
    fn ax25_timers_clamped() {
        // Missing keys (minimal file) → deserialize to whatever serde fills,
        // then clamp must guarantee safe minimums either way.
        let mut s: AppSettings = serde_json::from_str(r#"{"callsign":"KK6SEN"}"#).unwrap();
        clamp_ax25_timers(&mut s);
        assert!(s.kiss_t1_ms >= 100, "t1 below floor: {}", s.kiss_t1_ms);
        assert!(s.kiss_t2_ms >= 100, "t2 below floor: {}", s.kiss_t2_ms);
        assert!(s.kiss_t3_ms >= 5_000, "t3 below floor: {}", s.kiss_t3_ms);
        assert!(s.kiss_ka_check_ms >= 1_000, "ka_check below floor: {}", s.kiss_ka_check_ms);
        assert!(s.kiss_n2_max >= 1, "n2 below floor: {}", s.kiss_n2_max);

        // Explicit zeros (hand-edited file / old build) → raised to floors
        s.kiss_t1_ms = 0;
        s.kiss_t2_ms = 0;
        s.kiss_t3_ms = 0;
        s.kiss_ka_check_ms = 0;
        s.kiss_n2_max = 0;
        clamp_ax25_timers(&mut s);
        assert_eq!(s.kiss_t1_ms, 100);
        assert_eq!(s.kiss_t2_ms, 100);
        assert_eq!(s.kiss_t3_ms, 5_000);
        assert_eq!(s.kiss_ka_check_ms, 1_000);
        assert_eq!(s.kiss_n2_max, 1);

        // Absurd values → capped
        s.kiss_t1_ms = 999_999_999;
        s.kiss_t2_ms = 999_999_999;
        s.kiss_t3_ms = 999_999_999;
        s.kiss_ka_check_ms = 999_999_999;
        s.kiss_n2_max = 999;
        clamp_ax25_timers(&mut s);
        assert_eq!(s.kiss_t1_ms, 120_000);
        assert_eq!(s.kiss_t2_ms, 10_000);
        assert_eq!(s.kiss_t3_ms, 3_600_000);
        assert_eq!(s.kiss_ka_check_ms, 600_000);
        assert_eq!(s.kiss_n2_max, 50);

        // Valid defaults pass through untouched
        let mut d = AppSettings::default();
        clamp_ax25_timers(&mut d);
        assert_eq!(d.kiss_t1_ms, 10_000);
        assert_eq!(d.kiss_t2_ms, 1_000);
        assert_eq!(d.kiss_t3_ms, 300_000);
        assert_eq!(d.kiss_ka_check_ms, 60_000);
        assert_eq!(d.kiss_n2_max, 5);
    }

    /// Beacon interval 0 (empty field / hand-edited file) must be stored as
    /// 1 minute — disabling is the Enable checkbox, not the interval.
    #[test]
    fn beacon_interval_zero_defaults_to_one() {
        // A missing key already deserializes to the field default (240);
        // only an explicit 0 in the file needs raising.
        let mut s: AppSettings = serde_json::from_str(r#"{"callsign":"KK6SEN","beaconInterval":0}"#)
            .unwrap();
        assert_eq!(s.beacon_interval, 0, "explicit 0 must deserialize as-is");
        clamp_beacon_interval(&mut s);
        assert_eq!(s.beacon_interval, 1);

        let mut d = AppSettings::default();
        clamp_beacon_interval(&mut d);
        assert_eq!(d.beacon_interval, 240, "valid default passes through");
    }

    // ── .bak / .tmp self-healing ─────────────────────────────

    fn temp_settings_path(name: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "k2k-settings-test-{}-{}-{name}.json",
            std::process::id(),
            n
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}.bak", path.display()));
        let _ = std::fs::remove_file(path.with_extension("json.tmp"));
    }

    /// A corrupt main file must recover from the .bak written on the previous
    /// save, and the recovered data must be promoted back to the main file.
    /// The .bak is ALWAYS one save behind (first save has no backup), so the
    /// recovered settings are the last-good version.
    #[test]
    fn corrupt_main_recovers_from_bak() {
        let path = temp_settings_path("corrupt_main_recovers_from_bak");
        cleanup(&path);

        // Save #1: main only (no bak on the very first save — Python parity).
        let mut s = AppSettings::default();
        s.callsign = "KK6SEN".into();
        s.host = "192.168.1.1".into();
        save_settings_to_path(&path, &s).expect("save #1");

        // Save #2 rotates main → .bak, so the backup now holds save #1.
        s.callsign = "W6ABC".into();
        s.theme = "sepia".into();
        save_settings_to_path(&path, &s).expect("save #2");

        // Corrupt the main file.
        std::fs::write(&path, "{ definitely not json").expect("corrupt the main file");

        // Load must fall back to .bak and return the last GOOD save (#1).
        let loaded = load_settings_from_path(&path).expect("load").expect("some");
        assert_eq!(loaded.callsign, "KK6SEN");
        assert_eq!(loaded.host, "192.168.1.1");

        // And the main file must have been healed back to valid JSON.
        let healed = parse_settings_file(&path).expect("healed").expect("some");
        assert_eq!(healed.callsign, "KK6SEN");
        cleanup(&path);
    }

    /// A MISSING main file (crash in the remove+rename window) must recover
    /// from the orphaned .tmp that the atomic writer leaves behind.
    #[test]
    fn missing_main_recovers_from_tmp() {
        let path = temp_settings_path("missing_main_recovers_from_tmp");
        cleanup(&path);

        let mut s = AppSettings::default();
        s.callsign = "KK6SEN".into();
        s.host = "192.168.1.1".into();
        // Simulate a crash mid-atomic-write: only the .tmp exists (the rename
        // never happened), main is gone.
        std::fs::write(
            path.with_extension("json.tmp"),
            serde_json::to_string_pretty(&s).unwrap(),
        )
        .expect("write tmp");

        let loaded = load_settings_from_path(&path).expect("load").expect("some");
        assert_eq!(loaded.callsign, "KK6SEN");
        assert_eq!(loaded.host, "192.168.1.1");
        assert!(path.exists(), "recovered settings must be promoted to main");
        cleanup(&path);
    }

    /// With NO main, .tmp, or .bak, load reports None — the fresh-install
    /// path — instead of fabricating defaults.
    #[test]
    fn nothing_on_disk_reports_none() {
        let path = temp_settings_path("nothing_on_disk_reports_none");
        cleanup(&path);
        assert!(load_settings_from_path(&path).unwrap().is_none());
        cleanup(&path);
    }

    /// Every save rotates a *valid* main file to .bak. And a corrupt main
    /// must never clobber a good .bak (the load path may not have healed
    /// yet when a save fires).
    #[test]
    fn bak_rotation_preserves_good_backup() {
        let path = temp_settings_path("bak_rotation_preserves_good_backup");
        cleanup(&path);

        let mut s = AppSettings::default();
        s.callsign = "KK6SEN".into();
        save_settings_to_path(&path, &s).expect("save #1");
        assert!(!path.exists() || parse_settings_file(&path).is_ok_and(|v| v.is_some()));

        // A second save rotates the good main to bak.
        s.callsign = "W6ABC".into();
        save_settings_to_path(&path, &s).expect("save #2");
        let bak = format!("{}.bak", path.display());
        let bak_txt = std::fs::read_to_string(&bak).expect("read bak");
        assert!(bak_txt.contains("KK6SEN"), "bak must hold the prior save");

        // Now corrupt the main and save again — the GOOD bak must survive.
        std::fs::write(&path, "garbage").expect("corrupt main");
        save_settings_to_path(&path, &s).expect("save #3 over corrupt main");
        let bak_txt = std::fs::read_to_string(&bak).expect("read bak again");
        assert!(bak_txt.contains("KK6SEN"), "corrupt main must not clobber good bak");
        cleanup(&path);
    }
}
