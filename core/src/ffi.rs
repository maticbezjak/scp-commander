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
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr;

use std::cell::Cell;

use crate::transport::{self, Transport};
use crate::types::{Auth, Credentials, Error, HostKeyPolicy, Protocol};

/// Error codes surfaced via `scp_last_error_code`.
pub const SCP_ERR_NONE: c_int = 0;
pub const SCP_ERR_GENERIC: c_int = 1;
pub const SCP_ERR_UNKNOWN_HOST_KEY: c_int = 2;
pub const SCP_ERR_HOST_KEY_MISMATCH: c_int = 3;
/// Network-level failure (connect / IO / protocol) — a reconnect may help.
pub const SCP_ERR_NETWORK: c_int = 4;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
    static LAST_CODE: Cell<c_int> = const { Cell::new(SCP_ERR_NONE) };
    static LAST_FINGERPRINT: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: impl Into<String>) {
    let c = CString::new(msg.into()).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
    LAST_CODE.with(|c| c.set(SCP_ERR_GENERIC));
    LAST_FINGERPRINT.with(|f| *f.borrow_mut() = None);
}

/// Like `set_error` but preserves the error kind + fingerprint for host-key
/// failures so the UI can run the trust prompt.
fn set_error_typed(err: &Error) {
    set_error(err.to_string());
    let (code, fp) = match err {
        Error::UnknownHostKey { fingerprint } => (SCP_ERR_UNKNOWN_HOST_KEY, Some(fingerprint)),
        Error::HostKeyMismatch { fingerprint } => (SCP_ERR_HOST_KEY_MISMATCH, Some(fingerprint)),
        Error::Connect(_) | Error::Io(_) | Error::Protocol(_) => (SCP_ERR_NETWORK, None),
        _ => (SCP_ERR_GENERIC, None),
    };
    LAST_CODE.with(|c| c.set(code));
    LAST_FINGERPRINT.with(|f| {
        *f.borrow_mut() = fp.and_then(|s| CString::new(s.as_str()).ok());
    });
}

fn clear_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
    LAST_CODE.with(|c| c.set(SCP_ERR_NONE));
    LAST_FINGERPRINT.with(|f| *f.borrow_mut() = None);
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
///
/// Nullable/empty string parameters are treated as absent (Swift can't easily
/// pass nullable C strings). `auth_mode`: 0 = password (in `password`),
/// 1 = key file (`key_path`, with `password` as the passphrase), 2 = ssh-agent.
/// `host_key_mode`: 0 = strict (fail on unknown keys), 1 = accept new keys,
/// 2 = accept only the key whose SHA256 fingerprint equals
/// `expected_fingerprint` (from a prior `scp_last_fingerprint`).
/// Returns null on failure; check `scp_last_error_code` to distinguish
/// host-key prompts from real errors.
#[no_mangle]
pub extern "C" fn scp_connect(
    protocol: c_int,
    host: *const c_char,
    port: u16,
    username: *const c_char,
    password: *const c_char,
    bucket: *const c_char,
    region: *const c_char,
    host_key_mode: c_int,
    expected_fingerprint: *const c_char,
    auth_mode: c_int,
    key_path: *const c_char,
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
    let non_empty = |p: *const c_char| unsafe { cstr(p) }.filter(|s| !s.is_empty());

    let host_key = match host_key_mode {
        0 => HostKeyPolicy::Strict,
        1 => HostKeyPolicy::AcceptNew,
        2 => match non_empty(expected_fingerprint) {
            Some(fp) => HostKeyPolicy::AcceptFingerprint(fp.to_string()),
            None => {
                set_error("host_key_mode 2 requires expected_fingerprint");
                return ptr::null_mut();
            }
        },
        _ => {
            set_error("invalid host_key_mode");
            return ptr::null_mut();
        }
    };

    let auth = match auth_mode {
        0 => Auth::Password(pass.to_string()),
        1 => match non_empty(key_path) {
            Some(path) => Auth::KeyFile {
                path: path.to_string(),
                passphrase: (!pass.is_empty()).then(|| pass.to_string()),
            },
            None => {
                set_error("auth_mode 1 requires key_path");
                return ptr::null_mut();
            }
        },
        2 => Auth::Agent,
        _ => {
            set_error("invalid auth_mode");
            return ptr::null_mut();
        }
    };

    let mut creds = Credentials::basic(proto, host.to_string(), port, user.to_string(), auth);
    creds.bucket = non_empty(bucket).map(str::to_string);
    creds.region = non_empty(region).map(str::to_string);
    creds.host_key = host_key;

    match transport::connect(&creds) {
        Ok(inner) => Box::into_raw(Box::new(ScpSession { inner })),
        Err(e) => {
            set_error_typed(&e);
            ptr::null_mut()
        }
    }
}

