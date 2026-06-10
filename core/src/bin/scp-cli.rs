//! Tiny CLI to exercise the core without any GUI.
//!
//!   scp-cli [flags] ls    <url>              [password]
//!   scp-cli [flags] get   [-r] <url> <local> [password]
//!   scp-cli [flags] put   [-r] <url> <local> [password]
//!   scp-cli [flags] rm    [-r] <url>         [password]
//!   scp-cli [flags] mv    <url> <new-path>   [password]
//!   scp-cli [flags] mkdir <url>              [password]
//!   scp-cli [flags] sync  up|down <url> <local-dir> [password]
//!
//! flags: --accept-new (trust unknown SSH host keys)
//!        --agent      (authenticate via ssh-agent)
//!        --key <path> (private key file; password arg becomes the passphrase)
//!
//! url: sftp://user@host[:port]/path  or  ftp://[user@]host[:port]/path

use std::path::Path;
use std::process::exit;

use scp_core::ops::{self, SyncDirection, XferEvent};
use scp_core::types::{Auth, Credentials, Error, HostKeyPolicy, Protocol};
use scp_core::{connect, Result};

fn usage() -> ! {
    eprintln!(
        "usage:\n  scp-cli [flags] ls    <url>              [password]\n  scp-cli [flags] get   [-r] <url> <local> [password]\n  scp-cli [flags] put   [-r] <url> <local> [password]\n  scp-cli [flags] rm    [-r] <url>         [password]\n  scp-cli [flags] mv    <url> <new-path>   [password]\n  scp-cli [flags] mkdir <url>              [password]\n  scp-cli [flags] sync  up|down <url> <local-dir> [password]\n\nflags: --accept-new | --agent | --key <path>\nurl: sftp://user@host[:port]/path  or  ftp://[user@]host[:port]/path"
    );
    exit(2);
}

struct ParsedUrl {
    creds: Credentials,
    path: String,
}

struct Flags {
    host_key: HostKeyPolicy,
    agent: bool,
    key: Option<String>,
    recursive: bool,
}

fn parse_url(url: &str, password: Option<&str>, flags: &Flags) -> std::result::Result<ParsedUrl, String> {
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
    let auth = if flags.agent {
        Auth::Agent
    } else if let Some(key) = &flags.key {
        Auth::KeyFile {
            path: key.clone(),
            passphrase: password.map(str::to_string),
        }
    } else {
        match password {
            Some(pw) => Auth::Password(pw.to_string()),
            None if protocol == Protocol::Ftp => Auth::Anonymous,
            None => Auth::Password(String::new()),
        }
    };

    let mut creds = Credentials::basic(protocol, host, port, username, auth);
    creds.host_key = flags.host_key.clone();
    Ok(ParsedUrl { creds, path })
}

/// Progress callback printing per-file lines for multi-file operations.
fn print_events(ev: XferEvent) -> bool {
    if let XferEvent::Start { name, total, download } = ev {
        let arrow = if download { "<-" } else { "->" };
        println!("{arrow} {name} ({total} bytes)");
    }
    true
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    let mut flags = Flags {
        host_key: HostKeyPolicy::Strict,
        agent: false,
        key: None,
        recursive: false,
    };
    if let Some(pos) = args.iter().position(|a| a == "--accept-new") {
        args.remove(pos);
        flags.host_key = HostKeyPolicy::AcceptNew;
    }
    if let Some(pos) = args.iter().position(|a| a == "--agent") {
        args.remove(pos);
        flags.agent = true;
    }
    if let Some(pos) = args.iter().position(|a| a == "--key") {
        args.remove(pos);
        if pos >= args.len() {
            usage();
        }
        flags.key = Some(args.remove(pos));
    }
    if let Some(pos) = args.iter().position(|a| a == "-r") {
        args.remove(pos);
        flags.recursive = true;
    }

    let cmd = args.first().map(String::as_str).unwrap_or("");

    match cmd {
        "ls" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(2).map(String::as_str), &flags)
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
            let parsed = parse_url(url, args.get(3).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = if flags.recursive {
                ops::download_dir(t.as_mut(), &parsed.path, Path::new(local), &mut print_events)?
            } else {
                t.download(&parsed.path, Path::new(local))?
            };
            println!("downloaded {n} bytes -> {local}");
            t.disconnect();
        }
        "put" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let local = args.get(2).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(3).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = if flags.recursive {
                ops::upload_dir(t.as_mut(), Path::new(local), &parsed.path, &mut print_events)?
            } else {
                t.upload(Path::new(local), &parsed.path)?
            };
            println!("uploaded {n} bytes -> {}", parsed.path);
            t.disconnect();
        }
        "rm" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(2).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            if flags.recursive {
                ops::remove_dir_all(t.as_mut(), &parsed.path)?;
            } else {
                t.remove_file(&parsed.path)?;
            }
            println!("removed {}", parsed.path);
            t.disconnect();
        }
        "mv" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let to = args.get(2).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(3).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            t.rename(&parsed.path, to)?;
            println!("renamed {} -> {to}", parsed.path);
            t.disconnect();
        }
        "mkdir" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(2).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            t.mkdir(&parsed.path)?;
            println!("created {}", parsed.path);
            t.disconnect();
        }
        "chmod" => {
            let mode = args
                .get(1)
                .and_then(|m| u32::from_str_radix(m, 8).ok())
                .unwrap_or_else(|| usage());
            let url = args.get(2).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(3).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            t.set_permissions(&parsed.path, mode)?;
            println!("chmod {mode:o} {}", parsed.path);
            t.disconnect();
        }
        "sync" => {
            let dir = match args.get(1).map(String::as_str) {
                Some("up") => SyncDirection::Upload,
                Some("down") => SyncDirection::Download,
                _ => usage(),
            };
            let url = args.get(2).unwrap_or_else(|| usage());
            let local = args.get(3).unwrap_or_else(|| usage());
            let parsed = parse_url(url, args.get(4).map(String::as_str), &flags)
                .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let stats =
                ops::sync_dir(t.as_mut(), Path::new(local), &parsed.path, dir, &mut print_events)?;
            println!(
                "sync done: {} copied, {} unchanged, {} bytes",
                stats.copied, stats.skipped, stats.bytes
            );
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
