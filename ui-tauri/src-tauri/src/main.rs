// SCP Commander — Tauri frontend. All transport logic (SFTP/FTP/FTPS/S3, the
// transfer engine, jump host, sync, …) lives in the shared `scp-core` crate;
// this layer exposes it to the Svelte UI as Tauri commands + events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sites;
mod transfers;

use std::path::Path;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use scp_core::types::{Auth, Credentials, Entry, Error, HostKeyPolicy, JumpHost, Protocol};
use scp_core::{connect, Transport};
use serde::{Deserialize, Serialize};
use tauri::State;

use transfers::TransferManager;

/// The live remote session (single session for now; tabs come later).
#[derive(Default)]
pub struct Session(pub Mutex<Option<Box<dyn Transport>>>);

#[derive(Deserialize)]
pub struct ConnectForm {
    protocol: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    /// "password" | "key" | "agent"
    #[serde(default)]
    auth_mode: String,
    #[serde(default)]
    key_path: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    path: String,
    // SFTP jump host (bastion / ProxyJump)
    #[serde(default)]
    use_jump: bool,
    #[serde(default)]
    jump_host: String,
    #[serde(default)]
    jump_port: u16,
    #[serde(default)]
    jump_user: String,
    #[serde(default)]
    jump_password: String,
    #[serde(default)]
    jump_auth_mode: String,
    #[serde(default)]
    jump_key_path: String,
}

#[derive(Serialize)]
pub struct EntryDto {
    name: String,
    is_dir: bool,
    is_symlink: bool,
    size: u64,
    mtime: Option<i64>,
    perms: Option<String>,
}

impl From<&Entry> for EntryDto {
    fn from(e: &Entry) -> Self {
        EntryDto {
            name: e.name.clone(),
            is_dir: e.is_dir,
            is_symlink: e.is_symlink,
            size: e.size,
            mtime: e.mtime,
            perms: e.perms.clone(),
        }
    }
}

/// Result of a connect attempt — drives the host-key trust prompt in the UI.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConnectResult {
    Connected { entries: Vec<EntryDto>, path: String },
    UnknownHostKey { fingerprint: String },
    HostKeyMismatch { fingerprint: String },
    Error { message: String },
}

fn proto_from_str(s: &str) -> Result<Protocol, String> {
    match s {
        "sftp" => Ok(Protocol::Sftp),
        "ftp" => Ok(Protocol::Ftp),
        "ftps" => Ok(Protocol::Ftps),
        "s3" => Ok(Protocol::S3),
        other => Err(format!("unknown protocol: {other}")),
    }
}

fn build_creds(form: &ConnectForm, host_key: HostKeyPolicy) -> Result<Credentials, String> {
    let protocol = proto_from_str(&form.protocol)?;
    let auth = if protocol == Protocol::Sftp {
        match form.auth_mode.as_str() {
            "key" => Auth::KeyFile {
                path: form.key_path.clone(),
                passphrase: (!form.password.is_empty()).then(|| form.password.clone()),
            },
            "agent" => Auth::Agent,
            _ => Auth::Password(form.password.clone()),
        }
    } else {
        Auth::Password(form.password.clone())
    };
    let mut creds = Credentials::basic(
        protocol, form.host.clone(), form.port, form.username.clone(), auth);
    creds.host_key = host_key;
    if protocol == Protocol::S3 {
        creds.bucket = (!form.bucket.is_empty()).then(|| form.bucket.clone());
        creds.region = (!form.region.is_empty()).then(|| form.region.clone());
    }
    if protocol == Protocol::Sftp && form.use_jump && !form.jump_host.is_empty() {
        let jpass = form.jump_password.clone();
        let jauth = match form.jump_auth_mode.as_str() {
            "key" => Auth::KeyFile {
                path: form.jump_key_path.clone(),
                passphrase: (!jpass.is_empty()).then(|| jpass.clone()),
            },
            "agent" => Auth::Agent,
            _ => Auth::Password(jpass),
        };
        creds.jump = Some(JumpHost {
            host: form.jump_host.clone(),
            port: if form.jump_port == 0 { 22 } else { form.jump_port },
            username: form.jump_user.clone(),
            auth: jauth,
            host_key: HostKeyPolicy::AcceptNew,
        });
    }
    Ok(creds)
}

