use std::io::{Read, Write};
use std::path::Path;

use crate::ftp::FtpTransport;
use crate::s3::S3Transport;
use crate::sftp::SftpTransport;
use crate::types::{Credentials, Entry, Error, Protocol, Result};

/// Progress callback: `(bytes_transferred, total_bytes)`. `total` is 0 when the
/// size is unknown up front. Return `false` to cancel the transfer (it fails
/// with [`Error::Cancelled`]).
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64) -> bool;

/// A live connection to a server. Every protocol backend implements this, so
/// the UI layers only ever talk to `dyn Transport` and never know which
/// protocol is underneath.
pub trait Transport: Send {
    /// List the entries of a remote directory.
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>>;

    /// Download a remote file, streaming progress. Returns bytes transferred.
    fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress)
        -> Result<u64>;

    /// Upload a local file, streaming progress. Returns bytes transferred.
    fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64>;

    /// Create a remote directory.
    fn mkdir(&mut self, path: &str) -> Result<()>;

    /// Delete a remote file.
    fn remove_file(&mut self, path: &str) -> Result<()>;

    /// Delete an (empty) remote directory. Use [`crate::ops::remove_dir_all`]
    /// for recursive deletion.
    fn remove_dir(&mut self, path: &str) -> Result<()>;

    /// Rename/move a remote file or directory.
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;

    /// Change unix permissions (e.g. 0o644). Not every protocol supports
    /// this; the default refuses.
    fn set_permissions(&mut self, _path: &str, _mode: u32) -> Result<()> {
        Err(Error::NotImplemented(
            "permissions are not supported on this protocol".into(),
        ))
    }

    /// Resume a download from `offset` bytes, appending to the local file.
    /// Progress reports overall position (offset included). The default
    /// refuses; SFTP and FTP support it.
    fn download_resume(
        &mut self,
        _remote: &str,
        _local: &Path,
        _offset: u64,
        _progress: Progress,
    ) -> Result<u64> {
        Err(Error::NotImplemented(
            "resume is not supported on this protocol".into(),
        ))
    }

    /// Resume an upload: append the local file's tail after the remote
    /// file's current size (taken from the server). Returns total bytes at
    /// the remote afterwards. The default refuses; SFTP and FTP support it.
    fn upload_resume(&mut self, _local: &Path, _remote: &str, _progress: Progress)
        -> Result<u64>
    {
        Err(Error::NotImplemented(
            "upload resume is not supported on this protocol".into(),
        ))
    }

    /// Liveness probe / keepalive. Errors when the connection is dead.
    /// Also keeps NAT mappings warm when called periodically while idle.
    fn keepalive(&mut self) -> Result<()> {
        Ok(())
    }

    /// Download without progress reporting.
    fn download(&mut self, remote: &str, local: &Path) -> Result<u64> {
        self.download_progress(remote, local, &mut |_, _| true)
    }

    /// Upload without progress reporting.
    fn upload(&mut self, local: &Path, remote: &str) -> Result<u64> {
        self.upload_progress(local, remote, &mut |_, _| true)
    }

    /// Close the session. Default is a no-op; backends override if needed.
    fn disconnect(&mut self) {}
}

/// Open a connection using the given credentials, dispatching on protocol.
/// The returned transport transparently reconnects once when the session
/// turns out to be dead (network blip, NAT timeout) and retries the failed
/// operation.
pub fn connect(creds: &Credentials) -> Result<Box<dyn Transport>> {
    let inner = connect_raw(creds)?;
    Ok(Box::new(AutoReconnect {
        inner,
        creds: creds.clone(),
    }))
}

fn connect_raw(creds: &Credentials) -> Result<Box<dyn Transport>> {
    match creds.protocol {
        Protocol::Sftp => Ok(Box::new(SftpTransport::connect(creds)?)),
        Protocol::Ftp | Protocol::Ftps => Ok(Box::new(FtpTransport::connect(creds)?)),
        Protocol::S3 => Ok(Box::new(S3Transport::connect(creds)?)),
    }
}

/// Wrapper that revives dead sessions: when an operation fails AND a liveness
/// probe also fails, it reconnects with the stored credentials and retries
/// the operation once. Failures on a live session surface unchanged, so
/// ordinary errors (file not found, permission denied) never trigger churn.
struct AutoReconnect {
    inner: Box<dyn Transport>,
    creds: Credentials,
}

impl AutoReconnect {
    fn retryable(e: &Error) -> bool {
        matches!(e, Error::Io(_) | Error::Connect(_) | Error::Protocol(_))
    }

    fn run<T>(&mut self, mut op: impl FnMut(&mut dyn Transport) -> Result<T>) -> Result<T> {
        match op(self.inner.as_mut()) {
            Err(e) if Self::retryable(&e) => {
                if self.inner.keepalive().is_ok() {
                    return Err(e); // session alive — genuine failure
                }
                let mut fresh = connect_raw(&self.creds)?;
                let result = op(fresh.as_mut());
                self.inner = fresh;
                result
            }
            other => other,
        }
    }
}

impl Transport for AutoReconnect {
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
        self.run(|t| t.list_dir(path))
    }

    fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress)
        -> Result<u64>
    {
        self.run(|t| t.download_progress(remote, local, &mut *progress))
    }

    fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        self.run(|t| t.upload_progress(local, remote, &mut *progress))
    }

    fn download_resume(
        &mut self,
        remote: &str,
        local: &Path,
        offset: u64,
        progress: Progress,
    ) -> Result<u64> {
        self.run(|t| t.download_resume(remote, local, offset, &mut *progress))
    }

    fn upload_resume(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        self.run(|t| t.upload_resume(local, remote, &mut *progress))
    }

    fn mkdir(&mut self, path: &str) -> Result<()> {
        self.run(|t| t.mkdir(path))
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        self.run(|t| t.remove_file(path))
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        self.run(|t| t.remove_dir(path))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.run(|t| t.rename(from, to))
    }

    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()> {
        self.run(|t| t.set_permissions(path, mode))
    }

    fn keepalive(&mut self) -> Result<()> {
        self.inner.keepalive()
    }

    fn disconnect(&mut self) {
        self.inner.disconnect();
    }
}

/// Copy reader → writer in chunks, reporting `(transferred, total)` after each
/// and honouring cancellation.
pub(crate) fn copy_with_progress<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    total: u64,
    progress: Progress,
) -> Result<u64> {
    let mut buf = [0u8; 64 * 1024];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        done += n as u64;
        if !progress(done, total) {
            return Err(Error::Cancelled);
        }
    }
    Ok(done)
}
