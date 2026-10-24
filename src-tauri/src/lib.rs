// ── K2K Packet Desktop — Library Entry Point ──────────────
//
// This is the Tauri application entry point. It registers all modules,
// declares the command handlers, sets up the window event listeners
// (for saving/restoring window position), and runs the app.

mod agw;
mod app;
mod commands;
mod history;
mod kernel;
mod kiss;
#[cfg(test)]
mod live_test;
// link/ holds the transport seam. The KISS adapter (KissLink) is wired into
// commands.rs today; the AGW adapter (AgwLink) and the KISS inbound-decode
// helpers (feed/captured_tx/with_tx_sink) are used only by unit tests so far
// and are wired onto the seam in a later phase — hence the local allow.
#[allow(dead_code)]
mod link;
mod settings;
mod storage;

// Kernel modules re-exported at crate root path so existing `crate::session::…`
// etc. references keep resolving (module re-export preserves pub(crate)
// visibility — do NOT switch these to `pub use kernel::…::*;`).
pub use kernel::{ax25, bbs, bbs_link, netrom, session};

use settings::restore_window_state;
use tauri::Manager;
use std::sync::OnceLock;

/// Global path to the application log file, set once at startup.
/// Directory holding settings.json, pbbs_data.json and the daily log files.
pub static LOG_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Path of today's log file for a log kind ("raw", "unproto", "connected",
/// "admin", "settings"): <log_dir>/YYMMDD_<kind>.log — one file per day,
/// kept indefinitely.
pub fn daily_log_path(kind: &str) -> std::path::PathBuf {
    let dir = LOG_DIR
        .get()
        .cloned()
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!(
        "{}_{}.log",
        chrono::Local::now().format("%y%m%d"),
        kind
    ))
}

/// Append one line to today's log file of the given kind (creates it).
pub fn append_log(kind: &str, text: &str) {
    use std::io::Write;
    let path = daily_log_path(kind);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", text);
    }
}

/// Build and run the Tauri application.
///
/// This is called from main.rs. It:
///   1. Registers the Tauri plugin shell
///   2. Creates the global AppState (holds all KISS connections)
///   3. Registers all IPC command handlers
///   4. Restores the window position/size from saved settings
///   5. Saves window position on close
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance: a second launch focuses the existing window
        // instead of spawning a second process. Two app instances writing
        // settings.json is a realistic settings-wipe race (both load, one
        // window-state save overwrites the other's merge), and the frontend
        // window-state writer makes clicks on stale shortcuts observable.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .manage(session::AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::kiss_connect,
            commands::kiss_disconnect,
            commands::ax25_connect,
            commands::ax25_send_iframe,
            commands::ax25_disconnect,
            commands::ax25_send_ui,
            commands::agw_connect,
            commands::agw_disconnect_server,
            commands::append_log,
            commands::open_log_dir,
            commands::pbbs_admin_open,
            commands::pbbs_admin_send,
            commands::pbbs_admin_reset,
            commands::pbbs_users,
            commands::agw_ax25_connect,
            commands::agw_send_data,
            commands::agw_disconnect,
            commands::agw_send_ui,
            settings::save_settings,
            settings::load_settings,
            settings::reset_settings,
            settings::save_window_state,
            history::save_history,
            history::load_history,
            history::clear_history,
            history::remove_history,
            // Unified protocol commands
            commands::connect_to_server,
            commands::connect_session,
            commands::send_connected_data,
            commands::send_ui_frame,
        ])
        .setup(|app| {
            // Everything the app writes (settings.json, pbbs_data.json,
            // history.json, logs/) lives in one base directory. On Windows
            // that's the executable's folder — the directory the user
            // picked during installation. On Linux/macOS it's the standard
            // local data dir (the install dir is not user-writable there).
            let data_base = crate::storage::data_base(app.handle());
            // One-time migration from earlier layouts: ≤0.9.45 used
            // app_data_dir (Roaming on Windows); 0.9.46 used
            // app_local_data_dir. Copy any existing files across so nothing
            // is lost. Originals are left in place.
            let old_bases = [
                app.path().app_local_data_dir().unwrap_or_default(),
                app.path().app_data_dir().unwrap_or_default(),
            ];
            for old_base in old_bases {
                if old_base == data_base {
                    continue;
                }
                for f in ["settings.json", "settings.json.bak", "history.json", "pbbs_data.json", "pbbs_data.json.bak"] {
                    let src = old_base.join(f);
                    let dst = data_base.join(f);
                    if src.exists() && !dst.exists() {
                        let _ = std::fs::copy(&src, &dst);
                    }
                }
                // Also bring over any existing daily log files
                let old_logs = old_base.join("logs");
                if old_logs.is_dir() {
                    let _ = std::fs::create_dir_all(data_base.join("logs"));
                    if let Ok(rd) = std::fs::read_dir(&old_logs) {
                        for e in rd.flatten() {
                            let name = e.file_name();
                            let Some(name) = name.to_str() else { continue };
                            if name.ends_with(".log") {
                                let dst = data_base.join("logs").join(name);
                                if !dst.exists() {
                                    let _ = std::fs::copy(e.path(), &dst);
                                }
                            }
                        }
                    }
                }
            }
            let log_dir = data_base.join("logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let _ = LOG_DIR.set(log_dir.clone());
            // Startup header in today's raw log
            append_log("raw", &format!("=== K2K Packet Desktop v{} started ===", env!("CARGO_PKG_VERSION")));

            // Create the main window ourselves (instead of letting the
            // tauri.conf.json config auto-create it) so we can point the
            // WebView2 profile folder at the same base directory as the
            // rest of the app on Windows. Otherwise Tauri creates a second
            // AppData\Local\<id> folder for EBWebView next to the install
            // dir — the whole app must live in one folder.
            let handle = app.handle();
            let wb = tauri::WebviewWindowBuilder::new(
                handle,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("K2K Packet Desktop")
            .inner_size(920.0, 680.0)
            .min_inner_size(640.0, 480.0)
            .resizable(true)
            .center();
            #[cfg(target_os = "windows")]
            let wb = wb.data_directory(crate::storage::data_base(handle));
            let _ = wb.build();

            // Restore saved window position/size from last session
            let _ = restore_window_state(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            use tauri::WindowEvent;
            match event {
                WindowEvent::CloseRequested { .. } => {
                    // Save window geometry before closing so we can restore it next launch.
                    // Skip when hidden (minimized to tray / off-screen) — the geometry
                    // saved would be stale and the restore would pop the window back
                    // at the wrong position.
                    if !window.is_visible().unwrap_or(false) {
                        return;
                    }
                    let maximized = window.is_maximized().unwrap_or(false);
                    let handle = window.app_handle();
                    let Some(size) = window.outer_size().ok() else { return; };
                    let Some(pos) = window.outer_position().ok() else { return; };
                    let _ = settings::save_window_state_sync(
                        &handle,
                        size.width,
                        size.height,
                        pos.x,
                        pos.y,
                        maximized,
                    );
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running K2K Packet Desktop");
}
