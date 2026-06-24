# SCP Commander — a WinSCP-style file manager

A cross-platform dual-pane SFTP/FTP/S3 file manager: a Tauri + Svelte desktop
app over one shared Rust transfer core, running on macOS, Windows, and Linux.

```
                       ┌──────────────────────────┐
                       │  ui-tauri (Svelte + Rust) │   macOS · Windows · Linux
                       └────────────┬─────────────┘
                                    │ Rust rlib
                           ┌────────▼─────────┐
                           │  scp-core (Rust)  │
                           │  SFTP · FTP · S3  │
                           └──────────────────┘
```

> The original native front-ends (SwiftUI on macOS, GTK4 on Ubuntu, linked via a
> C FFI) were superseded by the Tauri app and retired from `master`. They remain
> available on the **`archive/native-apps`** branch.

## Layout

| Path          | What it is                                                        |
|---------------|-------------------------------------------------------------------|
| `core/`       | Shared Rust core: `Transport` trait, SFTP/FTP/S3 backends, multi-file ops, sync engine. |
| `core/src/bin/scp-cli.rs` | Headless CLI to exercise the core without any GUI.    |
| `ui-tauri/`   | The desktop app — Svelte 5 frontend + Tauri (Rust) backend linking `scp-core`. |
| `scripts/`    | `integration-test.sh` (Docker SFTP/FTP/S3 smoke test).            |

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
  endpoint fields in the connect dialog.

The app has:

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

### Jump host (bastion / ProxyJump)

To reach an SFTP server that's only accessible through a bastion, tick
**Connect through a jump host** in the Login dialog and fill in the bastion's
host/port/user and auth (password, key, or agent). The app authenticates to the
bastion, opens a tunnel to the target, and runs the whole SFTP session
end-to-end over it — the target's own host key is what gets verified. Jump
settings are saved with the site (the bastion password is re-entered or, better,
use a key/agent).

### Saving sites

Click **Save site…** in the Login dialog to bookmark the current credentials
under a name (optionally in a `Folder/Name` group). Saved sites appear in the
left sidebar; double-click one to connect instantly.

To delete or rename a site, right-click it in the sidebar.

**Tools menu** (in the Login dialog) can import existing connections so you
don't retype them:

- **Import from ~/.ssh/config** — turns every concrete `Host` entry into an
  SFTP site (grouped under an "SSH" folder), picking up HostName, User, Port,
  and IdentityFile.
- **Import from WinSCP INI** — migrates WinSCP `[Sessions\…]` entries.
- **Import / Export sites** — move sites between machines via a shared JSON
  file (passwords stay in the system keychain, never in the file).

### Preferences

A Preferences window collects the cross-session settings in one place:

- **Editor** — the app/command used for *Edit* on remote files (empty = the
  system default for each file type).
- **Parallel connections** — how many simultaneous transfer connections each
  session opens (1–8; applies to sessions connected afterwards).
- **Keepalive interval** — how often idle sessions send a NAT keepalive.
- **Default exclude masks** — masks pre-filled for folder transfers and sync.

On macOS it's the standard ⌘, window; on Ubuntu it's under the Login dialog's
**Tools** menu.

### Browsing

- The **left pane** always shows your local filesystem; the **right pane** shows
  the remote server.
- Click a column header (**Name / Size / Type / Changed / Rights**) to sort.
- Double-click a **folder** to enter it; double-click the **`..`** row at the
  top (or press **Backspace**) to go up one level.
- The **path bar** below the toolbar is editable — type a path and press Enter
  to jump directly (focus it with **⌘/Ctrl L**). The clock dropdown beside it
  lists **recent locations** for that pane.
- Toggle **show hidden files** with the eye-icon button (macOS toolbar) or the
  reveal button (Ubuntu toolbar).
- A **status line** under each pane shows the item count, or the number and
  total size of the current selection.
- The **local pane refreshes itself** when files change on disk (e.g. a download
  from another app), so you rarely need to refresh manually.
- Right-click any item for **Copy path**; local items also offer **Reveal in
  Finder** (macOS) / **Show in Files** (Ubuntu).
- **Synchronized browsing** (Options menu): entering or leaving a folder in one
  pane mirrors the move in the other pane whenever a folder of the same name
  exists there — handy for walking parallel local/remote trees.

### Transferring files

| Action | How |
|---|---|
| Upload file(s) | Select in the local pane → press **F5** or click the ↑ button |
| Download file(s) | Select in the remote pane → press **F5** or click the ↓ button |
| Drag and drop | Drag files between the two panes |
| Drag out | Drag an item onto the Desktop/Finder to download it (macOS); local items drag to the file manager on both platforms |
| Move instead of copy | Select → **F6** |

Folders are transferred recursively. When the destination already has a file —
or a folder — of the same name, you'll get an **Overwrite / Overwrite only
newer / Skip existing / Cancel** prompt. For a folder the choice applies to each
file inside it: *Skip existing* still copies files that are new, *Only newer*
replaces only files older than the source.