/// Code classifying the last error on this thread (see SCP_ERR_* values).
#[no_mangle]
pub extern "C" fn scp_last_error_code() -> c_int {
    LAST_CODE.with(|c| c.get())
}

/// SHA256 fingerprint attached to the last host-key error on this thread, or
/// null. Borrowed; do not free.
#[no_mangle]
pub extern "C" fn scp_last_fingerprint() -> *const c_char {
    LAST_FINGERPRINT.with(|f| match &*f.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
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
            set_error_typed(&e);
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
            set_error_typed(&e);
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
            set_error_typed(&e);
            -1
        }
    }
}

/// Progress callback: `(transferred, total, user_data)`. `total` is 0 if
/// unknown. Return 0 to continue, non-zero to cancel the transfer.
pub type ProgressCb = extern "C" fn(u64, u64, *mut c_void) -> c_int;

/// Multi-file operation callback. `kind`: 0 = starting `file` (`total` bytes,
/// `done` is 1 for downloads / 0 for uploads), 1 = byte progress for the
/// current file (`file` is null), 2 = current file finished. Return 0 to
/// continue, non-zero to cancel.
pub type XferCb = extern "C" fn(
    kind: c_int,
    file: *const c_char,
    done: u64,
    total: u64,
    user_data: *mut c_void,
) -> c_int;

/// Download with progress reporting. Returns bytes transferred, or -1 on error.
/// `cb` is invoked on the calling thread; `user_data` is passed back verbatim.
#[no_mangle]
pub extern "C" fn scp_download_cb(
    session: *mut ScpSession,
    remote: *const c_char,
    local: *const c_char,
    cb: Option<ProgressCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(remote), Some(local)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(remote) },
        unsafe { cstr(local) },
    ) else {
        set_error("invalid arguments to scp_download_cb");
        return -1;
    };
    let user = UserData(user_data);
    let mut report = |t: u64, total: u64| -> bool {
        match cb {
            Some(cb) => cb(t, total, user.0) == 0,
            None => true,
        }
    };
    match session
        .inner
        .download_progress(remote, Path::new(local), &mut report)
    {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Upload with progress reporting. Returns bytes transferred, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_upload_cb(
    session: *mut ScpSession,
    local: *const c_char,
    remote: *const c_char,
    cb: Option<ProgressCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(local), Some(remote)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(local) },
        unsafe { cstr(remote) },
    ) else {
        set_error("invalid arguments to scp_upload_cb");
        return -1;
    };
    let user = UserData(user_data);
    let mut report = |t: u64, total: u64| -> bool {
        match cb {
            Some(cb) => cb(t, total, user.0) == 0,
            None => true,
        }
    };
    match session
        .inner
        .upload_progress(Path::new(local), remote, &mut report)
    {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Adapt a C XferCb into the core's ops callback.
fn xfer_adapter<'a>(
    cb: Option<XferCb>,
    user: &'a UserData,
) -> impl FnMut(crate::ops::XferEvent) -> bool + 'a {
    move |ev| {
        let Some(cb) = cb else { return true };
        match ev {
            crate::ops::XferEvent::Start { name, total, download } => {
                let Ok(name) = CString::new(name) else { return true };
                cb(0, name.as_ptr(), download as u64, total, user.0) == 0
            }
            crate::ops::XferEvent::Bytes { done, total } => {
                cb(1, ptr::null(), done, total, user.0) == 0
            }
            crate::ops::XferEvent::DoneFile => cb(2, ptr::null(), 0, 0, user.0) == 0,
        }
    }
}