/// Connect to the server. With no `trust_fingerprint`, uses Strict host-key
/// checking and returns `unknown_host_key`/`host_key_mismatch` so the UI can
/// prompt; the UI then re-calls with the approved fingerprint.
#[tauri::command]
fn connect_session(
    form: ConnectForm,
    trust_fingerprint: Option<String>,
    session: State<Session>,
    transfers: State<TransferManager>,
) -> ConnectResult {
    let host_key = match trust_fingerprint {
        Some(fp) => HostKeyPolicy::AcceptFingerprint(fp),
        None => HostKeyPolicy::Strict,
    };
    let creds = match build_creds(&form, host_key) {
        Ok(c) => c,
        Err(message) => return ConnectResult::Error { message },
    };
    let start = if form.path.is_empty() { "/".to_string() } else { form.path.clone() };
    match connect(&creds) {
        Ok(mut transport) => match transport.list_dir(&start) {
            Ok(entries) => {
                let dto = entries.iter().map(EntryDto::from).collect();
                *session.0.lock().unwrap() = Some(transport);
                // The transfer worker opens its own link from these creds.
                transfers.set_creds(creds);
                ConnectResult::Connected { entries: dto, path: start }
            }
            Err(e) => ConnectResult::Error { message: e.to_string() },
        },
        Err(Error::UnknownHostKey { fingerprint }) => {
            ConnectResult::UnknownHostKey { fingerprint }
        }
        Err(Error::HostKeyMismatch { fingerprint }) => {
            ConnectResult::HostKeyMismatch { fingerprint }
        }
        Err(e) => ConnectResult::Error { message: e.to_string() },
    }
}

/// List a remote directory on the live session.
#[tauri::command]
fn list_remote(path: String, session: State<Session>) -> Result<Vec<EntryDto>, String> {
    let mut guard = session.0.lock().unwrap();
    let transport = guard.as_mut().ok_or("not connected")?;
    let entries = transport.list_dir(&path).map_err(|e| e.to_string())?;
    Ok(entries.iter().map(EntryDto::from).collect())
}

#[tauri::command]
fn disconnect(session: State<Session>) {
    *session.0.lock().unwrap() = None;
}

// --- Remote file management -------------------------------------------------

#[tauri::command]
fn remote_mkdir(path: String, session: State<Session>) -> Result<(), String> {
    let mut g = session.0.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    t.mkdir(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn remote_delete(path: String, is_dir: bool, session: State<Session>) -> Result<(), String> {
    let mut g = session.0.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    if is_dir {
        scp_core::ops::remove_dir_all(t.as_mut(), &path).map_err(|e| e.to_string())
    } else {
        t.remove_file(&path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn remote_rename(from: String, to: String, session: State<Session>) -> Result<(), String> {
    let mut g = session.0.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    t.rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
fn remote_chmod(path: String, mode: u32, session: State<Session>) -> Result<(), String> {
    let mut g = session.0.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    t.set_permissions(&path, mode).map_err(|e| e.to_string())
}

// --- Local filesystem -------------------------------------------------------

#[tauri::command]
fn local_mkdir(path: String) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn local_delete(path: String, is_dir: bool) -> Result<(), String> {
    if is_dir {
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_file(&path)
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn local_rename(from: String, to: String) -> Result<(), String> {
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct LocalEntry {
    name: String,
    is_dir: bool,
    is_symlink: bool,
    size: u64,
    mtime: Option<i64>,
}

/// List a local directory (the left pane).
#[tauri::command]
fn list_local(path: String) -> Result<Vec<LocalEntry>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        out.push(LocalEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            is_symlink: meta.file_type().is_symlink(),
            size: meta.len(),
            mtime,
        });
    }
    Ok(out)
}

/// The user's home directory (initial local pane location).
#[tauri::command]
fn home_local() -> String {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string())
}

/// The parent of a local path (for the Up button), or the path itself at root.
#[tauri::command]
fn parent_local(path: String) -> String {
    Path::new(&path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
        .unwrap_or(path)
}

fn main() {
    tauri::Builder::default()
        .manage(Session::default())
        .manage(TransferManager::default())
        .invoke_handler(tauri::generate_handler![
            connect_session,
            list_remote,
            disconnect,
            list_local,
            home_local,
            parent_local,
            remote_mkdir,
            remote_delete,
            remote_rename,
            remote_chmod,
            local_mkdir,
            local_delete,
            local_rename,
            sites::list_sites,
            sites::save_site,
            sites::delete_site,
            transfers::enqueue,
            transfers::cancel_transfer,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SCP Commander");
}
