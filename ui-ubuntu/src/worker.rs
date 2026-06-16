//! Session worker thread.
//!
//! GTK widgets are main-thread-only and every core call blocks, so the live
//! `Transport` lives on its own thread. The UI sends [`Cmd`]s over a std mpsc
//! channel; the worker replies with [`Event`]s over an async channel that the
//! main loop drains via `glib::spawn_future_local`. Transfers carry an
//! `Arc<AtomicBool>` cancel flag flipped by the UI's cancel buttons.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;

/// Process-wide transfer speed cap in KiB/s (0 = unlimited). Set by the
/// Transfer Settings dropdown; read by every transfer progress callback.
/// The cap applies per connection — with the 3-worker pool, aggregate
/// throughput can reach 3× this value when transfers run in parallel.
pub static SPEED_LIMIT_KBS: AtomicU64 = AtomicU64::new(0);

/// Sleep long enough that the bytes since `last_done` match the cap.
fn throttle(last_done: &mut u64, done: u64) {
    let kbs = SPEED_LIMIT_KBS.load(Ordering::Relaxed);
    if kbs > 0 && done > *last_done {
        let micros = (done - *last_done) * 1_000_000 / (kbs * 1024);
        if micros > 0 {
            thread::sleep(std::time::Duration::from_micros(micros.min(1_000_000)));
        }
    }
    *last_done = done;
}

/// Thread-safe pause flag shared between the UI pause button and the worker
/// progress callback. The worker calls `wait_while_paused()` on each tick.
pub struct PauseFlag(Mutex<bool>, Condvar);

impl PauseFlag {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(false), Condvar::new()))
    }
    pub fn pause(&self) {
        *self.0.lock().unwrap() = true;
    }
    pub fn resume(&self) {
        let mut p = self.0.lock().unwrap();
        *p = false;
        self.1.notify_one();
    }
    pub fn is_paused(&self) -> bool {
        *self.0.lock().unwrap()
    }
    pub fn wait_while_paused(&self) {
        let mut p = self.0.lock().unwrap();
        while *p { p = self.1.wait(p).unwrap(); }
    }
}

use scp_core::ops::{self, Filter, OverwritePolicy, SyncDirection, SyncOptions, SyncPlan, XferEvent};
use scp_core::types::{Credentials, Entry, Error};
use scp_core::{connect, Transport};

pub enum Cmd {
    /// `silent = true` suppresses the Event::Connected reply (used by pool workers).
    Connect { creds: Credentials, path: String, silent: bool },
    List { path: String },
    Download {
        id: u64,
        name: String,
        remote: String,
        local: PathBuf,
        /// Resume from this byte offset (0 = fresh download).
        resume: u64,
        cancel: Arc<AtomicBool>,
        pause: Arc<PauseFlag>,
    },
    Upload {
        id: u64,
        name: String,
        local: PathBuf,
        remote: String,
        /// Append after the remote file's current size instead of replacing.
        resume: bool,
        cancel: Arc<AtomicBool>,
        pause: Arc<PauseFlag>,
    },
    DownloadDir { id: u64, name: String, remote: String, local: PathBuf, excludes: String, overwrite: i32, cancel: Arc<AtomicBool>, pause: Arc<PauseFlag> },
    UploadDir { id: u64, name: String, local: PathBuf, remote: String, excludes: String, overwrite: i32, cancel: Arc<AtomicBool>, pause: Arc<PauseFlag> },
    Sync { id: u64, download: bool, local: PathBuf, remote: String, excludes: String, cancel: Arc<AtomicBool>, pause: Arc<PauseFlag> },
    /// Sync dry run; result arrives as Event::SyncPlanReady.
    SyncPlan { download: bool, local: PathBuf, remote: String, excludes: String, delete_extraneous: bool },
    /// Execute a remote command (SFTP only); result arrives as Event::ExecResult.
    Exec { cmd: String },
    /// Server-side file copy; success arrives as Event::OpOk.
    CopyFile { src: String, dst: String },
    /// Recursive remote search; result arrives as Event::FindResults.
    Find { base: String, mask: String },
    Mkdir { path: String },
    Delete { path: String, is_dir: bool },
    Rename { from: String, to: String },
    Chmod { path: String, mode: u32 },
}