/// Recursively download a remote directory. Returns total bytes, or -1.
#[no_mangle]
pub extern "C" fn scp_download_dir(
    session: *mut ScpSession,
    remote: *const c_char,
    local: *const c_char,
    excludes: *const c_char,
    overwrite_policy: c_int,
    cb: Option<XferCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(remote), Some(local)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(remote) },
        unsafe { cstr(local) },
    ) else {
        set_error("invalid arguments to scp_download_dir");
        return -1;
    };
    let filter = parse_filter(excludes);
    let policy = crate::ops::OverwritePolicy::from_code(overwrite_policy);
    let user = UserData(user_data);
    let mut adapter = xfer_adapter(cb, &user);
    match crate::ops::download_dir_opts(
        session.inner.as_mut(),
        remote,
        Path::new(local),
        &filter,
        policy,
        &mut adapter,
    ) {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Recursively upload a local directory. Returns total bytes, or -1.
#[no_mangle]
pub extern "C" fn scp_upload_dir(
    session: *mut ScpSession,
    local: *const c_char,
    remote: *const c_char,
    excludes: *const c_char,
    overwrite_policy: c_int,
    cb: Option<XferCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(local), Some(remote)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(local) },
        unsafe { cstr(remote) },
    ) else {
        set_error("invalid arguments to scp_upload_dir");
        return -1;
    };
    let filter = parse_filter(excludes);
    let policy = crate::ops::OverwritePolicy::from_code(overwrite_policy);
    let user = UserData(user_data);
    let mut adapter = xfer_adapter(cb, &user);
    match crate::ops::upload_dir_opts(
        session.inner.as_mut(),
        Path::new(local),
        remote,
        &filter,
        policy,
        &mut adapter,
    ) {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// One-way directory sync. `direction`: 0 = local→remote, 1 = remote→local.
/// `delete_extraneous`: non-zero enables mirror mode (removes destination
/// items that have no source counterpart).
/// Returns the number of files copied, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_sync_dir(
    session: *mut ScpSession,
    local: *const c_char,
    remote: *const c_char,
    direction: c_int,
    excludes: *const c_char,
    delete_extraneous: c_int,
    cb: Option<XferCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(local), Some(remote)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(local) },
        unsafe { cstr(remote) },
    ) else {
        set_error("invalid arguments to scp_sync_dir");
        return -1;
    };
    let dir = match direction {
        0 => crate::ops::SyncDirection::Upload,
        1 => crate::ops::SyncDirection::Download,
        _ => {
            set_error("invalid sync direction");
            return -1;
        }
    };
    let filter = parse_filter(excludes);
    let opts = crate::ops::SyncOptions { delete: delete_extraneous != 0 };
    let user = UserData(user_data);
    let mut adapter = xfer_adapter(cb, &user);
    match crate::ops::sync_dir_opts(
        session.inner.as_mut(),
        Path::new(local),
        remote,
        dir,
        &filter,
        &mut adapter,
        &opts,
    ) {
        Ok(stats) => stats.copied as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Sync dry run. Returns a JSON object the caller frees with scp_string_free:
/// {"dirs":[...],"items":[{"rel":...,"size":N,"reason":...}],"deletes":[...]}
/// or null on error. `direction`: 0 = local→remote, 1 = remote→local.
/// `delete_extraneous`: non-zero includes items to remove in mirror mode.
#[no_mangle]
pub extern "C" fn scp_sync_plan(
    session: *mut ScpSession,
    local: *const c_char,
    remote: *const c_char,
    direction: c_int,
    excludes: *const c_char,
    delete_extraneous: c_int,
) -> *mut c_char {
    clear_error();
    let (Some(session), Some(local), Some(remote)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(local) },
        unsafe { cstr(remote) },
    ) else {
        set_error("invalid arguments to scp_sync_plan");
        return ptr::null_mut();
    };
    let dir = match direction {
        0 => crate::ops::SyncDirection::Upload,
        1 => crate::ops::SyncDirection::Download,
        _ => {
            set_error("invalid sync direction");
            return ptr::null_mut();
        }
    };
    let filter = parse_filter(excludes);
    let opts = crate::ops::SyncOptions { delete: delete_extraneous != 0 };
    match crate::ops::plan_sync_opts(
        session.inner.as_mut(),
        Path::new(local),
        remote,
        dir,
        &filter,
        &opts,
    ) {
        Ok(plan) => match CString::new(plan_to_json(&plan)) {
            Ok(s) => s.into_raw(),
            Err(_) => {
                set_error("plan contained a NUL byte");
                ptr::null_mut()
            }
        },
        Err(e) => {
            set_error_typed(&e);
            ptr::null_mut()
        }
    }
}

