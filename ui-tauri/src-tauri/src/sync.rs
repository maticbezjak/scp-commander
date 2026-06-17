// Directory sync: a dry-run preview (sync_plan) and the actual mirror/copy run
// (sync_run, progress streamed as "sync" events). Runs on its own connection so
// the browse session stays responsive.

use std::path::Path;
use std::sync::Mutex;

use scp_core::connect;
use scp_core::ops::{plan_sync_opts, sync_dir_opts, Filter, SyncDirection, SyncOptions, XferEvent};
use scp_core::types::Credentials;
use tauri::{AppHandle, Emitter, State};

/// Holds the credentials of the freshly-connected browsing session so sync
/// operations can open their own dedicated connection.
#[derive(Default)]
pub struct SyncManager {
    creds: Mutex<Option<Credentials>>,
}

impl SyncManager {
    /// Remember the credentials of a freshly-connected session.
    pub fn set_creds(&self, creds: Credentials) {
        *self.creds.lock().unwrap() = Some(creds);
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

/// Dry run: compute what a sync would copy/delete without touching anything.
#[tauri::command]
pub fn sync_plan(
    local: String,
    remote: String,
    direction: String,
    mirror: bool,
    mgr: State<SyncManager>,
) -> Result<SyncPlanDto, String> {
    let dir = parse_direction(&direction)?;
    let creds = match mgr.creds.lock().unwrap().clone() {
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

    Ok(SyncPlanDto {
        items,
        dirs: plan.dirs,
        deletes: plan.deletes,
    })
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum SyncEvt {
    Progress { done: u64, total: u64 },
    Done { copied: u64, skipped: u64, bytes: u64 },
    Failed { message: String },
}

/// Run a sync on a dedicated connection, streaming progress as "sync" events.
/// Returns immediately; the work happens on a background thread.
#[tauri::command]
pub fn sync_run(
    local: String,
    remote: String,
    direction: String,
    mirror: bool,
    app: AppHandle,
    mgr: State<SyncManager>,
) -> Result<(), String> {
    let dir = parse_direction(&direction)?;
    let creds = match mgr.creds.lock().unwrap().clone() {
        Some(c) => c,
        None => return Err("not connected".into()),
    };

    std::thread::spawn(move || {
        let mut t = match connect(&creds) {
            Ok(t) => t,
            Err(e) => {
                let _ = app.emit("sync", SyncEvt::Failed { message: e.to_string() });
                return;
            }
        };

        let res = {
            let mut cb = |ev: XferEvent| -> bool {
                if let XferEvent::Bytes { done, total } = ev {
                    let _ = app.emit("sync", SyncEvt::Progress { done, total });
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
                        copied: stats.copied as u64,
                        skipped: stats.skipped as u64,
                        bytes: stats.bytes,
                    },
                );
            }
            Err(e) => {
                let _ = app.emit("sync", SyncEvt::Failed { message: e.to_string() });
            }
        }
    });

    Ok(())
}
