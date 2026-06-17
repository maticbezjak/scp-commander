// Per-session directory sync: a dry-run preview (plan) and the actual mirror/
// copy run (run, progress streamed as "sync" events tagged with the session
// id). Runs on its own connection so the browse session stays responsive. The
// Tauri command wrappers live in main.rs.

use std::path::Path;
use std::sync::Mutex;

use scp_core::connect;
use scp_core::ops::{plan_sync_opts, sync_dir_opts, Filter, SyncDirection, SyncOptions, XferEvent};
use scp_core::types::Credentials;
use tauri::{AppHandle, Emitter};

/// Holds the session's credentials so sync can open its own dedicated link.
pub struct SyncManager {
    sid: u32,
    creds: Mutex<Option<Credentials>>,
}

impl SyncManager {
    pub fn new(sid: u32) -> Self {
        Self { sid, creds: Mutex::new(None) }
    }

    pub fn set_creds(&self, creds: Credentials) {
        *self.creds.lock().unwrap() = Some(creds);
    }

    /// Dry run: compute what a sync would copy/delete without touching anything.
    pub fn plan(
        &self,
        local: String,
        remote: String,
        direction: String,
        mirror: bool,
    ) -> Result<SyncPlanDto, String> {
        let dir = parse_direction(&direction)?;
        let creds = match self.creds.lock().unwrap().clone() {
            Some(c) => c,
            None => return Err("not connected".into()),
        };
        let mut t = connect(&creds).map_err(|e| e.to_string())?;
        let plan = plan_sync_opts(
            t.as_mut(),
            Path::new(&local),
            &remote,
            dir,
            &Filter::empty(),
            &SyncOptions { delete: mirror },
        )
        .map_err(|e| e.to_string())?;

        let items = plan
            .items
            .into_iter()
            .map(|item| PlanItemDto {
                rel: item.rel,
                size: item.size,
                reason: item.reason.label().to_string(),
            })
            .collect();

        Ok(SyncPlanDto { items, dirs: plan.dirs, deletes: plan.deletes })
    }

    /// Run a sync on a dedicated connection; progress streams as "sync" events.
    /// Returns immediately; the work happens on a background thread.
    pub fn run(
        &self,
        local: String,
        remote: String,
        direction: String,
        mirror: bool,
        app: AppHandle,
    ) -> Result<(), String> {
        let dir = parse_direction(&direction)?;
        let creds = match self.creds.lock().unwrap().clone() {
            Some(c) => c,
            None => return Err("not connected".into()),
        };
        let sid = self.sid;

        std::thread::spawn(move || {
            let mut t = match connect(&creds) {
                Ok(t) => t,
                Err(e) => {
                    let _ = app.emit("sync", SyncEvt::Failed { session: sid, message: e.to_string() });
                    return;
                }
            };

            let res = {
                let mut cb = |ev: XferEvent| -> bool {
                    if let XferEvent::Bytes { done, total } = ev {
                        let _ = app.emit("sync", SyncEvt::Progress { session: sid, done, total });
                    }
                    true
                };
                sync_dir_opts(
                    t.as_mut(),
                    Path::new(&local),
                    &remote,
                    dir,
                    &Filter::empty(),
                    &mut cb,
                    &SyncOptions { delete: mirror },
                )
            };

            match res {
                Ok(stats) => {
                    let _ = app.emit(
                        "sync",
                        SyncEvt::Done {
                            session: sid,
                            copied: stats.copied as u64,
                            skipped: stats.skipped as u64,
                            bytes: stats.bytes,
                        },
                    );
                }
                Err(e) => {
                    let _ = app.emit("sync", SyncEvt::Failed { session: sid, message: e.to_string() });
                }
            }
        });

        Ok(())
    }
}

fn parse_direction(s: &str) -> Result<SyncDirection, String> {
    match s {
        "upload" => Ok(SyncDirection::Upload),
        "download" => Ok(SyncDirection::Download),
        _ => Err(format!("unknown sync direction: {s}")),
    }
}

#[derive(serde::Serialize)]
pub struct SyncPlanDto {
    items: Vec<PlanItemDto>,
    dirs: Vec<String>,
    deletes: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct PlanItemDto {
    rel: String,
    size: u64,
    reason: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum SyncEvt {
    Progress { session: u32, done: u64, total: u64 },
    Done { session: u32, copied: u64, skipped: u64, bytes: u64 },
    Failed { session: u32, message: String },
}
