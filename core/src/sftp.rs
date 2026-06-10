use std::fs::File;
use std::io;
use std::net::TcpStream;
use std::path::Path;

use ssh2::Session;

use crate::transport::Transport;
use crate::types::{Auth, Credentials, Entry, Error, Result};

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

    fn download(&mut self, remote: &str, local: &Path) -> Result<u64> {
        let mut remote_file = self
            .sftp
            .open(Path::new(remote))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let mut local_file = File::create(local)?;
        let n = io::copy(&mut remote_file, &mut local_file)?;
        Ok(n)
    }

    fn upload(&mut self, local: &Path, remote: &str) -> Result<u64> {
        let mut local_file = File::open(local)?;
        let mut remote_file = self
            .sftp
            .create(Path::new(remote))
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let n = io::copy(&mut local_file, &mut remote_file)?;
        Ok(n)
    }

    fn disconnect(&mut self) {
        let _ = self
            .session
            .disconnect(None, "bye", None);
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
