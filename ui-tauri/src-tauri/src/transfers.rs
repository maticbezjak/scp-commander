// Transfer queue. Runs on its own connection (a dedicated worker thread) so the
// browsing session stays responsive, processes jobs sequentially, and reports
// per-transfer progress / completion as Tauri events. Cancellation is a flag
// the progress callback observes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use scp_core::ops::{self, Filter, OverwritePolicy, XferEvent};
use scp_core::types::{Credentials, Error};
use scp_core::{connect, Transport};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

struct Job {
    id: u64,
    /// true = upload (local→remote), false = download (remote→local)
    upload: bool,
    is_dir: bool,
    name: String,
    local: String,
    remote: String,
    /// Folder overwrite policy (0 overwrite, 1 skip, 2 only-newer).
    overwrite: i32,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct TransferManager {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    creds: Mutex<Option<Credentials>>,
    sender: Mutex<Option<Sender<Job>>>,
    cancels: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    next_id: AtomicU64,
    /// Set when the browsing session (re)connects, so the worker rebuilds its
    /// own connection from the latest credentials before the next job.
    reconnect: AtomicBool,
}

impl TransferManager {
    /// Remember the credentials of a freshly-connected session for the worker.
    pub fn set_creds(&self, creds: Credentials) {
        *self.inner.creds.lock().unwrap() = Some(creds);
        self.inner.reconnect.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Evt {
    Started { id: u64, name: String, upload: bool, total: u64 },
    Progress { id: u64, done: u64, total: u64 },
    Done { id: u64, name: String, upload: bool },
    Failed { id: u64, message: String },
    Cancelled { id: u64 },
}

fn emit(app: &AppHandle, e: Evt) {
    let _ = app.emit("xfer", e);
}

fn is_transient(e: &Error) -> bool {
    matches!(e, Error::Connect(_) | Error::Io(_))
}

/// Queue a transfer; returns its id. `upload` = local→remote.
#[tauri::command]
pub fn enqueue(
    upload: bool,
    is_dir: bool,
    name: String,
    local: String,
    remote: String,
    #[allow(non_snake_case)] overwrite: Option<i32>,
    app: AppHandle,
    mgr: State<TransferManager>,
) -> Result<u64, String> {
    let inner = mgr.inner.clone();
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let cancel = Arc::new(AtomicBool::new(false));
    inner.cancels.lock().unwrap().insert(id, cancel.clone());

    // Lazily spawn the worker on first use.
    let mut sender = inner.sender.lock().unwrap();
    if sender.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        *sender = Some(tx);
        let worker_inner = inner.clone();
        let worker_app = app.clone();
        std::thread::spawn(move || worker(worker_inner, rx, worker_app));
    }
    let job = Job {
        id, upload, is_dir, name, local, remote,
        overwrite: overwrite.unwrap_or(0),
        cancel,
    };
    sender
        .as_ref()
        .unwrap()
        .send(job)
        .map_err(|_| "transfer worker stopped".to_string())?;
    Ok(id)
}

/// Cancel an in-flight or queued transfer.
#[tauri::command]
pub fn cancel_transfer(id: u64, mgr: State<TransferManager>) {
    if let Some(c) = mgr.inner.cancels.lock().unwrap().get(&id) {
        c.store(true, Ordering::Relaxed);
    }
}

fn worker(inner: Arc<Inner>, rx: Receiver<Job>, app: AppHandle) {
    let mut transport: Option<Box<dyn Transport>> = None;
    while let Ok(job) = rx.recv() {
        if job.cancel.load(Ordering::Relaxed) {
            emit(&app, Evt::Cancelled { id: job.id });
            inner.cancels.lock().unwrap().remove(&job.id);
            continue;
        }
        // (Re)connect the transfer link if needed.
        if transport.is_none() || inner.reconnect.swap(false, Ordering::Relaxed) {
            let creds = inner.creds.lock().unwrap().clone();
            transport = creds.and_then(|c| connect(&c).ok());
        }
        let Some(t) = transport.as_mut() else {
            emit(&app, Evt::Failed { id: job.id, message: "not connected".into() });
            inner.cancels.lock().unwrap().remove(&job.id);
            continue;
        };

        emit(&app, Evt::Started {
            id: job.id,
            name: job.name.clone(),
            upload: job.upload,
            total: 0,
        });
        let result = run_job(t.as_mut(), &job, &app);
        match result {
            Ok(_) => emit(&app, Evt::Done { id: job.id, name: job.name.clone(), upload: job.upload }),
            Err(Error::Cancelled) => emit(&app, Evt::Cancelled { id: job.id }),
            Err(e) => {
                if is_transient(&e) {
                    transport = None; // force a reconnect on the next job
                }
                emit(&app, Evt::Failed { id: job.id, message: e.to_string() });
            }
        }
        inner.cancels.lock().unwrap().remove(&job.id);
    }
}

fn run_job(t: &mut dyn Transport, job: &Job, app: &AppHandle) -> scp_core::Result<u64> {
    let id = job.id;
    let cancel = job.cancel.clone();
    let app = app.clone();
    if job.is_dir {
        let mut cb = |ev: XferEvent| -> bool {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            if let XferEvent::Bytes { done, total } = ev {
                emit(&app, Evt::Progress { id, done, total });
            }
            true
        };
        let filter = Filter::empty();
        let policy = OverwritePolicy::from_code(job.overwrite);
        if job.upload {
            ops::upload_dir_opts(t, Path::new(&job.local), &job.remote, &filter, policy, &mut cb)
        } else {
            ops::download_dir_opts(t, &job.remote, Path::new(&job.local), &filter, policy, &mut cb)
        }
    } else {
        let mut progress = |done: u64, total: u64| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            emit(&app, Evt::Progress { id, done, total });
            true
        };
        if job.upload {
            t.upload_progress(Path::new(&job.local), &job.remote, &mut progress)
        } else {
            t.download_progress(&job.remote, Path::new(&job.local), &mut progress)
        }
    }
}
