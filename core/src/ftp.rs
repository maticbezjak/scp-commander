use std::fs::File;
use std::io::{Cursor, Write};
use std::path::Path;

use suppaftp::list::File as ListFile;
use suppaftp::native_tls::TlsConnector;
use suppaftp::{FtpStream, NativeTlsConnector, NativeTlsFtpStream};

use crate::transport::Transport;
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
            Auth::KeyFile { .. } => {
                return Err(Error::Auth("FTP does not support key-file auth".into()));
            }
        };

        let conn = match creds.protocol {
            Protocol::Ftp => {
                let mut ftp = FtpStream::connect((creds.host.as_str(), creds.port))
                    .map_err(|e| Error::Connect(e.to_string()))?;
                ftp.login(user, pass)
                    .map_err(|e| Error::Auth(e.to_string()))?;
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
                ftp.login(user, pass)
                    .map_err(|e| Error::Auth(e.to_string()))?;
                Conn::Secure(ftp)
            }
            _ => return Err(Error::Protocol("not an FTP protocol".into())),
        };

        Ok(Self { conn })
    }
}

impl Transport for FtpTransport {
    fn list_dir(&mut self, path: &str) -> Result<Vec<Entry>> {
        let lines = with_conn!(self, c => c
            .list(Some(path))
            .map_err(|e| Error::Protocol(e.to_string()))?);

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
        let buf: Cursor<Vec<u8>> = with_conn!(self, c => c
            .retr_as_buffer(remote)
            .map_err(|e| Error::Protocol(e.to_string()))?);
        let bytes = buf.into_inner();
        let mut local_file = File::create(local)?;
        local_file.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }

    fn upload(&mut self, local: &Path, remote: &str) -> Result<u64> {
        let mut local_file = File::open(local)?;
        let n = with_conn!(self, c => c
            .put_file(remote, &mut local_file)
            .map_err(|e| Error::Protocol(e.to_string()))?);
        Ok(n)
    }

    fn disconnect(&mut self) {
        let _ = with_conn!(self, c => c.quit());
    }
}
