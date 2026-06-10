//! C ABI over the transfer core, consumed by the macOS SwiftUI front-end.
//!
//! Conventions:
//!   * Strings in are NUL-terminated UTF-8 (`const char *`).
//!   * `scp_list_dir` returns a heap-allocated JSON string the caller must
//!     release with `scp_string_free`.
//!   * On error, fallible calls return null / -1 and the message is available
//!     from `scp_last_error()` (valid until the next core call on this thread).
//!
//! See `include/scp_core.h` for the matching C header.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::path::Path;
use std::ptr;

use crate::transport::{self, Transport};
use crate::types::{Auth, Credentials, Protocol};

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: impl Into<String>) {
    let c = CString::new(msg.into()).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
}

fn clear_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

/// Opaque session handle handed back to the caller.
pub struct ScpSession {
    inner: Box<dyn Transport>,
}

/// Borrow a `*const c_char` as `&str`, or return `None` if null/invalid UTF-8.
unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

fn protocol_from_int(v: c_int) -> Option<Protocol> {
    match v {
        0 => Some(Protocol::Sftp),
        1 => Some(Protocol::Ftp),
        2 => Some(Protocol::Ftps),
        3 => Some(Protocol::S3),
        _ => None,
    }
}

/// Open a connection. `protocol`: 0=SFTP, 1=FTP, 2=FTPS, 3=S3.
/// `password` may be null (treated as empty). Returns null on failure.
#[no_mangle]
pub extern "C" fn scp_connect(
    protocol: c_int,
    host: *const c_char,
    port: u16,
    username: *const c_char,
    password: *const c_char,
) -> *mut ScpSession {
    clear_error();

    let (Some(proto), Some(host), Some(user)) = (
        protocol_from_int(protocol),
        unsafe { cstr(host) },
        unsafe { cstr(username) },
    ) else {
        set_error("invalid arguments to scp_connect");
        return ptr::null_mut();
    };
    let pass = unsafe { cstr(password) }.unwrap_or("");

    let creds = Credentials::basic(
        proto,
        host.to_string(),
        port,
        user.to_string(),
        Auth::Password(pass.to_string()),
    );

    match transport::connect(&creds) {
        Ok(inner) => Box::into_raw(Box::new(ScpSession { inner })),
        Err(e) => {
            set_error(e.to_string());
            ptr::null_mut()
        }
    }
}

/// List a remote directory. Returns a JSON array string (caller frees with
/// `scp_string_free`) or null on error.
#[no_mangle]
pub extern "C" fn scp_list_dir(session: *mut ScpSession, path: *const c_char) -> *mut c_char {
    clear_error();
    let Some(session) = (unsafe { session.as_mut() }) else {
        set_error("null session");
        return ptr::null_mut();
    };
    let Some(path) = (unsafe { cstr(path) }) else {
        set_error("invalid path");
        return ptr::null_mut();
    };

    match session.inner.list_dir(path) {
        Ok(entries) => match CString::new(entries_to_json(&entries)) {
            Ok(s) => s.into_raw(),
            Err(_) => {
                set_error("listing contained a NUL byte");
                ptr::null_mut()
            }
        },
        Err(e) => {
            set_error(e.to_string());
            ptr::null_mut()
        }
    }
}

/// Download a remote file to a local path. Returns bytes transferred, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_download(
    session: *mut ScpSession,
    remote: *const c_char,
    local: *const c_char,
) -> i64 {
    clear_error();
    let (Some(session), Some(remote), Some(local)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(remote) },
        unsafe { cstr(local) },
    ) else {
        set_error("invalid arguments to scp_download");
        return -1;
    };
    match session.inner.download(remote, Path::new(local)) {
        Ok(n) => n as i64,
        Err(e) => {
            set_error(e.to_string());
            -1
        }
    }
}

/// Upload a local file to a remote path. Returns bytes transferred, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_upload(
    session: *mut ScpSession,
    local: *const c_char,
    remote: *const c_char,
) -> i64 {
    clear_error();
    let (Some(session), Some(local), Some(remote)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(local) },
        unsafe { cstr(remote) },
    ) else {
        set_error("invalid arguments to scp_upload");
        return -1;
    };
    match session.inner.upload(Path::new(local), remote) {
        Ok(n) => n as i64,
        Err(e) => {
            set_error(e.to_string());
            -1
        }
    }
}

/// Close the session and free the handle. Safe to pass null.
#[no_mangle]
pub extern "C" fn scp_disconnect_free(session: *mut ScpSession) {
    if session.is_null() {
        return;
    }
    let mut boxed = unsafe { Box::from_raw(session) };
    boxed.inner.disconnect();
    drop(boxed);
}

/// Last error message on this thread, or null if none. Borrowed; do not free.
#[no_mangle]
pub extern "C" fn scp_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Free a string returned by the core (e.g. from `scp_list_dir`).
#[no_mangle]
pub extern "C" fn scp_string_free(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}

// --- tiny dependency-free JSON encoder for directory listings ---------------

fn entries_to_json(entries: &[crate::types::Entry]) -> String {
    let mut s = String::from("[");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"name\":");
        json_str(&mut s, &e.name);
        s.push_str(",\"is_dir\":");
        s.push_str(if e.is_dir { "true" } else { "false" });
        s.push_str(",\"size\":");
        s.push_str(&e.size.to_string());
        s.push_str(",\"mtime\":");
        match e.mtime {
            Some(m) => s.push_str(&m.to_string()),
            None => s.push_str("null"),
        }
        s.push_str(",\"perms\":");
        match &e.perms {
            Some(p) => json_str(&mut s, p),
            None => s.push_str("null"),
        }
        s.push('}');
    }
    s.push(']');
    s
}

fn json_str(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
