use std::path::Path;

use crate::transport::Transport;
use crate::types::{Credentials, Entry, Error, Result};

/// S3 / S3-compatible object storage backend.
///
/// Not implemented yet. The real version will pull an async S3 client
/// (e.g. `aws-sdk-s3` or `rust-s3`) behind the `s3` cargo feature and bridge
/// it to this synchronous trait with a small blocking runtime. Keeping it a
/// stub here means the first build stays fast and dependency-light while the
/// trait + dispatch wiring is already proven.
pub struct S3Transport;

impl S3Transport {
    pub fn connect(_creds: &Credentials) -> Result<Self> {
        Err(Error::NotImplemented(
            "S3 backend is not implemented yet (enable the `s3` feature once added)".into(),
        ))
    }
}

impl Transport for S3Transport {
    fn list_dir(&mut self, _path: &str) -> Result<Vec<Entry>> {
        Err(Error::NotImplemented("S3 list_dir".into()))
    }
    fn download(&mut self, _remote: &str, _local: &Path) -> Result<u64> {
        Err(Error::NotImplemented("S3 download".into()))
    }
    fn upload(&mut self, _local: &Path, _remote: &str) -> Result<u64> {
        Err(Error::NotImplemented("S3 upload".into()))
    }
}