Quitting the app while transfers are still running asks for confirmation first,
so an in-flight copy is never killed silently. Transfers that didn't finish are
remembered and **re-offered in the queue on the next launch** — click their
retry button (once reconnected) to run them again. While transfers are active
the count shows on the Dock icon (macOS) / in the window title (Ubuntu).

Transfers run in the background on a dedicated connection — you can keep
browsing while files copy. Watch progress in the **transfer queue** panel at the
bottom; each row has its own **×** cancel button.

Uploads are **atomic** by default — each file lands under a temporary name and
is renamed into place on success, so an interrupted transfer never leaves a
truncated file at the real name (toggle in Preferences). When the whole queue
finishes while the app is in the background, you get a desktop alert, and the
transfer window shows aggregate progress across all active transfers.

### Compare directories

**Mark → Select Files That Differ** (⇧⌘C on macOS) compares the focused pane
against the other and selects every entry that is missing on the other side or
differs in size/kind — press **F5** to transfer exactly those.

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
| `⌘/Ctrl C` · `⌘/Ctrl V` | Copy in one pane, paste in the other to queue a transfer |
| `⌘/Ctrl L` | Focus the path bar of the active pane |
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

### Keep remote directory up to date

Turn on **Commands → Keep Remote Directory Up To Date** (macOS) or the
sync-toggle button in the toolbar (Ubuntu) to pin the current local/remote
directory pair. From then on, any change under the local folder is pushed to
the remote automatically (debounced; honors exclude masks, and deletes
extraneous remote files when Mirror mode is on). Toggle it off to stop.

### Finding files

Click the 🔍 (search) button to search the current remote directory recursively
by name mask (e.g. `*.log`).  
Results appear in a list; double-click any hit to navigate to its directory.

### Remote editing

Right-click a remote file → **Edit**.  
The file downloads to a temp location and opens in your default editor.  
Every time you save, it uploads automatically.

### Custom commands

Save reusable remote command templates and run them on the current selection
(SFTP). `{}` in a template expands to the shell-quoted paths of the selected
files — e.g. `tar -czf backup.tgz {}` or `md5sum {}`. Manage them from the
**Commands → Custom Commands** menu (macOS) or the Login dialog's **Tools →
Custom commands…** (Ubuntu); output appears in the command-result window.

### Open in terminal / Copy URL

- **Open terminal** (🖥 button) — opens an SSH session to the current remote
  host in your system terminal (SFTP only).
- **Copy URL** — copies an `sftp://user@host/path` URL for the selected item to
  the clipboard.

---

## Build & run

### Prerequisites

- [Rust](https://rustup.rs) and Node 18+.
- **macOS:** `brew install libssh2 openssl@3 pkg-config`
- **Linux:** `libwebkit2gtk-4.1-dev libssh2-1-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev build-essential pkg-config`
- **Windows:** the MSVC toolchain + NASM (for libssh2's vendored OpenSSL).

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
scp-cli --agent ls sftp://user@host/    # ssh-agent auth
scp-cli --key ~/.ssh/id_ed25519 ls sftp://user@host/  [passphrase]
```

### Desktop app (Tauri)
```sh
cd ui-tauri
npm install
npm run tauri dev      # dev build with hot-reload
npm run tauri build    # release installers under ../target/release/bundle/
```
The Svelte 5 frontend uses `withGlobalTauri` (no `@tauri-apps/api` npm dep); the
Tauri backend (`ui-tauri/src-tauri`) links `scp-core` directly as an rlib.

## Packaging

`cargo tauri build` (or `npm run tauri build`) produces the platform installers
under `target/release/bundle/` — `.dmg`/`.app` (macOS), `.deb`/`.AppImage`
(Linux), `.msi`/`.exe` (Windows). The release workflow
(`.github/workflows/release.yml`) builds and attaches them to a GitHub Release
on a version tag (`git tag v1.2.3 && git push --tags`).

## Integration tests

`./scripts/integration-test.sh` spins up SFTP, FTP, and MinIO servers in
Docker and drives `scp-cli` through the full operation matrix (transfers,
recursive trees, sync + dry-run plan, find, rename, chmod, deletes) with
round-trip diffs. CI (.github/workflows/ci.yml) runs the same suite on every
push, alongside the unit tests and the Tauri app build on macOS / Linux /
Windows.

## Architecture

All protocol, transfer, and sync logic lives once in `scp-core` with no UI
dependency, consumed by the Tauri backend and the CLI as a plain Rust rlib. The
UI is a single Svelte codebase rendered in each platform's native webview, so
one frontend serves macOS, Windows, and Linux. The original native front-ends
(SwiftUI / GTK4, linked via a C FFI) are preserved on the
`archive/native-apps` branch.
