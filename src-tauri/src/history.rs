use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    pub entries: HashMap<String, Vec<String>>,
}

/// Load history from disk — returns an empty store if the file doesn't exist or is corrupted
fn load_history_from_disk() -> HistoryStore {
    let path = crate::storage::app_data_path_static("history.json");
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
fn save_history_to_disk(store: &HistoryStore) -> Result<(), String> {
    let path = crate::storage::app_data_path_static("history.json");
    crate::storage::write_json_file(&path, store, "history")
}

/// Save a single history entry (deduped, max N items)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn save_history(key: String, value: String, max: usize) -> Result<(), String> {
    let mut store = load_history_from_disk();
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
    save_history_to_disk(&store)
}

/// Load history for a given key
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn load_history(key: String) -> Result<Vec<String>, String> {
    let store = load_history_from_disk();
    Ok(store.entries.get(&key).cloned().unwrap_or_default())
}

/// Remove a single history entry for a given key
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn remove_history(key: String, value: String) -> Result<(), String> {
    let mut store = load_history_from_disk();
    if let Some(entries) = store.entries.get_mut(&key) {
        if let Some(pos) = entries.iter().position(|v| v == &value) {
            entries.remove(pos);
        }
    }
    save_history_to_disk(&store)
}

/// Clear history for a given key (or all if key is empty)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn clear_history(key: String) -> Result<(), String> {
    let mut store = load_history_from_disk();
    if key.is_empty() {
        store.entries.clear();
    } else {
        store.entries.remove(&key);
    }
    save_history_to_disk(&store)
}
