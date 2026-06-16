use std::fs::File;
use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use ssh2::{
    CheckResult, HashType, HostKeyType, KnownHostFileKind, KnownHostKeyFormat, Session,
};

use crate::transport::{copy_with_progress, Progress, Transport};
use crate::types::{Auth, Credentials, Entry, Error, ExecResult, HostKeyPolicy, Result};

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
            Auth::Password(pw) => {
                // Plain password first. Many servers (especially with 2FA/OTP)
                // only offer keyboard-interactive; fall back to it, feeding the
                // same secret to every prompt. A wrong secret simply fails both.
                let _ = session.userauth_password(&creds.username, pw);
                if !session.authenticated() {
                    let mut responder = SingleResponse { secret: pw };
                    session
                        .userauth_keyboard_interactive(&creds.username, &mut responder)
                        .map_err(auth_or_connect)?;
                }
            }
            Auth::KeyFile { path, passphrase } => session
                .userauth_pubkey_file(
                    &creds.username,
                    None,
                    Path::new(path),
                    passphrase.as_deref(),
                )
                .map_err(auth_or_connect)?,
            Auth::Agent => session
                .userauth_agent(&creds.username)
                .map_err(auth_or_connect)?,
            Auth::Anonymous => {
                return Err(Error::Auth("SFTP requires credentials".into()));
            }
        }

        if !session.authenticated() {
            return Err(Error::Auth("authentication rejected".into()));
        }

        // Let libssh2 piggyback keepalives on blocking calls too.
        session.set_keepalive(true, 30);

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

        // Resolving a symlink's target costs one extra round trip; cap it so
        // a directory full of links doesn't take O(links x RTT) to list.
        const MAX_SYMLINK_RESOLUTIONS: usize = 64;
        let mut resolved = 0usize;

        let mut out = Vec::with_capacity(entries.len());
        for (p, stat) in entries {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned());
            // readdir lstats: symlinks report as links, not their targets.
            // Follow them so a link to a directory navigates like one.
            let is_symlink = stat.file_type().is_symlink();
            let (is_dir, size) = if is_symlink && resolved < MAX_SYMLINK_RESOLUTIONS {
                resolved += 1;
                match self.sftp.stat(&p) {
                    Ok(target) => (target.is_dir(), target.size.unwrap_or(0)),
                    Err(_) => (false, 0), // dangling link
                }
            } else {
                (stat.is_dir(), stat.size.unwrap_or(0))
            };
            out.push(Entry {
                name,
                is_dir,
                size,
                mtime: stat.mtime.map(|m| m as i64),
                perms: stat.perm.map(perm_string),
                is_symlink,
                uid: stat.uid,
                gid: stat.gid,
            });
        }
        Ok(out)
    }

    fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress) -> Result<u64> {
        let mut remote_file = self
            .sftp
            .open(Path::new(remote))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let stat = remote_file.stat().ok();
        let total = stat.as_ref().and_then(|s| s.size).unwrap_or(0);
        let mut local_file = File::create(local)?;
        let n = copy_with_progress(&mut remote_file, &mut local_file, total, progress)?;
        preserve_mtime(&local_file, stat.and_then(|s| s.mtime));
        Ok(n)
    }

    fn download_resume(
        &mut self,
        remote: &str,
        local: &Path,
        offset: u64,
        progress: Progress,
    ) -> Result<u64> {
        use std::io::Seek;
        // The caller's offset is advisory only: the authoritative resume
        // point is the local file's actual length. This also makes retries
        // (e.g. AutoReconnect re-running this op after a mid-stream failure)
        // safe — a second attempt resumes from wherever the first stopped
        // instead of appending the same bytes twice.
        let _ = offset;
        let local_file = File::options().append(true).create(true).open(local)?;
        let offset = local_file.metadata()?.len();
        let mut local_file = local_file;

        let mut remote_file = self
            .sftp
            .open(Path::new(remote))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let stat = remote_file.stat().ok();
        let total = stat.as_ref().and_then(|s| s.size).unwrap_or(0);
        remote_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let mut report = |done: u64, total: u64| progress(offset + done, total);
        let n = copy_with_progress(&mut remote_file, &mut local_file, total, &mut report)?;
        preserve_mtime(&local_file, stat.and_then(|s| s.mtime));
        Ok(offset + n)
    }

    fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        let mut local_file = File::open(local)?;
        let meta = local_file.metadata().ok();
        let total = meta.as_ref().map(|m| m.len()).unwrap_or(0);

        // Atomic upload: stream into a temp sibling, then swap it into place so
        // an interrupted transfer can't leave a truncated file at the real name.
        if crate::atomic_uploads_enabled() {
            let temp = crate::upload_temp_name(remote);
            let n = {
                let mut remote_file = self
                    .sftp
                    .create(Path::new(&temp))
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                match copy_with_progress(&mut local_file, &mut remote_file, total, progress) {
                    Ok(n) => n,
                    Err(e) => {
                        drop(remote_file);
                        let _ = self.sftp.unlink(Path::new(&temp));
                        return Err(e);
                    }
                }
            };
            // Promote temp -> final. Unlink any existing target first so the
            // rename succeeds on servers that won't overwrite on rename.
            let _ = self.sftp.unlink(Path::new(remote));
            if let Err(e) = self.sftp.rename(Path::new(&temp), Path::new(remote), None) {
                let _ = self.sftp.unlink(Path::new(&temp));
                return Err(Error::Protocol(e.to_string()));
            }
            self.stamp_remote_mtime(remote, meta.as_ref());
            return Ok(n);
        }

        let n = {
            let mut remote_file = self
                .sftp
                .create(Path::new(remote))
                .map_err(|e| Error::Protocol(e.to_string()))?;
            copy_with_progress(&mut local_file, &mut remote_file, total, progress)?
        };
        self.stamp_remote_mtime(remote, meta.as_ref());
        Ok(n)
    }

    fn upload_resume(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        use std::io::Seek;
        // The server's current size is the authoritative resume point.
        let offset = self
            .sftp
            .stat(Path::new(remote))
            .ok()
            .and_then(|s| s.size)
            .unwrap_or(0);
        let mut local_file = File::open(local)?;
        let meta = local_file.metadata().ok();
        let total = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        if offset >= total {
            return Ok(offset); // nothing left to send
        }
        local_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(Error::Io)?;
        let n = {
            let mut remote_file = self
                .sftp
                .open_mode(
                    Path::new(remote),
                    ssh2::OpenFlags::WRITE | ssh2::OpenFlags::APPEND,
                    0o644,
                    ssh2::OpenType::File,
                )
                .map_err(|e| Error::Protocol(e.to_string()))?;
            let mut report = |done: u64, total: u64| progress(offset + done, total);
            copy_with_progress(&mut local_file, &mut remote_file, total, &mut report)?
        };
        self.stamp_remote_mtime(remote, meta.as_ref());
        Ok(offset + n)
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

    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()> {
        let stat = ssh2::FileStat {
            size: None,
            uid: None,
            gid: None,
            perm: Some(mode),
            atime: None,
            mtime: None,
        };
        self.sftp
            .setstat(Path::new(path), stat)
            .map_err(|e| Error::Protocol(e.to_string()))
    }

    fn keepalive(&mut self) -> Result<()> {
        // keepalive_send only WRITES a packet — on a half-open connection the
        // write succeeds into the void, so it cannot detect a dead session.
        // Keep it for NAT warming, but probe liveness with a real round trip.
        let _ = self.session.keepalive_send();
        self.sftp
            .stat(Path::new("."))
            .map(|_| ())
            .map_err(|e| Error::Protocol(e.to_string()))
    }

    fn disconnect(&mut self) {
        let _ = self.session.disconnect(None, "bye", None);
    }

    fn exec_command(&mut self, cmd: &str) -> Result<ExecResult> {
        let mut channel = self
            .session
            .channel_session()
            .map_err(|e| Error::Protocol(e.to_string()))?;
        channel.exec(cmd).map_err(|e| Error::Protocol(e.to_string()))?;
        let mut stdout = String::new();
        channel.read_to_string(&mut stdout)?;
        let mut stderr = String::new();
        channel.stderr().read_to_string(&mut stderr)?;
        channel.wait_close().map_err(|e| Error::Protocol(e.to_string()))?;
        let exit_code = channel.exit_status().unwrap_or(-1);
        Ok(ExecResult { exit_code, stdout, stderr })
    }

    fn copy_file(&mut self, src: &str, dst: &str) -> Result<u64> {
        // Server-side copy via read+write on the same SFTP session.
        let mut remote_src = self
            .sftp
            .open(Path::new(src))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let stat = remote_src.stat().ok();
        let size = stat.as_ref().and_then(|s| s.size).unwrap_or(0);
        let mut remote_dst = self
            .sftp
            .create(Path::new(dst))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let mut noop = |_, _| true;
        copy_with_progress(&mut remote_src, &mut remote_dst, size, &mut noop)
    }
}

