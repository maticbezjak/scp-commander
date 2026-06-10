//! Shared transfer core for the cross-platform file manager.
//!
//! All protocol logic, the transfer engine, and (eventually) the sync engine
//! live here with no UI dependency. The native front-ends consume it two ways:
//!   * the Ubuntu GTK app links it as a normal Rust `rlib`;
//!   * the macOS SwiftUI app links the `staticlib` and calls the C `ffi` layer.

pub mod ffi;
pub mod ftp;
pub mod ops;
pub mod s3;
pub mod sftp;
pub mod transport;
pub mod types;

pub use transport::{connect, Transport};
pub use types::{Auth, Credentials, Entry, Error, HostKeyPolicy, Protocol, Result};
