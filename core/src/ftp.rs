use std::fs::File;
use std::path::Path;

use suppaftp::list::File as ListFile;
use suppaftp::native_tls::TlsConnector;
use suppaftp::{FtpStream, NativeTlsConnector, NativeTlsFtpStream};

use crate::transport::{copy_with_progress, Progress, Transport};
use crate::types::{Auth, Credentials, Entry, Error, Protocol, Result};

/// FTP / FTPS backend (synchronous).
///
/// Plain FTP and FTPS use different concrete stream types in suppaftp
/// (`FtpStream` vs `NativeTlsFtpStream`), so we hold either in `Conn` and
/// dispatch each operation with the `with_conn!` macro.
pub struct FtpTransport {
    conn: Conn,
}

enum Conn {
    Plain(FtpStream),
    Secure(NativeTlsFtpStream),
}

/// Run the same expression against whichever stream variant is active. Both
/// arms reference identically-named inherent methods, so each monomorphizes
/// independently.
macro_rules! with_conn {
    ($self:expr, $s:ident => $body:expr) => {
        match &mut $self.conn {
            Conn::Plain($s) => $body,
            Conn::Secure($s) => $body,
        }
    };
}

impl FtpTransport {
    pub fn connect(creds: &Credentials) -> Result<Self> {
        let (user, pass) = match &creds.auth {
            Auth::Password(pw) => (creds.username.as_str(), pw.as_str()),
            Auth::Anonymous => ("anonymous", "anonymous@"),
            Auth::KeyFile { .. } | Auth::Agent => {
                return Err(Error::Auth("FTP supports password auth only".into()));
            }
        };

        let conn = match creds.protocol {
            Protocol::Ftp => {
                let mut ftp = FtpStream::connect((creds.host.as_str(), creds.port))
                    .map_err(|e| Error::Connect(e.to_string()))?;
                ftp.login(user, pass).map_err(login_error)?;
                Conn::Plain(ftp)
            }
            Protocol::Ftps => {
                // Connect plaintext, then upgrade the control channel to TLS
                // (explicit FTPS) before authenticating.
                let ftp = NativeTlsFtpStream::connect((creds.host.as_str(), creds.port))
                    .map_err(|e| Error::Connect(e.to_string()))?;
                let connector = NativeTlsConnector::from(
                    TlsConnector::new().map_err(|e| Error::Connect(e.to_string()))?,
                );
                let mut ftp = ftp
                    .into_secure(connector, &creds.host)
                    .map_err(|e| Error::Connect(e.to_string()))?;
                ftp.login(user, pass).map_err(login_error)?;
                Conn::Secure(ftp)
            }
            _ => return Err(Error::Protocol("not an FTP protocol".into())),
        };

        Ok(Self { conn })
    }
}

impl Transport for FtpTransport {
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
        // "LIST -a" makes most unix FTP servers include dotfiles (WinSCP
        // sends the same); fall back to a plain LIST for servers that treat
        // "-a" as a literal path.
        let lines = with_conn!(self, c => {
            match c.list(Some(&format!("-a {path}"))) {
                Ok(lines) if !lines.is_empty() => lines,
                _ => c.list(Some(path)).map_err(|e| Error::Protocol(e.to_string()))?,
            }
        });

