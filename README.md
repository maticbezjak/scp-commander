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
| `core/`       | Shared Rust core: `Transport` trait, SFTP/FTP/S3 backends, C FFI. |
| `core/src/bin/scp-cli.rs` | Headless CLI to exercise the core without any GUI.    |
| `ui-macos/`   | SwiftUI app (SwiftPM). Links `libscp_core.a` via `scp_core.h`.    |
| `ui-ubuntu/`  | GTK4 app (Rust). Links `scp-core` directly. Build on Linux only.  |

## Status

- **SFTP** — implemented (libssh2).
- **FTP** — implemented (plain). FTPS is stubbed behind the same trait.
- **S3** — stubbed; slots into the `Transport` trait when implemented.

Phasing from here: transfer queue → directory sync → FTPS/S3 → remote editor.

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
cargo build -p scp-core
./target/debug/scp-cli ls sftp://user@host/path        # prompts via arg: append password
./target/debug/scp-cli get sftp://user@host/file ./out  yourpassword
```

### macOS app
```sh
cargo build -p scp-core          # produces target/debug/libscp_core.a
cd ui-macos && swift run         # builds and launches the SwiftUI app
```
`Package.swift` links the staticlib from `../target/debug` plus
`-lssl -lcrypto -lz -liconv` and the `CoreFoundation` framework (the exact list
comes from `cargo rustc -- --print native-static-libs`). For a release build,
`cargo build -p scp-core --release` and change `coreLib` to `../target/release`.

### Ubuntu app (build on Linux)
```sh
sudo apt install libgtk-4-dev build-essential pkg-config libssl-dev libssh2-1-dev
cargo run -p scp-ubuntu
```
Won't compile on macOS — GTK4 is the Linux-native toolkit (which is the whole
point of going native per platform).

## How "native" is achieved

There is no shared UI code. Each OS gets its own front-end written in that
platform's idiomatic toolkit, so widgets, fonts, and window chrome are the real
system controls. Everything hard (protocols, transfers, sync) lives once in
`scp-core` and is reused by both.