/// Recursive remote search by mask. Returns a JSON array of
/// {"path":"/full/path","is_dir":bool,"size":N} (free with scp_string_free),
/// or null on error. At most `limit` results.
#[no_mangle]
pub extern "C" fn scp_find(
    session: *mut ScpSession,
    base: *const c_char,
    mask: *const c_char,
    limit: u32,
) -> *mut c_char {
    clear_error();
    let (Some(session), Some(base), Some(mask)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(base) },
        unsafe { cstr(mask) },
    ) else {
        set_error("invalid arguments to scp_find");
        return ptr::null_mut();
    };
    let mut keep_going = || true;
    match crate::ops::find(
        session.inner.as_mut(),
        base,
        mask,
        limit as usize,
        &mut keep_going,
    ) {
        Ok(hits) => {
            let mut out = String::from("[");
            for (i, (path, e)) in hits.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str("{\"path\":");
                json_str(&mut out, path);
                out.push_str(",\"is_dir\":");
                out.push_str(if e.is_dir { "true" } else { "false" });
                out.push_str(&format!(",\"size\":{}", e.size));
                out.push('}');
            }
            out.push(']');
            match CString::new(out) {
                Ok(s) => s.into_raw(),
                Err(_) => ptr::null_mut(),
            }
        }
        Err(e) => {
            set_error_typed(&e);
            ptr::null_mut()
        }
    }
}

fn parse_filter(excludes: *const c_char) -> crate::ops::Filter {
    unsafe { cstr(excludes) }
        .filter(|s| !s.is_empty())
        .map(crate::ops::Filter::parse)
        .unwrap_or_default()
}

fn plan_to_json(plan: &crate::ops::SyncPlan) -> String {
    let mut out = String::from("{\"dirs\":[");
    for (i, d) in plan.dirs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(&mut out, d);
    }
    out.push_str("],\"items\":[");
    for (i, item) in plan.items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"rel\":");
        json_str(&mut out, &item.rel);
        out.push_str(&format!(",\"size\":{}", item.size));
        out.push_str(",\"reason\":");
        json_str(&mut out, item.reason.label());
        out.push('}');
    }
    out.push_str("],\"deletes\":[");
    for (i, d) in plan.deletes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(&mut out, d);
    }
    out.push_str("]}");
    out
}

/// Create a remote directory. Returns 0, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_mkdir(session: *mut ScpSession, path: *const c_char) -> c_int {
    simple_op(session, path, |t, p| t.mkdir(p))
}

/// Delete a remote file. Returns 0, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_remove_file(session: *mut ScpSession, path: *const c_char) -> c_int {
    simple_op(session, path, |t, p| t.remove_file(p))
}

/// Recursively delete a remote directory. Returns 0, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_remove_dir_all(session: *mut ScpSession, path: *const c_char) -> c_int {
    simple_op(session, path, |t, p| crate::ops::remove_dir_all(t, p))
}

