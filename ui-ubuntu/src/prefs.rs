//! Simple `key=value` preferences at `~/.config/scp-commander/prefs.conf`.
//! Used for settings that don't belong to a single session (default editor,
//! transfer pool size, keepalive interval, default exclude masks).

use std::collections::BTreeMap;
use std::path::PathBuf;

fn path() -> PathBuf {
    let dir = gtk::glib::user_config_dir().join("scp-commander");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("prefs.conf")
}

fn load() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(path()) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

/// Non-empty string value for `key`, if present.
pub fn get(key: &str) -> Option<String> {
    load().get(key).cloned().filter(|v| !v.is_empty())
}

/// Integer value for `key`, or `default` if missing/unparseable.
pub fn get_int(key: &str, default: i64) -> i64 {
    get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Store `value` under `key` (empty string clears it).
pub fn set(key: &str, value: &str) {
    let mut map = load();
    if value.is_empty() {
        map.remove(key);
    } else {
        map.insert(key.to_string(), value.to_string());
    }
    let text: String = map.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let _ = std::fs::write(path(), text);
}
