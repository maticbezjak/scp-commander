use thiserror::Error;

/// Supported transfer protocols. Drives which `Transport` backend is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Sftp,
    Ftp,
    Ftps,
    S3,
}

/// How to authenticate to a server.
#[derive(Debug, Clone)]
pub enum Auth {
    Password(String),
    /// Path to a private key file, plus an optional passphrase.
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
    /// No credentials (anonymous FTP, public buckets).
    Anonymous,
}

/// Everything needed to open a session.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: Auth,
}

impl Credentials {
    /// The conventional default port for a protocol.
    pub fn default_port(protocol: Protocol) -> u16 {
        match protocol {
            Protocol::Sftp => 22,
            Protocol::Ftp => 21,
            Protocol::Ftps => 21,
            Protocol::S3 => 443,
        }
    }
}

/// A single directory entry (file or folder) in a remote or local listing.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time as a unix epoch (seconds), if the server reported it.
    pub mtime: Option<i64>,
    /// Unix-style permission string, e.g. "rwxr-xr-x", when available.
    pub perms: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("not implemented: {0}")]
    NotImplemented(String),
}
