// ─ K2K Packet Desktop — app layer (the one place tauri:: may appear) ─
//
// AppCore owns ONE state object (Radio + board + settings + links + an output
// sink), and `ui.rs` is the single Effect→event mapper (landed in a later
// phase). commands.rs (the current 2.4k-line god-file) becomes a stack of
// thin delegates over AppCore.
//
// `KernelOutput` for `AppHandle` (below) is where an `AppHandle` is turned
// into outward effects (events, settings access, data dir, logs).

use tauri::AppHandle;

pub mod comm;
pub mod state;

/// Direct impl on `AppHandle` so the app layer can pass `&AppHandle` anywhere
/// a `&dyn KernelOutput` is expected, and `Arc::new(app_handle.clone())` where
/// an owned output is needed — no wrapping boilerplate at call sites.
impl crate::kernel::output::KernelOutput for AppHandle {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        tauri::Emitter::emit(self, event, payload)
            .map(|_| ())
            .unwrap_or(());
    }
    fn load_settings(&self) -> Result<Option<crate::settings::AppSettings>, String> {
        crate::settings::load_settings_from_disk(self)
    }
    fn data_dir(&self) -> std::path::PathBuf {
        crate::storage::data_base(self)
    }
    fn log_line(&self, msg: &str) {
        crate::commands::log_line(msg);
    }
    fn log_packet(&self, data: &[u8], kind: &str) {
        crate::commands::log_packet(data, kind);
    }
    fn refresh_bbs_setup(&self) {}
}
