//! S3 / S3-compatible object storage backend.
//!
//! Real implementation is gated behind the `s3` cargo feature (pulls rust-s3).
//! Without the feature, a stub keeps the trait + dispatch wiring intact while
//! avoiding the extra dependency weight.

#[cfg(not(feature = "s3"))]
mod imp {
    use std::path::Path;

    use crate::transport::{Progress, Transport};
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
        fn download_progress(&mut self, _r: &str, _l: &Path, _p: Progress) -> Result<u64> {
            Err(Error::NotImplemented("S3 download".into()))
        }
        fn upload_progress(&mut self, _l: &Path, _r: &str, _p: Progress) -> Result<u64> {
            Err(Error::NotImplemented("S3 upload".into()))
        }
        fn mkdir(&mut self, _path: &str) -> Result<()> {
            Err(Error::NotImplemented("S3 mkdir".into()))
        }
        fn remove_file(&mut self, _path: &str) -> Result<()> {
            Err(Error::NotImplemented("S3 remove_file".into()))
        }
        fn remove_dir(&mut self, _path: &str) -> Result<()> {
            Err(Error::NotImplemented("S3 remove_dir".into()))
        }
        fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
            Err(Error::NotImplemented("S3 rename".into()))
        }
    }
}

#[cfg(feature = "s3")]
mod imp {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::path::Path;

    use s3::creds::Credentials as AwsCreds;
    use s3::{Bucket, Region};

