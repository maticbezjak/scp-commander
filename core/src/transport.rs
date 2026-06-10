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
pub fn connect(creds: &Credentials) -> Result<Box<dyn Transport>> {
    match creds.protocol {
        Protocol::Sftp => Ok(Box::new(SftpTransport::connect(creds)?)),
        Protocol::Ftp | Protocol::Ftps => Ok(Box::new(FtpTransport::connect(creds)?)),
        Protocol::S3 => Ok(Box::new(S3Transport::connect(creds)?)),
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
