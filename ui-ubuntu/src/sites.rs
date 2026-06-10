//! Saved connection sites, persisted as JSON under the user config dir
//! (`~/.config/scp-commander/sites.json` on Linux). A site stores the full
//! session, WinSCP-style: protocol, endpoint, auth method, key file, and S3
//! bucket/region. Passwords go to the Secret Service only when the user opts
//! in at save time.
//!
//! Site names may contain "/" to group sites into folders, exactly like
//! WinSCP: "Work/web1" shows as "web1" inside a "Work" folder.

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
    /// Index into the auth dropdown: 0=password, 1=key file, 2=agent.
    #[serde(default)]
    pub auth: u32,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub region: String,
}

impl Site {
    /// WinSCP-style folder: the part before the first "/", if any.
    pub fn folder(&self) -> Option<&str> {
        match self.name.split_once('/') {
            Some((f, _)) if !f.is_empty() => Some(f),
            _ => None,
        }
    }

    /// Name shown in the list (folder prefix stripped).
    pub fn display_name(&self) -> &str {
        match self.name.split_once('/') {
            Some((f, rest)) if !f.is_empty() => rest,
            _ => &self.name,
        }
    }
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
        let mut store = Self { sites, file };
        store.sort();
        store
    }

    pub fn add(&mut self, site: Site) {
        // Replace a same-named entry rather than duplicating.
        if let Some(existing) = self.sites.iter_mut().find(|s| s.name == site.name) {
            *existing = site;
        } else {
            self.sites.push(site);
        }
        self.sort();
        self.save();
    }

    pub fn rename(&mut self, index: usize, new_name: &str) {
        if new_name.is_empty() {
            return;
        }
        if let Some(site) = self.sites.get_mut(index) {
            site.name = new_name.to_string();
            self.sort();
            self.save();
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.sites.len() {
            self.sites.remove(index);
            self.save();
        }
    }

    /// Sorted so folder groups sit together: ungrouped sites first, then
    /// folders alphabetically, each group alphabetical by display name.
    fn sort(&mut self) {
        self.sites.sort_by(|a, b| {
            let key = |s: &Site| {
                (
                    s.folder().is_some(),
                    s.folder().unwrap_or("").to_lowercase(),
                    s.display_name().to_lowercase(),
                )
            };
            key(a).cmp(&key(b))
        });
    }

    fn save(&self) {
        if let Ok(data) = serde_json::to_vec_pretty(&self.sites) {
            let _ = fs::write(&self.file, data);
        }
    }
}
