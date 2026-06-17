# SCP Commander — Tauri frontend (cross-platform)

A third frontend over the shared `scp-core` engine, built with [Tauri v2]. The
backend (`src-tauri/`) calls `scp-core` directly — same engine as the macOS and
GTK apps — and the UI is plain web (`dist/index.html`), so one codebase runs on
**macOS, Windows, and Linux**.

## Layout

- `src-tauri/` — the Rust backend: `#[tauri::command]` handlers wrap `scp-core`
  (connect, list, transfers, …) and stream progress as Tauri events. Depends on
  the `scp-core` path crate; no FFI/sidecar needed.
- `src/` — the **Svelte 5 + Vite** frontend (`App.svelte`, `lib/Pane.svelte`,
  `lib/api.js`). Talks to the backend via `window.__TAURI__` (so no
  `@tauri-apps/api` dependency).
- `dist/` — Vite build output (gitignored; built by `npm run build`).

## Run / build

```sh
cd ui-tauri
npm install                 # once

# Dev with live reload (Tauri spins up Vite via beforeDevCommand):
cargo install tauri-cli --version '^2'
cargo tauri dev             # from ui-tauri/src-tauri

# Plain cargo: build the frontend first so `frontendDist` exists, then:
npm run build
cargo build -p scp-tauri    # from the repo root

cargo tauri build           # installers under target/release/bundle
```

CI builds the frontend (`npm ci && npm run build`) before `cargo build` on all
three OSes and uploads the Windows binary as an artifact.

## Windows

`scp-core`'s dependencies (libssh2/`ssh2`, `native-tls` → SChannel, `rust-s3`)
all build on Windows; this crate enables vendored OpenSSL on Windows so libssh2
builds without a system OpenSSL. CI builds and uploads a Windows artifact, so
you can download and test without a local toolchain.

[Tauri v2]: https://v2.tauri.app/
