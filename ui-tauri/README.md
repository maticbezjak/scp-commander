# SCP Commander — Tauri frontend (cross-platform)

A third frontend over the shared `scp-core` engine, built with [Tauri v2]. The
backend (`src-tauri/`) calls `scp-core` directly — same engine as the macOS and
GTK apps — and the UI is plain web (`dist/index.html`), so one codebase runs on
**macOS, Windows, and Linux**.

## Layout

- `src-tauri/` — the Rust backend: `#[tauri::command]` handlers wrap `scp-core`
  (connect, list_dir, …) and stream progress as Tauri events. Depends on the
  `scp-core` path crate; no FFI/sidecar needed.
- `dist/` — the static web UI (vanilla JS via `window.__TAURI__`, no npm build).

## Run / build

```sh
# Plain cargo (builds the binary; opens the window on a desktop session):
cargo run -p scp-tauri

# Or with the Tauri CLI (live reload, bundling):
cargo install tauri-cli --version '^2'
cargo tauri dev      # from ui-tauri/src-tauri
cargo tauri build    # produces installers under target/release/bundle
```

## Windows

`scp-core`'s dependencies (libssh2/`ssh2`, `native-tls` → SChannel, `rust-s3`)
all build on Windows; this crate enables vendored OpenSSL on Windows so libssh2
builds without a system OpenSSL. CI builds and uploads a Windows artifact, so
you can download and test without a local toolchain.

[Tauri v2]: https://v2.tauri.app/
