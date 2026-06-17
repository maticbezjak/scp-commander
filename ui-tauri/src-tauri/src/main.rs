// SCP Commander — Tauri frontend. The heavy lifting (SFTP/FTP/FTPS/S3, the
// transfer engine, jump host, sync, …) all lives in the shared `scp-core`
// crate; this layer just exposes it to the web UI as Tauri commands + events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

use std::path::Path;

use scp_core::types::{Auth, Credentials, Entry, HostKeyPolicy, Protocol};
use scp_core::{connect, Transport};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

/// The one live session for the spike (multi-tab comes later).
#[derive(Default)]
struct Session(Mutex<Option<Box<dyn Transport>>>);

#[derive(Deserialize)]
struct ConnectForm {
    protocol: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    #[serde(default)]
    path: String,
}

#[derive(Serialize)]
struct EntryDto {
    name: String,
    is_dir: bool,
    is_symlink: bool,
    size: u64,
    mtime: Option<i64>,
}

impl From<&Entry> for EntryDto {
    fn from(e: &Entry) -> Self {
        EntryDto {
            name: e.name.clone(),
            is_dir: e.is_dir,
            is_symlink: e.is_symlink,
            size: e.size,
            mtime: e.mtime,
        }
    }
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

/// Connect with the form's credentials and return the initial directory listing.
#[tauri::command]
fn connect_session(form: ConnectForm, session: State<Session>) -> Result<Vec<EntryDto>, String> {
    let protocol = proto_from_str(&form.protocol)?;
    let creds = Credentials {
        protocol,
        host: form.host,
        port: form.port,
        username: form.username,
        auth: Auth::Password(form.password),
        bucket: None,
        region: None,
        host_key: HostKeyPolicy::AcceptNew, // TOFU for the spike
        jump: None,
    };
    let start = if form.path.is_empty() { "/".to_string() } else { form.path };
    let mut transport = connect(&creds).map_err(|e| e.to_string())?;
    let entries = transport.list_dir(&start).map_err(|e| e.to_string())?;
    let dto = entries.iter().map(EntryDto::from).collect();
    *session.0.lock().unwrap() = Some(transport);
    Ok(dto)
}

/// List a remote directory on the live session.
#[tauri::command]
fn list_dir(path: String, session: State<Session>) -> Result<Vec<EntryDto>, String> {
    let mut guard = session.0.lock().unwrap();
    let transport = guard.as_mut().ok_or("not connected")?;
    let entries = transport.list_dir(&path).map_err(|e| e.to_string())?;
    Ok(entries.iter().map(EntryDto::from).collect())
}

#[derive(Clone, Serialize)]
struct Progress {
    name: String,
    done: u64,
    total: u64,
}

/// Download a remote file to a local path, emitting "xfer-progress" events as
/// bytes flow. Returns the total bytes written.
#[tauri::command]
fn download(
    remote: String,
    local: String,
    app: tauri::AppHandle,
    session: State<Session>,
) -> Result<u64, String> {
    let name = remote.rsplit('/').next().unwrap_or(&remote).to_string();
    let mut guard = session.0.lock().unwrap();
    let transport = guard.as_mut().ok_or("not connected")?;
    let mut progress = |done: u64, total: u64| {
        let _ = app.emit("xfer-progress", Progress { name: name.clone(), done, total });
        true
    };
    transport
        .download_progress(&remote, Path::new(&local), &mut progress)
        .map_err(|e| e.to_string())
}

/// Upload a local file to a remote path, emitting progress events.
#[tauri::command]
fn upload(
    local: String,
    remote: String,
    app: tauri::AppHandle,
    session: State<Session>,
) -> Result<u64, String> {
    let name = remote.rsplit('/').next().unwrap_or(&remote).to_string();
    let mut guard = session.0.lock().unwrap();
    let transport = guard.as_mut().ok_or("not connected")?;
    let mut progress = |done: u64, total: u64| {
        let _ = app.emit("xfer-progress", Progress { name: name.clone(), done, total });
        true
    };
    transport
        .upload_progress(Path::new(&local), &remote, &mut progress)
        .map_err(|e| e.to_string())
}

/// The OS temp dir — where the spike drops downloads.
#[tauri::command]
fn temp_dir() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

#[tauri::command]
fn disconnect(session: State<Session>) {
    *session.0.lock().unwrap() = None;
}

fn main() {
    tauri::Builder::default()
        .manage(Session::default())
        .invoke_handler(tauri::generate_handler![
            connect_session,
            list_dir,
            download,
            upload,
            temp_dir,
            disconnect
        ])
        .run(tauri::generate_context!())
        .expect("error while running SCP Commander");
}
