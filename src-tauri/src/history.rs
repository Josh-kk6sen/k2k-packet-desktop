use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    pub entries: HashMap<String, Vec<String>>,
}

/// Load history from disk — returns an empty store if the file doesn't exist or is corrupted
fn load_history_from_disk(app: &AppHandle) -> HistoryStore {
    let path = crate::storage::app_data_path(app, "history.json");
    if !path.exists() {
        return HistoryStore::default();
    }
    match fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).unwrap_or_default()
        }
        Err(_) => HistoryStore::default(),
    }
}

/// Save history to disk
fn save_history_to_disk(app: &AppHandle, store: &HistoryStore) -> Result<(), String> {
    let path = crate::storage::app_data_path(app, "history.json");
    crate::storage::write_json_file(&path, store, "history")
}

/// Save a single history entry (deduped, max N items)
#[tauri::command]
pub fn save_history(app: AppHandle, key: String, value: String, max: usize) -> Result<(), String> {
    let mut store = load_history_from_disk(&app);
    let entries = store.entries.entry(key).or_insert_with(Vec::new);
    // Remove duplicates
    if let Some(pos) = entries.iter().position(|v| v == &value) {
        entries.remove(pos);
    }
    // Add newest at the end
    entries.push(value);
    // Trim to max (remove oldest from the beginning)
    if entries.len() > max {
        let excess = entries.len() - max;
        entries.drain(0..excess);
    }
    save_history_to_disk(&app, &store)
}

/// Load history for a given key
#[tauri::command]
pub fn load_history(app: AppHandle, key: String) -> Result<Vec<String>, String> {
    let store = load_history_from_disk(&app);
    Ok(store.entries.get(&key).cloned().unwrap_or_default())
}

/// Remove a single history entry for a given key
#[tauri::command]
pub fn remove_history(app: AppHandle, key: String, value: String) -> Result<(), String> {
    let mut store = load_history_from_disk(&app);
    if let Some(entries) = store.entries.get_mut(&key) {
        if let Some(pos) = entries.iter().position(|v| v == &value) {
            entries.remove(pos);
        }
    }
    save_history_to_disk(&app, &store)
}

/// Clear history for a given key (or all if key is empty)
#[tauri::command]
pub fn clear_history(app: AppHandle, key: String) -> Result<(), String> {
    let mut store = load_history_from_disk(&app);
    if key.is_empty() {
        store.entries.clear();
    } else {
        store.entries.remove(&key);
    }
    save_history_to_disk(&app, &store)
}
