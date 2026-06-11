//! Session worker thread.
//!
//! GTK widgets are main-thread-only and every core call blocks, so the live
//! `Transport` lives on its own thread. The UI sends [`Cmd`]s over a std mpsc
//! channel; the worker replies with [`Event`]s over an async channel that the
//! main loop drains via `glib::spawn_future_local`. Transfers carry an
//! `Arc<AtomicBool>` cancel flag flipped by the UI's cancel buttons.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use scp_core::ops::{self, Filter, SyncDirection, SyncPlan, XferEvent};
use scp_core::types::{Credentials, Entry, Error};
use scp_core::{connect, Transport};

pub enum Cmd {
    Connect { creds: Credentials, path: String },
    List { path: String },
    Download {
        id: u64,
        name: String,
        remote: String,
        local: PathBuf,
        /// Resume from this byte offset (0 = fresh download).
        resume: u64,
        cancel: Arc<AtomicBool>,
    },
    Upload {
        id: u64,
        name: String,
        local: PathBuf,
        remote: String,
        /// Append after the remote file's current size instead of replacing.
        resume: bool,
        cancel: Arc<AtomicBool>,
    },
    DownloadDir { id: u64, name: String, remote: String, local: PathBuf, excludes: String, cancel: Arc<AtomicBool> },
    UploadDir { id: u64, name: String, local: PathBuf, remote: String, excludes: String, cancel: Arc<AtomicBool> },
    Sync { id: u64, download: bool, local: PathBuf, remote: String, excludes: String, cancel: Arc<AtomicBool> },
    /// Sync dry run; result arrives as Event::SyncPlanReady.
    SyncPlan { download: bool, local: PathBuf, remote: String, excludes: String },
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
                Cmd::Connect { creds, path } => match connect(&creds) {
                    Ok(mut t) => match t.list_dir(&path) {
                        Ok(entries) => {
                            session = Some(t);
                            send(Event::Connected { path, entries });
                        }
                        Err(e) => send(Event::Error(format!("list failed: {e}"))),
                    },
                    Err(Error::UnknownHostKey { fingerprint }) => {
                        send(Event::HostKeyUnknown { fingerprint });
                    }
                    Err(e) => send(Event::Error(format!("connect failed: {e}"))),
                },

                Cmd::List { path } => {
                    with_session(&mut session, &send, |t| match t.list_dir(&path) {
                        Ok(entries) => send(Event::Listed { path: path.clone(), entries }),
                        Err(e) => send(Event::Error(format!("list failed: {e}"))),
                    });
                }

                Cmd::Download { id, name, remote, local, resume, cancel } => {
                    transfer(&mut session, &send, id, name, cancel, true, |t, progress| {
                        if resume > 0 {
                            t.download_resume(&remote, &local, resume, progress)
                        } else {
                            t.download_progress(&remote, &local, progress)
                        }
                    });
                }

                Cmd::Upload { id, name, local, remote, resume, cancel } => {
                    transfer(&mut session, &send, id, name, cancel, false, |t, progress| {
                        if resume {
                            t.upload_resume(&local, &remote, progress)
                        } else {
                            t.upload_progress(&local, &remote, progress)
                        }
                    });
                }

                Cmd::DownloadDir { id, name, remote, local, excludes, cancel } => {
                    let filter = Filter::parse(&excludes);
                    multi(&mut session, &send, id, name, cancel, true, |t, cb| {
                        ops::download_dir(t, &remote, &local, &filter, cb)
                    });
                }

                Cmd::UploadDir { id, name, local, remote, excludes, cancel } => {
                    let filter = Filter::parse(&excludes);
                    multi(&mut session, &send, id, name, cancel, false, |t, cb| {
                        ops::upload_dir(t, &local, &remote, &filter, cb)
                    });
                }

                Cmd::Sync { id, download, local, remote, excludes, cancel } => {
                    let filter = Filter::parse(&excludes);
                    let dir = if download { SyncDirection::Download } else { SyncDirection::Upload };
                    let name = format!("Sync {}", if download { "⬇" } else { "⬆" });
                    multi(&mut session, &send, id, name, cancel, download, |t, cb| {
                        ops::sync_dir(t, &local, &remote, dir, &filter, cb)
                            .map(|s| s.copied as u64)
                    });
                }

                Cmd::SyncPlan { download, local, remote, excludes } => {
                    let dir = if download {
                        SyncDirection::Download
                    } else {
                        SyncDirection::Upload
                    };
                    let filter = Filter::parse(&excludes);
                    with_session(&mut session, &send, |t| {
                        match ops::plan_sync(t, &local, &remote, dir, &filter) {
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

/// Single-file transfer with throttled progress + cancellation.
fn transfer(
    session: &mut Option<Box<dyn Transport>>,
    send: &impl Fn(Event),
    id: u64,
    name: String,
    cancel: Arc<AtomicBool>,
    download: bool,
    op: impl FnOnce(&mut dyn Transport, scp_core::transport::Progress) -> scp_core::Result<u64>,
) {
    let Some(t) = session.as_mut() else {
        send(Event::Failed { id, message: "not connected".into() });
        return;
    };
    let mut throttle = Throttle::new();
    let mut progress = |done: u64, total: u64| -> bool {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        if throttle.should_send(done, total) {
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
    download: bool,
    op: impl FnOnce(&mut dyn Transport, ops::XferCb) -> scp_core::Result<u64>,
) {
    let Some(t) = session.as_mut() else {
        send(Event::Failed { id, message: "not connected".into() });
        return;
    };
    let mut throttle = Throttle::new();
    let mut cb = |ev: XferEvent| -> bool {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        match ev {
            XferEvent::Start { name, total, .. } => {
                throttle = Throttle::new();
                send(Event::FileStart { id, file: name.to_string(), total });
            }
            XferEvent::Bytes { done, total } => {
                if throttle.should_send(done, total) {
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
