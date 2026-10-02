//! Shared JSON-file persistence helpers.
//!
//! The settings and history layers both write JSON files into the app
//! data directory — this keeps the path-building and the
//! create-dir → serialize → write sequence in one place.

use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

/// Absolute path of `file` inside the app data directory.
///
/// Everything (settings.json, history.json, pbbs_data.json, logs/) shares
/// one base. On Windows the base is the executable's folder — the exact
/// directory the user chose at install time — so the whole app lives in
/// one place (portable-style). On Linux/macOS the install dir is not
/// user-writable, so the standard local data dir is used instead.
pub fn app_data_path(app: &AppHandle, file: &str) -> PathBuf {
    let mut path = data_base(app);
    path.push(file);
    path
}

/// The single base directory for all app data (see `app_data_path`).
pub fn data_base(app: &AppHandle) -> PathBuf {
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
