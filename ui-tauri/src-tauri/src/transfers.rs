// Per-session transfer queue: a pool of up to N concurrent workers, each with
// its own connection, pulling from one shared queue. Per-transfer progress is
// reported as "xfer" events tagged with the owning session id so the UI can
// route them to the right tab. Cancellation is a flag the progress callback
// observes. The Tauri command wrappers live in main.rs (they resolve the
// session first); this module is a plain manager.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use scp_core::ops::{self, Filter, OverwritePolicy, XferEvent};
use scp_core::types::{Credentials, Error};
use scp_core::{connect, Transport};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

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
    /// Resume a partial transfer instead of starting fresh (set on Retry).
    resume: bool,
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
    /// The session this manager belongs to; stamped onto every event.
    sid: u32,
    creds: Mutex<Option<Credentials>>,
    sender: Mutex<Option<Sender<Job>>>,
    receiver: Mutex<Option<SharedRx>>,
    cancels: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    next_id: AtomicU64,
    /// Bumped whenever the session (re)connects; each worker rebuilds its own
    /// connection when the counter moves ahead of the generation it built for.
    creds_gen: AtomicU64,
    /// Desired number of live workers (0 means "use the default").
    target: AtomicU64,
    /// Current number of live workers.
    workers: AtomicU64,
    /// Transfer speed cap in KiB/s (0 = unlimited).
    speed_kbs: AtomicU64,
    /// When true, in-flight transfers block at their next progress tick.
    paused: AtomicBool,
}

impl Inner {
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
    pub fn new(sid: u32) -> Self {
        Self {
            inner: Arc::new(Inner { sid, ..Default::default() }),
        }
    }

    /// Remember the session's credentials for the workers; bump the generation
    /// so each worker reconnects before its next job.
    pub fn set_creds(&self, creds: Credentials) {
        *self.inner.creds.lock().unwrap() = Some(creds);
        self.inner.creds_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// Cap transfer speed in KiB/s (0 = unlimited).
    pub fn set_speed(&self, kbs: u64) {
        self.inner.speed_kbs.store(kbs, Ordering::Relaxed);
    }

    /// Pause/resume: paused transfers block at their next progress tick.
    pub fn set_paused(&self, paused: bool) {
        self.inner.paused.store(paused, Ordering::Relaxed);
    }

    /// Set the desired number of concurrent transfer workers (at least 1).
    pub fn set_max(&self, n: u32) {
        self.inner.target.store(n.max(1) as u64, Ordering::Relaxed);
    }

    /// Cancel an in-flight or queued transfer.
    pub fn cancel(&self, id: u64) {
        if let Some(c) = self.inner.cancels.lock().unwrap().get(&id) {
            c.store(true, Ordering::Relaxed);
        }
    }

    /// Queue a transfer; returns its id. `upload` = local→remote.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_job(
        &self,
        upload: bool,
        is_dir: bool,
        name: String,
        local: String,
        remote: String,
        overwrite: Option<i32>,
        resume: bool,
        app: AppHandle,
    ) -> Result<u64, String> {
        let inner = self.inner.clone();
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
            resume,
            cancel,
        };
        sender
            .as_ref()
            .unwrap()
            .send(job)
            .map_err(|_| "transfer worker stopped".to_string())?;
        drop(sender);

