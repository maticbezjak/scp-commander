//! Multi-file operations built on top of [`Transport`]: recursive folder
//! transfers, recursive delete, and one-way directory synchronization.
//!
//! All take a [`XferCb`] that receives file-level events and byte progress;
//! returning `false` from it cancels the whole operation.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::transport::Transport;
use crate::types::{Entry, Error, Result};

/// Events emitted while a multi-file operation runs.
pub enum XferEvent<'a> {
    /// About to transfer a file. `download` says which way the bytes flow.
    Start {
        name: &'a str,
        total: u64,
        download: bool,
    },
    /// Byte progress for the file announced by the last `Start`.
    Bytes { done: u64, total: u64 },
    /// The file announced by the last `Start` finished.
    DoneFile,
}

/// Return `false` to cancel the operation ([`Error::Cancelled`]).
pub type XferCb<'a> = &'a mut dyn FnMut(XferEvent) -> bool;

/// What to do when a recursive folder transfer reaches a file that already
/// exists at the destination. Chosen once for the whole operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverwritePolicy {
    /// Always overwrite — the historical (and default) behavior.
    #[default]
    Overwrite,
    /// Never overwrite — copy only files missing at the destination.
    Skip,
    /// Overwrite only when the source differs in size or is newer.
    OnlyNewer,
}

impl OverwritePolicy {
    /// Map a UI/FFI integer code to a policy (unknown codes → Overwrite).
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => OverwritePolicy::Skip,
            2 => OverwritePolicy::OnlyNewer,
            _ => OverwritePolicy::Overwrite,
        }
    }
}

/// Direction for [`sync_dir`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// Local is the source of truth; copy missing/changed files to the server.
    Upload,
    /// Remote is the source of truth; copy missing/changed files locally.
    Download,
}

/// Extra flags for sync operations.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncOptions {
    /// Mirror mode: delete destination files that have no source counterpart.
    /// Upload: removes remote files not present locally.
    /// Download: removes local files not present on the remote.
    pub delete: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub copied: usize,
    pub skipped: usize,
    pub bytes: u64,
}

/// Exclusion masks for multi-file operations, WinSCP-style:
/// `"*.tmp; .git/; node_modules/"` — `;`-separated, `*` wildcards, a trailing
/// `/` restricts the pattern to directories.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    patterns: Vec<(String, bool)>, // (pattern, dir_only)
}

impl Filter {
    pub fn parse(spec: &str) -> Self {
        let patterns = spec
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| match p.strip_suffix('/') {
                Some(dir) => (dir.to_string(), true),
                None => (p.to_string(), false),
            })
            .collect();
        Self { patterns }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// True when `name` should be skipped.
    pub fn excludes(&self, name: &str, is_dir: bool) -> bool {
        self.patterns.iter().any(|(pat, dir_only)| {
            (!dir_only || is_dir) && glob_match(pat.as_bytes(), name.as_bytes())
        })
    }
}

/// Case-insensitive `*`-wildcard match.
fn glob_match(pat: &[u8], name: &[u8]) -> bool {
    if pat.is_empty() {
        return name.is_empty();
    }
    match pat[0] {
        b'*' => {
            glob_match(&pat[1..], name) || (!name.is_empty() && glob_match(pat, &name[1..]))
        }
        c => {
            !name.is_empty()
                && name[0].eq_ignore_ascii_case(&c)
                && glob_match(&pat[1..], &name[1..])
        }
    }
}

/// One entry of a sync dry run.
#[derive(Debug, Clone)]
pub struct PlanItem {
    /// Path relative to the sync roots, e.g. "sub/file.txt".
    pub rel: String,
    pub size: u64,
    pub reason: PlanReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanReason {
    Missing,
    SizeDiffers,
    Newer,
}

impl PlanReason {
    pub fn label(&self) -> &'static str {
        match self {
            PlanReason::Missing => "missing",
            PlanReason::SizeDiffers => "size differs",
            PlanReason::Newer => "newer",
        }
    }
}

/// A sync dry run: the files that would be copied and the destination
/// directories that must exist first (both relative to the roots).
/// In mirror mode, `deletes` holds destination-relative paths to remove.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    pub items: Vec<PlanItem>,
    pub dirs: Vec<String>,
    /// Paths at the destination that would be deleted (mirror mode only).
    pub deletes: Vec<String>,
}

