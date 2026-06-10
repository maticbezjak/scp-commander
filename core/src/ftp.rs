use std::fs::File;
use std::io::{Cursor, Write};
use std::path::Path;

use suppaftp::list::File as ListFile;
use suppaftp::FtpStream;

use crate::transport::Transport;
use crate::types::{Auth, Credentials, Entry, Error, Protocol, Result};

/// Plain FTP backend (synchronous).
///
/// FTPS (Protocol::Ftps) is intentionally not wired up yet: it needs the
/// `native-tls` feature and the `into_secure` upgrade dance. The trait and
/// dispatch are already in place, so it slots in here without touching the UI.
pub struct FtpTransport {
    ftp: FtpStream,
}

impl FtpTransport {
    pub fn connect(creds: &Credentials) -> Result<Self> {
        if creds.protocol == Protocol::Ftps {
            return Err(Error::NotImplemented(
                "FTPS (FTP over TLS) is not implemented yet".into(),
            ));
        }

        let mut ftp = FtpStream::connect((creds.host.as_str(), creds.port))
            .map_err(|e| Error::Connect(e.to_string()))?;

        let (user, pass) = match &creds.auth {
            Auth::Password(pw) => (creds.username.as_str(), pw.as_str()),
            Auth::Anonymous => ("anonymous", "anonymous@"),
            Auth::KeyFile { .. } => {
                return Err(Error::Auth("FTP does not support key-file auth".into()));
            }
        };
        ftp.login(user, pass)
            .map_err(|e| Error::Auth(e.to_string()))?;

        Ok(Self { ftp })
    }
}

impl Transport for FtpTransport {
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
        let lines = self
            .ftp
            .list(Some(path))
            .map_err(|e| Error::Protocol(e.to_string()))?;

        let mut out = Vec::new();
        for line in lines {
            // Skip lines the parser doesn't understand rather than failing the
            // whole listing — FTP server output formats vary widely.
            if let Ok(f) = line.parse::<ListFile>() {
                out.push(Entry {
                    name: f.name().to_string(),
                    is_dir: f.is_directory(),
                    size: f.size() as u64,
                    mtime: None,
                    perms: None,
                });
            }
        }
        Ok(out)
    }

    fn download(&mut self, remote: &str, local: &Path) -> Result<u64> {
        let buf: Cursor<Vec<u8>> = self
            .ftp
            .retr_as_buffer(remote)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let bytes = buf.into_inner();
        let mut local_file = File::create(local)?;
        local_file.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }

    fn upload(&mut self, local: &Path, remote: &str) -> Result<u64> {
        let mut local_file = File::open(local)?;
        let n = self
            .ftp
            .put_file(remote, &mut local_file)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        Ok(n)
    }

    fn disconnect(&mut self) {
        let _ = self.ftp.quit();
    }
}
