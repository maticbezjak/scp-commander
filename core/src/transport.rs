use std::path::Path;

use crate::ftp::FtpTransport;
use crate::s3::S3Transport;
use crate::sftp::SftpTransport;
use crate::types::{Credentials, Entry, Protocol, Result};

/// Progress callback: `(bytes_transferred, total_bytes)`. `total` is 0 when the
/// size is unknown up front.
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// A live connection to a server. Every protocol backend implements this, so
/// the UI layers only ever talk to `dyn Transport` and never know which
/// protocol is underneath.
pub trait Transport: Send {
    /// List the entries of a remote directory.
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>>;

    /// Download a remote file to a local path. Returns bytes transferred.
    fn download(&mut self, remote: &str, local: &Path) -> Result<u64>;

    /// Upload a local file to a remote path. Returns bytes transferred.
    fn upload(&mut self, local: &Path, remote: &str) -> Result<u64>;

    /// Download while reporting progress. Default delegates to [`download`] and
    /// reports a single final tick; backends that can stream override this for
    /// granular updates.
    fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress) -> Result<u64> {
        let n = self.download(remote, local)?;
        progress(n, n);
        Ok(n)
    }

    /// Upload while reporting progress. Default delegates to [`upload`].
    fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        let n = self.upload(local, remote)?;
        progress(n, n);
        Ok(n)
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