impl SftpTransport {
    /// Mirror the local file's mtime onto the freshly uploaded remote file,
    /// so sync comparisons stay honest in both directions. Best-effort.
    fn stamp_remote_mtime(&self, remote: &str, meta: Option<&std::fs::Metadata>) {
        let Some(mtime) = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
        else {
            return;
        };
        let stat = ssh2::FileStat {
            size: None,
            uid: None,
            gid: None,
            perm: None,
            atime: Some(mtime),
            mtime: Some(mtime),
        };
        let _ = self.sftp.setstat(Path::new(remote), stat);
    }
}

/// Keyboard-interactive responder that answers every prompt with one secret —
/// covers servers that wrap the password in keyboard-interactive, and the
/// common single-prompt OTP/2FA case (user types the code in the password box).
struct SingleResponse<'a> {
    secret: &'a str,
}

impl ssh2::KeyboardInteractivePrompt for SingleResponse<'_> {
    fn prompt<'a>(
        &mut self,
        _username: &str,
        _instructions: &str,
        prompts: &[ssh2::Prompt<'a>],
    ) -> Vec<String> {
        prompts.iter().map(|_| self.secret.to_string()).collect()
    }
}

/// Auth calls also surface transport failures (socket died mid-login):
/// labelling those "authentication failed" sends the UI into wrong-password
/// flows. Only genuine auth rejections map to Error::Auth.
fn auth_or_connect(e: ssh2::Error) -> Error {
    match e.code() {
        // LIBSSH2_ERROR_AUTHENTICATION_FAILED / _PUBLICKEY_UNVERIFIED
        ssh2::ErrorCode::Session(-18) | ssh2::ErrorCode::Session(-19) => {
            Error::Auth(e.to_string())
        }
        _ => Error::Connect(e.to_string()),
    }
}

