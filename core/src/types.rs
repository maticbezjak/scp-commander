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
    /// Ask the running SSH agent (ssh-agent) to authenticate. SFTP only.
    Agent,
    /// No credentials (anonymous FTP, public buckets).
    Anonymous,
}

/// What to do when an SSH server's host key is not already known.
///
/// The intended UI flow: connect with `Strict` (the default); on
/// [`Error::UnknownHostKey`], show the fingerprint to the user; retry with
/// `AcceptFingerprint` pinned to exactly what they approved. A key that
/// contradicts a stored one always fails, regardless of policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// Fail with [`Error::UnknownHostKey`] when the key isn't in any store.
    Strict,
    /// Trust-on-first-use: remember unknown keys without asking (CLI flag).
    AcceptNew,
    /// Accept and remember the key only if its SHA256 fingerprint matches.
    AcceptFingerprint(String),
}

/// A bastion/jump host to tunnel through. The real session is established to
/// the target *over* an SSH direct-tcpip channel opened on this host (ProxyJump).
#[derive(Debug, Clone)]
pub struct JumpHost {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: Auth,
    /// How to treat the bastion's host key (defaults to trust-on-first-use).
    pub host_key: HostKeyPolicy,
}

/// Everything needed to open a session.
///
/// For SFTP/FTP/FTPS: `host`/`port`/`username`/`auth` are used. For S3,
/// `username` is the access key, `auth` carries the secret, `bucket` names the
/// bucket, `region` the region, and `host` (if set) is a custom endpoint for
/// S3-compatible storage (e.g. MinIO).
#[derive(Debug, Clone)]
pub struct Credentials {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: Auth,
    /// S3 only: bucket name.
    pub bucket: Option<String>,
    /// S3 only: region (e.g. "us-east-1").
    pub region: Option<String>,
    /// SFTP only: how to treat servers whose host key isn't known yet.
    pub host_key: HostKeyPolicy,
    /// SFTP only: optional bastion to tunnel the connection through.
    pub jump: Option<JumpHost>,
}

impl Credentials {
    /// Build credentials for a host-based protocol (SFTP/FTP/FTPS).
    pub fn basic(protocol: Protocol, host: String, port: u16, username: String, auth: Auth) -> Self {
        Self {
            protocol,
            host,
            port,
            username,
            auth,
            bucket: None,
            region: None,
            host_key: HostKeyPolicy::Strict,
            jump: None,
        }
    }

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
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub name: String,
    /// True for directories — including symlinks that point at directories,
    /// so they navigate correctly.
    pub is_dir: bool,
    pub size: u64,
    /// Modification time as a unix epoch (seconds), if the server reported it.
    pub mtime: Option<i64>,
    /// Unix-style permission string, e.g. "rwxr-xr-x", when available.
    pub perms: Option<String>,
    pub is_symlink: bool,
    /// Owner and group numeric IDs, when available (SFTP only).
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// Result of running a remote command via [`Transport::exec_command`].
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
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
    #[error("cancelled")]
    Cancelled,
    #[error("unknown server host key: {fingerprint}")]
    UnknownHostKey { fingerprint: String },
    #[error(
        "HOST KEY MISMATCH — the server's key ({fingerprint}) contradicts the stored one; \
         possible man-in-the-middle attack"
    )]
    HostKeyMismatch { fingerprint: String },
}