/// Compute what [`sync_dir`] would copy, without copying anything.
pub fn plan_sync(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
) -> Result<SyncPlan> {
    plan_sync_opts(t, local, remote, direction, filter, &SyncOptions::default())
}

/// Like [`plan_sync`] but with extra options (e.g. mirror delete).
pub fn plan_sync_opts(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
    opts: &SyncOptions,
) -> Result<SyncPlan> {
    let mut plan = SyncPlan::default();
    plan_inner(t, local, remote, direction, filter, "", &mut plan)?;
    if opts.delete {
        collect_deletes(t, local, remote, direction, filter, "", &mut plan)?;
    }
    Ok(plan)
}

fn plan_inner(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
    rel: &str,
    plan: &mut SyncPlan,
) -> Result<()> {
    let rel_join = |name: &str| {
        if rel.is_empty() {
            name.to_string()
        } else {
            format!("{rel}/{name}")
        }
    };
    match direction {
        SyncDirection::Upload => {
            let remote_entries: Vec<Entry> = t.list_dir(remote).unwrap_or_default();
            let dest_exists = !remote_entries.is_empty() || rel.is_empty();
            for entry in fs::read_dir(local)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type()?.is_dir();
                if filter.excludes(&name, is_dir) {
                    continue;
                }
                if is_dir {
                    let child_rel = rel_join(&name);
                    if !remote_entries.iter().any(|e| e.name == name && e.is_dir) {
                        plan.dirs.push(child_rel.clone());
                    }
                    plan_inner(
                        t,
                        &entry.path(),
                        &join(remote, &name),
                        direction,
                        filter,
                        &child_rel,
                        plan,
                    )?;
                    continue;
                }
                let meta = entry.metadata()?;
                let dst = remote_entries.iter().find(|e| e.name == name && !e.is_dir);
                if let Some(reason) =
                    copy_reason(meta.len(), mtime_unix(&meta), dst.map(|e| (e.size, e.mtime)))
                {
                    plan.items.push(PlanItem {
                        rel: rel_join(&name),
                        size: meta.len(),
                        reason,
                    });
                }
            }
            let _ = dest_exists;
        }
        SyncDirection::Download => {
            for e in t.list_dir(remote)? {
                if filter.excludes(&e.name, e.is_dir) {
                    continue;
                }
                if e.is_dir && e.is_symlink {
                    continue;
                }
                let child_local = local.join(&e.name);
                if e.is_dir {
                    let child_rel = rel_join(&e.name);
                    if !child_local.is_dir() {
                        plan.dirs.push(child_rel.clone());
                    }
                    plan_inner(
                        t,
                        &child_local,
                        &join(remote, &e.name),
                        direction,
                        filter,
                        &child_rel,
                        plan,
                    )?;
                    continue;
                }
                let dst = fs::metadata(&child_local)
                    .ok()
                    .map(|m| (m.len(), mtime_unix(&m)));
                if let Some(reason) = copy_reason(e.size, e.mtime, dst) {
                    plan.items.push(PlanItem {
                        rel: rel_join(&e.name),
                        size: e.size,
                        reason,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Why a file would be copied, or None when it's up to date.
fn copy_reason(
    src_size: u64,
    src_mtime: Option<i64>,
    dst: Option<(u64, Option<i64>)>,
) -> Option<PlanReason> {
    let Some((dst_size, dst_mtime)) = dst else {
        return Some(PlanReason::Missing);
    };
    if src_size != dst_size {
        return Some(PlanReason::SizeDiffers);
    }
    match (src_mtime, dst_mtime) {
        (Some(s), Some(d)) if s > d + 2 => Some(PlanReason::Newer),
        _ => None,
    }
}

/// Recursively search the remote tree for names matching `mask` (e.g.
/// "*.log"). Stops at `limit` results; `keep_going` is polled so the UI can
/// cancel. Returns full remote paths with their entries.
pub fn find(
    t: &mut dyn Transport,
    base: &str,
    mask: &str,
    limit: usize,
    keep_going: &mut dyn FnMut() -> bool,
) -> Result<Vec<(String, Entry)>> {
    let mut out = Vec::new();
    find_inner(t, base, mask, limit, keep_going, &mut out)?;
    Ok(out)
}

fn find_inner(
    t: &mut dyn Transport,
    base: &str,
    mask: &str,
    limit: usize,
    keep_going: &mut dyn FnMut() -> bool,
    out: &mut Vec<(String, Entry)>,
) -> Result<()> {
    if out.len() >= limit || !keep_going() {
        return Ok(());
    }
    for e in t.list_dir(base)? {
        if out.len() >= limit || !keep_going() {
            return Ok(());
        }
        let full = join(base, &e.name);
        if glob_match(mask.as_bytes(), e.name.as_bytes()) {
            out.push((full.clone(), e.clone()));
        }
        if e.is_dir && !e.is_symlink {
            // Unsearchable subdirectories shouldn't abort the whole search.
            let _ = find_inner(t, &full, mask, limit, keep_going, out);
        }
    }
    Ok(())
}

/// Download a remote directory tree into `local` (created if needed).
pub fn download_dir(
    t: &mut dyn Transport,
    remote: &str,
    local: &Path,
    filter: &Filter,
    cb: XferCb,
) -> Result<u64> {
    download_dir_opts(t, remote, local, filter, OverwritePolicy::Overwrite, cb)
}

/// Like [`download_dir`] but skips/overwrites existing files per `policy`.
pub fn download_dir_opts(
    t: &mut dyn Transport,
    remote: &str,
    local: &Path,
    filter: &Filter,
    policy: OverwritePolicy,
    cb: XferCb,
) -> Result<u64> {
    fs::create_dir_all(local)?;
    let entries = t.list_dir(remote)?;
    let mut bytes = 0u64;
    for e in entries {
        if filter.excludes(&e.name, e.is_dir) {
            continue;
        }
        if e.is_dir && e.is_symlink {
            // Never recurse through symlinked directories: they can form
            // cycles and point outside the tree being copied.
            continue;
        }
        let child_remote = join(remote, &e.name);
        let child_local = local.join(&e.name);
        if e.is_dir {
            bytes += download_dir_opts(t, &child_remote, &child_local, filter, policy, cb)?;
        } else {
            let dst = fs::metadata(&child_local).ok().map(|m| (m.len(), mtime_unix(&m)));
            if policy_allows(policy, e.size, e.mtime, dst) {
                bytes += one_file(t, &child_remote, &child_local, e.size, true, cb)?;
            }
        }
    }
    Ok(bytes)
}

/// Upload a local directory tree under `remote` (created if needed).
pub fn upload_dir(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    filter: &Filter,
    cb: XferCb,
) -> Result<u64> {
    upload_dir_opts(t, local, remote, filter, OverwritePolicy::Overwrite, cb)
}

/// Like [`upload_dir`] but skips/overwrites existing files per `policy`.
pub fn upload_dir_opts(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    filter: &Filter,
    policy: OverwritePolicy,
    cb: XferCb,
) -> Result<u64> {
    // Tolerate "already exists" — there is no portable way to distinguish it
    // across protocols, and a genuinely broken connection fails on use anyway.
    let _ = t.mkdir(remote);
    // Skip/OnlyNewer need to know what is already on the server; Overwrite
    // doesn't care, so we avoid the extra round-trip in that case.
    let remote_entries: Vec<Entry> = if policy == OverwritePolicy::Overwrite {
        Vec::new()
    } else {
        t.list_dir(remote).unwrap_or_default()
    };
    let mut bytes = 0u64;
    for entry in fs::read_dir(local)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type()?.is_dir();
        if filter.excludes(&name, is_dir) {
            continue;
        }
        let child_remote = join(remote, &name);
        let child_local = entry.path();
        if is_dir {
            bytes += upload_dir_opts(t, &child_local, &child_remote, filter, policy, cb)?;
        } else {
            let meta = entry.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let src_mtime = meta.as_ref().and_then(mtime_unix);
            let dst = remote_entries
                .iter()
                .find(|e| e.name == name && !e.is_dir)
                .map(|e| (e.size, e.mtime));
            if policy_allows(policy, size, src_mtime, dst) {
                bytes += one_file(t, &child_remote, &child_local, size, false, cb)?;
            }
        }
    }
    Ok(bytes)
}

/// Per-file copy decision for a folder transfer under an [`OverwritePolicy`].
fn policy_allows(
    policy: OverwritePolicy,
    src_size: u64,
    src_mtime: Option<i64>,
    dst: Option<(u64, Option<i64>)>,
) -> bool {
    match policy {
        OverwritePolicy::Overwrite => true,
        OverwritePolicy::Skip => dst.is_none(),
        OverwritePolicy::OnlyNewer => needs_copy(src_size, src_mtime, dst),
    }
}

/// Recursively delete a remote directory. Symlinks are unlinked, never
/// followed — recursing through a link would delete the *target's* contents.
pub fn remove_dir_all(t: &mut dyn Transport, path: &str) -> Result<()> {
    for e in t.list_dir(path)? {
        let child = join(path, &e.name);
        if e.is_dir && !e.is_symlink {
            remove_dir_all(t, &child)?;
        } else {
            // Plain file, or a symlink (to anything): remove the node itself.
            t.remove_file(&child)?;
        }
    }
    t.remove_dir(path)
}

/// One-way directory synchronization. A file is copied when it is missing on
/// the destination, differs in size, or the source is newer (when both sides
/// report mtimes; a 2-second tolerance absorbs filesystem granularity).
pub fn sync_dir(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
    cb: XferCb,
) -> Result<SyncStats> {
    sync_dir_opts(t, local, remote, direction, filter, cb, &SyncOptions::default())
}

/// Like [`sync_dir`] but with extra options (mirror delete mode).
pub fn sync_dir_opts(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
    cb: XferCb,
    opts: &SyncOptions,
) -> Result<SyncStats> {
    let mut stats = SyncStats::default();
    sync_inner(t, local, remote, direction, filter, cb, &mut stats)?;
    if opts.delete {
        delete_extraneous(t, local, remote, direction, filter)?;
    }
    Ok(stats)
}

fn sync_inner(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
    cb: XferCb,
    stats: &mut SyncStats,
) -> Result<()> {
    match direction {
        SyncDirection::Upload => {
            let _ = t.mkdir(remote);
            let remote_entries: Vec<Entry> = t.list_dir(remote).unwrap_or_default();
            for entry in fs::read_dir(local)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type()?.is_dir();
                if filter.excludes(&name, is_dir) {
                    continue;
                }
                let child_remote = join(remote, &name);
                if is_dir {
                    sync_inner(t, &entry.path(), &child_remote, direction, filter, cb, stats)?;
                    continue;
                }
                let meta = entry.metadata()?;
                let src_size = meta.len();
                let src_mtime = mtime_unix(&meta);
                let dst = remote_entries.iter().find(|e| e.name == name && !e.is_dir);
                if needs_copy(src_size, src_mtime, dst.map(|e| (e.size, e.mtime))) {
                    stats.bytes += one_file(t, &child_remote, &entry.path(), src_size, false, cb)?;
                    stats.copied += 1;
                } else {
                    stats.skipped += 1;
                }
            }
        }
        SyncDirection::Download => {
            fs::create_dir_all(local)?;
            for e in t.list_dir(remote)? {
                if filter.excludes(&e.name, e.is_dir) {
                    continue;
                }
                if e.is_dir && e.is_symlink {
                    continue; // never sync through symlinked directories
                }
                let child_remote = join(remote, &e.name);
                let child_local = local.join(&e.name);
                if e.is_dir {
                    sync_inner(t, &child_local, &child_remote, direction, filter, cb, stats)?;
                    continue;
                }
                let dst = fs::metadata(&child_local)
                    .ok()
                    .map(|m| (m.len(), mtime_unix(&m)));
                if needs_copy(e.size, e.mtime, dst) {
                    stats.bytes += one_file(t, &child_remote, &child_local, e.size, true, cb)?;
                    stats.copied += 1;
                } else {
                    stats.skipped += 1;
                }
            }
        }
    }
    Ok(())
}

/// Copy decision: missing destination, size difference, or newer source.
fn needs_copy(
    src_size: u64,
    src_mtime: Option<i64>,
    dst: Option<(u64, Option<i64>)>,
) -> bool {
    let Some((dst_size, dst_mtime)) = dst else { return true };
    if src_size != dst_size {
        return true;
    }
    match (src_mtime, dst_mtime) {
        (Some(s), Some(d)) => s > d + 2,
        _ => false, // same size, no mtime info — assume unchanged
    }
}

fn one_file(
    t: &mut dyn Transport,
    remote: &str,
    local: &Path,
    size: u64,
    download: bool,
    cb: XferCb,
) -> Result<u64> {
    if !cb(XferEvent::Start { name: remote, total: size, download }) {
        return Err(Error::Cancelled);
    }
    let mut progress = |done: u64, total: u64| cb(XferEvent::Bytes { done, total });
    let n = if download {
        t.download_progress(remote, local, &mut progress)?
    } else {
        t.upload_progress(local, remote, &mut progress)?
    };
    if !cb(XferEvent::DoneFile) {
        return Err(Error::Cancelled);
    }
    Ok(n)
}

/// Remove destination items that have no counterpart on the source side.
/// Called only in mirror mode; errors deleting a single file are non-fatal.
fn delete_extraneous(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
) -> Result<()> {
    match direction {
        SyncDirection::Upload => {
            // Delete remote files not present locally.
            for entry in t.list_dir(remote).unwrap_or_default() {
                if filter.excludes(&entry.name, entry.is_dir) {
                    continue;
                }
                let child_remote = join(remote, &entry.name);
                let child_local = local.join(&entry.name);
                if entry.is_dir && !entry.is_symlink {
                    if child_local.is_dir() {
                        delete_extraneous(t, &child_local, &child_remote, direction, filter)?;
                    } else {
                        let _ = remove_dir_all(t, &child_remote);
                    }
                } else if !child_local.exists() {
                    let _ = t.remove_file(&child_remote);
                }
            }
        }
        SyncDirection::Download => {
            // Delete local files not present on the remote.
            let remote_entries = t.list_dir(remote).unwrap_or_default();
            let Ok(rd) = fs::read_dir(local) else { return Ok(()); };
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if filter.excludes(&name, is_dir) {
                    continue;
                }
                if !remote_entries.iter().any(|e| e.name == name) {
                    let path = entry.path();
                    if is_dir {
                        let _ = fs::remove_dir_all(&path);
                    } else {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Collect delete candidates for the plan dry-run (mirror mode).
fn collect_deletes(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
    filter: &Filter,
    rel: &str,
    plan: &mut SyncPlan,
) -> Result<()> {
    let rel_join = |name: &str| {
        if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") }
    };
    match direction {
        SyncDirection::Upload => {
            for entry in t.list_dir(remote).unwrap_or_default() {
                if filter.excludes(&entry.name, entry.is_dir) { continue; }
                let child_local = local.join(&entry.name);
                if !child_local.exists() {
                    plan.deletes.push(rel_join(&entry.name));
                }
            }
        }
        SyncDirection::Download => {
            let remote_entries = t.list_dir(remote).unwrap_or_default();
            let Ok(rd) = fs::read_dir(local) else { return Ok(()); };
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if filter.excludes(&name, is_dir) { continue; }
                if !remote_entries.iter().any(|e| e.name == name) {
                    plan.deletes.push(rel_join(&name));
                }
            }
        }
    }
    Ok(())
}

fn mtime_unix(meta: &fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

fn join(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::transport::Progress;

    /// In-memory Transport: object keys → contents, plus an explicit dir set.
    /// Paths in `symlinks` are reported as symlinked directories.
    #[derive(Default)]
    struct FakeTransport {
        files: BTreeMap<String, Vec<u8>>,
        dirs: std::collections::BTreeSet<String>,
        symlinks: std::collections::BTreeSet<String>,
    }

    impl FakeTransport {
        fn norm(p: &str) -> String {
            let t = p.trim_end_matches('/');
            if t.is_empty() { "/".into() } else { t.into() }
        }
    }

    impl Transport for FakeTransport {
        fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
            let base = Self::norm(path);
            let prefix = if base == "/" { "/".to_string() } else { format!("{base}/") };
            let mut out = Vec::new();
            for d in &self.dirs {
                if let Some(rest) = d.strip_prefix(&prefix) {
                    if !rest.is_empty() && !rest.contains('/') {
                        out.push(Entry {
                            name: rest.into(),
                            is_dir: true,
                            size: 0,
                            mtime: None,
                            perms: None,
                            is_symlink: self.symlinks.contains(d),
                            uid: None,
                            gid: None,
                        });
                    }
                }
            }
            for (k, v) in &self.files {
                if let Some(rest) = k.strip_prefix(&prefix) {
                    if !rest.is_empty() && !rest.contains('/') {
                        out.push(Entry {
                            name: rest.into(),
                            is_dir: false,
                            size: v.len() as u64,
                            mtime: None,
                            perms: None,
                            is_symlink: false,
                            uid: None,
                            gid: None,
                        });
                    }
                }
            }
            Ok(out)
        }

        fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress) -> Result<u64> {
            let key = Self::norm(remote);
            let data = self
                .files
                .get(&key)
                .ok_or_else(|| Error::Protocol(format!("no such file {key}")))?
                .clone();
            if !progress(data.len() as u64, data.len() as u64) {
                return Err(Error::Cancelled);
            }
            fs::write(local, &data)?;
            Ok(data.len() as u64)
        }

        fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
            let data = fs::read(local)?;
            if !progress(data.len() as u64, data.len() as u64) {
                return Err(Error::Cancelled);
            }
            self.files.insert(Self::norm(remote), data.clone());
            Ok(data.len() as u64)
        }

        fn mkdir(&mut self, path: &str) -> Result<()> {
            self.dirs.insert(Self::norm(path));
            Ok(())
        }

        fn remove_file(&mut self, path: &str) -> Result<()> {
            let key = Self::norm(path);
            if self.files.remove(&key).is_some() {
                return Ok(());
            }
            // Unlinking a symlinked directory removes the link node only.
            if self.symlinks.remove(&key) {
                self.dirs.remove(&key);
                return Ok(());
            }
            Err(Error::Protocol("no such file".into()))
        }

        fn remove_dir(&mut self, path: &str) -> Result<()> {
            self.dirs.remove(&Self::norm(path));
            Ok(())
        }

        fn rename(&mut self, from: &str, to: &str) -> Result<()> {
            let data = self
                .files
                .remove(&Self::norm(from))
                .ok_or_else(|| Error::Protocol("no such file".into()))?;
            self.files.insert(Self::norm(to), data);
            Ok(())
        }
    }

    fn fake_with(files: &[(&str, &str)], dirs: &[&str]) -> FakeTransport {
        let mut t = FakeTransport::default();
        for (k, v) in files {
            t.files.insert(k.to_string(), v.as_bytes().to_vec());
        }
        for d in dirs {
            t.dirs.insert(d.to_string());
        }
        t
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scp-core-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn download_dir_recurses_and_reports() {
        let mut t = fake_with(
            &[("/docs/a.txt", "aaa"), ("/docs/sub/b.txt", "bbbb")],
            &["/docs", "/docs/sub"],
        );
        let local = tempdir("dl");
        let mut started = Vec::new();
        let bytes = download_dir(&mut t, "/docs", &local, &Filter::empty(), &mut |ev| {
            if let XferEvent::Start { name, .. } = ev {
                started.push(name.to_string());
            }
            true
        })
        .unwrap();
        assert_eq!(bytes, 7);
        assert_eq!(fs::read_to_string(local.join("a.txt")).unwrap(), "aaa");
        assert_eq!(fs::read_to_string(local.join("sub/b.txt")).unwrap(), "bbbb");
        assert_eq!(started.len(), 2);
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn upload_dir_recurses() {
        let local = tempdir("ul");
        fs::write(local.join("x.bin"), b"12345").unwrap();
        fs::create_dir(local.join("nested")).unwrap();
        fs::write(local.join("nested/y.bin"), b"67").unwrap();

        let mut t = FakeTransport::default();
        let bytes = upload_dir(&mut t, &local, "/up", &Filter::empty(), &mut |_| true).unwrap();
        assert_eq!(bytes, 7);
        assert_eq!(t.files.get("/up/x.bin").unwrap(), b"12345");
        assert_eq!(t.files.get("/up/nested/y.bin").unwrap(), b"67");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn upload_dir_skip_policy_keeps_existing_copies_new() {
        let local = tempdir("ul-skip");
        fs::write(local.join("keep.bin"), b"NEWLOCAL").unwrap();
        fs::write(local.join("fresh.bin"), b"hi").unwrap();

        // Remote already has keep.bin (different content) but not fresh.bin.
        let mut t = fake_with(&[("/up/keep.bin", "OLDREMOTE")], &["/up"]);
        let bytes =
            upload_dir_opts(&mut t, &local, "/up", &Filter::empty(), OverwritePolicy::Skip, &mut |_| true)
                .unwrap();
        // Only fresh.bin (2 bytes) is copied; keep.bin is left untouched.
        assert_eq!(bytes, 2);
        assert_eq!(t.files.get("/up/keep.bin").unwrap(), b"OLDREMOTE");
        assert_eq!(t.files.get("/up/fresh.bin").unwrap(), b"hi");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn download_dir_skip_policy_keeps_existing_copies_new() {
        let mut t = fake_with(&[("/d/keep.txt", "REMOTE"), ("/d/new.txt", "added")], &["/d"]);
        let local = tempdir("dl-skip");
        fs::write(local.join("keep.txt"), b"LOCALWINS").unwrap();

        download_dir_opts(&mut t, "/d", &local, &Filter::empty(), OverwritePolicy::Skip, &mut |_| true)
            .unwrap();
        // keep.txt already existed locally → untouched; new.txt is pulled down.
        assert_eq!(fs::read_to_string(local.join("keep.txt")).unwrap(), "LOCALWINS");
        assert_eq!(fs::read_to_string(local.join("new.txt")).unwrap(), "added");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn upload_dir_overwrite_policy_replaces_existing() {
        let local = tempdir("ul-ow");
        fs::write(local.join("f.bin"), b"NEW").unwrap();
        let mut t = fake_with(&[("/up/f.bin", "OLD")], &["/up"]);
        upload_dir_opts(&mut t, &local, "/up", &Filter::empty(), OverwritePolicy::Overwrite, &mut |_| true)
            .unwrap();
        assert_eq!(t.files.get("/up/f.bin").unwrap(), b"NEW");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn cancellation_stops_mid_operation() {
        let mut t = fake_with(
            &[("/d/1", "x"), ("/d/2", "y"), ("/d/3", "z")],
            &["/d"],
        );
        let local = tempdir("cancel");
        let mut starts = 0;
        let err = download_dir(&mut t, "/d", &local, &Filter::empty(), &mut |ev| {
            if matches!(ev, XferEvent::Start { .. }) {
                starts += 1;
                return starts < 2; // cancel at the second file
            }
            true
        })
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(starts, 2);
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn remove_dir_all_unlinks_symlinked_dirs_without_recursing() {
        let mut t = fake_with(&[("/d/file", "x"), ("/elsewhere/secret", "s")], &["/d", "/elsewhere"]);
        // /d/link is a symlinked dir; its "target" /elsewhere must survive.
        t.dirs.insert("/d/link".into());
        t.symlinks.insert("/d/link".into());
        remove_dir_all(&mut t, "/d").unwrap();
        assert!(t.files.contains_key("/elsewhere/secret"), "deleted through a symlink!");
        assert!(!t.dirs.contains("/d"));
        assert!(!t.dirs.contains("/d/link"));
    }

    #[test]
    fn download_dir_skips_symlinked_dirs() {
        let mut t = fake_with(&[("/d/real.txt", "ok"), ("/d/link/inner.txt", "no")], &["/d"]);
        t.dirs.insert("/d/link".into());
        t.symlinks.insert("/d/link".into());
        let local = tempdir("symdl");
        download_dir(&mut t, "/d", &local, &Filter::empty(), &mut |_| true).unwrap();
        assert!(local.join("real.txt").exists());
        assert!(!local.join("link").exists(), "recursed through a symlink!");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn remove_dir_all_clears_tree() {
        let mut t = fake_with(
            &[("/d/1", "x"), ("/d/sub/2", "y")],
            &["/d", "/d/sub"],
        );
        remove_dir_all(&mut t, "/d").unwrap();
        assert!(t.files.is_empty());
        assert!(t.dirs.is_empty());
    }

    #[test]
    fn sync_upload_copies_missing_and_changed_only() {
        let local = tempdir("sync");
        fs::write(local.join("same.txt"), b"unchanged").unwrap();
        fs::write(local.join("bigger.txt"), b"now-longer").unwrap();
        fs::write(local.join("new.txt"), b"fresh").unwrap();

        let mut t = fake_with(
            &[("/r/same.txt", "unchanged"), ("/r/bigger.txt", "short")],
            &["/r"],
        );
        let stats = sync_dir(&mut t, &local, "/r", SyncDirection::Upload, &Filter::empty(), &mut |_| true).unwrap();
        assert_eq!(stats.copied, 2); // bigger.txt (size diff) + new.txt (missing)
        assert_eq!(stats.skipped, 1); // same.txt
        assert_eq!(t.files.get("/r/bigger.txt").unwrap(), b"now-longer");
        assert_eq!(t.files.get("/r/new.txt").unwrap(), b"fresh");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn filter_masks_work() {
        let f = Filter::parse("*.tmp; .git/; node_modules/");
        assert!(f.excludes("a.tmp", false));
        assert!(f.excludes("A.TMP", false));
        assert!(!f.excludes("a.tmpx", false));
        assert!(f.excludes(".git", true));
        assert!(!f.excludes(".git", false)); // dir-only pattern
        assert!(f.excludes("node_modules", true));
        assert!(!f.excludes("main.rs", false));
        assert!(!Filter::empty().excludes("anything", true));
    }

    #[test]
    fn upload_dir_respects_filter() {
        let local = tempdir("flt");
        fs::write(local.join("keep.rs"), b"k").unwrap();
        fs::write(local.join("skip.tmp"), b"s").unwrap();
        fs::create_dir(local.join(".git")).unwrap();
        fs::write(local.join(".git/config"), b"g").unwrap();
        let mut t = FakeTransport::default();
        upload_dir(&mut t, &local, "/up", &Filter::parse("*.tmp; .git/"), &mut |_| true)
            .unwrap();
        assert!(t.files.contains_key("/up/keep.rs"));
        assert!(!t.files.contains_key("/up/skip.tmp"));
        assert!(!t.files.contains_key("/up/.git/config"));
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn plan_sync_reports_without_copying() {
        let local = tempdir("plan");
        fs::write(local.join("same.txt"), b"unchanged").unwrap();
        fs::write(local.join("new.txt"), b"fresh").unwrap();
        fs::create_dir(local.join("sub")).unwrap();
        fs::write(local.join("sub/inner.txt"), b"i").unwrap();

        let mut t = fake_with(&[("/r/same.txt", "unchanged")], &["/r"]);
        let before = t.files.clone();
        let plan =
            plan_sync(&mut t, &local, "/r", SyncDirection::Upload, &Filter::empty()).unwrap();
        assert_eq!(t.files, before, "plan must not copy anything");
        let rels: Vec<&str> = plan.items.iter().map(|i| i.rel.as_str()).collect();
        assert!(rels.contains(&"new.txt"));
        assert!(rels.contains(&"sub/inner.txt"));
        assert!(!rels.contains(&"same.txt"));
        assert!(plan.dirs.contains(&"sub".to_string()));
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn find_matches_masks_recursively() {
        let mut t = fake_with(
            &[("/r/a.log", "1"), ("/r/sub/b.log", "2"), ("/r/sub/c.txt", "3")],
            &["/r", "/r/sub"],
        );
        let hits = find(&mut t, "/r", "*.log", 100, &mut || true).unwrap();
        let mut paths: Vec<&str> = hits.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["/r/a.log", "/r/sub/b.log"]);
    }

    #[test]
    fn sync_download_mirror() {
        let mut t = fake_with(&[("/r/only-remote.txt", "hello")], &["/r"]);
        let local = tempdir("sync-dl");
        let stats =
            sync_dir(&mut t, &local, "/r", SyncDirection::Download, &Filter::empty(), &mut |_| true).unwrap();
        assert_eq!(stats.copied, 1);
        assert_eq!(
            fs::read_to_string(local.join("only-remote.txt")).unwrap(),
            "hello"
        );
        // Second run: nothing to do.
        let stats2 =
            sync_dir(&mut t, &local, "/r", SyncDirection::Download, &Filter::empty(), &mut |_| true).unwrap();
        assert_eq!(stats2.copied, 0);
        assert_eq!(stats2.skipped, 1);
        let _ = fs::remove_dir_all(&local);
    }
}
