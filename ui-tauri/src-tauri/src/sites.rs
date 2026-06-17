// Saved connection profiles, persisted as JSON under the config dir. Passwords
// are intentionally not stored here (keychain integration is a later step).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Site {
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub use_jump: bool,
    #[serde(default)]
    pub jump_host: String,
    #[serde(default)]
    pub jump_port: u16,
    #[serde(default)]
    pub jump_user: String,
    #[serde(default)]
    pub jump_auth_mode: String,
    #[serde(default)]
    pub jump_key_path: String,
}

fn sites_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config/scp-commander/sites-tauri.json"))
}

fn read() -> Vec<Site> {
    sites_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write(sites: &[Site]) -> Result<(), String> {
    let path = sites_path().ok_or("cannot locate config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(sites).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_sites() -> Vec<Site> {
    read()
}

/// Add or replace a site (matched by name).
#[tauri::command]
pub fn save_site(site: Site) -> Result<(), String> {
    let mut sites = read();
    if let Some(existing) = sites.iter_mut().find(|s| s.name == site.name) {
        *existing = site;
    } else {
        sites.push(site);
    }
    sites.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    write(&sites)
}

#[tauri::command]
pub fn delete_site(name: String) -> Result<(), String> {
    let mut sites = read();
    sites.retain(|s| s.name != name);
    write(&sites)
}
