//! Workspace persistence: the open tabs (connection settings + current
//! directories) are saved on quit and restored on the next launch, WinSCP's
//! "save workspace" behavior. Passwords stay in the keyring.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct TabState {
    pub proto: u32,
    pub host: String,
    pub port: String,
    pub user: String,
    pub auth: u32,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub region: String,
    pub remote_path: String,
    pub local_path: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Workspace {
    pub tabs: Vec<TabState>,
}

fn path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("scp-commander")
        .join("workspace.json")
}

pub fn save(ws: &Workspace) {
    if let Ok(data) = serde_json::to_vec_pretty(ws) {
        let _ = fs::create_dir_all(path().parent().unwrap());
        let _ = fs::write(path(), data);
    }
}

pub fn load() -> Workspace {
    fs::read(path())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}
