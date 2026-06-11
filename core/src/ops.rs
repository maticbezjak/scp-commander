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

/// Direction for [`sync_dir`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// Local is the source of truth; copy missing/changed files to the server.
    Upload,
    /// Remote is the source of truth; copy missing/changed files locally.
    Download,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub copied: usize,
    pub skipped: usize,
    pub bytes: u64,
}

/// Download a remote directory tree into `local` (created if needed).
pub fn download_dir(
    t: &mut dyn Transport,
    remote: &str,
    local: &Path,
    cb: XferCb,
) -> Result<u64> {
    fs::create_dir_all(local)?;
    let entries = t.list_dir(remote)?;
    let mut bytes = 0u64;
    for e in entries {
        if e.is_dir && e.is_symlink {
            // Never recurse through symlinked directories: they can form
            // cycles and point outside the tree being copied.
            continue;
        }
        let child_remote = join(remote, &e.name);
        let child_local = local.join(&e.name);
        if e.is_dir {
            bytes += download_dir(t, &child_remote, &child_local, cb)?;
        } else {
            bytes += one_file(t, &child_remote, &child_local, e.size, true, cb)?;
        }
    }
    Ok(bytes)
}

/// Upload a local directory tree under `remote` (created if needed).
pub fn upload_dir(t: &mut dyn Transport, local: &Path, remote: &str, cb: XferCb) -> Result<u64> {
    // Tolerate "already exists" — there is no portable way to distinguish it
    // across protocols, and a genuinely broken connection fails on use anyway.
    let _ = t.mkdir(remote);
    let mut bytes = 0u64;
    for entry in fs::read_dir(local)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_remote = join(remote, &name);
        let child_local = entry.path();
        if entry.file_type()?.is_dir() {
            bytes += upload_dir(t, &child_local, &child_remote, cb)?;
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            bytes += one_file(t, &child_remote, &child_local, size, false, cb)?;
        }
    }
    Ok(bytes)
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
    cb: XferCb,
) -> Result<SyncStats> {
    let mut stats = SyncStats::default();
    sync_inner(t, local, remote, direction, cb, &mut stats)?;
    Ok(stats)
}

fn sync_inner(
    t: &mut dyn Transport,
    local: &Path,
    remote: &str,
    direction: SyncDirection,
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
                let child_remote = join(remote, &name);
                if entry.file_type()?.is_dir() {
                    sync_inner(t, &entry.path(), &child_remote, direction, cb, stats)?;
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
                if e.is_dir && e.is_symlink {
                    continue; // never sync through symlinked directories
                }
                let child_remote = join(remote, &e.name);
                let child_local = local.join(&e.name);
                if e.is_dir {
                    sync_inner(t, &child_local, &child_remote, direction, cb, stats)?;
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
        let bytes = download_dir(&mut t, "/docs", &local, &mut |ev| {
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
        let bytes = upload_dir(&mut t, &local, "/up", &mut |_| true).unwrap();
        assert_eq!(bytes, 7);
        assert_eq!(t.files.get("/up/x.bin").unwrap(), b"12345");
        assert_eq!(t.files.get("/up/nested/y.bin").unwrap(), b"67");
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
        let err = download_dir(&mut t, "/d", &local, &mut |ev| {
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
        download_dir(&mut t, "/d", &local, &mut |_| true).unwrap();
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
        let stats = sync_dir(&mut t, &local, "/r", SyncDirection::Upload, &mut |_| true).unwrap();
        assert_eq!(stats.copied, 2); // bigger.txt (size diff) + new.txt (missing)
        assert_eq!(stats.skipped, 1); // same.txt
        assert_eq!(t.files.get("/r/bigger.txt").unwrap(), b"now-longer");
        assert_eq!(t.files.get("/r/new.txt").unwrap(), b"fresh");
        let _ = fs::remove_dir_all(&local);
    }

    #[test]
    fn sync_download_mirror() {
        let mut t = fake_with(&[("/r/only-remote.txt", "hello")], &["/r"]);
        let local = tempdir("sync-dl");
        let stats =
            sync_dir(&mut t, &local, "/r", SyncDirection::Download, &mut |_| true).unwrap();
        assert_eq!(stats.copied, 1);
        assert_eq!(
            fs::read_to_string(local.join("only-remote.txt")).unwrap(),
            "hello"
        );
        // Second run: nothing to do.
        let stats2 =
            sync_dir(&mut t, &local, "/r", SyncDirection::Download, &mut |_| true).unwrap();
        assert_eq!(stats2.copied, 0);
        assert_eq!(stats2.skipped, 1);
        let _ = fs::remove_dir_all(&local);
    }
}