/// Resume a download from `offset` bytes (appends to the local file).
/// Returns total bytes (offset included), or -1 on error.
#[no_mangle]
pub extern "C" fn scp_download_resume_cb(
    session: *mut ScpSession,
    remote: *const c_char,
    local: *const c_char,
    offset: u64,
    cb: Option<ProgressCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(remote), Some(local)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(remote) },
        unsafe { cstr(local) },
    ) else {
        set_error("invalid arguments to scp_download_resume_cb");
        return -1;
    };
    let user = UserData(user_data);
    let mut report = |t: u64, total: u64| -> bool {
        match cb {
            Some(cb) => cb(t, total, user.0) == 0,
            None => true,
        }
    };
    match session
        .inner
        .download_resume(remote, Path::new(local), offset, &mut report)
    {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Resume an upload: appends the local file's tail after the remote file's
/// current size. Returns total bytes at the remote afterwards, or -1.
#[no_mangle]
pub extern "C" fn scp_upload_resume_cb(
    session: *mut ScpSession,
    local: *const c_char,
    remote: *const c_char,
    cb: Option<ProgressCb>,
    user_data: *mut c_void,
) -> i64 {
    clear_error();
    let (Some(session), Some(local), Some(remote)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(local) },
        unsafe { cstr(remote) },
    ) else {
        set_error("invalid arguments to scp_upload_resume_cb");
        return -1;
    };
    let user = UserData(user_data);
    let mut report = |t: u64, total: u64| -> bool {
        match cb {
            Some(cb) => cb(t, total, user.0) == 0,
            None => true,
        }
    };
    match session
        .inner
        .upload_resume(Path::new(local), remote, &mut report)
    {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Liveness probe / NAT keepalive. Returns 0 if the session answered,
/// -1 if it appears dead.
#[no_mangle]
pub extern "C" fn scp_keepalive(session: *mut ScpSession) -> c_int {
    clear_error();
    let Some(session) = (unsafe { session.as_mut() }) else {
        set_error("null session");
        return -1;
    };
    match session.inner.keepalive() {
        Ok(()) => 0,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Change unix permissions (mode, e.g. 0644 octal). Returns 0, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_chmod(session: *mut ScpSession, path: *const c_char, mode: u32) -> c_int {
    simple_op(session, path, |t, p| t.set_permissions(p, mode))
}

/// Rename/move a remote file or directory. Returns 0, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_rename(
    session: *mut ScpSession,
    from: *const c_char,
    to: *const c_char,
) -> c_int {
    clear_error();
    let (Some(session), Some(from), Some(to)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(from) },
        unsafe { cstr(to) },
    ) else {
        set_error("invalid arguments to scp_rename");
        return -1;
    };
    match session.inner.rename(from, to) {
        Ok(()) => 0,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

fn simple_op(
    session: *mut ScpSession,
    path: *const c_char,
    op: impl FnOnce(&mut dyn Transport, &str) -> crate::types::Result<()>,
) -> c_int {
    clear_error();
    let (Some(session), Some(path)) = (unsafe { session.as_mut() }, unsafe { cstr(path) }) else {
        set_error("invalid arguments");
        return -1;
    };
    match op(session.inner.as_mut(), path) {
        Ok(()) => 0,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Execute a remote command on the server (SFTP/SSH sessions only). Returns a
/// JSON string `{"exit_code":N,"stdout":"...","stderr":"..."}` (free with
/// scp_string_free), or null on error.
#[no_mangle]
pub extern "C" fn scp_exec_command(
    session: *mut ScpSession,
    cmd: *const c_char,
) -> *mut c_char {
    clear_error();
    let (Some(session), Some(cmd)) =
        (unsafe { session.as_mut() }, unsafe { cstr(cmd) })
    else {
        set_error("invalid arguments to scp_exec_command");
        return ptr::null_mut();
    };
    match session.inner.exec_command(cmd) {
        Ok(r) => {
            let mut out = String::from("{\"exit_code\":");
            out.push_str(&r.exit_code.to_string());
            out.push_str(",\"stdout\":");
            json_str(&mut out, &r.stdout);
            out.push_str(",\"stderr\":");
            json_str(&mut out, &r.stderr);
            out.push('}');
            match CString::new(out) {
                Ok(s) => s.into_raw(),
                Err(_) => {
                    set_error("exec output contained NUL byte");
                    ptr::null_mut()
                }
            }
        }
        Err(e) => {
            set_error_typed(&e);
            ptr::null_mut()
        }
    }
}

/// Server-side file copy (same session). Returns bytes copied, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_copy_file(
    session: *mut ScpSession,
    src: *const c_char,
    dst: *const c_char,
) -> i64 {
    clear_error();
    let (Some(session), Some(src), Some(dst)) = (
        unsafe { session.as_mut() },
        unsafe { cstr(src) },
        unsafe { cstr(dst) },
    ) else {
        set_error("invalid arguments to scp_copy_file");
        return -1;
    };
    match session.inner.copy_file(src, dst) {
        Ok(n) => n as i64,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
    }
}

/// Wrapper so the opaque `user_data` pointer can be moved into the progress
/// closure without tripping the borrow checker on the raw pointer.
struct UserData(*mut c_void);

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

/// List SCP Commander's trusted SSH host keys (its own store, not the system
/// ~/.ssh/known_hosts) as JSON: [{"host":"…","key_type":"…"}]. Caller frees
/// with scp_string_free; returns null only on an out-of-memory NUL error.
#[no_mangle]
pub extern "C" fn scp_list_known_hosts() -> *mut c_char {
    let hosts = crate::sftp::list_known_hosts();
    let mut s = String::from("[");
    for (i, h) in hosts.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"host\":");
        json_str(&mut s, &h.host);
        s.push_str(",\"key_type\":");
        json_str(&mut s, &h.key_type);
        s.push('}');
    }
    s.push(']');
    CString::new(s).map(CString::into_raw).unwrap_or(ptr::null_mut())
}

/// Forget a trusted host: remove every entry for `host` from the app store so
/// the next connection re-prompts. Returns entries removed, or -1 on error.
#[no_mangle]
pub extern "C" fn scp_remove_known_host(host: *const c_char) -> c_int {
    clear_error();
    let Some(host) = (unsafe { cstr(host) }) else {
        set_error("invalid host");
        return -1;
    };
    match crate::sftp::remove_known_host(host) {
        Ok(n) => n as c_int,
        Err(e) => {
            set_error_typed(&e);
            -1
        }
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
        s.push_str(",\"is_symlink\":");
        s.push_str(if e.is_symlink { "true" } else { "false" });
        s.push_str(",\"uid\":");
        match e.uid {
            Some(u) => s.push_str(&u.to_string()),
            None => s.push_str("null"),
        }
        s.push_str(",\"gid\":");
        match e.gid {
            Some(g) => s.push_str(&g.to_string()),
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