/// Stamp the downloaded file with the server's modification time, so sync
/// comparisons and humans see the original date. Best-effort.
fn preserve_mtime(file: &File, mtime: Option<u64>) {
    if let Some(m) = mtime {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(m);
        let _ = file.set_modified(when);
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

    let mut matched = false;
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
            CheckResult::Match => matched = true,
            // Fail-closed: a recorded mismatch in ANY store (most importantly
            // the user's authoritative ~/.ssh/known_hosts) is terminal — a
            // Match elsewhere must not override it.
            CheckResult::Mismatch => {
                return Err(Error::HostKeyMismatch { fingerprint });
            }
            CheckResult::NotFound | CheckResult::Failure => {}
        }
    }
    if matched {
        return Ok(());
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
        // The same dir holds sites.json (hostnames/usernames) — keep it private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
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

/// One entry in the app's trusted-host store (host + key algorithm).
pub struct KnownHost {
    pub host: String,
    pub key_type: String,
}

/// List the SSH host keys SCP Commander has trusted (its own store, not the
/// system `~/.ssh/known_hosts`). Entries are written with plain hostnames.
pub fn list_known_hosts() -> Vec<KnownHost> {
    let Some(path) = app_known_hosts_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split_whitespace();
            let host = parts.next()?.to_string();
            let key_type = parts.next()?.to_string();
            Some(KnownHost { host, key_type })
        })
        .collect()
}

/// Forget a trusted host: remove every entry for `host` from the app store so
/// the next connection re-prompts. Returns how many lines were removed.
pub fn remove_known_host(host: &str) -> Result<usize> {
    let Some(path) = app_known_hosts_path() else { return Ok(0) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(0) };
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0usize;
    for line in text.lines() {
        if line.split_whitespace().next() == Some(host) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    if removed > 0 {
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(&path, out)?;
    }
    Ok(removed)
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
