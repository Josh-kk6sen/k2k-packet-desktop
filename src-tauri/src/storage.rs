//! Shared JSON-file persistence helpers.
//!
//! The settings and history layers both write JSON files into the app
//! data directory — this keeps the path-building and the
//! create-dir → serialize → write sequence in one place.
//!
//! The base directory is resolved ONCE at startup (desktop setup or the
//! headless `--web` entry point) and stored in `DATA_DIR`, so every layer —
//! including code that has no `AppHandle` (headless serve mode) — agrees on
//! where settings.json / history.json / pbbs_data.json / logs live.

use std::path::PathBuf;
use std::sync::OnceLock;
#[cfg(feature = "desktop")]
use tauri::AppHandle;
#[cfg(feature = "desktop")]
use tauri::Manager;

/// The one data directory for this process. Set at startup; see `set_data_dir`.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Pin the data directory for this process. First call wins; later calls are
/// ignored (the desktop setup runs once, before any command can execute).
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

/// Resolve and pin the data dir for a process with no Tauri app (headless).
/// Same layout rules as the desktop app so both modes share settings/logs.
pub fn init_headless_data_dir() -> PathBuf {
    let dir = default_data_base();
    set_data_dir(dir.clone());
    dir
}

/// The base directory, resolved without Tauri: Windows keeps everything in
/// the executable's folder (portable-style, the directory chosen at install
/// time); Linux/macOS use the standard local data dir for the app id.
fn default_data_base() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.to_path_buf();
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
            if !dir.is_empty() {
                return PathBuf::from(dir).join("K2KDesktop");
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home).join(".local/share/K2KDesktop");
            }
        }
    }
    PathBuf::from(".")
}

/// The single base directory for all app data. Always the pinned directory
/// once `set_data_dir` has run; otherwise a best-effort default.
pub fn data_base_static() -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(default_data_base)
}

/// Absolute path of `file` inside the app data directory, without needing an
/// `AppHandle` (headless/browser mode).
pub fn app_data_path_static(file: &str) -> PathBuf {
    data_base_static().join(file)
}

/// The single base directory for all app data (see `app_data_path`).
///
/// Desktop only: it asks Tauri for the platform data dir on first run. The
/// headless entry point pins the directory itself (`init_headless_data_dir`),
/// so every later lookup goes through `data_base_static`.
#[cfg(feature = "desktop")]
pub fn data_base(app: &AppHandle) -> PathBuf {
    if DATA_DIR.get().is_some() {
        return data_base_static();
    }
    #[cfg(target_os = "windows")]
    {
        // Portable-style: keep all app data in the executable's folder —
        // the exact directory the user chose at install time. (Tauri's
        // executable_dir() resolves to %ProgramFiles% on Windows, and
        // std::env::current_exe() gives the real binary path.)
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.to_path_buf();
            }
        }
    }
    app.path().app_local_data_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Serialize `value` as pretty JSON and write it to `path`, creating the
/// parent directory first. `what` names the file in error messages
/// (e.g. "settings" → "Failed to write settings: ...").
pub fn write_json_file(
    path: &PathBuf,
    value: &impl serde::Serialize,
    what: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {} dir: {}", what, e))?;
    }
    let contents = serde_json::to_string_pretty(value)
        .map_err(|e| format!("Failed to serialize {}: {}", what, e))?;
    // Atomic write: write to a temp file in the same directory, then rename
    // over the target. A plain std::fs::write truncates-then-fills, so a
    // concurrent reader (the message-board gate reads settings.json per
    // frame) can catch a half-written file and parse garbage — which
    // silently disabled the board. Rename is atomic on the same filesystem.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &contents)
        .map_err(|e| format!("Failed to write {}: {}", what, e))?;
    if let Err(_e) = std::fs::rename(&tmp, path) {
        // Windows: rename over an existing file can fail — fall back to
        // remove+rename (the tiny non-atomic gap is better than a failure).
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path).map_err(|e| format!("Failed to write {}: {}", what, e))?;
    }
    Ok(())
}
