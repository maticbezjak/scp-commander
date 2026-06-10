//! Session worker thread.
//!
//! GTK widgets are main-thread-only and every core call blocks, so the live
//! `Transport` lives on its own thread. The UI sends [`Cmd`]s over a std mpsc
//! channel; the worker replies with [`Event`]s over an async channel that the
//! main loop drains via `glib::spawn_future_local`.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use scp_core::types::{Credentials, Entry};
use scp_core::{connect, Transport};

pub enum Cmd {
    Connect { creds: Credentials, path: String },
    List { path: String },
    Download { id: u64, name: String, remote: String, local: PathBuf },
    Upload { id: u64, name: String, local: PathBuf, remote: String },
}

pub enum Event {
    Connected { path: String, entries: Vec<Entry> },
    Listed { path: String, entries: Vec<Entry> },
    Progress { id: u64, done: u64, total: u64 },
    /// `download` distinguishes which pane needs refreshing afterwards.
    Done { id: u64, name: String, bytes: u64, download: bool },
    Failed { id: u64, message: String },
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

        for cmd in rx {
            match cmd {
                Cmd::Connect { creds, path } => match connect(&creds) {
                    Ok(mut t) => match t.list_dir(&path) {
                        Ok(entries) => {
                            session = Some(t);
                            send(Event::Connected { path, entries });
                        }
                        Err(e) => send(Event::Error(format!("list failed: {e}"))),
                    },
                    Err(e) => send(Event::Error(format!("connect failed: {e}"))),
                },

                Cmd::List { path } => {
                    let Some(t) = session.as_mut() else {
                        send(Event::Error("not connected".into()));
                        continue;
                    };
                    match t.list_dir(&path) {
                        Ok(entries) => send(Event::Listed { path, entries }),
                        Err(e) => send(Event::Error(format!("list failed: {e}"))),
                    }
                }

                Cmd::Download { id, name, remote, local } => {
                    let Some(t) = session.as_mut() else {
                        send(Event::Failed { id, message: "not connected".into() });
                        continue;
                    };
                    let mut throttle = Throttle::new();
                    let mut progress = |done: u64, total: u64| {
                        if throttle.should_send(done, total) {
                            let _ = events.send_blocking(Event::Progress { id, done, total });
                        }
                    };
                    match t.download_progress(&remote, &local, &mut progress) {
                        Ok(bytes) => send(Event::Done { id, name, bytes, download: true }),
                        Err(e) => send(Event::Failed { id, message: e.to_string() }),
                    }
                }

                Cmd::Upload { id, name, local, remote } => {
                    let Some(t) = session.as_mut() else {
                        send(Event::Failed { id, message: "not connected".into() });
                        continue;
                    };
                    let mut throttle = Throttle::new();
                    let mut progress = |done: u64, total: u64| {
                        if throttle.should_send(done, total) {
                            let _ = events.send_blocking(Event::Progress { id, done, total });
                        }
                    };
                    match t.upload_progress(&local, &remote, &mut progress) {
                        Ok(bytes) => send(Event::Done { id, name, bytes, download: false }),
                        Err(e) => send(Event::Failed { id, message: e.to_string() }),
                    }
                }
            }
        }

        if let Some(mut t) = session.take() {
            t.disconnect();
        }
    });

    tx
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
