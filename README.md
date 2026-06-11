# SCP Commander — a WinSCP-style file manager for macOS & Ubuntu

A dual-pane SFTP/FTP file manager with **native** front-ends on each platform,
sharing one transfer core. SwiftUI on macOS, GTK4 on Ubuntu, Rust underneath.

```
            ┌──────────────────────┐     ┌──────────────────────┐
            │  macOS UI (SwiftUI)   │     │  Ubuntu UI (GTK4)     │
            └──────────┬───────────┘     └──────────┬───────────┘
                       │ C FFI (staticlib)          │ Rust rlib
                       └─────────────┬──────────────┘
                            ┌────────▼─────────┐
                            │  scp-core (Rust)  │
                            │  SFTP · FTP · S3  │
                            └──────────────────┘
```

## Layout

| Path          | What it is                                                        |
|---------------|-------------------------------------------------------------------|
| `core/`       | Shared Rust core: `Transport` trait, SFTP/FTP/S3 backends, multi-file ops, C FFI. |
| `core/src/bin/scp-cli.rs` | Headless CLI to exercise the core without any GUI.    |
| `ui-macos/`   | SwiftUI app (SwiftPM). Links `libscp_core.a` via `scp_core.h`.    |
| `ui-ubuntu/`  | GTK4 app (Rust). Links `scp-core` directly.                       |
| `scripts/`    | Packaging: `package-macos.sh` (.app + zip), `package-deb.sh` (.deb). |

## Status

Protocols:

- **SFTP** — libssh2; password / key-file / ssh-agent auth; streaming transfer
  progress; host key verification (checked against `~/.ssh/known_hosts`
  read-only and the app's own store `~/.config/scp-commander/known_hosts`;
  unknown servers trigger a fingerprint trust prompt, mismatches always fail).
- **FTP / FTPS** — streaming transfers; FTPS upgrades the control channel via
  native-tls.
- **S3** — behind the `s3` cargo feature (rust-s3): ranged-GET streaming
  downloads, multipart streaming uploads (8 MiB memory bound), bucket/region/
  endpoint fields in both UIs.

Both apps (SwiftUI + GTK4) have:

- **overwrite protection** (Overwrite / Skip prompts; partials resume in
  both directions) and **preview-first sync** (WinSCP-style checklist)
- a **dedicated transfer connection per tab** — transfers never block
  browsing; **exclusion masks** ("*.tmp; .git/") for folder ops and sync
- **Find Files** (recursive remote search), sites with **initial
  directories**, **workspace restore** on launch, Finder/Nautilus **drops**,
  Open in Terminal, Copy URL, sftp:// URL registration
- **keyboard commander**: F5 copy, F6 move, F2 rename, Del, Backspace
  parent, Tab to switch panes; **multi-select** with batch operations
- **auto-reconnect** (dead sessions revive transparently, with 30s NAT
  keepalives), **download resume**, server **mtimes preserved** on download
- editable path bars, show-hidden toggle, transfer **speed + ETA**
- **WinSCP.ini import** (Tools menu) alongside the JSON site exchange

- WinSCP-style **Login dialog** (sites + session form), **session tabs**
  (independent connections per tab), and multi-column panes
  (Name | Size | Type | Changed | Rights) with per-pane toolbars
- a **Properties dialog** with an rwx checkbox grid (chmod over SFTP/FTP,
  POSIX permissions locally)
- dual-pane local/remote browsing, drag-and-drop between panes (files and
  folders, recursive)
- a transfer queue with live progress and per-transfer **cancel**
- file management: new folder, rename, delete (recursive on folders), via
  context menus
- **one-way directory sync** in either direction (size + mtime comparison)
- **remote editing**: open a remote file in your editor; saves upload
  automatically
- saved sites with passwords in the **macOS Keychain** / **Secret Service**
  (GNOME Keyring)

The core's multi-file ops (recursive transfers, sync, recursive delete) are
unit-tested against an in-memory fake transport (`cargo test -p scp-core`).

## Build & run

### Prerequisites (macOS)
```sh
brew install libssh2 openssl@3 pkg-config
# Rust: https://rustup.rs   |   Swift 5.9+ (Xcode or Command Line Tools)
```
`pkg-config` must find `libssh2.pc` (Homebrew installs it under
`/opt/homebrew/lib/pkgconfig`).

### Core + CLI
```sh
cargo build -p scp-core                 # SFTP + FTP/FTPS
cargo build -p scp-core --features s3   # also build the S3 backend
cargo test  -p scp-core                 # unit tests (ops engine, fingerprints)

scp-cli ls    sftp://user@host/path  password
scp-cli get   -r sftp://user@host/dir ./local-dir  password   # recursive
scp-cli sync  up sftp://user@host/dir ./local-dir  password   # one-way sync
scp-cli mkdir sftp://user@host/new-dir  password
scp-cli --agent ls sftp://user@host/    # ssh-agent auth
scp-cli --key ~/.ssh/id_ed25519 ls sftp://user@host/  [passphrase]
```

### macOS app
```sh
cargo build -p scp-core --features s3   # produces target/debug/libscp_core.a
cd ui-macos && swift run                # builds and launches the SwiftUI app
```
`Package.swift` links the staticlib from `../target/debug` (override with
`SCP_CORE_LIB=../target/release`) plus `-lssl -lcrypto -lz -liconv` and the
`CoreFoundation`/`Security` frameworks (the exact list comes from
`cargo rustc -- --print native-static-libs`).

### Ubuntu app
```sh
sudo apt install libgtk-4-dev build-essential pkg-config libssl-dev libssh2-1-dev
cargo run -p scp-ubuntu --features scp-core/s3
```
Needs GTK ≥ 4.10 (Ubuntu 24.04+). The GTK app targets Linux, but it also
compiles and runs against Homebrew's gtk4 (`brew install gtk4`) on macOS —
handy for development; the native-feel target remains GNOME/Ubuntu.

The Ubuntu app runs all core calls on a worker thread (GTK widgets are
main-thread-only): commands go over a std mpsc channel, events come back over
an async channel drained by `glib::spawn_future_local`.

## Packaging

```sh
./scripts/package-macos.sh   # dist/ScpCommander.app (icon included) + zip
./scripts/package-deb.sh     # dist/scp-commander_<ver>_<arch>.deb (run on Ubuntu)
# Flatpak manifest: packaging/flatpak/net.manto.ScpCommander.yml
```

## Integration tests

`./scripts/integration-test.sh` spins up SFTP, FTP, and MinIO servers in
Docker and drives `scp-cli` through the full operation matrix (transfers,
recursive trees, sync + dry-run plan, find, rename, chmod, deletes) with
round-trip diffs — 33 checks, all green. CI (.github/workflows/ci.yml) runs
the same suite on every push, alongside the unit tests and both app builds.

## How "native" is achieved

There is no shared UI code. Each OS gets its own front-end written in that
platform's idiomatic toolkit, so widgets, fonts, and window chrome are the real
system controls. Everything hard (protocols, transfers, sync) lives once in
`scp-core` and is reused by both.
