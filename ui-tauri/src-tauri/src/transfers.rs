// Transfers. Phase 1 keeps simple blocking download/upload with progress
// events; Phase 2 grows this into a queued, cancellable, pooled manager.

use std::path::Path;

use serde::Serialize;
use tauri::{Emitter, State};

use crate::Session;

/// Placeholder for the upcoming transfer queue (cancel flags, pool, etc.).
#[derive(Default)]
pub struct TransferManager;

#[derive(Clone, Serialize)]
struct Progress {
    name: String,
    done: u64,
    total: u64,
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Download a remote file to a local path, emitting "xfer-progress" events.
#[tauri::command]
pub fn download(
    remote: String,
    local: String,
    app: tauri::AppHandle,
    session: State<Session>,
) -> Result<u64, String> {
    let name = basename(&remote);
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

/// Upload a local file to a remote path, emitting "xfer-progress" events.
#[tauri::command]
pub fn upload(
    local: String,
    remote: String,
    app: tauri::AppHandle,
    session: State<Session>,
) -> Result<u64, String> {
    let name = basename(&remote);
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
