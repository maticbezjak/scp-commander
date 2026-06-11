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

// --- Interchange format (shared with the macOS app) --------------------------

/// Versioned, human-readable export format. Both the macOS and Ubuntu apps
/// read and write this, so sites can move between machines and platforms.
/// Passwords are intentionally not part of it — they stay in the keyring.
#[derive(Serialize, Deserialize)]
struct SiteExportFile {
    scp_commander_sites: u32,
    sites: Vec<SiteExport>,
}

#[derive(Serialize, Deserialize)]
struct SiteExport {
    name: String,
    protocol: String, // sftp | ftp | ftps | s3
    host: String,
    port: String,
    user: String,
    auth: String, // password | key | agent
    #[serde(default)]
    key_path: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    region: String,
}

impl SiteExport {
    fn from_site(site: &Site) -> Self {
        Self {
            name: site.name.clone(),
            protocol: ["sftp", "ftp", "ftps", "s3"][site.proto as usize % 4].to_string(),
            host: site.host.clone(),
            port: site.port.clone(),
            user: site.user.clone(),
            auth: ["password", "key", "agent"][site.auth as usize % 3].to_string(),
            key_path: site.key_path.clone(),
            bucket: site.bucket.clone(),
            region: site.region.clone(),
        }
    }

    fn into_site(self) -> Site {
        let proto = match self.protocol.as_str() {
            "ftp" => 1,
            "ftps" => 2,
            "s3" => 3,
            _ => 0,
        };
        let auth = match self.auth.as_str() {
            "key" => 1,
            "agent" => 2,
            _ => 0,
        };
        Site {
            name: self.name,
            proto,
            host: self.host,
            port: self.port,
            user: self.user,
            auth,
            key_path: self.key_path,
            bucket: self.bucket,
            region: self.region,
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

    /// Serialize all sites to the cross-platform interchange format.
    pub fn export_interchange(&self) -> Result<String, String> {
        let file = SiteExportFile {
            scp_commander_sites: 1,
            sites: self.sites.iter().map(SiteExport::from_site).collect(),
        };
        serde_json::to_string_pretty(&file).map_err(|e| e.to_string())
    }

    /// Merge sites from interchange data (same-named sites are replaced).
    /// Returns the number of sites in the file.
    pub fn import_interchange(&mut self, data: &str) -> Result<usize, String> {
        let file: SiteExportFile = serde_json::from_str(data).map_err(|e| e.to_string())?;
        let count = file.sites.len();
        for exported in file.sites {
            self.add(exported.into_site());
        }
        Ok(count)
    }

    /// Import sessions from a WinSCP.ini file ([Sessions\Name] blocks).
    /// Session names are URL-encoded and may contain "/" folders, which map
    /// straight onto our folder grouping. Returns the number imported.
    pub fn import_winscp_ini(&mut self, ini: &str) -> Result<usize, String> {
        let mut count = 0;
        let mut current: Option<WinScpSession> = None;
        for raw in ini.lines() {
            let line = raw.trim();
            if line.starts_with('[') {
                if let Some(done) = current.take() {
                    if self.flush_winscp(done) {
                        count += 1;
                    }
                }
                if let Some(name) = line
                    .strip_prefix("[Sessions\\")
                    .and_then(|l| l.strip_suffix(']'))
                {
                    // "Default%20Settings" holds defaults, not a real site.
                    if name != "Default%20Settings" {
                        current = Some(WinScpSession {
                            name: url_decode(name),
                            ..Default::default()
                        });
                    }
                }
                continue;
            }
            let (Some(session), Some((key, value))) = (current.as_mut(), line.split_once('='))
            else {
                continue;
            };
            match key {
                "HostName" => session.host = value.to_string(),
                "PortNumber" => session.port = Some(value.to_string()),
                "UserName" => session.user = value.to_string(),
                "FSProtocol" => session.fs_protocol = value.parse().unwrap_or(0),
                "FtpSecure" => session.ftp_secure = value != "0",
                "PublicKeyFile" => session.key_path = url_decode(value),
                _ => {}
            }
        }
        if let Some(done) = current.take() {
            if self.flush_winscp(done) {
                count += 1;
            }
        }
        if count == 0 {
            return Err("no [Sessions\\…] entries found — is this a WinSCP.ini?".into());
        }
        Ok(count)
    }

    fn flush_winscp(&mut self, s: WinScpSession) -> bool {
        if s.host.is_empty() || s.name.is_empty() {
            return false;
        }
        // WinSCP FSProtocol: 5 = FTP (FtpSecure upgrades to FTPS), 7 = S3,
        // everything else (0/1/2…) is the SSH family → SFTP here.
        let proto = match s.fs_protocol {
            5 => {
                if s.ftp_secure {
                    2
                } else {
                    1
                }
            }
            7 => 3,
            _ => 0,
        };
        let port = s.port.unwrap_or_else(|| {
            match proto {
                1 | 2 => "21",
                3 => "443",
                _ => "22",
            }
            .to_string()
        });
        let auth = if !s.key_path.is_empty() && proto == 0 { 1 } else { 0 };
        self.add(Site {
            name: s.name,
            proto,
            host: s.host,
            port,
            user: s.user,
            auth,
            key_path: s.key_path,
            bucket: String::new(),
            region: String::new(),
        });
        true
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

/// Accumulator for one [Sessions\…] block while parsing WinSCP.ini.
#[derive(Default)]
struct WinScpSession {
    name: String,
    host: String,
    port: Option<String>,
    user: String,
    fs_protocol: u32,
    ftp_secure: bool,
    key_path: String,
}

/// Decode WinSCP's %XX escapes (session names, key paths).
///
/// Works purely on bytes: slicing the &str at byte offsets panicked when a
/// `%` was followed by part of a multibyte UTF-8 character (a crafted .ini
/// could crash the app). Embedded NULs are stripped — they'd otherwise break
/// every later CString bridge.
fn url_decode(input: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                let value = (h << 4) | l;
                if value != 0 {
                    out.push(value);
                }
                i += 3;
                continue;
            }
        }
        if bytes[i] != 0 {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SitesStore {
        SitesStore { sites: Vec::new(), file: PathBuf::from("/dev/null") }
    }

    /// Exactly what the macOS app writes — the cross-platform contract.
    #[test]
    fn imports_macos_export() {
        let json = r#"{
            "scp_commander_sites": 1,
            "sites": [
                {
                    "auth": "key",
                    "bucket": "",
                    "host": "example.com",
                    "key_path": "/home/u/.ssh/id_ed25519",
                    "name": "Work/web1",
                    "port": "2222",
                    "protocol": "sftp",
                    "region": "",
                    "user": "deploy"
                }
            ]
        }"#;
        let mut s = store();
        assert_eq!(s.import_interchange(json).unwrap(), 1);
        let site = &s.sites[0];
        assert_eq!(site.proto, 0);
        assert_eq!(site.auth, 1);
        assert_eq!(site.host, "example.com");
        assert_eq!(site.folder(), Some("Work"));
        assert_eq!(site.display_name(), "web1");
    }

    #[test]
    fn imports_winscp_ini() {
        let ini = "\
[Configuration]\nRandomSeedFile=x\n\
[Sessions\\Default%20Settings]\nHostName=ignored\n\
[Sessions\\My%20Server]\nHostName=example.com\nUserName=root\nPortNumber=2222\n\
[Sessions\\Work/web1]\nHostName=web1.example\nUserName=deploy\nFSProtocol=5\nFtpSecure=1\n\
[Sessions\\Keyed]\nHostName=keyed.example\nUserName=ops\nPublicKeyFile=C:%5Ckeys%5Cid.ppk\n";
        let mut s = store();
        assert_eq!(s.import_winscp_ini(ini).unwrap(), 3);
        let by_name = |n: &str| s.sites.iter().find(|x| x.name == n).unwrap();
        let server = by_name("My Server");
        assert_eq!(server.proto, 0);
        assert_eq!(server.port, "2222");
        let web1 = by_name("Work/web1");
        assert_eq!(web1.proto, 2); // FTP + FtpSecure → FTPS
        assert_eq!(web1.port, "21");
        assert_eq!(web1.folder(), Some("Work"));
        let keyed = by_name("Keyed");
        assert_eq!(keyed.auth, 1);
        assert_eq!(keyed.key_path, "C:\\keys\\id.ppk");
    }

    #[test]
    fn url_decode_is_panic_proof() {
        // Regression: "%aé" used to panic (str slice inside a multibyte char).
        assert_eq!(url_decode("%a\u{e9}"), "%a\u{e9}");
        assert_eq!(url_decode("My%20Site"), "My Site");
        assert_eq!(url_decode("a%00b"), "ab"); // NULs stripped
        assert_eq!(url_decode("%"), "%");
        assert_eq!(url_decode("%zz"), "%zz");
    }

    #[test]
    fn export_import_round_trip() {
        let mut s = store();
        s.sites.push(Site {
            name: "S3/backups".into(),
            proto: 3,
            host: "minio.local".into(),
            port: "443".into(),
            user: "AKIA123".into(),
            auth: 0,
            key_path: String::new(),
            bucket: "backups".into(),
            region: "us-east-1".into(),
        });
        let json = s.export_interchange().unwrap();
        let mut other = store();
        assert_eq!(other.import_interchange(&json).unwrap(), 1);
        assert_eq!(other.sites[0].proto, 3);
        assert_eq!(other.sites[0].bucket, "backups");
        // Importing again replaces rather than duplicating.
        assert_eq!(other.import_interchange(&json).unwrap(), 1);
        assert_eq!(other.sites.len(), 1);
    }
}