    use crate::transport::{Progress, Transport};
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
                Auth::KeyFile { .. } | Auth::Agent => {
                    return Err(Error::Auth(
                        "S3 uses access/secret keys, not SSH credentials".into(),
                    ));
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

        fn object_size(&self, key: &str) -> Option<u64> {
            let (head, code) = self.bucket.head_object_blocking(key).ok()?;
            if code != 200 {
                return None;
            }
            head.content_length.and_then(|l| u64::try_from(l).ok())
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

    /// Transfers stream in bounded chunks: rust-s3's "blocking" stream APIs
    /// still demand tokio AsyncRead/AsyncWrite, so instead we use ranged GETs
    /// for downloads and multipart uploads for big files — pure blocking HTTP,
    /// memory bounded by CHUNK regardless of file size.
    const CHUNK: u64 = 8 * 1024 * 1024;

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
                    // strip_prefix, not trim_start_matches: the latter strips
                    // REPEATS, so "photos/photos/" under "photos/" vanished.
                    let name = cp
                        .prefix
                        .strip_prefix(&prefix)
                        .unwrap_or(&cp.prefix)
                        .trim_end_matches('/')
                        .to_string();
                    if !name.is_empty() {
                        out.push(Entry {
                            name,
                            is_dir: true,
                            size: 0,
                            mtime: None,
                            perms: None,
                            is_symlink: false,
                        });
                    }
                }
                // Objects directly under this prefix.
                for obj in page.contents {
                    if obj.key == prefix {
                        continue; // the prefix placeholder itself
                    }
                    let name = obj
                        .key
                        .strip_prefix(&prefix)
                        .unwrap_or(&obj.key)
                        .to_string();
                    if name.is_empty() || name.contains('/') {
                        continue;
                    }
                    let mtime = chrono::DateTime::parse_from_rfc3339(&obj.last_modified)
                        .ok()
                        .map(|d| d.timestamp());
                    out.push(Entry {
                        name,
                        is_dir: false,
                        size: obj.size,
                        mtime,
                        perms: None,
                        is_symlink: false,
                    });
                }
            }
            Ok(out)
        }

        fn download_progress(
            &mut self,
            remote: &str,
            local: &Path,
            progress: Progress,
        ) -> Result<u64> {
            let key = to_key(remote);
            let Some(total) = self.object_size(&key) else {
                // Size unknown (no HEAD permission?) — single-shot fallback.
                let resp = self
                    .bucket
                    .get_object_blocking(&key)
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                let bytes = resp.bytes();
                std::fs::write(local, bytes.as_ref())?;
                if !progress(bytes.len() as u64, bytes.len() as u64) {
                    return Err(Error::Cancelled);
                }
                return Ok(bytes.len() as u64);
            };

            let mut file = File::create(local)?;
            let mut done: u64 = 0;
            while done < total {
                let end = (done + CHUNK - 1).min(total - 1);
                let resp = self
                    .bucket
                    .get_object_range_blocking(&key, done, Some(end))
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                let bytes = resp.bytes();
                if bytes.is_empty() {
                    return Err(Error::Protocol("S3 returned an empty range".into()));
                }
                file.write_all(bytes)?;
                done += bytes.len() as u64;
                if !progress(done, total) {
                    return Err(Error::Cancelled);
                }
            }
            Ok(done)
        }

        fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
            let key = to_key(remote);
            let mut file = File::open(local)?;
            let total = file.metadata().map(|m| m.len()).unwrap_or(0);

            // Small files: one PUT (bounded by CHUNK anyway).
            if total <= CHUNK {
                let mut buf = Vec::with_capacity(total as usize);
                file.read_to_end(&mut buf)?;
                self.bucket
                    .put_object_blocking(&key, &buf)
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                if !progress(total, total) {
                    return Err(Error::Cancelled);
                }
                return Ok(total);
            }

            // Big files: multipart, one CHUNK in memory at a time.
            let content_type = "application/octet-stream";
            let mp = self
                .bucket
                .initiate_multipart_upload_blocking(&key, content_type)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            let mut parts = Vec::new();
            let mut done: u64 = 0;
            let mut part_number: u32 = 1;
            let result = (|| -> Result<u64> {
                let mut buf = vec![0u8; CHUNK as usize];
                loop {
                    let mut filled = 0;
                    while filled < buf.len() {
                        let n = file.read(&mut buf[filled..])?;
                        if n == 0 {
                            break;
                        }
                        filled += n;
                    }
                    if filled == 0 {
                        break;
                    }
                    let part = self
                        .bucket
                        .put_multipart_chunk_blocking(
                            buf[..filled].to_vec(),
                            &key,
                            part_number,
                            &mp.upload_id,
                            content_type,
                        )
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    parts.push(part);
                    part_number += 1;
                    done += filled as u64;
                    if !progress(done, total) {
                        return Err(Error::Cancelled);
                    }
                    if filled < buf.len() {
                        break; // EOF
                    }
                }
                self.bucket
                    .complete_multipart_upload_blocking(&key, &mp.upload_id, parts.clone())
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                Ok(done)
            })();
            if result.is_err() {
                // Don't leave half-uploaded parts accruing storage costs.
                let _ = self.bucket.abort_upload_blocking(&key, &mp.upload_id);
            }
            result
        }

        fn mkdir(&mut self, path: &str) -> Result<()> {
            // S3 has no directories; create the conventional zero-byte marker.
            self.bucket
                .put_object_blocking(to_prefix(path), &[])
                .map_err(|e| Error::Protocol(e.to_string()))?;
            Ok(())
        }

        fn remove_file(&mut self, path: &str) -> Result<()> {
            self.bucket
                .delete_object_blocking(to_key(path))
                .map_err(|e| Error::Protocol(e.to_string()))?;
            Ok(())
        }

        fn remove_dir(&mut self, path: &str) -> Result<()> {
            // Delete the marker object if present; implicit prefixes vanish
            // once their contents are gone, so absence is not an error.
            let _ = self.bucket.delete_object_blocking(to_prefix(path));
            Ok(())
        }

        fn rename(&mut self, from: &str, to: &str) -> Result<()> {
            let (from_key, to_key_) = (to_key(from), to_key(to));
            self.bucket
                .copy_object_internal_blocking(&from_key, &to_key_)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            self.bucket
                .delete_object_blocking(&from_key)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{to_key, to_prefix};

        #[test]
        fn path_normalization() {
            assert_eq!(to_prefix("/"), "");
            assert_eq!(to_prefix("/photos"), "photos/");
            assert_eq!(to_prefix("/photos/2026/"), "photos/2026/");
            assert_eq!(to_key("/photos/cat.jpg"), "photos/cat.jpg");
        }
    }
}

pub use imp::S3Transport;