        // Spawn workers up to the current target (bump the count before the
        // spawn so concurrent enqueues read an accurate value).
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
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Evt {
    Started {
        session: u32,
        id: u64,
        name: String,
        upload: bool,
        total: u64,
        // Echoed back so the UI can re-enqueue this transfer on Retry.
        local: String,
        remote: String,
        is_dir: bool,
        overwrite: i32,
    },
    Progress { session: u32, id: u64, done: u64, total: u64 },
    Done { session: u32, id: u64, name: String, upload: bool },
    Failed { session: u32, id: u64, message: String },
    Cancelled { session: u32, id: u64 },
}

fn emit(app: &AppHandle, e: Evt) {
    let _ = app.emit("xfer", e);
}

fn is_transient(e: &Error) -> bool {
    matches!(e, Error::Connect(_) | Error::Io(_))
}

fn worker(inner: Arc<Inner>, rx: SharedRx, app: AppHandle) {
    let sid = inner.sid;
    let mut my_gen = 0u64;
    let mut transport: Option<Box<dyn Transport>> = None;

    loop {
        // Shrink the pool if we're over target. Check BEFORE recv so no job is
        // ever pulled and dropped. Never let the pool fall below 1.
        {
            let workers = inner.workers.load(Ordering::Relaxed);
            if workers > inner.effective_target() && workers > 1 {
                inner.workers.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        }

        // Grab the next job, holding the receiver lock only across the recv.
        let job = {
            let guard = rx.lock().unwrap();
            match guard.recv() {
                Ok(job) => job,
                Err(_) => {
                    inner.workers.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        };

        if job.cancel.load(Ordering::Relaxed) {
            emit(&app, Evt::Cancelled { session: sid, id: job.id });
            inner.cancels.lock().unwrap().remove(&job.id);
            continue;
        }

        let latest_gen = inner.creds_gen.load(Ordering::Relaxed);
        if transport.is_none() || latest_gen != my_gen {
            let creds = inner.creds.lock().unwrap().clone();
            transport = creds.and_then(|c| connect(&c).ok());
            my_gen = latest_gen;
        }

        let Some(t) = transport.as_mut() else {
            emit(&app, Evt::Failed { session: sid, id: job.id, message: "not connected".into() });
            inner.cancels.lock().unwrap().remove(&job.id);
            continue;
        };

        emit(&app, Evt::Started {
            session: sid,
            id: job.id,
            name: job.name.clone(),
            upload: job.upload,
            total: 0,
            local: job.local.clone(),
            remote: job.remote.clone(),
            is_dir: job.is_dir,
            overwrite: job.overwrite,
        });
        let result = run_job(t.as_mut(), &job, &app, &inner);
        match result {
            Ok(_) => emit(&app, Evt::Done { session: sid, id: job.id, name: job.name.clone(), upload: job.upload }),
            Err(Error::Cancelled) => emit(&app, Evt::Cancelled { session: sid, id: job.id }),
            Err(e) => {
                if is_transient(&e) {
                    transport = None;
                }
                emit(&app, Evt::Failed { session: sid, id: job.id, message: e.to_string() });
            }
        }
        inner.cancels.lock().unwrap().remove(&job.id);
    }
}

fn run_job(t: &mut dyn Transport, job: &Job, app: &AppHandle, inner: &Arc<Inner>) -> scp_core::Result<u64> {
    let sid = inner.sid;
    let id = job.id;
    let cancel = job.cancel.clone();
    let app = app.clone();
    // Pause + speed-cap gate, shared by both the dir and file callbacks. While
    // paused it blocks (still cancellable); with a speed cap it sleeps to hold
    // the byte rate. Returns false to abort (cancelled).
    let mut last_done: u64 = 0;
    let gate = inner.clone();
    let mut throttle = move |done: u64| -> bool {
        while gate.paused.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(120));
        }
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        // Only react to forward byte progress, so non-byte ticks (done == 0)
        // don't reset the rate baseline and over-throttle the next chunk.
        if done > last_done {
            let kbs = gate.speed_kbs.load(Ordering::Relaxed);
            if kbs > 0 {
                let chunk = done - last_done;
                let micros = chunk.saturating_mul(1_000_000) / kbs.saturating_mul(1024).max(1);
                if micros > 0 {
                    std::thread::sleep(Duration::from_micros(micros));
                }
            }
            last_done = done;
        }
        true
    };
    if job.is_dir {
        let mut cb = |ev: XferEvent| -> bool {
            if let XferEvent::Bytes { done, total } = ev {
                if !throttle(done) {
                    return false;
                }
                emit(&app, Evt::Progress { session: sid, id, done, total });
                true
            } else {
                throttle(0)
            }
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
            if !throttle(done) {
                return false;
            }
            emit(&app, Evt::Progress { session: sid, id, done, total });
            true
        };
        if job.resume {
            // Continue a partial (Retry): resume the upload, or download from
            // the size already on disk.
            if job.upload {
                t.upload_resume(Path::new(&job.local), &job.remote, &mut progress)
            } else {
                let offset = std::fs::metadata(&job.local).map(|m| m.len()).unwrap_or(0);
                t.download_resume(&job.remote, Path::new(&job.local), offset, &mut progress)
            }
        } else if job.upload {
            t.upload_progress(Path::new(&job.local), &job.remote, &mut progress)
        } else {
            t.download_progress(&job.remote, Path::new(&job.local), &mut progress)
        }
    }
}