        let mut out = Vec::new();
        for line in lines {
            // Skip lines the parser doesn't understand rather than failing the
            // whole listing — FTP server output formats vary widely.
            if let Ok(f) = line.parse::<ListFile>() {
                // LIST -a includes "." and ".." — recursing into those would
                // loop forever.
                if f.name() == "." || f.name() == ".." {
                    continue;
                }
                let mtime = f
                    .modified()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs() as i64)
                    .filter(|m| *m > 0);
                out.push(Entry {
                    name: f.name().to_string(),
                    is_dir: f.is_directory(),
                    size: f.size() as u64,
                    mtime,
                    perms: None,
                    is_symlink: f.is_symlink(),
                });
            }
        }
        Ok(out)
    }

    fn download_progress(&mut self, remote: &str, local: &Path, progress: Progress) -> Result<u64> {
        let total = with_conn!(self, c => c.size(remote).ok()).unwrap_or(0) as u64;
        let mtime = with_conn!(self, c => c.mdtm(remote).ok());
        let mut local_file = File::create(local)?;
        let n = with_conn!(self, c => {
            let mut stream = c
                .retr_as_stream(remote)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            match copy_with_progress(&mut stream, &mut local_file, total, progress) {
                Ok(n) => {
                    c.finalize_retr_stream(stream)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    Ok(n)
                }
                Err(e) => {
                    // Best-effort cleanup so the control connection survives.
                    let _ = c.finalize_retr_stream(stream);
                    Err(e)
                }
            }
        })?;
        preserve_ftp_mtime(&local_file, mtime);
        Ok(n)
    }

    fn download_resume(
        &mut self,
        remote: &str,
        local: &Path,
        offset: u64,
        progress: Progress,
    ) -> Result<u64> {
        let total = with_conn!(self, c => c.size(remote).ok()).unwrap_or(0) as u64;
        let mtime = with_conn!(self, c => c.mdtm(remote).ok());
        // The caller's offset is advisory; resume from the file's true length
        // so retries can never append the same bytes twice.
        let _ = offset;
        let mut local_file = File::options().append(true).create(true).open(local)?;
        let offset = local_file.metadata()?.len();
        let mut report = |done: u64, total: u64| progress(offset + done, total);
        let n = with_conn!(self, c => {
            c.resume_transfer(offset as usize)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            let mut stream = c
                .retr_as_stream(remote)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            match copy_with_progress(&mut stream, &mut local_file, total, &mut report) {
                Ok(n) => {
                    c.finalize_retr_stream(stream)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    Ok(n)
                }
                Err(e) => {
                    let _ = c.finalize_retr_stream(stream);
                    Err(e)
                }
            }
        })?;
        preserve_ftp_mtime(&local_file, mtime);
        Ok(offset + n)
    }

    fn upload_progress(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        let mut local_file = File::open(local)?;
        let total = local_file.metadata().map(|m| m.len()).unwrap_or(0);
        with_conn!(self, c => {
            let mut stream = c
                .put_with_stream(remote)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            match copy_with_progress(&mut local_file, &mut stream, total, progress) {
                Ok(n) => {
                    c.finalize_put_stream(stream)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    Ok(n)
                }
                Err(e) => {
                    let _ = c.finalize_put_stream(stream);
                    Err(e)
                }
            }
        })
    }

    fn upload_resume(&mut self, local: &Path, remote: &str, progress: Progress) -> Result<u64> {
        use std::io::Seek;
        let offset = with_conn!(self, c => c.size(remote).ok()).unwrap_or(0) as u64;
        let mut local_file = File::open(local)?;
        let total = local_file.metadata().map(|m| m.len()).unwrap_or(0);
        if offset >= total {
            return Ok(offset);
        }
        local_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(Error::Io)?;
        let mut report = |done: u64, total: u64| progress(offset + done, total);
        let n = with_conn!(self, c => {
            let mut stream = c
                .append_with_stream(remote)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            match copy_with_progress(&mut local_file, &mut stream, total, &mut report) {
                Ok(n) => {
                    c.finalize_put_stream(stream)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    Ok(n)
                }
                Err(e) => {
                    let _ = c.finalize_put_stream(stream);
                    Err(e)
                }
            }
        })?;
        Ok(offset + n)
    }

    fn mkdir(&mut self, path: &str) -> Result<()> {
        with_conn!(self, c => c.mkdir(path).map_err(|e| Error::Protocol(e.to_string())))
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        with_conn!(self, c => c.rm(path).map_err(|e| Error::Protocol(e.to_string())))
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        with_conn!(self, c => c.rmdir(path).map_err(|e| Error::Protocol(e.to_string())))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        with_conn!(self, c => c.rename(from, to).map_err(|e| Error::Protocol(e.to_string())))
    }

    fn set_permissions(&mut self, path: &str, mode: u32) -> Result<()> {
        // Not in the FTP RFCs, but the SITE CHMOD extension is near-universal
        // on unix servers.
        let cmd = format!("SITE CHMOD {:o} {}", mode & 0o7777, path);
        with_conn!(self, c => c
            .site(&cmd)
            .map(|_| ())
            .map_err(|e| Error::Protocol(e.to_string())))
    }

    fn keepalive(&mut self) -> Result<()> {
        with_conn!(self, c => c.noop().map_err(|e| Error::Protocol(e.to_string())))
    }

    fn disconnect(&mut self) {
        let _ = with_conn!(self, c => c.quit());
    }
}

/// A 5xx "not logged in" is an auth rejection; everything else during login
/// (dropped socket, timeouts) is a connection problem — labelling those
/// "authentication failed" sends the UI into wrong-password flows.
fn login_error(e: suppaftp::FtpError) -> Error {
    match &e {
        suppaftp::FtpError::UnexpectedResponse(resp)
            if resp.status == suppaftp::Status::NotLoggedIn =>
        {
            Error::Auth(e.to_string())
        }
        suppaftp::FtpError::UnexpectedResponse(_) => Error::Auth(e.to_string()),
        _ => Error::Connect(e.to_string()),
    }
}

/// Stamp the downloaded file with the server's MDTM timestamp. Best-effort.
fn preserve_ftp_mtime(file: &File, mtime: Option<chrono::NaiveDateTime>) {
    if let Some(dt) = mtime {
        let secs = dt.and_utc().timestamp();
        if secs > 0 {
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64);
            let _ = file.set_modified(when);
        }
    }
}
