//! Shared transfer core for the cross-platform file manager.
//!
//! All protocol logic, the transfer engine, and the sync engine live here with
//! no UI dependency. The Tauri app (`ui-tauri`) and the CLI link it directly as
//! a Rust `rlib`.

pub mod ftp;
pub mod jump;
pub mod ops;
pub mod s3;
pub mod sftp;
pub mod transport;
pub mod types;

pub use transport::{connect, Transport};
pub use types::{Auth, Credentials, Entry, Error, HostKeyPolicy, Protocol, Result};

use std::sync::atomic::{AtomicBool, Ordering};

/// When set, fresh uploads (SFTP/FTP) write to a temporary name and rename into
/// place on success, so an interrupted upload never leaves a truncated file at
/// the real name. Process-global: shared by every pooled connection.
static ATOMIC_UPLOADS: AtomicBool = AtomicBool::new(true);

/// Enable/disable atomic uploads (default enabled).
pub fn set_atomic_uploads(on: bool) {
    ATOMIC_UPLOADS.store(on, Ordering::Relaxed);
}

/// Whether atomic uploads are currently enabled.
pub fn atomic_uploads_enabled() -> bool {
    ATOMIC_UPLOADS.load(Ordering::Relaxed)
}

/// A sibling temp path for `remote` (".<name>.scp-part.<pid>.<n>") used while an
/// atomic upload is in flight. Unique per process and call to avoid collisions.
pub(crate) fn upload_temp_name(remote: &str) -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let (dir, base) = match remote.rfind('/') {
        Some(i) => (&remote[..=i], &remote[i + 1..]),
        None => ("", remote),
    };
    format!("{dir}.{base}.scp-part.{pid}.{n}")
}
