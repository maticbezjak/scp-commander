//! Saved connection sites, persisted as JSON under the user config dir
//! (`~/.config/scp-commander/sites.json` on Linux). Passwords are not stored —
//! the user enters them at connect time, same policy as the macOS app.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Site {
    pub name: String,
    /// Index into the protocol dropdown: 0=SFTP, 1=FTP, 2=FTPS, 3=S3.
    pub proto: u32,
    pub host: String,
    pub port: String,
    pub user: String,
}

pub struct SitesStore {
    pub sites: Vec<Site>,
    file: PathBuf,
}

impl SitesStore {
    pub fn load() -> Self {
        let dir = gtk::glib::user_config_dir().join("scp-commander");
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("sites.json");
        let sites = fs::read(&file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self { sites, file }
    }

    pub fn add(&mut self, site: Site) {
        // Replace a same-named entry rather than duplicating.
        if let Some(existing) = self.sites.iter_mut().find(|s| s.name == site.name) {
            *existing = site;
        } else {
            self.sites.push(site);
        }
        self.save();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.sites.len() {
            self.sites.remove(index);
            self.save();
        }
    }

    fn save(&self) {
        if let Ok(data) = serde_json::to_vec_pretty(&self.sites) {
            let _ = fs::write(&self.file, data);
        }
    }
}
