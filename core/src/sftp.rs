use std::fs::File;
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use ssh2::{
    CheckResult, HashType, HostKeyType, KnownHostFileKind, KnownHostKeyFormat, Session,
};

use crate::transport::{copy_with_progress, Progress, Transport};
use crate::types::{Auth, Credentials, Entry, Error, HostKeyPolicy, Result};

/// SFTP backend backed by libssh2 (synchronous).
pub struct SftpTransport {
    session: Session,
    sftp: ssh2::Sftp,
}

impl SftpTransport {
    pub fn connect(creds: &Credentials) -> Result<Self> {
        let tcp = TcpStream::connect((creds.host.as_str(), creds.port))
            .map_err(|e| Error::Connect(e.to_string()))?;

        let mut session = Session::new().map_err(|e| Error::Connect(e.to_string()))?;
        session.set_tcp_stream(tcp);
        session
            .handshake()
            .map_err(|e| Error::Connect(e.to_string()))?;

        // Verify the server's identity BEFORE sending any credentials.
        verify_host_key(&session, creds)?;

        match &creds.auth {
            Auth::Password(pw) => session
                .userauth_password(&creds.username, pw)
                .map_err(|e| Error::Auth(e.to_string()))?,
            Auth::KeyFile { path, passphrase } => session
                .userauth_pubkey_file(
                    &creds.username,
                    None,
                    Path::new(path),
                    passphrase.as_deref(),
                )
                .map_err(|e| Error::Auth(e.to_string()))?,
            Auth::Agent => session
                .userauth_agent(&creds.username)
                .map_err(|e| Error::Auth(e.to_string()))?,
            Auth::Anonymous => {
                return Err(Error::Auth("SFTP requires credentials".into()));
            }
        }

        if !session.authenticated() {
            return Err(Error::Auth("authentication rejected".into()));
        }

        let sftp = session.sftp().map_err(|e| Error::Protocol(e.to_string()))?;
        Ok(Self { session, sftp })
    }
}

impl Transport for SftpTransport {
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
        let entries = self
            .sftp
            .readdir(Path::new(path))
            .map_err(|e| Error::Protocol(e.to_string()))?;

        let mut out = Vec::with_capacity(entries.len());
        for (p, stat) in entries {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned());
            out.push(Entry {
                name,
                is_dir: stat.is_dir(),
                size: stat.size.unwrap_or(0),
                mtime: stat.mtime.map(|m| m as i64),
                perms: stat.perm.map(perm_string),
            });
        }
        Ok(out)
    }

    fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress) -> Result<u64> {
        let mut remote_file = self
            .sftp
            .open(Path::new(remote))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let total = remote_file.stat().ok().and_then(|s| s.size).unwrap_or(0);
        let mut local_file = File::create(local)?;
        copy_with_progress(&mut remote_file, &mut local_file, total, progress)
    }

    fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        let mut local_file = File::open(local)?;
        let total = local_file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut remote_file = self
            .sftp
            .create(Path::new(remote))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        copy_with_progress(&mut local_file, &mut remote_file, total, progress)
    }

    fn mkdir(&mut self, path: &str) -> Result<()> {
        self.sftp
            .mkdir(Path::new(path), 0o755)
            .map_err(|e| Error::Protocol(e.to_string()))
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        self.sftp
            .unlink(Path::new(path))
            .map_err(|e| Error::Protocol(e.to_string()))
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        self.sftp
            .rmdir(Path::new(path))
            .map_err(|e| Error::Protocol(e.to_string()))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.sftp
            .rename(Path::new(from), Path::new(to), None)
            .map_err(|e| Error::Protocol(e.to_string()))
    }

    fn disconnect(&mut self) {
        let _ = self
            .session
            .disconnect(None, "bye", None);
    }
}

// --- host key verification ---------------------------------------------------

