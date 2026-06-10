//! Tiny CLI to exercise the core without any GUI.
//!
//!   scp-cli ls   sftp://user@host[:port]/path        [password]
//!   scp-cli get  sftp://user@host[:port]/remote/file ./local      [password]
//!   scp-cli put  sftp://user@host[:port]/remote/file ./local      [password]
//!
//! `ftp://` also works (anonymous if user is omitted).

use std::path::Path;
use std::process::exit;

use scp_core::types::{Auth, Credentials, Error, HostKeyPolicy, Protocol};
use scp_core::{connect, Result};

fn usage() -> ! {
    eprintln!(
        "usage:\n  scp-cli [--accept-new] ls  <url> [password]\n  scp-cli [--accept-new] get <url> <local> [password]\n  scp-cli [--accept-new] put <url> <local> [password]\n\nurl: sftp://user@host[:port]/path  or  ftp://[user@]host[:port]/path\n--accept-new: trust and remember unknown SSH host keys (first connect)"
    );
    exit(2);
}

struct ParsedUrl {
    creds: Credentials,
    path: String,
}

fn parse_url(
    url: &str,
    password: Option<&str>,
    host_key: HostKeyPolicy,
) -> std::result::Result<ParsedUrl, String> {
    let (scheme, rest) = url.split_once("://").ok_or("missing scheme://")?;
    let protocol = match scheme {
        "sftp" => Protocol::Sftp,
        "ftp" => Protocol::Ftp,
        "ftps" => Protocol::Ftps,
        other => return Err(format!("unsupported scheme: {other}")),
    };

    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };

    let (userinfo, hostport) = match authority.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };

    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse().map_err(|_| "invalid port".to_string())?,
        ),
        None => (hostport.to_string(), Credentials::default_port(protocol)),
    };

    let username = userinfo.unwrap_or("anonymous").to_string();
    let auth = match password {
        Some(pw) => Auth::Password(pw.to_string()),
        None if protocol == Protocol::Ftp => Auth::Anonymous,
        None => Auth::Password(String::new()),
    };

    let mut creds = Credentials::basic(protocol, host, port, username, auth);
    creds.host_key = host_key;
    Ok(ParsedUrl { creds, path })
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let host_key = if let Some(pos) = args.iter().position(|a| a == "--accept-new") {
        args.remove(pos);
        HostKeyPolicy::AcceptNew
    } else {
        HostKeyPolicy::Strict
    };
    let cmd = args.first().map(String::as_str).unwrap_or("");

    match cmd {
        "ls" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(2).map(String::as_str), host_key)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let mut entries = t.list_dir(&parsed.path)?;
            entries.sort_by(|a, b| (b.is_dir, &a.name).cmp(&(a.is_dir, &b.name)));
            for e in entries {
                let kind = if e.is_dir { "d" } else { "-" };
                let perms = e.perms.unwrap_or_else(|| "?????????".into());
                println!("{kind}{perms} {:>12} {}", e.size, e.name);
            }
            t.disconnect();
        }
        "get" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let local = args.get(2).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(3).map(String::as_str), host_key)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = t.download(&parsed.path, Path::new(local))?;
            println!("downloaded {n} bytes -> {local}");
            t.disconnect();
        }
        "put" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let local = args.get(2).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(3).map(String::as_str), host_key)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = t.upload(Path::new(local), &parsed.path)?;
            println!("uploaded {n} bytes -> {}", parsed.path);
            t.disconnect();
        }
        _ => usage(),
    }
    Ok(())
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1);
}

fn main() {
    if let Err(e) = run() {
        if matches!(e, Error::UnknownHostKey { .. }) {
            eprintln!("error: {e}");
            eprintln!("hint: verify the fingerprint with the server admin, then re-run with --accept-new to trust it");
            exit(1);
        }
        fail(&e.to_string());
    }
}
