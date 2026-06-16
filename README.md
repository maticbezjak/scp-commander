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

## Using the app

### Connecting

1. Launch the app — the **Login dialog** opens automatically.
2. Choose a **protocol** (SFTP · FTP · FTPS · S3) and fill in the host, port,
   and credentials.
   - SFTP supports **password**, **private key file**, or **ssh-agent** auth
     (pick from the Authentication drop-down).
   - S3: leave the host blank for AWS, or enter a custom endpoint (e.g. MinIO).
3. Tick **Remember password** to store the password securely (macOS Keychain /
   GNOME Keyring). Next time you type the same host + user, the password fills
   in automatically.
4. Click **Login** (or press Enter).  
   If the server's host key is new, a fingerprint prompt appears — review it and
   click **Trust & Connect** to accept.

### Saving sites

Click **Save site…** in the Login dialog to bookmark the current credentials
under a name (optionally in a `Folder/Name` group). Saved sites appear in the
left sidebar; double-click one to connect instantly.

To delete or rename a site, right-click it in the sidebar.

### Browsing

- The **left pane** always shows your local filesystem; the **right pane** shows
  the remote server.
- Click a column header (**Name / Size / Type / Changed / Rights**) to sort.
- Double-click a **folder** to enter it; double-click the **`..`** row at the
  top (or press **Backspace**) to go up one level.
- The **path bar** below the toolbar is editable — type a path and press Enter
  to jump directly.
- Toggle **show hidden files** with the eye-icon button (macOS toolbar) or the
  reveal button (Ubuntu toolbar).

### Transferring files

| Action | How |
|---|---|
| Upload file(s) | Select in the local pane → press **F5** or click the ↑ button |
| Download file(s) | Select in the remote pane → press **F5** or click the ↓ button |
| Drag and drop | Drag files between the two panes |
| Move instead of copy | Select → **F6** |

Folders are transferred recursively. When the destination already has a file —
or a folder — of the same name, you'll get an **Overwrite / Overwrite only
newer / Skip existing / Cancel** prompt. For a folder the choice applies to each
file inside it: *Skip existing* still copies files that are new, *Only newer*
replaces only files older than the source.

Quitting the app while transfers are still running asks for confirmation first,
so an in-flight copy is never killed silently.

Transfers run in the background on a dedicated connection — you can keep
browsing while files copy. Watch progress in the **transfer queue** panel at the
bottom; each row has its own **×** cancel button.

### Multi-select

- **Click** to select one item; **Shift-click** extends the selection;
  **Ctrl/Cmd-click** (macOS) or **Ctrl-click** (Ubuntu) toggles individual
  items.
- All selected items transfer together when you press F5/F6 or drag.

### Keyboard shortcuts

| Key | Action |
|---|---|
| `F5` | Copy (transfer) selected items to the other pane |
| `F6` | Move selected items to the other pane |
| `F2` | Rename selected item |
| `F3` | View selected file in the built-in read-only viewer |
| `Del` | Delete selected item(s) |
| `Backspace` | Navigate to parent directory |
| `Tab` | Switch focus between left and right pane |
| `Enter` | Open folder / transfer file |

### Directory sync

Click the **↑ sync** or **↓ sync** button in the toolbar to synchronise a pair
of local/remote directories.  
A **preview checklist** shows exactly which files will be copied or deleted —
review it, tick/untick items, then confirm.  
Tick **Mirror** to also delete destination items that have no source counterpart.

### Finding files

Click the 🔍 (search) button to search the current remote directory recursively
by name mask (e.g. `*.log`).  
Results appear in a list; double-click any hit to navigate to its directory.

### Remote editing

Right-click a remote file → **Edit**.  
The file downloads to a temp location and opens in your default editor.  
Every time you save, it uploads automatically.

### Open in terminal / Copy URL

- **Open terminal** (🖥 button) — opens an SSH session to the current remote
  host in your system terminal (SFTP only).
- **Copy URL** — copies an `sftp://user@host/path` URL for the selected item to
  the clipboard.

---

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
