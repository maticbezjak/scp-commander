// Assorted backend commands grouped into three concerns:
//
//   Group A — remote command execution: run an arbitrary shell command over the
//             live remote session and return its exit code, stdout and stderr.
//   Group B — known-hosts manager: list and remove entries from the SSH
//             known_hosts store via `scp_core::sftp`.
//   Group C — preferences store: load/save user preferences as JSON under
//             `~/.config/scp-commander/prefs-tauri.json`, keeping the process-global
//             atomic-uploads toggle in sync.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

// ---------------------------------------------------------------------------
// Group A — remote command execution
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct ExecDto {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[tauri::command]
pub fn remote_exec(
    session_id: u32,
    cmd: String,
    sessions: State<crate::Sessions>,
) -> Result<ExecDto, String> {
    let s = sessions.get(session_id).ok_or("not connected")?;
    let mut g = s.transport.lock().unwrap();
    let t = g.as_mut().ok_or("not connected")?;
    let r = t.exec_command(&cmd).map_err(|e| e.to_string())?;
    Ok(ExecDto {
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
    })
}

// ---------------------------------------------------------------------------
// Group B — known-hosts manager
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct KnownHostDto {
    host: String,
    key_type: String,
}

#[tauri::command]
pub fn known_hosts_list() -> Vec<KnownHostDto> {
    scp_core::sftp::list_known_hosts()
        .into_iter()
        .map(|h| KnownHostDto {
            host: h.host,
            key_type: h.key_type,
        })
        .collect()
}

#[tauri::command]
pub fn known_hosts_remove(host: String) -> Result<usize, String> {
    scp_core::sftp::remove_known_host(&host).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Group C — preferences store
// ---------------------------------------------------------------------------

fn yes() -> bool {
    true
}

fn two() -> u32 {
    2
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Prefs {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "yes")]
    pub confirm_delete: bool,
    #[serde(default = "yes")]
    pub confirm_overwrite: bool,
    #[serde(default = "yes")]
    pub atomic_uploads: bool,
    #[serde(default = "two")]
    pub max_parallel: u32,
    /// Show Owner/Group columns on the remote pane (off by default — the values
    /// are uninformative on many servers and crowd the listing).
    #[serde(default)]
    pub show_owner_group: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            show_hidden: false,
            confirm_delete: true,
            confirm_overwrite: true,
            atomic_uploads: true,
            max_parallel: 2,
            show_owner_group: false,
        }
    }
}

fn prefs_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config/scp-commander/prefs-tauri.json"))
}

#[tauri::command]
pub fn load_prefs() -> Prefs {
    let prefs: Prefs = prefs_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    scp_core::set_atomic_uploads(prefs.atomic_uploads);
    prefs
}

#[tauri::command]
pub fn save_prefs(prefs: Prefs) -> Result<(), String> {
    let path = prefs_path().ok_or("cannot locate config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&prefs).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    scp_core::set_atomic_uploads(prefs.atomic_uploads);
    Ok(())
}
