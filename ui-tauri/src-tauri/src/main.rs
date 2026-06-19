// SCP Commander — Tauri frontend. All transport logic (SFTP/FTP/FTPS/S3, the
// transfer engine, jump host, sync, …) lives in the shared `scp-core` crate;
// this layer exposes it to the Svelte UI as Tauri commands + events.
//
// Sessions: the app supports multiple concurrent server connections (tabs).
// Each lives in a `SessionState` (its own transport + transfer pool + sync
// engine) keyed by a numeric id in the `Sessions` registry; every remote
// command takes a `session_id`, and transfer/sync events carry it so the UI
// routes them to the right tab.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod extras;
mod sites;
mod sync;
mod transfers;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use scp_core::types::{Auth, Credentials, Entry, Error, HostKeyPolicy, JumpHost, Protocol};
use scp_core::{connect, Transport};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use transfers::TransferManager;

/// Open (or focus) the separate Transfers window — a second webview that
/// mirrors the transfer queue, like the native app's transfer window.
#[tauri::command]
fn open_transfers_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("transfers") {
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "transfers", WebviewUrl::App("index.html#transfers".into()))
        .title("Transfers")
        .inner_size(580.0, 420.0)
        .min_inner_size(420.0, 220.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// One live server connection (a tab): its browse transport plus the per-session
/// transfer pool and sync engine.
pub struct SessionState {
    pub transport: Mutex<Option<Box<dyn Transport>>>,
    pub transfers: TransferManager,
    pub sync: sync::SyncManager,
}

impl SessionState {
    fn new(id: u32) -> Arc<Self> {
        Arc::new(Self {
            transport: Mutex::new(None),
            transfers: TransferManager::new(id),
            sync: sync::SyncManager::new(id),
        })
    }
}

/// Registry of all open sessions, keyed by id.
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<u32, Arc<SessionState>>>,
    next_id: AtomicU32,
    /// Default parallel-transfer count applied to new sessions (from prefs).
    default_parallel: AtomicU32,
}

impl Sessions {
    pub fn get(&self, id: u32) -> Option<Arc<SessionState>> {
        self.map.lock().unwrap().get(&id).cloned()
    }
    /// Allocate a fresh session slot and return its id + state.
    fn create(&self) -> (u32, Arc<SessionState>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let s = SessionState::new(id);
        let p = self.default_parallel.load(Ordering::Relaxed);
        if p > 0 {
            s.transfers.set_max(p);
        }
        self.map.lock().unwrap().insert(id, s.clone());
        (id, s)
    }
    fn remove(&self, id: u32) {
        self.map.lock().unwrap().remove(&id);
    }
}

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
    uid: Option<u32>,
    gid: Option<u32>,
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
            uid: e.uid,
            gid: e.gid,
        }
    }
}

/// Result of a connect attempt — drives the host-key trust prompt in the UI.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConnectResult {
    Connected { session_id: u32, entries: Vec<EntryDto>, path: String },
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