/// Check the server's host key against the user's `~/.ssh/known_hosts`
/// (read-only — we never write there) and the app's own store. A match in
/// either accepts; a contradiction always fails; an unknown key defers to the
/// session's [`HostKeyPolicy`]. Newly accepted keys go to the app store only.
fn verify_host_key(session: &Session, creds: &Credentials) -> Result<()> {
    let (key, key_type) = session
        .host_key()
        .ok_or_else(|| Error::Connect("server presented no host key".into()))?;
    let fingerprint = sha256_fingerprint(session)
        .ok_or_else(|| Error::Connect("could not hash server host key".into()))?;

    let mut mismatch = false;
    for path in [user_known_hosts_path(), app_known_hosts_path()]
        .into_iter()
        .flatten()
        .filter(|p| p.exists())
    {
        let mut store = session
            .known_hosts()
            .map_err(|e| Error::Connect(e.to_string()))?;
        if store.read_file(&path, KnownHostFileKind::OpenSSH).is_err() {
            continue; // unreadable/corrupt file — treat as no information
        }
        match store.check_port(&creds.host, creds.port, key) {
            CheckResult::Match => return Ok(()),
            CheckResult::Mismatch => mismatch = true,
            CheckResult::NotFound | CheckResult::Failure => {}
        }
    }
    if mismatch {
        return Err(Error::HostKeyMismatch { fingerprint });
    }

    match &creds.host_key {
        HostKeyPolicy::Strict => Err(Error::UnknownHostKey { fingerprint }),
        HostKeyPolicy::AcceptNew => remember_host_key(session, creds, key, key_type),
        HostKeyPolicy::AcceptFingerprint(approved) => {
            if *approved == fingerprint {
                remember_host_key(session, creds, key, key_type)
            } else {
                // The key changed between the prompt and the retry.
                Err(Error::HostKeyMismatch { fingerprint })
            }
        }
    }
}

/// Append the key to the app's known_hosts store.
fn remember_host_key(
    session: &Session,
    creds: &Credentials,
    key: &[u8],
    key_type: HostKeyType,
) -> Result<()> {
    let path = app_known_hosts_path()
        .ok_or_else(|| Error::Connect("cannot locate home directory".into()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut store = session
        .known_hosts()
        .map_err(|e| Error::Connect(e.to_string()))?;
    if path.exists() {
        let _ = store.read_file(&path, KnownHostFileKind::OpenSSH);
    }

    // known_hosts convention: bare hostname for port 22, "[host]:port" otherwise.
    let host_entry = if creds.port == 22 {
        creds.host.clone()
    } else {
        format!("[{}]:{}", creds.host, creds.port)
    };
    store
        .add(&host_entry, key, "added by scp-commander", known_host_format(key_type)?)
        .map_err(|e| Error::Connect(e.to_string()))?;
    store
        .write_file(&path, KnownHostFileKind::OpenSSH)
        .map_err(|e| Error::Connect(e.to_string()))?;
    Ok(())
}

fn known_host_format(t: HostKeyType) -> Result<KnownHostKeyFormat> {
    match t {
        HostKeyType::Rsa => Ok(KnownHostKeyFormat::SshRsa),
        HostKeyType::Dss => Ok(KnownHostKeyFormat::SshDss),
        HostKeyType::Ecdsa256 => Ok(KnownHostKeyFormat::Ecdsa256),
        HostKeyType::Ecdsa384 => Ok(KnownHostKeyFormat::Ecdsa384),
        HostKeyType::Ecdsa521 => Ok(KnownHostKeyFormat::Ecdsa521),
        HostKeyType::Ed25519 => Ok(KnownHostKeyFormat::Ed25519),
        HostKeyType::Unknown => Err(Error::Connect("unsupported host key type".into())),
    }
}

/// OpenSSH-style fingerprint: "SHA256:" + unpadded base64 of the key hash.
fn sha256_fingerprint(session: &Session) -> Option<String> {
    let hash = session.host_key_hash(HashType::Sha256)?;
    Some(format!("SHA256:{}", base64_nopad(hash)))
}

fn user_known_hosts_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".ssh/known_hosts"))
}

fn app_known_hosts_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/scp-commander/known_hosts"))
}

fn base64_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_nopad;

    #[test]
    fn base64_matches_openssh_style() {
        // RFC 4648 vectors, minus padding (OpenSSH fingerprints drop it).
        assert_eq!(base64_nopad(b""), "");
        assert_eq!(base64_nopad(b"f"), "Zg");
        assert_eq!(base64_nopad(b"fo"), "Zm8");
        assert_eq!(base64_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64_nopad(b"foobar"), "Zm9vYmFy");
    }
}

/// Render a unix mode bitmask as an `rwxr-xr-x`-style string (permission bits only).
fn perm_string(mode: u32) -> String {
    const FLAGS: [(u32, char); 9] = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    FLAGS
        .iter()
        .map(|&(bit, ch)| if mode & bit != 0 { ch } else { '-' })
        .collect()
}
