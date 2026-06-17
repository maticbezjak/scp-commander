// Transfer queue. Runs as a pool of up to N concurrent worker threads (N is
// configurable at runtime via `set_max_parallel`, default 2). Each worker owns
// its OWN connection, so transfers truly run in parallel on separate links and
// the browsing session stays responsive. Jobs are pulled from a single shared
// queue (an `mpsc` receiver behind a mutex): a worker locks the receiver only
// long enough to grab the next job, releases it immediately, then processes the
// job on its own connection. Per-transfer progress / completion is reported as
// Tauri events. Cancellation is a flag the progress callback observes.

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

/// Default worker count when no explicit target has been set.
const DEFAULT_PARALLEL: u64 = 2;

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

/// Shared, lockable receiver so multiple workers can pull from one queue.
type SharedRx = Arc<Mutex<Receiver<Job>>>;

#[derive(Default)]
pub struct TransferManager {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    creds: Mutex<Option<Credentials>>,
    sender: Mutex<Option<Sender<Job>>>,
    /// The shared receiver, created lazily alongside the sender.
    receiver: Mutex<Option<SharedRx>>,
    cancels: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    next_id: AtomicU64,
    /// Bumped whenever the browsing session (re)connects. Each worker remembers
    /// the generation it last connected with and rebuilds its own connection
    /// from the latest credentials when the counter moves ahead of it.
    creds_gen: AtomicU64,
    /// Desired number of live workers (0 means "use the default").
    target: AtomicU64,
    /// Current number of live workers.
    workers: AtomicU64,
}

impl Inner {
    /// Effective worker target, treating 0 as the default.
    fn effective_target(&self) -> u64 {
        let t = self.target.load(Ordering::Relaxed);
        if t == 0 {
            DEFAULT_PARALLEL
        } else {
            t
        }
    }
}

impl TransferManager {
    /// Remember the credentials of a freshly-connected session for the workers.
    /// Bumping the generation makes every worker reconnect before its next job.
    pub fn set_creds(&self, creds: Credentials) {
        *self.inner.creds.lock().unwrap() = Some(creds);
        self.inner.creds_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the desired number of concurrent transfer workers (at least 1).
    pub fn set_max(&self, n: u32) {
        self.inner
            .target
            .store(n.max(1) as u64, Ordering::Relaxed);
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

    // Lazily create the shared queue on first use.
    let mut sender = inner.sender.lock().unwrap();
    if sender.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        *sender = Some(tx);
        *inner.receiver.lock().unwrap() = Some(Arc::new(Mutex::new(rx)));
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
    drop(sender);

    // Spawn workers up to the current target. We bump `workers` here (before the
    // spawn) so the count this loop reads stays accurate across concurrent
    // enqueue calls.
    let shared_rx = inner
        .receiver
        .lock()
        .unwrap()
        .as_ref()
        .expect("receiver created above")
        .clone();
    while inner.workers.load(Ordering::Relaxed) < inner.effective_target() {
        inner.workers.fetch_add(1, Ordering::Relaxed);
        let worker_inner = inner.clone();
        let worker_app = app.clone();
        let worker_rx = shared_rx.clone();
        std::thread::spawn(move || worker(worker_inner, worker_rx, worker_app));
    }

    Ok(id)
}

/// Cancel an in-flight or queued transfer.
#[tauri::command]
pub fn cancel_transfer(id: u64, mgr: State<TransferManager>) {
    if let Some(c) = mgr.inner.cancels.lock().unwrap().get(&id) {
        c.store(true, Ordering::Relaxed);
    }
}

/// Set the maximum number of concurrent transfers.
#[tauri::command]
pub fn set_max_parallel(n: u32, mgr: State<TransferManager>) {
    mgr.set_max(n);
}

fn worker(inner: Arc<Inner>, rx: SharedRx, app: AppHandle) {
    // The generation this worker's `transport` was built for. Starts at 0 so the
    // first job always (re)connects from the latest credentials.
    let mut my_gen = 0u64;
    let mut transport: Option<Box<dyn Transport>> = None;

    loop {
        // Shrink the pool if we're over target. Check BEFORE locking/recv so no
        // job is ever pulled and then dropped. Never let the pool fall below 1.
        {
            let workers = inner.workers.load(Ordering::Relaxed);
            if workers > inner.effective_target() && workers > 1 {
                inner.workers.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        }

        // Grab the next job, holding the receiver lock only for the recv.
        let job = {
            let guard = rx.lock().unwrap();
            match guard.recv() {
                Ok(job) => job,
                Err(_) => {
                    // Channel closed: no more jobs will ever arrive.
                    inner.workers.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        };

        if job.cancel.load(Ordering::Relaxed) {
            emit(&app, Evt::Cancelled { id: job.id });
            inner.cancels.lock().unwrap().remove(&job.id);
            continue;
        }

        // (Re)connect this worker's own link if needed: no transport yet, or the
        // credentials generation has moved past the one we built for.
        let latest_gen = inner.creds_gen.load(Ordering::Relaxed);
        if transport.is_none() || latest_gen != my_gen {
            let creds = inner.creds.lock().unwrap().clone();
            transport = creds.and_then(|c| connect(&c).ok());
            my_gen = latest_gen;
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