/// Connect to a server, opening a NEW session on success and returning its id.
/// With no `trust_fingerprint`, uses Strict host-key checking and returns
/// `unknown_host_key`/`host_key_mismatch` so the UI can prompt; the UI then
/// re-calls with the approved fingerprint.
#[tauri::command]
fn connect_session(
    form: ConnectForm,
    trust_fingerprint: Option<String>,
    sessions: State<Sessions>,
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
                let (id, s) = sessions.create();
                *s.transport.lock().unwrap() = Some(transport);
                // The transfer worker and sync engine each open their own link.
                s.transfers.set_creds(creds.clone());
                s.sync.set_creds(creds);
                ConnectResult::Connected { session_id: id, entries: dto, path: start }
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

/// Resolve a session or return a "not connected" error string.
fn session_of(sessions: &State<Sessions>, id: u32) -> Result<Arc<SessionState>, String> {
    sessions.get(id).ok_or_else(|| "not connected".to_string())
}

/// List a remote directory on the given session.
#[tauri::command]
fn list_remote(session_id: u32, path: String, sessions: State<Sessions>) -> Result<Vec<EntryDto>, String> {
    let s = session_of(&sessions, session_id)?;
    let mut guard = s.transport.lock().unwrap();
    let transport = guard.as_mut().ok_or("not connected")?;
    let entries = transport.list_dir(&path).map_err(|e| e.to_string())?;
    Ok(entries.iter().map(EntryDto::from).collect())
}

#[tauri::command]
fn disconnect(session_id: u32, sessions: State<Sessions>) {
    sessions.remove(session_id);
}

// --- Remote file management -------------------------------------------------

#[tauri::command]
fn remote_mkdir(session_id: u32, path: String, sessions: State<Sessions>) -> Result<(), String> {
    let s = session_of(&sessions, session_id)?;
    let mut g = s.transport.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    t.mkdir(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn remote_delete(session_id: u32, path: String, is_dir: bool, sessions: State<Sessions>) -> Result<(), String> {
    let s = session_of(&sessions, session_id)?;
    let mut g = s.transport.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    if is_dir {
        scp_core::ops::remove_dir_all(t.as_mut(), &path).map_err(|e| e.to_string())
    } else {
        t.remove_file(&path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn remote_rename(session_id: u32, from: String, to: String, sessions: State<Sessions>) -> Result<(), String> {
    let s = session_of(&sessions, session_id)?;
    let mut g = s.transport.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    t.rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
fn remote_chmod(session_id: u32, path: String, mode: u32, sessions: State<Sessions>) -> Result<(), String> {
    let s = session_of(&sessions, session_id)?;
    let mut g = s.transport.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    t.set_permissions(&path, mode).map_err(|e| e.to_string())
}

// --- Transfers (per session) ------------------------------------------------

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn enqueue(
    session_id: u32,
    upload: bool,
    is_dir: bool,
    name: String,
    local: String,
    remote: String,
    overwrite: Option<i32>,
    app: AppHandle,
    sessions: State<Sessions>,
) -> Result<u64, String> {
    let s = session_of(&sessions, session_id)?;
    s.transfers.enqueue_job(upload, is_dir, name, local, remote, overwrite, app)
}

#[tauri::command]
fn cancel_transfer(session_id: u32, id: u64, sessions: State<Sessions>) {
    if let Some(s) = sessions.get(session_id) {
        s.transfers.cancel(id);
    }
}

/// Set the max concurrent transfers for every session and remember it as the
/// default for sessions opened later.
#[tauri::command]
fn set_max_parallel(n: u32, sessions: State<Sessions>) {
    sessions.default_parallel.store(n.max(1), Ordering::Relaxed);
    for s in sessions.map.lock().unwrap().values() {
        s.transfers.set_max(n);
    }
}

// --- Sync (per session) -----------------------------------------------------

#[tauri::command]
fn sync_plan(
    session_id: u32,
    local: String,
    remote: String,
    direction: String,
    mirror: bool,
    sessions: State<Sessions>,
) -> Result<sync::SyncPlanDto, String> {
    let s = session_of(&sessions, session_id)?;
    s.sync.plan(local, remote, direction, mirror)
}

#[tauri::command]
fn sync_run(
    session_id: u32,
    local: String,
    remote: String,
    direction: String,
    mirror: bool,
    app: AppHandle,
    sessions: State<Sessions>,
) -> Result<(), String> {
    let s = session_of(&sessions, session_id)?;
    s.sync.run(local, remote, direction, mirror, app)
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
        .manage(Sessions::default())
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
            sync_plan,
            sync_run,
            extras::remote_exec,
            extras::reveal_path,
            extras::remote_copy,
            extras::local_read_text,
            extras::remote_read_text,
            extras::known_hosts_list,
            extras::known_hosts_remove,
            extras::load_prefs,
            extras::save_prefs,
            enqueue,
            cancel_transfer,
            set_max_parallel,
            open_transfers_window,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SCP Commander");
}