pub enum Event {
    Connected { path: String, entries: Vec<Entry> },
    Listed { path: String, entries: Vec<Entry> },
    /// Strict connect hit a server whose key isn't known; UI should prompt.
    HostKeyUnknown { fingerprint: String },
    Progress { id: u64, done: u64, total: u64 },
    /// A multi-file operation moved on to a new file.
    FileStart { id: u64, file: String, total: u64 },
    FileDone { id: u64 },
    /// `download` distinguishes which pane needs refreshing afterwards.
    /// For sync rows, `bytes` is the number of files copied.
    Done { id: u64, name: String, bytes: u64, download: bool },
    Cancelled { id: u64, name: String },
    Failed { id: u64, message: String },
    /// A remote management op (mkdir/delete/rename) finished; refresh.
    OpOk { message: String },
    /// Result of Cmd::SyncPlan.
    SyncPlanReady { download: bool, local: PathBuf, remote: String, plan: SyncPlan },
    /// Result of Cmd::Find.
    FindResults { base: String, mask: String, hits: Vec<(String, Entry)> },
    /// Result of Cmd::Exec.
    ExecResult { exit_code: i32, stdout: String, stderr: String },
    Error(String),
}

/// Spawn the worker; returns the command sender. Events go to `events`.
pub fn spawn(events: async_channel::Sender<Event>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();

    thread::spawn(move || {
        let mut session: Option<Box<dyn Transport>> = None;
        let send = |ev: Event| {
            let _ = events.send_blocking(ev);
        };

        loop {
            // Idle keepalive: ping every 30s so NAT mappings stay warm and a
            // dead session is detected (the AutoReconnect wrapper revives it
            // on the next real operation).
            let cmd = match rx.recv_timeout(std::time::Duration::from_secs(30)) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(t) = session.as_mut() {
                        let _ = t.keepalive();
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            match cmd {
                Cmd::Connect { creds, path, silent } => match connect(&creds) {
                    Ok(mut t) => {
                        if silent {
                            session = Some(t);
                        } else {
                            match t.list_dir(&path) {
                                Ok(entries) => {
                                    session = Some(t);
                                    send(Event::Connected { path, entries });
                                }
                                Err(e) => send(Event::Error(format!("list failed: {e}"))),
                            }
                        }
                    }
                    Err(Error::UnknownHostKey { fingerprint }) => {
                        if !silent { send(Event::HostKeyUnknown { fingerprint }); }
                    }
                    Err(e) => {
                        if !silent { send(Event::Error(format!("connect failed: {e}"))); }
                    }
                },
                Cmd::List { path } => {
                    with_session(&mut session, &send, |t| match t.list_dir(&path) {
                        Ok(entries) => send(Event::Listed { path: path.clone(), entries }),
                        Err(e) => send(Event::Error(format!("list failed: {e}"))),
                    });
                }

                Cmd::Download { id, name, remote, local, resume, cancel, pause } => {
                    transfer(&mut session, &send, id, name, cancel, pause, true, |t, progress| {
                        if resume > 0 {
                            t.download_resume(&remote, &local, resume, progress)
                        } else {
                            t.download_progress(&remote, &local, progress)
                        }
                    });
                }

                Cmd::Upload { id, name, local, remote, resume, cancel, pause } => {
                    transfer(&mut session, &send, id, name, cancel, pause, false, |t, progress| {
                        if resume {
                            t.upload_resume(&local, &remote, progress)
                        } else {
                            t.upload_progress(&local, &remote, progress)
                        }
                    });
                }

                Cmd::DownloadDir { id, name, remote, local, excludes, overwrite, cancel, pause } => {
                    let filter = Filter::parse(&excludes);
                    let policy = OverwritePolicy::from_code(overwrite);
                    multi(&mut session, &send, id, name, cancel, pause, true, |t, cb| {
                        ops::download_dir_opts(t, &remote, &local, &filter, policy, cb)
                    });
                }

                Cmd::UploadDir { id, name, local, remote, excludes, overwrite, cancel, pause } => {
                    let filter = Filter::parse(&excludes);
                    let policy = OverwritePolicy::from_code(overwrite);
                    multi(&mut session, &send, id, name, cancel, pause, false, |t, cb| {
                        ops::upload_dir_opts(t, &local, &remote, &filter, policy, cb)
                    });
                }

                Cmd::Sync { id, download, local, remote, excludes, cancel, pause } => {
                    let filter = Filter::parse(&excludes);
                    let dir = if download { SyncDirection::Download } else { SyncDirection::Upload };
                    let name = format!("Sync {}", if download { "⬇" } else { "⬆" });
                    multi(&mut session, &send, id, name, cancel, pause, download, |t, cb| {
                        ops::sync_dir(t, &local, &remote, dir, &filter, cb)
                            .map(|s| s.copied as u64)
                    });
                }

                Cmd::SyncPlan { download, local, remote, excludes, delete_extraneous } => {
                    let dir = if download {
                        SyncDirection::Download
                    } else {
                        SyncDirection::Upload
                    };
                    let filter = Filter::parse(&excludes);
                    let opts = SyncOptions { delete: delete_extraneous };
                    with_session(&mut session, &send, |t| {
                        match ops::plan_sync_opts(t, &local, &remote, dir, &filter, &opts) {
                            Ok(plan) => send(Event::SyncPlanReady {
                                download,
                                local: local.clone(),
                                remote: remote.clone(),
                                plan,
                            }),
                            Err(e) => send(Event::Error(format!("sync preview failed: {e}"))),
                        }
                    });
                }

                Cmd::Exec { cmd } => {
                    with_session(&mut session, &send, |t| {
                        match t.exec_command(&cmd) {
                            Ok(r) => send(Event::ExecResult {
                                exit_code: r.exit_code,
                                stdout: r.stdout,
                                stderr: r.stderr,
                            }),
                            Err(e) => send(Event::Error(format!("exec failed: {e}"))),
                        }
                    });
                }

                Cmd::CopyFile { src, dst } => {
                    with_session(&mut session, &send, |t| match t.copy_file(&src, &dst) {
                        Ok(_) => send(Event::OpOk { message: format!("Copied to {dst}") }),
                        Err(e) => send(Event::Error(e.to_string())),
                    });
                }

                Cmd::Find { base, mask } => {
                    with_session(&mut session, &send, |t| {
                        match ops::find(t, &base, &mask, 500, &mut || true) {
                            Ok(hits) => send(Event::FindResults {
                                base: base.clone(),
                                mask: mask.clone(),
                                hits,
                            }),
                            Err(e) => send(Event::Error(format!("find failed: {e}"))),
                        }
                    });
                }

                Cmd::Mkdir { path } => {
                    with_session(&mut session, &send, |t| match t.mkdir(&path) {
                        Ok(()) => send(Event::OpOk { message: format!("Created {path}") }),
                        Err(e) => send(Event::Error(e.to_string())),
                    });
                }

                Cmd::Delete { path, is_dir } => {
                    with_session(&mut session, &send, |t| {
                        let result = if is_dir {
                            ops::remove_dir_all(t, &path)
                        } else {
                            t.remove_file(&path)
                        };
                        match result {
                            Ok(()) => send(Event::OpOk { message: format!("Deleted {path}") }),
                            Err(e) => send(Event::Error(e.to_string())),
                        }
                    });
                }

                Cmd::Rename { from, to } => {
                    with_session(&mut session, &send, |t| match t.rename(&from, &to) {
                        Ok(()) => send(Event::OpOk { message: format!("Renamed to {to}") }),
                        Err(e) => send(Event::Error(e.to_string())),
                    });
                }

                Cmd::Chmod { path, mode } => {
                    with_session(&mut session, &send, |t| {
                        match t.set_permissions(&path, mode) {
                            Ok(()) => send(Event::OpOk {
                                message: format!("Permissions set to {mode:o}"),
                            }),
                            Err(e) => send(Event::Error(e.to_string())),
                        }
                    });
                }
            }
        }

        if let Some(mut t) = session.take() {
            t.disconnect();
        }
    });

    tx
}

fn with_session(
    session: &mut Option<Box<dyn Transport>>,
    send: &impl Fn(Event),
    f: impl FnOnce(&mut dyn Transport),
) {
    match session.as_mut() {
        Some(t) => f(t.as_mut()),
        None => send(Event::Error("not connected".into())),
    }
}

/// Single-file transfer with throttled progress, pause, and cancellation.
fn transfer(
    session: &mut Option<Box<dyn Transport>>,
    send: &impl Fn(Event),
    id: u64,
    name: String,
    cancel: Arc<AtomicBool>,
    pause: Arc<PauseFlag>,
    download: bool,
    op: impl FnOnce(&mut dyn Transport, scp_core::transport::Progress) -> scp_core::Result<u64>,
) {
    let Some(t) = session.as_mut() else {
        send(Event::Failed { id, message: "not connected".into() });
        return;
    };
    let mut ui_throttle = Throttle::new();
    let mut speed_done: u64 = 0;
    let mut progress = |done: u64, total: u64| -> bool {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        pause.wait_while_paused();
        throttle(&mut speed_done, done);
        if ui_throttle.should_send(done, total) {
            send(Event::Progress { id, done, total });
        }
        true
    };
    match op(t.as_mut(), &mut progress) {
        Ok(bytes) => send(Event::Done { id, name, bytes, download }),
        Err(Error::Cancelled) => send(Event::Cancelled { id, name }),
        Err(e) => send(Event::Failed { id, message: e.to_string() }),
    }
}

/// Multi-file operation (folder transfer / sync) with per-file events.
fn multi(
    session: &mut Option<Box<dyn Transport>>,
    send: &impl Fn(Event),
    id: u64,
    name: String,
    cancel: Arc<AtomicBool>,
    pause: Arc<PauseFlag>,
    download: bool,
    op: impl FnOnce(&mut dyn Transport, ops::XferCb) -> scp_core::Result<u64>,
) {
    let Some(t) = session.as_mut() else {
        send(Event::Failed { id, message: "not connected".into() });
        return;
    };
    let mut ui_throttle = Throttle::new();
    let mut speed_done: u64 = 0;
    let mut cb = |ev: XferEvent| -> bool {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        pause.wait_while_paused();
        match ev {
            XferEvent::Start { name, total, .. } => {
                ui_throttle = Throttle::new();
                speed_done = 0;
                send(Event::FileStart { id, file: name.to_string(), total });
            }
            XferEvent::Bytes { done, total } => {
                throttle(&mut speed_done, done);
                if ui_throttle.should_send(done, total) {
                    send(Event::Progress { id, done, total });
                }
            }
            XferEvent::DoneFile => send(Event::FileDone { id }),
        }
        true
    };
    match op(t.as_mut(), &mut cb) {
        Ok(bytes) => send(Event::Done { id, name, bytes, download }),
        Err(Error::Cancelled) => send(Event::Cancelled { id, name }),
        Err(e) => send(Event::Failed { id, message: e.to_string() }),
    }
}

/// The SFTP copy loop reports every 64 KiB; don't flood the UI channel.
struct Throttle {
    last: u64,
}

impl Throttle {
    fn new() -> Self {
        Self { last: 0 }
    }

    fn should_send(&mut self, done: u64, total: u64) -> bool {
        const STEP: u64 = 256 * 1024;
        if done == total || done >= self.last + STEP {
            self.last = done;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Transfer connection pool

/// Spawn `n` dedicated transfer workers that share the same event channel.
/// Each worker maintains its own independent Transport connection and handles
/// only transfer-related commands (Connect/Disconnect/Download/Upload/…).
/// Browse commands (List, Mkdir, Delete, …) are silently ignored.
pub fn spawn_pool(n: usize, events: async_channel::Sender<Event>) -> Vec<mpsc::Sender<Cmd>> {
    (0..n)
        .map(|_| {
            let (tx, rx) = mpsc::channel::<Cmd>();
            let ev = events.clone();
            thread::spawn(move || pool_run(rx, ev));
            tx
        })
        .collect()
}

fn pool_run(rx: mpsc::Receiver<Cmd>, events: async_channel::Sender<Event>) {
    let mut session: Option<Box<dyn Transport>> = None;
    let mut creds: Option<Credentials> = None;
    let send = move |ev: Event| {
        let _ = events.send_blocking(ev);
    };
    // Lazy reconnect: if the silent connect failed (or the session died and
    // was dropped), retry with the stored credentials before each transfer
    // instead of failing every command with "not connected".
    fn ensure(session: &mut Option<Box<dyn Transport>>, creds: &Option<Credentials>) {
        if session.is_none() {
            if let Some(c) = creds {
                *session = connect(c).ok();
            }
        }
    }

    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(cmd) => match cmd {
                Cmd::Connect { creds: c, .. } => {
                    // Silent connect — no Event::Connected; failures retried on first use.
                    session = connect(&c).ok();
                    creds = Some(c);
                }
                Cmd::Download { id, name, remote, local, resume, cancel, pause } => {
                    ensure(&mut session, &creds);
                    transfer(&mut session, &send, id, name, cancel, pause, true, |t, p| {
                        if resume > 0 { t.download_resume(&remote, &local, resume, p) }
                        else { t.download_progress(&remote, &local, p) }
                    });
                }
                Cmd::Upload { id, name, local, remote, resume, cancel, pause } => {
                    ensure(&mut session, &creds);
                    transfer(&mut session, &send, id, name, cancel, pause, false, |t, p| {
                        if resume { t.upload_resume(&local, &remote, p) }
                        else { t.upload_progress(&local, &remote, p) }
                    });
                }
                Cmd::DownloadDir { id, name, remote, local, excludes, overwrite, cancel, pause } => {
                    ensure(&mut session, &creds);
                    let filter = Filter::parse(&excludes);
                    let policy = OverwritePolicy::from_code(overwrite);
                    multi(&mut session, &send, id, name, cancel, pause, true, |t, cb| {
                        ops::download_dir_opts(t, &remote, &local, &filter, policy, cb)
                    });
                }
                Cmd::UploadDir { id, name, local, remote, excludes, overwrite, cancel, pause } => {
                    ensure(&mut session, &creds);
                    let filter = Filter::parse(&excludes);
                    let policy = OverwritePolicy::from_code(overwrite);
                    multi(&mut session, &send, id, name, cancel, pause, false, |t, cb| {
                        ops::upload_dir_opts(t, &local, &remote, &filter, policy, cb)
                    });
                }
                Cmd::Sync { id, download, local, remote, excludes, cancel, pause } => {
                    ensure(&mut session, &creds);
                    let filter = Filter::parse(&excludes);
                    let dir = if download { SyncDirection::Download } else { SyncDirection::Upload };
                    let name = format!("Sync {}", if download { "⬇" } else { "⬆" });
                    multi(&mut session, &send, id, name, cancel, pause, download, |t, cb| {
                        ops::sync_dir(t, &local, &remote, dir, &filter, cb)
                            .map(|s| s.copied as u64)
                    });
                }
                _ => {} // browse/management commands don't belong on the xfer pool
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(ref mut t) = session { let _ = t.keepalive(); }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
