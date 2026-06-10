//! S3 / S3-compatible object storage backend.
//!
//! Real implementation is gated behind the `s3` cargo feature (pulls rust-s3).
//! Without the feature, a stub keeps the trait + dispatch wiring intact while
//! avoiding the extra dependency weight.

#[cfg(not(feature = "s3"))]
mod imp {
    use std::path::Path;

    use crate::transport::Transport;
    use crate::types::{Credentials, Entry, Error, Result};

    pub struct S3Transport;

    impl S3Transport {
        pub fn connect(_creds: &Credentials) -> Result<Self> {
            Err(Error::NotImplemented(
                "S3 backend not built (compile with --features s3)".into(),
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
}

#[cfg(feature = "s3")]
mod imp {
    use std::fs;
    use std::path::Path;

    use s3::creds::Credentials as AwsCreds;
    use s3::{Bucket, Region};

    use crate::transport::Transport;
    use crate::types::{Auth, Credentials, Entry, Error, Result};

    pub struct S3Transport {
        bucket: Box<Bucket>,
    }

    impl S3Transport {
        pub fn connect(creds: &Credentials) -> Result<Self> {
            let bucket_name = creds
                .bucket
                .as_deref()
                .ok_or_else(|| Error::Connect("S3 requires a bucket name".into()))?;
            let region_str = creds.region.clone().unwrap_or_else(|| "us-east-1".into());

            // A non-empty host means an S3-compatible endpoint (MinIO, R2, …);
            // otherwise talk to AWS for the named region.
            let region = if creds.host.is_empty() {
                region_str
                    .parse::<Region>()
                    .map_err(|e| Error::Connect(e.to_string()))?
            } else {
                let endpoint = if creds.host.contains("://") {
                    creds.host.clone()
                } else {
                    format!("https://{}", creds.host)
                };
                Region::Custom {
                    region: region_str,
                    endpoint,
                }
            };

            let aws_creds = match &creds.auth {
                Auth::Password(secret) => {
                    AwsCreds::new(Some(&creds.username), Some(secret), None, None, None)
                        .map_err(|e| Error::Auth(e.to_string()))?
                }
                Auth::Anonymous => {
                    AwsCreds::anonymous().map_err(|e| Error::Auth(e.to_string()))?
                }
                Auth::KeyFile { .. } => {
                    return Err(Error::Auth("S3 does not support key-file auth".into()));
                }
            };

            let mut bucket = Bucket::new(bucket_name, region, aws_creds)
                .map_err(|e| Error::Connect(e.to_string()))?;
            // Path-style addressing works against both AWS and most compatible
            // servers (virtual-host style often isn't configured on MinIO).
            if !creds.host.is_empty() {
                bucket = bucket.with_path_style();
            }
            Ok(Self { bucket })
        }
    }

    /// Normalize a UI path ("/foo/bar/") into an S3 key prefix ("foo/bar/").
    fn to_prefix(path: &str) -> String {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() || trimmed.ends_with('/') {
            trimmed.to_string()
        } else {
            format!("{trimmed}/")
        }
    }

    /// Normalize a UI path into a bare object key (no leading slash).
    fn to_key(path: &str) -> String {
        path.trim_start_matches('/').to_string()
    }

    impl Transport for S3Transport {
        fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
            let prefix = to_prefix(path);
            let pages = self
                .bucket
                .list_blocking(prefix.clone(), Some("/".to_string()))
                .map_err(|e| Error::Protocol(e.to_string()))?;

            let mut out = Vec::new();
            for page in pages {
                // "Folders" — common prefixes under the delimiter.
                for cp in page.common_prefixes.unwrap_or_default() {
                    let name = cp
                        .prefix
                        .trim_start_matches(&prefix)
                        .trim_end_matches('/')
                        .to_string();
                    if !name.is_empty() {
                        out.push(Entry {
                            name,
                            is_dir: true,
                            size: 0,
                            mtime: None,
                            perms: None,
                        });
                    }
                }
                // Objects directly under this prefix.
                for obj in page.contents {
                    if obj.key == prefix {
                        continue; // the prefix placeholder itself
                    }
                    let name = obj.key.trim_start_matches(&prefix).to_string();
                    if name.is_empty() || name.contains('/') {
                        continue;
                    }
                    out.push(Entry {
                        name,
                        is_dir: false,
                        size: obj.size,
                        mtime: None,
                        perms: None,
                    });
                }
            }
            Ok(out)
        }

        fn download(&mut self, remote: &str, local: &Path) -> Result<u64> {
            let resp = self
                .bucket
                .get_object_blocking(to_key(remote))
                .map_err(|e| Error::Protocol(e.to_string()))?;
            let bytes = resp.bytes();
            fs::write(local, bytes.as_ref())?;
            Ok(bytes.len() as u64)
        }

        fn upload(&mut self, local: &Path, remote: &str) -> Result<u64> {
            let content = fs::read(local)?;
            self.bucket
                .put_object_blocking(to_key(remote), &content)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            Ok(content.len() as u64)
        }
    }
}

pub use imp::S3Transport;
