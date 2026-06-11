//! Tiny CLI to exercise the core without any GUI.
//!
//!   scp-cli [flags] ls    <url>              [password]
//!   scp-cli [flags] get   [-r] <url> <local> [password]
//!   scp-cli [flags] put   [-r] <url> <local> [password]
//!   scp-cli [flags] rm    [-r] <url>         [password]
//!   scp-cli [flags] mv    <url> <new-path>   [password]
//!   scp-cli [flags] mkdir <url>              [password]
//!   scp-cli [flags] cp    <url> <new-remote> [password]
//!   scp-cli [flags] exec  <url> <command>    [password]
//!   scp-cli [flags] sync  up|down <url> <local-dir> [password]
//!   scp-cli [flags] plan  up|down <url> <local-dir> [password]
//!   scp-cli [flags] find  <url> <mask>       [password]
//!
//! flags: --accept-new  (trust unknown SSH host keys)
//!        --agent       (authenticate via ssh-agent)
//!        --key <path>  (private key file; password arg = passphrase)
//!        --exclude "*.tmp; .git/"
//!        --delete      (mirror mode: delete extraneous destination files)
//!        --speed <kbs> (transfer speed limit in KiB/s)
//!
//! url: sftp://user@host[:port]/path
//!      ftp://[user@]host[:port]/path
//!      ftps://[user@]host[:port]/path
//!      s3://accesskey@endpoint[:port]/bucket[/prefix]

use std::path::Path;
use std::process::exit;
use std::thread;
use std::time::Duration;

use scp_core::ops::{self, Filter, SyncDirection, SyncOptions, XferEvent};
use scp_core::transport::Progress;
use scp_core::types::{Auth, Credentials, Error, HostKeyPolicy, Protocol};
use scp_core::{connect, Result};

fn usage() -> ! {
    eprintln!(
        "usage:
  scp-cli [flags] ls    <url>              [password]
  scp-cli [flags] get   [-r] <url> <local> [password]
  scp-cli [flags] put   [-r] <url> <local> [password]
  scp-cli [flags] rm    [-r] <url>         [password]
  scp-cli [flags] mv    <url> <new-path>   [password]
  scp-cli [flags] mkdir <url>              [password]
  scp-cli [flags] cp    <url> <new-remote> [password]
  scp-cli [flags] exec  <url> <command>    [password]
  scp-cli [flags] chmod <mode> <url>       [password]
  scp-cli [flags] sync  up|down <url> <local-dir> [password]
  scp-cli [flags] plan  up|down <url> <local-dir> [password]
  scp-cli [flags] find  <url> <mask>       [password]

flags: --accept-new | --agent | --key <path>
       --exclude \"*.tmp; .git/\"
       --delete   (mirror sync: remove extraneous destination items)
       --speed <kbs>  (KiB/s speed cap; 0 = unlimited)

url: sftp://user@host[:port]/path  |  ftp[s]://user@host[:port]/path
     s3://accesskey@endpoint[:port]/bucket[/prefix]"
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
    filter: Filter,
    delete: bool,
    speed_kbs: u64,
}

fn parse_url(
    url: &str,
    password: Option<&str>,
    flags: &Flags,
) -> std::result::Result<ParsedUrl, String> {
    let (scheme, rest) = url.split_once("://").ok_or("missing scheme://")?;
    let protocol = match scheme {
        "sftp" => Protocol::Sftp,
        "ftp" => Protocol::Ftp,
        "ftps" => Protocol::Ftps,
        "s3" => Protocol::S3,
        other => return Err(format!("unsupported scheme: {other}")),
    };

    let (authority, raw_path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };

    let (userinfo, hostport) = match authority.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };

    let (host, port_parsed) = match hostport.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().map_err(|_| "invalid port".to_string())?),
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

    if protocol == Protocol::S3 {
        // raw_path = "/bucket/prefix/..." — first component is the bucket.
        let trimmed = raw_path.trim_start_matches('/');
        let (bucket, key_prefix) = match trimmed.split_once('/') {
            Some((b, k)) => (b.to_string(), format!("/{k}")),
            None => (trimmed.to_string(), "/".to_string()),
        };
        let mut creds = Credentials::basic(protocol, host, port_parsed, username, auth);
        creds.bucket = Some(bucket);
        return Ok(ParsedUrl { creds, path: key_prefix });
    }

    let mut creds =
        Credentials::basic(protocol, host, port_parsed, username, auth);
    creds.host_key = flags.host_key.clone();
    Ok(ParsedUrl { creds, path: raw_path })
}

