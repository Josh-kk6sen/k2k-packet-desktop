// ─ K2K Packet Desktop — the app-layer output handle ─
//
// `Out` is what the command layer holds instead of a raw `tauri::AppHandle`.
// It is the same contract either way (`KernelOutput`), so every command body
// is written once and runs unchanged in both front-ends:
//
//   * `Out::tauri(handle)` — the desktop window; events go to the WebView.
//   * `Out::web(sink)`     — headless serve mode; events go to the browser
//                            (WebSocket broadcast + a replay ring buffer so a
//                            late-joining or reloaded tab still gets a state
//                            snapshot).
//
// Settings/data-dir/log access no longer touch Tauri at all: the data
// directory is pinned once at startup (`storage::set_data_dir`). That is what
// lets the same command bodies run with no window in the process.

use crate::kernel::output::KernelOutput;
use crate::settings::AppSettings;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::AppHandle;

#[derive(Clone)]
pub struct Out(Arc<Inner>);

enum Inner {
    Tauri(AppHandle),
    Web(Arc<crate::web::Sink>),
}

impl Out {
    pub fn tauri(handle: AppHandle) -> Self {
        Self(Arc::new(Inner::Tauri(handle)))
    }

    pub fn web(sink: Arc<crate::web::Sink>) -> Self {
        Self(Arc::new(Inner::Web(sink)))
    }
}

impl KernelOutput for Out {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        match &*self.0 {
            Inner::Tauri(handle) => {
                let _ = tauri::Emitter::emit(handle, event, payload);
            }
            Inner::Web(sink) => sink.emit(event, payload),
        }
    }

    fn load_settings(&self) -> Result<Option<AppSettings>, String> {
        crate::settings::load_settings_from_disk()
    }

    fn data_dir(&self) -> PathBuf {
        crate::storage::data_base_static()
    }

    fn log_line(&self, msg: &str) {
        crate::commands::log_line(msg);
    }

    fn log_packet(&self, data: &[u8], kind: &str) {
        crate::commands::log_packet(data, kind);
    }

    fn refresh_bbs_setup(&self) {
        // Both front-ends re-read the board setup from settings on use; there
        // is no cached setup to refresh here.
    }
}