/// Progress callback that optionally sleeps to enforce a speed cap.
fn make_progress(speed_kbs: u64) -> impl FnMut(u64, u64) -> bool {
    let mut last_done: u64 = 0;
    move |done: u64, _total: u64| -> bool {
        if speed_kbs > 0 && done > last_done {
            let chunk = done - last_done;
            // Sleep proportional to how long this chunk should have taken.
            let micros = chunk * 1_000_000 / (speed_kbs * 1024);
            if micros > 0 {
                thread::sleep(Duration::from_micros(micros));
            }
        }
        last_done = done;
        true
    }
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
        filter: Filter::empty(),
        delete: false,
        speed_kbs: 0,
    };

    // Parse flags (all must appear before the command).
    if let Some(pos) = args.iter().position(|a| a == "--exclude") {
        args.remove(pos);
        if pos >= args.len() { usage(); }
        flags.filter = Filter::parse(&args.remove(pos));
    }
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
        if pos >= args.len() { usage(); }
        flags.key = Some(args.remove(pos));
    }
    if let Some(pos) = args.iter().position(|a| a == "-r") {
        args.remove(pos);
        flags.recursive = true;
    }
    if let Some(pos) = args.iter().position(|a| a == "--delete") {
        args.remove(pos);
        flags.delete = true;
    }
    if let Some(pos) = args.iter().position(|a| a == "--speed") {
        args.remove(pos);
        if pos >= args.len() { usage(); }
        flags.speed_kbs =
            args.remove(pos).parse::<u64>().unwrap_or_else(|_| fail("--speed requires a number"));
    }

    let cmd = args.first().map(String::as_str).unwrap_or("");

    match cmd {
        "ls" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(2).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let mut entries = t.list_dir(&parsed.path)?;
            entries.sort_by(|a, b| (b.is_dir, &a.name).cmp(&(a.is_dir, &b.name)));
            for e in entries {
                let kind = if e.is_dir { "d" } else { "-" };
                let perms = e.perms.unwrap_or_else(|| "?????????".into());
                let owner = e.uid.map(|u| u.to_string()).unwrap_or_else(|| "?".into());
                let group = e.gid.map(|g| g.to_string()).unwrap_or_else(|| "?".into());
                println!(
                    "{kind}{perms}  {owner:>8} {group:>8} {:>12} {}",
                    e.size, e.name
                );
            }
            t.disconnect();
        }
        "get" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let local = args.get(2).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = if flags.recursive {
                ops::download_dir(
                    t.as_mut(),
                    &parsed.path,
                    Path::new(local),
                    &flags.filter,
                    &mut print_events,
                )?
            } else {
                let speed = flags.speed_kbs;
                let mut prog = make_progress(speed);
                let mut p: Progress = &mut prog;
                t.download_progress(&parsed.path, Path::new(local), &mut p)?
            };
            println!("downloaded {n} bytes -> {local}");
            t.disconnect();
        }
        "put" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let local = args.get(2).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = if flags.recursive {
                ops::upload_dir(
                    t.as_mut(),
                    Path::new(local),
                    &parsed.path,
                    &flags.filter,
                    &mut print_events,
                )?
            } else {
                let speed = flags.speed_kbs;
                let mut prog = make_progress(speed);
                let mut p: Progress = &mut prog;
                t.upload_progress(Path::new(local), &parsed.path, &mut p)?
            };
            println!("uploaded {n} bytes -> {}", parsed.path);
            t.disconnect();
        }
        "rm" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(2).map(String::as_str), &flags)
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
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            t.rename(&parsed.path, to)?;
            println!("renamed {} -> {to}", parsed.path);
            t.disconnect();
        }
        "mkdir" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(2).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            t.mkdir(&parsed.path)?;
            println!("created {}", parsed.path);
            t.disconnect();
        }
        "cp" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let dst = args.get(2).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let n = t.copy_file(&parsed.path, dst)?;
            println!("copied {} -> {dst} ({n} bytes)", parsed.path);
            t.disconnect();
        }
        "exec" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let remote_cmd = args.get(2).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let result = t.exec_command(remote_cmd)?;
            if !result.stdout.is_empty() {
                print!("{}", result.stdout);
            }
            if !result.stderr.is_empty() {
                eprint!("{}", result.stderr);
            }
            t.disconnect();
            exit(result.exit_code);
        }
        "chmod" => {
            let mode = args
                .get(1)
                .and_then(|m| u32::from_str_radix(m, 8).ok())
                .unwrap_or_else(|| usage());
            let url = args.get(2).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            t.set_permissions(&parsed.path, mode)?;
            println!("chmod {mode:o} {}", parsed.path);
            t.disconnect();
        }
        "plan" => {
            let dir = match args.get(1).map(String::as_str) {
                Some("up") => SyncDirection::Upload,
                Some("down") => SyncDirection::Download,
                _ => usage(),
            };
            let url = args.get(2).unwrap_or_else(|| usage());
            let local = args.get(3).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(4).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let opts = SyncOptions { delete: flags.delete };
            let mut t = connect(&parsed.creds)?;
            let plan = ops::plan_sync_opts(
                t.as_mut(),
                Path::new(local),
                &parsed.path,
                dir,
                &flags.filter,
                &opts,
            )?;
            for d in &plan.dirs {
                println!("mkdir {d}");
            }
            for item in &plan.items {
                println!(
                    "copy  {} ({} bytes, {})",
                    item.rel, item.size, item.reason.label()
                );
            }
            for d in &plan.deletes {
                println!("delete {d}");
            }
            println!(
                "plan: {} dir(s), {} copy, {} delete",
                plan.dirs.len(),
                plan.items.len(),
                plan.deletes.len()
            );
            t.disconnect();
        }
        "find" => {
            let url = args.get(1).unwrap_or_else(|| usage());
            let mask = args.get(2).unwrap_or_else(|| usage());
            let parsed =
                parse_url(url, args.get(3).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let mut t = connect(&parsed.creds)?;
            let hits = ops::find(t.as_mut(), &parsed.path, mask, 1000, &mut || true)?;
            for (path, e) in &hits {
                println!("{}{}", path, if e.is_dir { "/" } else { "" });
            }
            println!("{} match(es)", hits.len());
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
            let parsed =
                parse_url(url, args.get(4).map(String::as_str), &flags)
                    .unwrap_or_else(|e| fail(&e));
            let opts = SyncOptions { delete: flags.delete };
            let mut t = connect(&parsed.creds)?;
            let stats = ops::sync_dir_opts(
                t.as_mut(),
                Path::new(local),
                &parsed.path,
                dir,
                &flags.filter,
                &mut print_events,
                &opts,
            )?;
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
            eprintln!(
                "hint: verify the fingerprint with the server admin, then re-run with \
                 --accept-new to trust it"
            );
            exit(1);
        }
        fail(&e.to_string());
    }
}
