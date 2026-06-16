//! GTK4 native front-end for Ubuntu/Linux.
//!
//! Feature parity with the macOS app: dual-pane browsing, SFTP password /
//! key-file / ssh-agent auth, host-key trust prompt, transfer queue with
//! per-row cancel, folder transfers, one-way sync, file management context
//! menus, remote edit-in-editor, and saved sites with Secret Service
//! passwords.
//!
//! Build on Linux (or against Homebrew gtk4 on macOS for compile-checking):
//!   sudo apt install libgtk-4-dev build-essential
//!   cargo run -p scp-ubuntu

mod pool;
mod prefs;
mod secrets;
mod sites;
mod worker;
mod workspace;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::SystemTime;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ColumnView, ColumnViewColumn,
    DragSource, DropDown, DropTarget, Entry as GtkEntry, Label, ListBox, ListItem, Orientation,
    PasswordEntry, Popover, ProgressBar, ScrolledWindow, SelectionMode, SignalListItemFactory,
    MultiSelection,
};

use scp_core::types::{Auth, Credentials, Entry, HostKeyPolicy, Protocol};
use sites::{Site, SitesStore};
use worker::{Cmd, Event, PauseFlag};

const APP_ID: &str = "net.manto.ScpCommander";

const PROTO_LABELS: [&str; 4] = ["SFTP", "FTP", "FTPS", "S3"];
const AUTH_LABELS: [&str; 3] = ["Password", "Key file", "SSH agent"];

fn proto_from_index(i: u32) -> Protocol {
    match i {
        1 => Protocol::Ftp,
        2 => Protocol::Ftps,
        3 => Protocol::S3,
        _ => Protocol::Sftp,
    }
}

fn main() -> glib::ExitCode {
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_activate(|app| build_ui(app, None));
    // `scp-ubuntu sftp://user@host:port/path` (or a registered URL handler)
    // prefills the Login dialog from the first URI.
    app.connect_open(|app, files, _| {
        let uri = files.first().map(|f| f.uri().to_string());
        build_ui(app, uri.as_deref());
    });
    app.run()
}

// ---------------------------------------------------------------------------
// Shared UI state

/// One pane's list widgets plus the entries backing the visible rows.
struct Pane {
    model: gio::ListStore,
    selection: MultiSelection,
    entries: Rc<RefCell<Vec<Entry>>>,
    path_entry: GtkEntry,
    /// WinSCP-style status line: item count, or size of the selection.
    info_label: Label,
}

struct TransferRow {
    container: gtk::Frame,
    bar: ProgressBar,
    /// WinSCP-style "17% Uploading" headline.
    title: Label,
    /// "File: <current file>" line (multi-file ops update it per file).
    file_label: Label,
    /// "Time left … · Time elapsed … · Speed …" line.
    detail: Label,
    cancel_btn: Button,
    pause_btn: Button,
    retry_btn: Button,
    /// Re-runs the same transfer; set by the dispatch sites after sending.
    retry: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    cancel: Arc<AtomicBool>,
    pause: Arc<PauseFlag>,
    finished: bool,
    /// True only on a successful finish — used to persist/re-offer the rest.
    succeeded: bool,
    files_done: u32,
    download: bool,
    /// Descriptor for queue persistence: display name (trailing "/" = folder)
    /// and the source/target paths exactly as shown.
    name: String,
    source: String,
    target: String,
    started: std::time::Instant,
    last_done: u64,
    last_at: Option<std::time::Instant>,
    speed: f64,
}

/// A reusable remote command template ("{}" expands to selected file paths).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CustomCommand {
    name: String,
    template: String,
}

/// One unfinished transfer persisted across launches (re-offered as retryable).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PendingTransfer {
    download: bool,
    is_folder: bool,
    name: String,
    remote: String,
    local: String,
}

struct EditWatch {
    remote: String,
    local: PathBuf,
    last_mtime: SystemTime,
    /// Command channel of the session that opened the file.
    cmd: mpsc::Sender<Cmd>,
}

/// Source side of an F6 move, deleted after the transfer completes.
enum MoveSource {
    Local { path: PathBuf, is_dir: bool },
    Remote { path: String, is_dir: bool },
}

/// One server session, WinSCP-tab-style: its own worker thread, connection,
/// and cached remote listing. The remote pane shows the active session.
struct Session {
    /// Browse connection: listings and file management.
    cmd: mpsc::Sender<Cmd>,
    /// Pool of N transfer connections so transfers run in parallel without
    /// blocking browsing (WinSCP's background-transfer model).
    xfer_pool: pool::TransferPool,
    creds: RefCell<Option<Credentials>>,
    remote_path: RefCell<String>,
    connected: Cell<bool>,
    cache: RefCell<Vec<Entry>>,
    title: RefCell<String>,
    /// Initial directory at connect time — target of the Home button.
    home_path: RefCell<String>,
}

/// Late-bound hook the row factories use to open the context menu.
type MenuHook = Rc<RefCell<Option<Box<dyn Fn(u32, f64, f64)>>>>;

struct App {
    window: ApplicationWindow,
    /// WinSCP-style Login dialog (modal; hidden on successful connect).
    login_window: gtk::Window,
    // Session tabs
    sessions: RefCell<Vec<Rc<Session>>>,
    active_tab: Cell<usize>,
    tabs_box: GtkBox,
    local: Pane,
    remote: Pane,
    local_path: RefCell<PathBuf>,
    status: Label,
    // Transfers window
    transfers_window: gtk::Window,
    transfers_box: GtkBox,
    transfers_panel: GtkBox,
    transfer_rows: RefCell<HashMap<u64, TransferRow>>,
    next_id: RefCell<u64>,
    // Connection form
    proto_dd: DropDown,
    auth_dd: DropDown,
    host_entry: GtkEntry,
    port_entry: GtkEntry,
    user_entry: GtkEntry,
    pass_entry: PasswordEntry,
    remember_pw_check: gtk::CheckButton,
    key_entry: GtkEntry,
    bucket_entry: GtkEntry,
    region_entry: GtkEntry,
    // Host key trust prompt
    hostkey_bar: GtkBox,
    hostkey_label: Label,
    pending_connect: RefCell<Option<(Credentials, String)>>,
    pending_fingerprint: RefCell<Option<String>>,
    // Commander state
    exclude_entry: GtkEntry,
    show_hidden: Cell<bool>,
    mirror_sync: Cell<bool>,
    /// Synchronized browsing: mirror folder enter/leave onto the other pane.
    sync_browse: Cell<bool>,
    focused_local: Cell<bool>,
    // Type-ahead: letters typed within 1s jump to the first matching row.
    type_buf: RefCell<String>,
    type_at: Cell<Option<std::time::Instant>>,
    // Navigation history per pane: (back stack, forward stack).
    local_hist: RefCell<(Vec<PathBuf>, Vec<PathBuf>)>,
    remote_hist: RefCell<(Vec<String>, Vec<String>)>,
    /// Set while a remote Back/Forward listing is in flight so the resulting
    /// Listed event isn't re-recorded as a new navigation.
    remote_hist_suppress: Cell<bool>,
    /// Transfer id -> source to delete once the move-transfer succeeds.
    pending_move: RefCell<HashMap<u64, MoveSource>>,
    // Context menus: the clicked/selected Entry is captured at popup time so
    // an async listing refresh can't retarget the action at a different row.
    local_menu_target: RefCell<Option<Entry>>,
    remote_menu_target: RefCell<Option<Entry>>,
    sites_menu_index: Cell<usize>,
    // Edit-in-editor
    edit_pending: RefCell<HashMap<u64, (String, PathBuf)>>,
    edits: RefCell<Vec<EditWatch>>,
    /// View (F3): transfer id -> (file name, temp path) awaiting the viewer.
    view_pending: RefCell<HashMap<u64, (String, PathBuf)>>,
    // Sites
    sites: RefCell<SitesStore>,
    sites_list: ListBox,
    /// Session log: timestamped copy of every status line (ring buffer).
    log_buf: RefCell<Vec<String>>,
    /// Set once the user confirms "Quit Anyway" so the close handler stops
    /// re-prompting and lets the window actually close.
    quit_confirmed: Cell<bool>,
    /// Watches the current local directory so externally-changed files refresh
    /// automatically; re-armed by `load_local` for the displayed directory.
    local_monitor: RefCell<Option<gio::FileMonitor>>,
    local_reload_pending: Cell<bool>,
    /// "Keep up to date": pinned (local, remote) pair + its own watcher, pushing
    /// local changes to the remote automatically.
    keep_pair: RefCell<Option<(PathBuf, String)>>,
    keep_monitor: RefCell<Option<gio::FileMonitor>>,
    keep_pending: Cell<bool>,
    /// Weak self, set once after construction, so `&self` methods can hand an
    /// owned handle to async callbacks (the local monitor).
    me: RefCell<Weak<App>>,
}

impl App {
    fn set_status(&self, text: &str) {
        self.status.set_text(text);
        let stamp = glib::DateTime::now_local()
            .ok()
            .and_then(|dt| dt.format("%H:%M:%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let mut buf = self.log_buf.borrow_mut();
        buf.push(format!("{stamp}  {text}"));
        let extra = buf.len().saturating_sub(500);
        if extra > 0 {
            buf.drain(..extra);
        }
    }

    /// The active tab's session.
    fn session(&self) -> Rc<Session> {
        self.sessions.borrow()[self.active_tab.get()].clone()
    }

    // -- Tabs ----------------------------------------------------------------

    fn select_tab(self: &Rc<Self>, index: usize) {
        if index >= self.sessions.borrow().len() {
            return;
        }
        self.active_tab.set(index);
        let session = self.session();
        let path = session.remote_path.borrow().clone();
        let cache = session.cache.borrow().clone();
        self.remote.show(&cache, &path, self.show_hidden.get());
        self.update_transfer_title();
        self.refresh_tabs();
    }

    fn new_tab(self: &Rc<Self>) {
        let session = create_session(self);
        self.sessions.borrow_mut().push(session);
        let last = self.sessions.borrow().len() - 1;
        self.select_tab(last);
        self.login_window.present();
    }

    fn close_tab(self: &Rc<Self>, index: usize) {
        {
            let mut sessions = self.sessions.borrow_mut();
            if index >= sessions.len() {
                return;
            }
            // Dropping the sender ends the worker thread, which disconnects.
            sessions.remove(index);
            if sessions.is_empty() {
                drop(sessions);
                let fresh = create_session(self);
                self.sessions.borrow_mut().push(fresh);
            }
        }
        let count = self.sessions.borrow().len();
        self.select_tab(self.active_tab.get().min(count - 1));
    }

    /// Rebuild the WinSCP-style tab strip.
    fn refresh_tabs(self: &Rc<Self>) {
        while let Some(child) = self.tabs_box.first_child() {
            self.tabs_box.remove(&child);
        }
        let count = self.sessions.borrow().len();
        for index in 0..count {
            let title = self.sessions.borrow()[index].title.borrow().clone();
            let tab = GtkBox::builder()
                .orientation(Orientation::Horizontal)
                .spacing(0)
                .build();
            let label_btn = Button::with_label(&title);
            label_btn.add_css_class("flat");
            if index == self.active_tab.get() {
                label_btn.add_css_class("suggested-action");
            }
            label_btn.connect_clicked(glib::clone!(
                #[strong(rename_to = state)] self,
                move |_| state.select_tab(index)
            ));
            let close = Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.set_tooltip_text(Some("Close tab"));
            close.connect_clicked(glib::clone!(
                #[strong(rename_to = state)] self,
                move |_| state.close_tab(index)
            ));
            tab.append(&label_btn);
            tab.append(&close);
            self.tabs_box.append(&tab);
        }
        let plus = Button::from_icon_name("list-add-symbolic");
        plus.add_css_class("flat");
        plus.set_tooltip_text(Some("New tab"));
        plus.connect_clicked(glib::clone!(
            #[strong(rename_to = state)] self,
            move |_| state.new_tab()
        ));
        self.tabs_box.append(&plus);
    }

    fn selected_auth(&self) -> u32 {
        if proto_from_index(self.proto_dd.selected()) == Protocol::Sftp {
            self.auth_dd.selected()
        } else {
            0
        }
    }

    // -- Local pane ---------------------------------------------------------

    fn load_local(&self) {
        let path = self.local_path.borrow().clone();
        let mut entries: Vec<Entry> = std::fs::read_dir(&path)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| {
                        let meta = e.metadata().ok();
                        let mtime = meta
                            .as_ref()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64);
                        Entry {
                            name: e.file_name().to_string_lossy().into_owned(),
                            is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                            mtime,
                            perms: None,
                            is_symlink: e.path().symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false),
                            uid: None,
                            gid: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        worker::sort_entries(&mut entries);
        self.local.show(&entries, &path.to_string_lossy(), self.show_hidden.get());
        self.arm_local_monitor(&path);
    }

    /// (Re)watch `path` so files created/removed by other apps refresh the
    /// local pane automatically. Bursts are debounced into one reload.
    fn arm_local_monitor(&self, path: &Path) {
        let file = gio::File::for_path(path);
        let monitor = match file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(m) => m,
            Err(_) => {
                *self.local_monitor.borrow_mut() = None;
                return;
            }
        };
        let me = self.me.borrow().clone();
        monitor.connect_changed(move |_, _, _, _| {
            let Some(state) = me.upgrade() else { return };
            if state.local_reload_pending.replace(true) {
                return; // a reload is already scheduled
            }
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(300),
                glib::clone!(
                    #[strong] state,
                    move || {
                        state.local_reload_pending.set(false);
                        state.load_local();
                    }
                ),
            );
        });
        *self.local_monitor.borrow_mut() = Some(monitor);
    }

    /// Toggle continuous local→remote sync of the current directory pair.
    fn toggle_keep_up_to_date(self: &Rc<Self>) {
        if self.keep_pair.borrow().is_some() {
            *self.keep_monitor.borrow_mut() = None;
            *self.keep_pair.borrow_mut() = None;
            self.set_status("Stopped keeping directory up to date");
            return;
        }
        if !self.session().connected.get() {
            self.set_status("Connect first to keep a directory up to date");
            return;
        }
        let local = self.local_path.borrow().clone();
        let remote = self.session().remote_path.borrow().clone();
        *self.keep_pair.borrow_mut() = Some((local.clone(), remote.clone()));

        // Watch the pinned local dir; push on change (debounced).
        let file = gio::File::for_path(&local);
        if let Ok(monitor) =
            file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            let me = self.me.borrow().clone();
            monitor.connect_changed(move |_, _, _, _| {
                let Some(state) = me.upgrade() else { return };
                if state.keep_pending.replace(true) {
                    return;
                }
                glib::timeout_add_local_once(
                    std::time::Duration::from_millis(1000),
                    glib::clone!(#[strong] state, move || {
                        state.keep_pending.set(false);
                        state.run_keep_sync();
                    }),
                );
            });
            *self.keep_monitor.borrow_mut() = Some(monitor);
        }
        self.set_status(&format!("Keeping {remote} up to date from {}", local.display()));
        self.run_keep_sync(); // initial push
    }

    /// Send a silent local→remote sync for the pinned pair to the worker.
    fn run_keep_sync(&self) {
        let Some((local, remote)) = self.keep_pair.borrow().clone() else { return };
        if !self.session().connected.get() {
            return;
        }
        let _ = self.session().cmd.send(Cmd::KeepSync {
            local,
            remote,
            excludes: self.exclude_masks(),
            mirror: self.mirror_sync.get(),
        });
    }

    fn open_local(self: &Rc<Self>, index: u32) {
        if index == 0 { self.local_up(); return; }
        let Some(entry) = self.local.entry_at(index) else { return };
        if entry.is_dir {
            self.record_local_history();
            self.local_path.borrow_mut().push(&entry.name);
            self.load_local();
            // Synchronized browsing: follow into the same-named remote folder.
            if self.sync_browse.get() && self.session().connected.get()
                && self.session().cache.borrow().iter().any(|e| e.is_dir && e.name == entry.name)
            {
                let path = join_posix(&self.session().remote_path.borrow(), &entry.name);
                let _ = self.session().cmd.send(Cmd::List { path });
            }
        } else {
            self.upload(&entry);
        }
    }

    fn local_up(&self) {
        self.record_local_history();
        self.local_path.borrow_mut().pop();
        self.load_local();
        // Mirror "up" on the remote pane (inline, so it can't loop back here).
        if self.sync_browse.get() && self.session().connected.get() {
            let parent = parent_posix(&self.session().remote_path.borrow());
            let _ = self.session().cmd.send(Cmd::List { path: parent });
        }
    }

    // -- Navigation history (back / forward / home) ---------------------------

    fn record_local_history(&self) {
        let cur = self.local_path.borrow().clone();
        let mut hist = self.local_hist.borrow_mut();
        hist.0.push(cur);
        hist.1.clear();
    }

    fn go_back_local(&self) {
        let Some(prev) = self.local_hist.borrow_mut().0.pop() else { return };
        self.local_hist.borrow_mut().1.push(self.local_path.borrow().clone());
        *self.local_path.borrow_mut() = prev;
        self.load_local();
    }

    fn go_forward_local(&self) {
        let Some(next) = self.local_hist.borrow_mut().1.pop() else { return };
        self.local_hist.borrow_mut().0.push(self.local_path.borrow().clone());
        *self.local_path.borrow_mut() = next;
        self.load_local();
    }

    fn go_home_local(&self) {
        let home = glib::home_dir();
        if *self.local_path.borrow() != home {
            self.record_local_history();
            *self.local_path.borrow_mut() = home;
        }
        self.load_local();
    }

    fn go_back_remote(&self) {
        if !self.session().connected.get() { return; }
        let Some(prev) = self.remote_hist.borrow_mut().0.pop() else { return };
        self.remote_hist.borrow_mut().1.push(self.session().remote_path.borrow().clone());
        self.remote_hist_suppress.set(true);
        let _ = self.session().cmd.send(Cmd::List { path: prev });
    }

    fn go_forward_remote(&self) {
        if !self.session().connected.get() { return; }
        let Some(next) = self.remote_hist.borrow_mut().1.pop() else { return };
        self.remote_hist.borrow_mut().0.push(self.session().remote_path.borrow().clone());
        self.remote_hist_suppress.set(true);
        let _ = self.session().cmd.send(Cmd::List { path: next });
    }

    fn go_home_remote(&self) {
        if !self.session().connected.get() { return; }
        let home = self.session().home_path.borrow().clone();
        let _ = self.session().cmd.send(Cmd::List { path: home });
    }

    // -- Remote pane --------------------------------------------------------

    fn connect_clicked(&self) {
        let protocol = proto_from_index(self.proto_dd.selected());
        let host = self.host_entry.text().to_string();
        let bucket = self.bucket_entry.text().to_string();
        if protocol == Protocol::S3 {
            if bucket.is_empty() {
                self.set_status("S3 needs a bucket name");
                return;
            }
        } else if host.is_empty() {
            self.set_status("Enter a host first");
            return;
        }
        let port = self
            .port_entry
            .text()
            .parse::<u16>()
            .unwrap_or_else(|_| Credentials::default_port(protocol));

        let password = self.pass_entry.text().to_string();
        let auth = if protocol == Protocol::Sftp {
            match self.auth_dd.selected() {
                1 => {
                    let key = self.key_entry.text().to_string();
                    if key.is_empty() {
                        self.set_status("Choose a private key file first");
                        return;
                    }
                    Auth::KeyFile {
                        path: key,
                        passphrase: (!password.is_empty()).then_some(password),
                    }
                }
                2 => Auth::Agent,
                _ => Auth::Password(password),
            }
        } else {
            Auth::Password(password)
        };

        let mut creds = Credentials::basic(
            protocol,
            host,
            port,
            self.user_entry.text().to_string(),
            auth,
        );
        if protocol == Protocol::S3 {
            creds.bucket = Some(bucket);
            let region = self.region_entry.text().to_string();
            creds.region = (!region.is_empty()).then_some(region);
        }
        self.start_connect(creds);
    }

    fn start_connect(&self, creds: Credentials) {
        self.hostkey_bar.set_visible(false);
        let session = self.session();
        // Reusing a tab to reach a DIFFERENT server: the old server's cwd
        // almost certainly doesn't exist there - start at "/" instead.
        let target = Self::session_label(&creds);
        let path = if session.connected.get() && *session.title.borrow() != target {
            "/".to_string()
        } else {
            session.remote_path.borrow().clone()
        };
        *session.creds.borrow_mut() = Some(creds.clone());
        *self.pending_connect.borrow_mut() = Some((creds.clone(), path.clone()));
        self.set_status("Connecting…");
        let _ = session.cmd.send(Cmd::Connect { creds, path, silent: false });
    }

    /// Reconnect the given session using its stored credentials (called by the
    /// reconnect dialog countdown or button).
    fn do_connect(&self, session: Rc<Session>) {
        if let Some(creds) = session.creds.borrow().clone() {
            let path = session.remote_path.borrow().clone();
            session.connected.set(false);
            self.set_status("Reconnecting…");
            let _ = session.cmd.send(Cmd::Connect { creds, path, silent: false });
        }
    }

    /// "user@host" (or bucket) label for a set of credentials.
    fn session_label(creds: &Credentials) -> String {
        let target = if creds.host.is_empty() {
            creds.bucket.clone().unwrap_or_default()
        } else {
            creds.host.clone()
        };
        if creds.username.is_empty() {
            target
        } else {
            format!("{}@{}", creds.username, target)
        }
    }

    /// "Trust & Connect" on the host key bar: retry pinned to the approved key.
    fn trust_host_key(&self) {
        let Some(fingerprint) = self.pending_fingerprint.borrow_mut().take() else { return };
        let Some((mut creds, _)) = self.pending_connect.borrow_mut().take() else { return };
        creds.host_key = HostKeyPolicy::AcceptFingerprint(fingerprint);
        self.start_connect(creds);
    }

    fn open_remote(self: &Rc<Self>, index: u32) {
        if index == 0 { self.remote_up(); return; }
        let Some(entry) = self.remote.entry_at(index) else { return };
        if entry.is_dir {
            let path = join_posix(&self.session().remote_path.borrow(), &entry.name);
            let _ = self.session().cmd.send(Cmd::List { path });
            // Synchronized browsing: follow into the same-named local folder.
            if self.sync_browse.get() {
                let child = self.local_path.borrow().join(&entry.name);
                if child.is_dir() {
                    self.record_local_history();
                    *self.local_path.borrow_mut() = child;
                    self.load_local();
                }
            }
        } else {
            self.download(&entry);
        }
    }

    fn remote_up(&self) {
        if !self.session().connected.get() {
            return;
        }
        let parent = parent_posix(&self.session().remote_path.borrow());
        let _ = self.session().cmd.send(Cmd::List { path: parent });
        // Mirror "up" on the local pane (inline, so it can't loop back here).
        if self.sync_browse.get() {
            self.record_local_history();
            self.local_path.borrow_mut().pop();
            self.load_local();
        }
    }

    fn refresh_remote(&self) {
        if self.session().connected.get() {
            let path = self.session().remote_path.borrow().clone();
            let _ = self.session().cmd.send(Cmd::List { path });
        }
    }

    // -- Transfers ----------------------------------------------------------

    fn download(self: &Rc<Self>, entry: &Entry) -> Option<u64> {
        self.download_with(entry, 0)
    }

    /// Download with an explicit folder overwrite policy (0/1/2). Files ignore
    /// it (the caller already decided); folders pass it to the recursive copy.
    fn download_with(self: &Rc<Self>, entry: &Entry, overwrite: i32) -> Option<u64> {
        let id = self.download_inner(entry, overwrite);
        if let Some(id) = id {
            let st = self.clone();
            let e = entry.clone();
            self.set_retry(id, Rc::new(move || { st.download_with(&e, overwrite); }));
        }
        id
    }

    fn download_inner(self: &Rc<Self>, entry: &Entry, overwrite: i32) -> Option<u64> {
        if !self.session().connected.get() {
            return None;
        }
        let remote = join_posix(&self.session().remote_path.borrow(), &entry.name);
        let local = self.local_path.borrow().join(&entry.name);
        if entry.is_dir {
            let (id, cancel, pause) = self.add_transfer(
                &format!("{}/", entry.name), true, 0, &remote, &local.display().to_string());
            let _ = self.session().xfer_pool.send(Cmd::DownloadDir {
                id,
                name: entry.name.clone(),
                remote,
                local,
                excludes: self.exclude_masks(),
                overwrite,
                cancel,
                pause,
            });
            return Some(id);
        } else {
            // Resume when a smaller partial file is already present locally.
            let resume = std::fs::metadata(&local)
                .map(|m| m.len())
                .ok()
                .filter(|len| *len > 0 && *len < entry.size)
                .unwrap_or(0);
            let (id, cancel, pause) = self.add_transfer(
                &entry.name, true, entry.size, &remote, &local.display().to_string());
            let _ = self.session().xfer_pool.send(Cmd::Download {
                id,
                name: entry.name.clone(),
                remote,
                local,
                resume,
                cancel,
                pause,
            });
            return Some(id);
        }
    }

    fn upload(self: &Rc<Self>, entry: &Entry) -> Option<u64> {
        self.upload_with(entry, 0)
    }

    /// Upload with an explicit folder overwrite policy (0/1/2). Files ignore it
    /// (the caller already decided); folders pass it to the recursive copy.
    fn upload_with(self: &Rc<Self>, entry: &Entry, overwrite: i32) -> Option<u64> {
        let id = self.upload_inner(entry, overwrite);
        if let Some(id) = id {
            let st = self.clone();
            let e = entry.clone();
            self.set_retry(id, Rc::new(move || { st.upload_with(&e, overwrite); }));
        }
        id
    }

    fn upload_inner(self: &Rc<Self>, entry: &Entry, overwrite: i32) -> Option<u64> {
        if !self.session().connected.get() {
            self.set_status("Connect first to upload");
            return None;
        }
        let local = self.local_path.borrow().join(&entry.name);
        let remote = join_posix(&self.session().remote_path.borrow(), &entry.name);
        if entry.is_dir {
            let (id, cancel, pause) = self.add_transfer(
                &format!("{}/", entry.name), false, 0, &local.display().to_string(), &remote);
            let _ = self.session().xfer_pool.send(Cmd::UploadDir {
                id,
                name: entry.name.clone(),
                local,
                remote,
                excludes: self.exclude_masks(),
                overwrite,
                cancel,
                pause,
            });
            return Some(id);
        } else {
            // Resume when the remote file is a smaller partial of this one.
            let resume = self
                .session()
                .cache
                .borrow()
                .iter()
                .any(|r| !r.is_dir && r.name == entry.name && r.size > 0 && r.size < entry.size);
            let (id, cancel, pause) = self.add_transfer(
                &entry.name, false, entry.size, &local.display().to_string(), &remote);
            let _ = self.session().xfer_pool.send(Cmd::Upload {
                id,
                name: entry.name.clone(),
                local,
                remote,
                resume,
                cancel,
                pause,
            });
            return Some(id);
        }
    }

    /// Sync is preview-first, WinSCP-style: compute the plan, show the
    /// checklist, copy only what the user approves.
    fn sync(self: &Rc<Self>, download: bool) {
        if !self.session().connected.get() {
            self.set_status("Connect first to sync");
            return;
        }
        let local = self.local_path.borrow().clone();
        let remote = self.session().remote_path.borrow().clone();
        self.set_status("Computing sync preview…");
        let _ = self.session().cmd.send(Cmd::SyncPlan {
            download,
            local,
            remote,
            excludes: self.exclude_masks(),
            delete_extraneous: self.mirror_sync.get(),
        });
    }

    /// Execute approved sync items (rel paths) after the preview.
    fn run_sync_items(
        self: &Rc<Self>,
        download: bool,
        local_root: &std::path::Path,
        remote_root: &str,
        dirs: Vec<String>,
        items: Vec<(String, u64)>,
        deletes: Vec<String>,
    ) {
        for d in &dirs {
            if download {
                let _ = std::fs::create_dir_all(local_root.join(d));
            } else {
                let _ = self
                    .session()
                    .cmd
                    .send(Cmd::Mkdir { path: join_posix(remote_root, d) });
            }
        }
        for (rel, size) in items {
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            let local = local_root.join(&rel);
            let remote = join_posix(remote_root, &rel);
            if download {
                if let Some(parent) = local.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let (id, cancel, pause) = self.add_transfer(
                    &name, true, size, &remote, &local.display().to_string());
                let _ = self.session().xfer_pool.send(Cmd::Download {
                    id,
                    name,
                    remote,
                    local,
                    resume: 0,
                    cancel,
                    pause,
                });
            } else {
                let (id, cancel, pause) = self.add_transfer(
                    &name, false, size, &local.display().to_string(), &remote);
                let _ = self.session().xfer_pool.send(Cmd::Upload {
                    id,
                    name,
                    local,
                    remote,
                    resume: false,
                    cancel,
                    pause,
                });
            }
        }
        // Mirror-mode: delete destination items with no source counterpart.
        for rel in &deletes {
            if download {
                let _ = std::fs::remove_file(local_root.join(rel))
                    .or_else(|_| std::fs::remove_dir_all(local_root.join(rel)));
            } else {
                let path = join_posix(remote_root, rel);
                let _ = self.session().cmd.send(Cmd::Delete { path, is_dir: false });
            }
        }
        if !deletes.is_empty() {
            self.set_status(&format!("Sync: deleting {} extraneous item(s)", deletes.len()));
        }
    }

    #[allow(dead_code)]
    fn sync_immediate(self: &Rc<Self>, download: bool) {
        let local = self.local_path.borrow().clone();
        let remote = self.session().remote_path.borrow().clone();
        let title = format!("Sync {} {}", if download { "⬇" } else { "⬆" }, remote);
        let local_str = local.display().to_string();
        let (id, cancel, pause) = if download {
            self.add_transfer(&title, true, 0, &remote, &local_str)
        } else {
            self.add_transfer(&title, false, 0, &local_str, &remote)
        };
        let _ = self.session().xfer_pool.send(Cmd::Sync {
            id,
            download,
            local,
            remote,
            excludes: self.exclude_masks(),
            cancel,
            pause,
        });
    }

    /// Add a WinSCP-style progress card to the transfer window:
    /// "17% Uploading" headline, File:/Target: lines, bar, time/speed detail.
    fn add_transfer(
        &self, name: &str, download: bool, total: u64, source: &str, target: &str,
    ) -> (u64, Arc<AtomicBool>, Arc<PauseFlag>) {
        let id = {
            let mut next = self.next_id.borrow_mut();
            *next += 1;
            *next
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let pause = PauseFlag::new();
        let _ = name;

        let title = Label::builder()
            .label(if download { "Downloading" } else { "Uploading" })
            .xalign(0.0)
            .hexpand(true)
            .build();
        title.add_css_class("heading");

        // Pause / resume toggle
        let pause_icon = gtk::Image::from_icon_name("media-playback-pause-symbolic");
        let pause_btn = Button::new();
        pause_btn.set_child(Some(&pause_icon));
        pause_btn.add_css_class("flat");
        pause_btn.set_tooltip_text(Some("Pause"));
        pause_btn.connect_clicked(glib::clone!(
            #[strong] pause_icon,
            #[strong] pause,
            move |btn| {
                if pause.is_paused() {
                    pause.resume();
                    pause_icon.set_icon_name(Some("media-playback-pause-symbolic"));
                    btn.set_tooltip_text(Some("Pause"));
                } else {
                    pause.pause();
                    pause_icon.set_icon_name(Some("media-playback-start-symbolic"));
                    btn.set_tooltip_text(Some("Resume"));
                }
            }
        ));

        // Cancel — also resumes pause so worker can observe the cancel flag
        let cancel_btn = Button::from_icon_name("process-stop-symbolic");
        cancel_btn.add_css_class("flat");
        cancel_btn.set_tooltip_text(Some("Cancel"));
        cancel_btn.connect_clicked(glib::clone!(
            #[strong] cancel,
            #[strong] pause,
            move |_| {
                pause.resume();
                cancel.store(true, Ordering::Relaxed);
            }
        ));

        // Retry — hidden until the transfer fails/cancels with a retry hook.
        let retry: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let retry_btn = Button::from_icon_name("view-refresh-symbolic");
        retry_btn.add_css_class("flat");
        retry_btn.set_tooltip_text(Some("Retry this transfer"));
        retry_btn.set_visible(false);
        retry_btn.connect_clicked(glib::clone!(
            #[strong] retry,
            move |btn| {
                btn.set_visible(false);
                let f = retry.borrow().clone();
                if let Some(f) = f { f(); }
            }
        ));

        let header = GtkBox::builder().orientation(Orientation::Horizontal).spacing(4).build();
        header.append(&title);
        header.append(&retry_btn);
        header.append(&pause_btn);
        header.append(&cancel_btn);

        let path_line = |prefix: &str, text: &str| {
            let l = Label::builder()
                .label(format!("{prefix}{text}"))
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Start)
                .build();
            l.add_css_class("caption");
            l.add_css_class("dim-label");
            l
        };
        let file_label = path_line("File:  ", source);
        let target_label = path_line("Target:  ", target);

        let bar = ProgressBar::builder()
            .hexpand(true)
            .show_text(true)
            .build();
        if total > 0 {
            bar.set_text(Some(&human_size(total)));
        }

        let detail = Label::builder().label("").xalign(0.0).build();
        detail.add_css_class("caption");
        detail.add_css_class("dim-label");

        let vbox = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(3)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .build();
        vbox.append(&header);
        vbox.append(&file_label);
        vbox.append(&target_label);
        vbox.append(&bar);
        vbox.append(&detail);

        let card = gtk::Frame::builder()
            .child(&vbox)
            .margin_top(3)
            .margin_bottom(3)
            .margin_start(6)
            .margin_end(6)
            .build();
        self.transfers_box.prepend(&card);
        self.transfers_window.present();

        self.transfer_rows.borrow_mut().insert(
            id,
            TransferRow {
                container: card,
                bar,
                title,
                file_label,
                detail,
                cancel_btn,
                pause_btn,
                retry_btn,
                retry,
                cancel: cancel.clone(),
                pause: pause.clone(),
                finished: false,
                succeeded: false,
                files_done: 0,
                download,
                name: name.to_string(),
                source: source.to_string(),
                target: target.to_string(),
                started: std::time::Instant::now(),
                last_done: 0,
                last_at: None,
                speed: 0.0,
            },
        );
        self.update_transfer_title();
        (id, cancel, pause)
    }

    fn clear_finished(&self) {
        let mut rows = self.transfer_rows.borrow_mut();
        rows.retain(|_, row| {
            if row.finished {
                self.transfers_box.remove(&row.container);
                false
            } else {
                true
            }
        });
        if rows.is_empty() {
            self.transfers_window.hide();
        }
    }

    fn cancel_all(&self) {
        for row in self.transfer_rows.borrow().values() {
            if !row.finished {
                row.pause.resume();
                row.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Transfers still running — used by the quit guard.
    fn active_transfers(&self) -> usize {
        self.transfer_rows.borrow().values().filter(|r| !r.finished).count()
    }

    /// The plain window title for the active session (no transfer indicator).
    fn base_title(&self) -> String {
        let s = self.session();
        if s.connected.get() {
            format!("{} — SCP Commander", s.title.borrow())
        } else {
            "SCP Commander".to_string()
        }
    }

    /// Reflect running transfers in the window title (and thus the taskbar /
    /// window list) — GTK has no portable Unity launcher badge.
    fn update_transfer_title(&self) {
        let n = self.active_transfers();
        let base = self.base_title();
        let title = if n > 0 { format!("[⇅ {n}] {base}") } else { base };
        self.window.set_title(Some(&title));
    }

    fn queue_file() -> PathBuf {
        glib::user_config_dir().join("scp-commander").join("queue.json")
    }

    /// Persist every transfer that didn't succeed (called at quit) so it can be
    /// re-offered on next launch. An empty set removes the file.
    fn save_queue(&self) {
        let pending: Vec<PendingTransfer> = self
            .transfer_rows
            .borrow()
            .values()
            .filter(|r| !r.succeeded)
            .map(|r| {
                let is_folder = r.name.ends_with('/');
                let (remote, local) = if r.download {
                    (r.source.clone(), r.target.clone())
                } else {
                    (r.target.clone(), r.source.clone())
                };
                PendingTransfer {
                    download: r.download,
                    is_folder,
                    name: r.name.trim_end_matches('/').to_string(),
                    remote,
                    local,
                }
            })
            .collect();
        let path = Self::queue_file();
        if pending.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else if let Ok(data) = serde_json::to_vec_pretty(&pending) {
            let _ = std::fs::write(&path, data);
        }
    }

    /// Re-offer last session's unfinished transfers as failed rows with retry.
    fn restore_queue(self: &Rc<Self>) {
        let path = Self::queue_file();
        let Ok(data) = std::fs::read(&path) else { return };
        let _ = std::fs::remove_file(&path); // consumed
        let Ok(pending) = serde_json::from_slice::<Vec<PendingTransfer>>(&data) else { return };
        if pending.is_empty() {
            return;
        }
        let count = pending.len();
        for p in pending {
            let display = if p.is_folder { format!("{}/", p.name) } else { p.name.clone() };
            let (source, target) = if p.download {
                (p.remote.clone(), p.local.clone())
            } else {
                (p.local.clone(), p.remote.clone())
            };
            let (id, _cancel, _pause) = self.add_transfer(&display, p.download, 0, &source, &target);
            let st = self.clone();
            self.set_retry(id, Rc::new(move || st.rerun_pending(&p)));
            self.finish_row(id, "Interrupted last session", false);
        }
        self.set_status(&format!(
            "{count} unfinished transfer(s) from last session — retry from the queue"
        ));
        self.transfers_window.present();
    }

    /// Re-run a persisted transfer by absolute paths against the active session.
    fn rerun_pending(self: &Rc<Self>, p: &PendingTransfer) {
        if !self.session().connected.get() {
            self.set_status(&format!("Connect first to retry {}", p.name));
            return;
        }
        let remote = p.remote.clone();
        let local = PathBuf::from(&p.local);
        let (source, target) = if p.download {
            (p.remote.clone(), p.local.clone())
        } else {
            (p.local.clone(), p.remote.clone())
        };
        let display = if p.is_folder { format!("{}/", p.name) } else { p.name.clone() };
        let (id, cancel, pause) = self.add_transfer(&display, p.download, 0, &source, &target);
        let st = self.clone();
        let pc = p.clone();
        self.set_retry(id, Rc::new(move || st.rerun_pending(&pc)));
        let cmd = match (p.download, p.is_folder) {
            (true, true) => Cmd::DownloadDir {
                id, name: p.name.clone(), remote, local,
                excludes: self.exclude_masks(), overwrite: 0, cancel, pause,
            },
            (true, false) => Cmd::Download {
                id, name: p.name.clone(), remote, local, resume: 0, cancel, pause,
            },
            (false, true) => Cmd::UploadDir {
                id, name: p.name.clone(), local, remote,
                excludes: self.exclude_masks(), overwrite: 0, cancel, pause,
            },
            (false, false) => Cmd::Upload {
                id, name: p.name.clone(), local, remote, resume: false, cancel, pause,
            },
        };
        let _ = self.session().xfer_pool.send(cmd);
    }

    /// Show a prompt to execute a command on the remote (SFTP only).
    fn menu_exec_command(self: &Rc<Self>) {
        let session = self.session();
        if !session.connected.get() {
            self.set_status("Connect first");
            return;
        }
        let creds = session.creds.borrow().clone();
        if creds.map(|c| c.protocol) != Some(scp_core::types::Protocol::Sftp) {
            self.set_status("Execute command is only available on SFTP sessions");
            return;
        }
        let state = self.clone();
        prompt(&self.window, "Execute remote command", "", move |cmd| {
            if cmd.is_empty() {
                return;
            }
            state.set_status(&format!("Executing: {cmd}…"));
            let _ = state.session().cmd.send(Cmd::Exec { cmd });
        });
    }

    /// Show a prompt for the duplicate (server-side copy) name.
    fn menu_copy_file(self: &Rc<Self>) {
        let Some(entry) = self.menu_entry(false) else { return };
        if entry.is_dir {
            self.set_status("Server-side copy works on files only");
            return;
        }
        let state = self.clone();
        let src = join_posix(&self.session().remote_path.borrow(), &entry.name);
        let base = self.session().remote_path.borrow().clone();
        let initial_name = entry.name.clone();
        let initial_name2 = initial_name.clone();
        prompt(&self.window, "Duplicate as…", &initial_name, move |new_name| {
            if new_name.is_empty() || new_name == initial_name2 {
                return;
            }
            let dst = join_posix(&base, &new_name);
            let _ = state.session().cmd.send(Cmd::CopyFile { src: src.clone(), dst });
        });
    }

    fn finish_row(&self, id: u64, text: &str, full: bool) {
        if let Some(row) = self.transfer_rows.borrow_mut().get_mut(&id) {
            if full {
                row.bar.set_fraction(1.0);
            }
            row.bar.set_text(Some(text));
            row.title.set_text(text);
            let el = row.started.elapsed().as_secs();
            row.detail.set_text(&format!(
                "Time elapsed {}:{:02}:{:02}", el / 3600, (el / 60) % 60, el % 60));
            row.finished = true;
            row.succeeded = full;
            row.cancel_btn.set_visible(false);
            row.pause_btn.set_visible(false);
            // Failed/cancelled transfers with a retry hook get a retry button.
            if !full && row.retry.borrow().is_some() {
                row.retry_btn.set_visible(true);
            }
            row.pause.resume(); // unblock worker if it was paused
        }
        self.update_transfer_title();
    }

    /// Attach a retry closure to a transfer row (shows the ↻ button on failure).
    fn set_retry(&self, id: u64, f: Rc<dyn Fn()>) {
        if let Some(row) = self.transfer_rows.borrow().get(&id) {
            *row.retry.borrow_mut() = Some(f);
        }
    }

    // -- Context menu actions -------------------------------------------------

    /// All selected row indices in a pane (multi-selection).
    fn selected_indices(&self, local_pane: bool) -> Vec<u32> {
        let pane = if local_pane { &self.local } else { &self.remote };
        let bitset = pane.selection.selection();
        (0..bitset.size()).map(|i| bitset.nth(i as u32)).collect()
    }

    fn selected_entries(&self, local_pane: bool) -> Vec<Entry> {
        let pane = if local_pane { &self.local } else { &self.remote };
        let entries = pane.entries.borrow();
        self.selected_indices(local_pane)
            .into_iter()
            .filter_map(|i| entries.get(i as usize).cloned())
            .filter(|e| e.name != "..")
            .collect()
    }

    /// Point the menu index at the pane's first selected row (for toolbar
    /// buttons that reuse menu actions). Returns false when nothing's selected.
    fn select_for_menu(&self, local_pane: bool) -> bool {
        let Some(entry) = self.selected_entries(local_pane).into_iter().next() else {
            return false;
        };
        if local_pane {
            *self.local_menu_target.borrow_mut() = Some(entry);
        } else {
            *self.remote_menu_target.borrow_mut() = Some(entry);
        }
        true
    }

    fn menu_entry(&self, local_pane: bool) -> Option<Entry> {
        if local_pane {
            self.local_menu_target.borrow().clone()
        } else {
            self.remote_menu_target.borrow().clone()
        }
    }

    fn menu_transfer(self: &Rc<Self>, local_pane: bool) {
        let mut targets = self.selected_entries(local_pane);
        if targets.is_empty() {
            if let Some(entry) = self.menu_entry(local_pane) {
                targets.push(entry);
            }
        }
        self.request_transfers(targets, local_pane);
    }

    fn menu_open(self: &Rc<Self>, local_pane: bool) {
        let Some(entry) = self.menu_entry(local_pane) else { return };
        if local_pane {
            if entry.is_dir {
                self.local_path.borrow_mut().push(&entry.name);
                self.load_local();
            } else {
                self.upload(&entry);
            }
        } else if entry.is_dir {
            let path = join_posix(&self.session().remote_path.borrow(), &entry.name);
            let _ = self.session().cmd.send(Cmd::List { path });
        } else {
            self.download(&entry);
        }
    }

    fn menu_rename(self: &Rc<Self>, local_pane: bool) {
        let Some(entry) = self.menu_entry(local_pane) else { return };
        let state = self.clone();
        let old_name = entry.name.clone();
        prompt(
            &self.window,
            "Rename",
            &entry.name,
            move |new_name| {
                if new_name.is_empty() || new_name == old_name {
                    return;
                }
                if local_pane {
                    let base = state.local_path.borrow().clone();
                    match std::fs::rename(base.join(&old_name), base.join(&new_name)) {
                        Ok(()) => state.load_local(),
                        Err(e) => state.set_status(&format!("Error: {e}")),
                    }
                } else {
                    let base = state.session().remote_path.borrow().clone();
                    let _ = state.session().cmd.send(Cmd::Rename {
                        from: join_posix(&base, &old_name),
                        to: join_posix(&base, &new_name),
                    });
                }
            },
        );
    }

    fn menu_delete(self: &Rc<Self>, local_pane: bool) {
        // Batch over the selection; fall back to the clicked row.
        let mut targets = self.selected_entries(local_pane);
        if targets.is_empty() {
            let Some(entry) = self.menu_entry(local_pane) else { return };
            targets.push(entry);
        }
        let message = if targets.len() == 1 {
            format!("Delete {}?", targets[0].name)
        } else {
            format!("Delete {} items?", targets.len())
        };
        let detail = if targets.iter().any(|e| e.is_dir) {
            "Folders and everything inside them will be deleted."
        } else {
            "This cannot be undone."
        };
        let state = self.clone();
        let dialog = gtk::AlertDialog::builder()
            .message(message)
            .detail(detail)
            .buttons(["Cancel", "Delete"])
            .cancel_button(0)
            .default_button(0)
            .build();
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| {
                if result != Ok(1) {
                    return;
                }
                for entry in &targets {
                    if local_pane {
                        let path = state.local_path.borrow().join(&entry.name);
                        let outcome = if entry.is_dir {
                            std::fs::remove_dir_all(&path)
                        } else {
                            std::fs::remove_file(&path)
                        };
                        if let Err(e) = outcome {
                            state.set_status(&format!("Error: {e}"));
                        }
                    } else {
                        let path =
                            join_posix(&state.session().remote_path.borrow(), &entry.name);
                        let _ = state
                            .session()
                            .cmd
                            .send(Cmd::Delete { path, is_dir: entry.is_dir });
                    }
                }
                if local_pane {
                    state.load_local();
                }
            },
        );
    }

    /// Remote only: download to a temp copy, open it, re-upload on save.
    fn menu_edit(self: &Rc<Self>) {
        let Some(entry) = self.menu_entry(false) else { return };
        if entry.is_dir {
            self.set_status("Only files can be edited");
            return;
        }
        if !self.session().connected.get() {
            return;
        }
        let remote = join_posix(&self.session().remote_path.borrow(), &entry.name);
        // Per-user runtime dir (XDG_RUNTIME_DIR is 0700) + a random leaf:
        // a fixed, sequential path under shared /tmp would let other local
        // users pre-create it and read or replace the file being edited.
        let base = glib::user_runtime_dir().join("scp-commander-edit");
        if std::fs::create_dir_all(&base).is_err() {
            self.set_status("Could not create temp directory");
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
        }
        let dir = base.join(glib::uuid_string_random().as_str());
        if std::fs::create_dir(&dir).is_err() {
            self.set_status("Could not create temp directory");
            return;
        }
        let local = dir.join(&entry.name);
        let (id, cancel, pause) = self.add_transfer(
            &entry.name, true, entry.size, &remote, &local.display().to_string());
        self.edit_pending
            .borrow_mut()
            .insert(id, (remote.clone(), local.clone()));
        let _ = self.session().xfer_pool.send(Cmd::Download {
            id,
            name: entry.name.clone(),
            remote,
            local,
            resume: 0,
            cancel,
            pause,
        });
    }

    /// View (F3): read-only text preview. Local files read directly; remote
    /// ones download to a throwaway temp copy first.
    fn menu_view(self: &Rc<Self>, local_pane: bool) {
        let Some(entry) = self.menu_entry(local_pane) else { return };
        if entry.is_dir {
            self.set_status("Only files can be viewed");
            return;
        }
        if local_pane {
            let path = self.local_path.borrow().join(&entry.name);
            viewer_dialog(&self.window, &entry.name, &read_preview(&path));
            return;
        }
        if !self.session().connected.get() {
            return;
        }
        let remote = join_posix(&self.session().remote_path.borrow(), &entry.name);
        let base = glib::user_runtime_dir().join("scp-commander-view");
        if std::fs::create_dir_all(&base).is_err() {
            self.set_status("Could not create temp directory");
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
        }
        let local = base.join(glib::uuid_string_random().as_str());
        let (id, cancel, pause) = self.add_transfer(
            &entry.name, true, entry.size, &remote, &local.display().to_string());
        self.view_pending.borrow_mut().insert(id, (entry.name.clone(), local.clone()));
        let _ = self.session().xfer_pool.send(Cmd::Download {
            id,
            name: entry.name.clone(),
            remote,
            local,
            resume: 0,
            cancel,
            pause,
        });
    }

    fn menu_properties(self: &Rc<Self>, local_pane: bool) {
        let Some(entry) = self.menu_entry(local_pane) else { return };
        // Current mode: remote from the listing, local from the filesystem.
        let current_mode = if local_pane {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(self.local_path.borrow().join(&entry.name))
                    .map(|m| m.permissions().mode() & 0o777)
                    .ok()
            }
            #[cfg(not(unix))]
            {
                None
            }
        } else {
            entry.perms.as_deref().and_then(parse_mode)
        };
        let location = if local_pane {
            self.local_path.borrow().to_string_lossy().into_owned()
        } else {
            self.session().remote_path.borrow().clone()
        };
        // S3 has no permission model; everything else can try chmod.
        let can_chmod = local_pane || proto_from_index(self.proto_dd.selected()) != Protocol::S3;

        let state = self.clone();
        let entry_for_apply = entry.clone();
        properties_dialog(
            &self.window,
            &entry,
            &location,
            current_mode,
            can_chmod,
            move |mode| {
                let entry = &entry_for_apply;
                if local_pane {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let path = state.local_path.borrow().join(&entry.name);
                        match std::fs::set_permissions(
                            &path,
                            std::fs::Permissions::from_mode(mode),
                        ) {
                            Ok(()) => {
                                state.set_status(&format!(
                                    "Permissions of {} set to {mode:o}",
                                    entry.name
                                ));
                                state.load_local();
                            }
                            Err(e) => state.set_status(&format!("Error: {e}")),
                        }
                    }
                } else {
                    let path = join_posix(&state.session().remote_path.borrow(), &entry.name);
                    let _ = state.session().cmd.send(Cmd::Chmod { path, mode });
                }
            },
        );
    }

    fn new_folder(self: &Rc<Self>, local_pane: bool) {
        let state = self.clone();
        prompt(&self.window, "New folder", "", move |name| {
            if name.is_empty() {
                return;
            }
            if local_pane {
                let path = state.local_path.borrow().join(&name);
                match std::fs::create_dir(&path) {
                    Ok(()) => state.load_local(),
                    Err(e) => state.set_status(&format!("Error: {e}")),
                }
            } else {
                let path = join_posix(&state.session().remote_path.borrow(), &name);
                let _ = state.session().cmd.send(Cmd::Mkdir { path });
            }
        });
    }

    // -- Edit watches ---------------------------------------------------------

    fn poll_edits(self: &Rc<Self>) {
        let changed: Vec<(String, PathBuf, mpsc::Sender<Cmd>)> = {
            let mut edits = self.edits.borrow_mut();
            // Prune watches whose temp file vanished or whose session's
            // worker is gone (tab closed) — otherwise they pile up forever.
            edits.retain(|w| {
                w.local.exists() && w.cmd.send(Cmd::List { path: String::new() }).is_ok()
            });
            let mut out = Vec::new();
            for watch in edits.iter_mut() {
                let Ok(meta) = std::fs::metadata(&watch.local) else { continue };
                let Ok(mtime) = meta.modified() else { continue };
                if mtime > watch.last_mtime {
                    watch.last_mtime = mtime;
                    out.push((watch.remote.clone(), watch.local.clone(), watch.cmd.clone()));
                }
            }
            out
        };
        for (remote, local, cmd) in changed {
            let name = local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
            let (id, cancel, pause) = self.add_transfer(
                &name, false, size, &local.display().to_string(), &remote);
            let _ = cmd.send(Cmd::Upload { id, name, local, remote, resume: false, cancel, pause });
        }
    }

    /// F6: move = transfer the focused pane's selection, then delete sources
    /// once their transfers succeed.
    fn move_selected(self: &Rc<Self>) {
        let local_pane = self.focused_local.get();
        for entry in self.selected_entries(local_pane) {
            let id = if local_pane {
                self.upload(&entry)
            } else {
                self.download(&entry)
            };
            let Some(id) = id else { continue };
            let source = if local_pane {
                MoveSource::Local {
                    path: self.local_path.borrow().join(&entry.name),
                    is_dir: entry.is_dir,
                }
            } else {
                MoveSource::Remote {
                    path: join_posix(&self.session().remote_path.borrow(), &entry.name),
                    is_dir: entry.is_dir,
                }
            };
            self.pending_move.borrow_mut().insert(id, source);
        }
    }

    /// Current exclusion masks from the toolbar entry.
    fn exclude_masks(&self) -> String {
        self.exclude_entry.text().to_string()
    }

    /// F5: copy the focused pane's selection to the other side.
    fn transfer_selected(self: &Rc<Self>) {
        let local_pane = self.focused_local.get();
        let entries = self.selected_entries(local_pane);
        self.request_transfers(entries, local_pane);
    }

    /// Start transfers with WinSCP-style overwrite protection: entries whose
    /// destination already holds a same-or-larger file prompt before
    /// clobbering (smaller partials auto-resume; folders merge as before).
    fn request_transfers(self: &Rc<Self>, entries: Vec<Entry>, local_pane: bool) {
        let mut ready = Vec::new();
        let mut conflicts = Vec::new();
        for e in entries {
            let conflict = if e.is_dir {
                // A folder conflicts when a folder of the same name already
                // exists at the destination (its files may collide on merge).
                if local_pane {
                    self.session()
                        .cache
                        .borrow()
                        .iter()
                        .any(|r| r.is_dir && r.name == e.name)
                } else {
                    self.local_path.borrow().join(&e.name).is_dir()
                }
            } else if local_pane {
                // Upload: destination is the remote listing.
                self.session()
                    .cache
                    .borrow()
                    .iter()
                    .any(|r| !r.is_dir && r.name == e.name && r.size >= e.size)
            } else {
                // Download: destination is the local directory.
                std::fs::metadata(self.local_path.borrow().join(&e.name))
                    .map(|m| m.len() >= e.size)
                    .unwrap_or(false)
            };
            if conflict {
                conflicts.push(e);
            } else {
                ready.push(e);
            }
        }
        for e in &ready {
            if local_pane {
                self.upload(e);
            } else {
                self.download(e);
            }
        }
        if conflicts.is_empty() {
            return;
        }
        // Destination size + mtime for a conflicting entry (for the detail
        // text and the "only newer" decision).
        let dest_info = |state: &App, e: &Entry| -> Option<(u64, Option<i64>)> {
            if local_pane {
                state
                    .session()
                    .cache
                    .borrow()
                    .iter()
                    .find(|r| !r.is_dir && r.name == e.name)
                    .map(|r| (r.size, r.mtime))
            } else {
                std::fs::metadata(state.local_path.borrow().join(&e.name))
                    .ok()
                    .map(|m| {
                        let mtime = m
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64);
                        (m.len(), mtime)
                    })
            }
        };
        let fmt_stamp = |m: Option<i64>| {
            m.and_then(|s| glib::DateTime::from_unix_local(s).ok())
                .and_then(|dt| dt.format("%d.%m.%Y %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".into())
        };
        let any_dir = conflicts.iter().any(|e| e.is_dir);
        let message = if conflicts.len() == 1 {
            format!("{} already exists at the destination.", conflicts[0].name)
        } else {
            format!("{} items already exist at the destination.", conflicts.len())
        };
        // WinSCP-style detail: source vs target size + mtime with newer hint.
        // Folders merge, so the choice applies to each file inside them.
        let detail = if any_dir {
            "Your choice applies to each file inside:\n\
             Overwrite — replace all · Only newer — replace older files · \
             Skip — keep existing, copy only new files."
                .to_string()
        } else if conflicts.len() == 1 {
            let e = &conflicts[0];
            match dest_info(self, e) {
                Some((dsize, dmtime)) => {
                    let hint = match (e.mtime, dmtime) {
                        (Some(s), Some(t)) if s > t => " — source is newer",
                        (Some(s), Some(t)) if s < t => " — source is older",
                        (Some(_), Some(_)) => " — same time",
                        _ => "",
                    };
                    format!(
                        "Source: {} · {}\nTarget: {} · {}{hint}",
                        human_size(e.size),
                        fmt_stamp(e.mtime),
                        human_size(dsize),
                        fmt_stamp(dmtime),
                    )
                }
                None => "Overwrite replaces the existing copies.".into(),
            }
        } else {
            let newer = conflicts
                .iter()
                .filter(|e| {
                    matches!(
                        (e.mtime, dest_info(self, e).and_then(|(_, m)| m)),
                        (Some(s), Some(t)) if s > t
                    )
                })
                .count();
            format!("{newer} of {} source files are newer than their targets.", conflicts.len())
        };
        let state = self.clone();
        let dialog = gtk::AlertDialog::builder()
            .message(message)
            .detail(detail)
            .buttons(["Cancel", "Skip existing", "Only newer", "Overwrite"])
            .cancel_button(0)
            .default_button(1)
            .build();
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |result| {
            // Map the button to a folder overwrite-policy code (0/1/2). Single
            // files use it directly; folders pass it to the recursive copy.
            let policy: i32 = match result {
                Ok(3) => 0, // Overwrite
                Ok(2) => 2, // Only newer
                Ok(1) => 1, // Skip existing
                _ => return, // Cancel
            };
            for e in &conflicts {
                if e.is_dir {
                    // Folders merge per-file. "Skip existing" still copies new
                    // files inside, so the whole folder is never dropped.
                    if local_pane {
                        state.upload_with(e, policy);
                    } else {
                        state.download_with(e, policy);
                    }
                    continue;
                }
                // Single file: Skip drops it; Only-newer compares mtimes.
                if policy == 1 {
                    continue;
                }
                if policy == 2 {
                    let dest_mtime = if local_pane {
                        state
                            .session()
                            .cache
                            .borrow()
                            .iter()
                            .find(|r| !r.is_dir && r.name == e.name)
                            .and_then(|r| r.mtime)
                    } else {
                        std::fs::metadata(state.local_path.borrow().join(&e.name))
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                    };
                    match (e.mtime, dest_mtime) {
                        (Some(s), Some(t)) if s > t => {}
                        _ => continue,
                    }
                }
                if local_pane {
                    state.upload(e);
                } else {
                    state.download(e);
                }
            }
        });
    }

    fn navigate_local(self: &Rc<Self>, text: &str) {
        let expanded = if let Some(rest) = text.strip_prefix("~") {
            glib::home_dir().join(rest.trim_start_matches('/'))
        } else {
            PathBuf::from(text)
        };
        if expanded.is_dir() {
            if *self.local_path.borrow() != expanded {
                self.record_local_history();
            }
            *self.local_path.borrow_mut() = expanded;
            self.load_local();
        } else {
            self.set_status(&format!("No such directory: {text}"));
            self.load_local();
        }
    }

    fn navigate_remote(self: &Rc<Self>, text: &str) {
        if self.session().connected.get() {
            let path = if text.is_empty() { "/".to_string() } else { text.to_string() };
            let _ = self.session().cmd.send(Cmd::List { path });
        }
    }

    fn set_focus(self: &Rc<Self>, local: bool) {
        self.focused_local.set(local);
    }

    /// Type-ahead: select the first row whose name starts with the typed
    /// letters; the buffer resets after a second of silence.
    fn type_ahead(&self, local: bool, c: char) {
        let now = std::time::Instant::now();
        let fresh = self
            .type_at
            .get()
            .map(|t| now.duration_since(t).as_millis() >= 1000)
            .unwrap_or(true);
        self.type_at.set(Some(now));
        let mut buf = self.type_buf.borrow_mut();
        if fresh {
            buf.clear();
        }
        buf.extend(c.to_lowercase());
        let pane = if local { &self.local } else { &self.remote };
        let entries = pane.entries.borrow();
        if let Some(i) = entries
            .iter()
            .position(|e| e.name != ".." && e.name.to_lowercase().starts_with(buf.as_str()))
        {
            pane.selection.select_item(i as u32, true);
        }
    }

    // -- Mark menu: selection commands on the focused pane --------------------

    fn focused_pane(&self) -> &Pane {
        if self.focused_local.get() { &self.local } else { &self.remote }
    }

    fn mark_select_all(&self) {
        let pane = self.focused_pane();
        pane.selection.select_all();
        pane.selection.unselect_item(0); // skip the ".." parent row
    }

    fn mark_unselect_all(&self) {
        self.focused_pane().selection.unselect_all();
    }

    fn mark_invert(&self) {
        let pane = self.focused_pane();
        let n = pane.selection.n_items();
        for i in 1..n { // skip the ".." parent row
            if pane.selection.is_selected(i) {
                pane.selection.unselect_item(i);
            } else {
                pane.selection.select_item(i, false);
            }
        }
    }

    /// Open an interactive SSH session to the current server in a terminal.
    fn open_terminal(self: &Rc<Self>) {
        let session = self.session();
        let Some(creds) = session.creds.borrow().clone() else {
            self.set_status("Connect first");
            return;
        };
        if creds.protocol != Protocol::Sftp {
            self.set_status("Terminal sessions need SFTP (SSH)");
            return;
        }
        let target = if creds.username.is_empty() {
            creds.host.clone()
        } else {
            format!("{}@{}", creds.username, creds.host)
        };
        let ssh = format!("ssh -p {} {}", creds.port, target);
        // Try the common terminal emulators in order.
        let candidates: [(&str, Vec<&str>); 4] = [
            ("x-terminal-emulator", vec!["-e"]),
            ("gnome-terminal", vec!["--"]),
            ("konsole", vec!["-e"]),
            ("xterm", vec!["-e"]),
        ];
        for (bin, args) in candidates {
            let mut cmd = std::process::Command::new(bin);
            for a in &args {
                cmd.arg(a);
            }
            if args == ["--"] {
                cmd.args(["ssh", "-p", &creds.port.to_string(), &target]);
            } else {
                cmd.arg(&ssh);
            }
            if cmd.spawn().is_ok() {
                self.set_status(&format!("Opened terminal: {ssh}"));
                return;
            }
        }
        self.set_status("No terminal emulator found (tried x-terminal-emulator, gnome-terminal, konsole, xterm)");
    }

    /// Copy an sftp:// URL for the menu-target remote entry to the clipboard.
    fn copy_remote_url(self: &Rc<Self>) {
        let Some(entry) = self.menu_entry(false) else { return };
        let session = self.session();
        let Some(creds) = session.creds.borrow().clone() else { return };
        let scheme = match creds.protocol {
            Protocol::Ftp | Protocol::Ftps => "ftp",
            Protocol::S3 => "s3",
            _ => "sftp",
        };
        let path = join_posix(&session.remote_path.borrow(), &entry.name);
        let user = if creds.username.is_empty() {
            String::new()
        } else {
            format!("{}@", creds.username)
        };
        let url = format!("{scheme}://{user}{}:{}{path}", creds.host, creds.port);
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&url);
            self.set_status(&format!("Copied {url}"));
        }
    }

    /// Copy the selected item's full path (local filesystem or remote POSIX).
    fn menu_copy_path(self: &Rc<Self>, local_pane: bool) {
        let Some(entry) = self.menu_entry(local_pane) else { return };
        let path = if local_pane {
            self.local_path.borrow().join(&entry.name).display().to_string()
        } else {
            join_posix(&self.session().remote_path.borrow(), &entry.name)
        };
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&path);
            self.set_status(&format!("Copied {path}"));
        }
    }

    /// Open the system file manager with the selected local item highlighted.
    fn menu_show_in_files(self: &Rc<Self>) {
        let Some(entry) = self.menu_entry(true) else { return };
        let path = self.local_path.borrow().join(&entry.name);
        let file = gio::File::for_path(&path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        launcher.open_containing_folder(
            Some(&self.window),
            gio::Cancellable::NONE,
            |_| {},
        );
    }

    /// Persist the open tabs (settings + paths) for the next launch.
    fn save_workspace(&self) {
        let mut ws = workspace::Workspace::default();
        for session in self.sessions.borrow().iter() {
            let Some(creds) = session.creds.borrow().clone() else { continue };
            if !session.connected.get() {
                continue;
            }
            ws.tabs.push(workspace::TabState {
                proto: match creds.protocol {
                    Protocol::Ftp => 1,
                    Protocol::Ftps => 2,
                    Protocol::S3 => 3,
                    _ => 0,
                },
                host: creds.host.clone(),
                port: creds.port.to_string(),
                user: creds.username.clone(),
                auth: match &creds.auth {
                    Auth::KeyFile { .. } => 1,
                    Auth::Agent => 2,
                    _ => 0,
                },
                key_path: match &creds.auth {
                    Auth::KeyFile { path, .. } => path.clone(),
                    _ => String::new(),
                },
                bucket: creds.bucket.clone().unwrap_or_default(),
                region: creds.region.clone().unwrap_or_default(),
                remote_path: session.remote_path.borrow().clone(),
                local_path: self.local_path.borrow().to_string_lossy().into_owned(),
            });
        }
        workspace::save(&ws);
    }

    /// Recreate last session's tabs; auto-login where credentials are
    /// available (keyring password, key file, or agent). Returns false when
    /// there was nothing to restore.
    fn restore_workspace(self: &Rc<Self>) -> bool {
        let ws = workspace::load();
        if ws.tabs.is_empty() {
            return false;
        }
        let mut restored = false;
        for (i, tab) in ws.tabs.iter().enumerate() {
            if i > 0 {
                let session = create_session(self);
                self.sessions.borrow_mut().push(session);
                let last = self.sessions.borrow().len() - 1;
                self.active_tab.set(last);
            }
            // Prefill the form so a failed auto-login leaves a ready dialog.
            self.proto_dd.set_selected(tab.proto);
            self.auth_dd.set_selected(tab.auth);
            self.host_entry.set_text(&tab.host);
            self.port_entry.set_text(&tab.port);
            self.user_entry.set_text(&tab.user);
            self.key_entry.set_text(&tab.key_path);
            self.bucket_entry.set_text(&tab.bucket);
            self.region_entry.set_text(&tab.region);
            let p = PathBuf::from(&tab.local_path);
            if p.is_dir() {
                *self.local_path.borrow_mut() = p;
            }
            *self.session().remote_path.borrow_mut() = tab.remote_path.clone();
            let password = if tab.auth == 0 {
                secrets::load(&secrets::account(
                    PROTO_LABELS[tab.proto as usize % 4],
                    &tab.user,
                    &tab.host,
                    &tab.port,
                ))
            } else {
                None
            };
            match password {
                Some(pw) => {
                    self.pass_entry.set_text(&pw);
                    self.connect_clicked();
                    restored = true;
                }
                None if tab.auth != 0 => {
                    self.connect_clicked();
                    restored = true;
                }
                None => {
                    // No stored password: leave the prefilled login dialog up.
                    self.login_window.present();
                    restored = true;
                }
            }
        }
        self.load_local();
        self.refresh_tabs();
        restored
    }

    // -- Sites (WinSCP-style) -------------------------------------------------

    fn site_account(site: &Site) -> String {
        secrets::account(
            PROTO_LABELS[site.proto as usize % 4],
            &site.user,
            &site.host,
            &site.port,
        )
    }

    /// "Save session as site" dialog: name (Folder/Name groups) plus an
    /// explicit opt-in for password storage, like WinSCP.
    fn begin_save_site(self: &Rc<Self>) {
        let host = self.host_entry.text().to_string();
        let user = self.user_entry.text().to_string();
        let default_name = if host.is_empty() {
            "New site".to_string()
        } else if user.is_empty() {
            host
        } else {
            format!("{user}@{host}")
        };
        let can_save_password =
            self.selected_auth() == 0 && !self.pass_entry.text().is_empty();
        let state = self.clone();
        save_site_dialog(
            &self.window,
            &default_name,
            can_save_password,
            move |name, save_password| state.perform_save_site(&name, save_password),
        );
    }

    fn perform_save_site(&self, name: &str, save_password: bool) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let is_sftp = proto_from_index(self.proto_dd.selected()) == Protocol::Sftp;
        // Remember the current directories so loading the site lands there
        // (WinSCP's "Remote directory" advanced setting).
        let (remote_dir, local_dir) = if self.session().connected.get() {
            (
                self.session().remote_path.borrow().clone(),
                self.local_path.borrow().to_string_lossy().into_owned(),
            )
        } else {
            (String::new(), String::new())
        };
        let site = Site {
            name: name.to_string(),
            proto: self.proto_dd.selected(),
            host: self.host_entry.text().to_string(),
            port: self.port_entry.text().to_string(),
            user: self.user_entry.text().to_string(),
            auth: if is_sftp { self.auth_dd.selected() } else { 0 },
            key_path: self.key_entry.text().to_string(),
            bucket: self.bucket_entry.text().to_string(),
            region: self.region_entry.text().to_string(),
            remote_dir,
            local_dir,
        };
        let account = Self::site_account(&site);
        self.sites.borrow_mut().add(site);
        self.refresh_sites_list();

        let password = self.pass_entry.text().to_string();
        if save_password && !password.is_empty() {
            match secrets::save(&account, &password) {
                Ok(()) => self.set_status(&format!("Saved site “{name}” (password in keyring)")),
                Err(e) => self.set_status(&format!("Saved site “{name}” (keyring: {e})")),
            }
        } else {
            self.set_status(&format!("Saved site “{name}”"));
        }
    }

    /// Edit: fill the connection form from a site (single click in the list).
    fn load_site(&self, index: usize) {
        let Some(site) = self.sites.borrow().sites.get(index).cloned() else { return };
        // Order matters: proto first (it resets the default port), then port.
        self.proto_dd.set_selected(site.proto);
        self.auth_dd.set_selected(site.auth);
        self.host_entry.set_text(&site.host);
        self.port_entry.set_text(&site.port);
        self.user_entry.set_text(&site.user);
        self.key_entry.set_text(&site.key_path);
        self.bucket_entry.set_text(&site.bucket);
        self.region_entry.set_text(&site.region);
        if !site.remote_dir.is_empty() {
            *self.session().remote_path.borrow_mut() = site.remote_dir.clone();
        }
        if !site.local_dir.is_empty() {
            let p = PathBuf::from(&site.local_dir);
            if p.is_dir() {
                *self.local_path.borrow_mut() = p;
                self.load_local();
            }
        }
        if site.auth == 0 {
            if let Some(password) = secrets::load(&Self::site_account(&site)) {
                self.pass_entry.set_text(&password);
                self.set_status(&format!("Loaded “{}” — password from keyring", site.name));
                return;
            }
            self.pass_entry.set_text("");
            self.set_status(&format!("Loaded “{}” — enter password and Connect", site.name));
        } else {
            self.pass_entry.set_text("");
            self.set_status(&format!("Loaded “{}”", site.name));
        }
    }

    /// Login: load the site and connect immediately (double click / menu).
    fn login_site(&self, index: usize) {
        let Some(site) = self.sites.borrow().sites.get(index).cloned() else { return };
        self.load_site(index);
        let needs_password =
            site.auth == 0 && self.pass_entry.text().is_empty() && site.proto != 1;
        if needs_password {
            self.set_status(&format!(
                "“{}” has no stored password — enter it and Connect",
                site.name
            ));
            return;
        }
        self.connect_clicked();
    }

    fn rename_site(self: &Rc<Self>, index: usize) {
        let Some(site) = self.sites.borrow().sites.get(index).cloned() else { return };
        let state = self.clone();
        prompt(&self.window, "Rename site (Folder/Name groups)", &site.name, move |new_name| {
            state.sites.borrow_mut().rename(index, &new_name);
            state.refresh_sites_list();
        });
    }

    fn delete_site(&self, index: usize) {
        if let Some(site) = self.sites.borrow().sites.get(index).cloned() {
            secrets::delete(&Self::site_account(&site));
        }
        self.sites.borrow_mut().remove(index);
        self.refresh_sites_list();
    }

    /// Export all sites to a JSON file (no passwords — those stay in the
    /// keyring). The format is shared with the macOS app.
    fn export_sites(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_initial_name(Some("scp-commander-sites.json"));
        let state = self.clone();
        dialog.save(Some(&self.login_window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            match state.sites.borrow().export_interchange() {
                Ok(json) => match std::fs::write(&path, json) {
                    Ok(()) => state.set_status(&format!(
                        "Exported {} site(s) to {}",
                        state.sites.borrow().sites.len(),
                        path.display()
                    )),
                    Err(e) => state.set_status(&format!("Export failed: {e}")),
                },
                Err(e) => state.set_status(&format!("Export failed: {e}")),
            }
        });
    }

    /// Import sites from a JSON export (merges; same-named sites replaced).
    fn import_sites(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        let state = self.clone();
        dialog.open(Some(&self.login_window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let data = match std::fs::read_to_string(&path) {
                Ok(d) => d,
                Err(e) => {
                    state.set_status(&format!("Import failed: {e}"));
                    return;
                }
            };
            let outcome = state.sites.borrow_mut().import_interchange(&data);
            match outcome {
                Ok(count) => {
                    state.refresh_sites_list();
                    state.set_status(&format!("Imported {count} site(s)"));
                }
                Err(e) => state.set_status(&format!("Import failed: {e}")),
            }
        });
    }

    /// Import sessions from a WinSCP.ini file.
    fn import_winscp(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        let state = self.clone();
        dialog.open(Some(&self.login_window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let data = match std::fs::read_to_string(&path) {
                Ok(d) => d,
                Err(e) => {
                    state.set_status(&format!("Import failed: {e}"));
                    return;
                }
            };
            let outcome = state.sites.borrow_mut().import_winscp_ini(&data);
            match outcome {
                Ok(count) => {
                    state.refresh_sites_list();
                    state.set_status(&format!(
                        "Imported {count} site(s) from WinSCP (re-enter passwords)"
                    ));
                }
                Err(e) => state.set_status(&format!("Import failed: {e}")),
            }
        });
    }

    /// Import hosts from ~/.ssh/config into saved sites (grouped under "SSH/").
    fn import_ssh_config(self: &Rc<Self>) {
        let path = glib::home_dir().join(".ssh").join("config");
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => {
                self.set_status("No ~/.ssh/config found");
                return;
            }
        };
        match self.sites.borrow_mut().import_ssh_config(&data) {
            Ok(count) => {
                self.refresh_sites_list();
                self.set_status(&format!("Imported {count} host(s) from ~/.ssh/config"));
            }
            Err(e) => self.set_status(&format!("Import failed: {e}")),
        }
    }

    /// WinSCP-style Preferences: settings that don't belong to one session.
    fn preferences_dialog(self: &Rc<Self>) {
        let win = gtk::Window::builder()
            .title("Preferences")
            .transient_for(&self.login_window)
            .modal(true)
            .default_width(460)
            .build();

        let editor_entry = GtkEntry::builder()
            .hexpand(true)
            .text(prefs::get("editor").unwrap_or_default())
            .placeholder_text("e.g. code, gedit — empty uses the system default")
            .build();
        let pool_spin = gtk::SpinButton::with_range(1.0, 8.0, 1.0);
        pool_spin.set_value(prefs::get_int("pool_size", pool::DEFAULT_POOL_SIZE as i64) as f64);
        let ka_spin = gtk::SpinButton::with_range(5.0, 300.0, 5.0);
        ka_spin.set_value(prefs::get_int("keepalive_secs", 30) as f64);
        let masks_entry = GtkEntry::builder()
            .hexpand(true)
            .text(prefs::get("exclude_masks").unwrap_or_default())
            .placeholder_text("*.tmp; .git/")
            .build();
        let atomic_check = gtk::CheckButton::with_label("Upload to temporary file first");
        atomic_check.set_active(prefs::get_int("atomic_uploads", 1) != 0);
        atomic_check.set_tooltip_text(Some(
            "Uploads land under a temp name and rename on success, so an interrupted \
             transfer never leaves a truncated file."));

        let grid = gtk::Grid::builder()
            .row_spacing(8).column_spacing(10)
            .margin_top(14).margin_bottom(8).margin_start(14).margin_end(14)
            .build();
        let label = |t: &str| Label::builder().label(t).xalign(0.0).build();
        grid.attach(&label("Editor command:"), 0, 0, 1, 1);
        grid.attach(&editor_entry, 1, 0, 1, 1);
        grid.attach(&label("Parallel connections:"), 0, 1, 1, 1);
        grid.attach(&pool_spin, 1, 1, 1, 1);
        grid.attach(&label("Keepalive (seconds):"), 0, 2, 1, 1);
        grid.attach(&ka_spin, 1, 2, 1, 1);
        grid.attach(&label("Default exclude masks:"), 0, 3, 1, 1);
        grid.attach(&masks_entry, 1, 3, 1, 1);
        grid.attach(&atomic_check, 0, 4, 2, 1);

        let hint = Label::builder()
            .label("Connection count applies to sessions opened afterwards.")
            .xalign(0.0)
            .build();
        hint.add_css_class("caption");
        hint.add_css_class("dim-label");

        let save = Button::with_label("Save");
        save.add_css_class("suggested-action");
        let close = Button::with_label("Close");
        let btns = GtkBox::builder()
            .orientation(Orientation::Horizontal).spacing(8).halign(gtk::Align::End)
            .margin_bottom(12).margin_end(14).margin_top(4)
            .build();
        btns.append(&close);
        btns.append(&save);

        let vbox = GtkBox::builder().orientation(Orientation::Vertical).spacing(4).build();
        vbox.append(&grid);
        let hint_box = GtkBox::builder().margin_start(14).margin_end(14).build();
        hint_box.append(&hint);
        vbox.append(&hint_box);
        vbox.append(&btns);
        win.set_child(Some(&vbox));

        close.connect_clicked(glib::clone!(#[weak] win, move |_| win.close()));
        save.connect_clicked(glib::clone!(
            #[strong(rename_to = state)] self,
            #[weak] win,
            #[weak] editor_entry,
            #[weak] pool_spin,
            #[weak] ka_spin,
            #[weak] masks_entry,
            #[weak] atomic_check,
            move |_| {
                prefs::set("editor", &editor_entry.text());
                prefs::set("pool_size", &pool_spin.value_as_int().to_string());
                let ka = ka_spin.value_as_int();
                prefs::set("keepalive_secs", &ka.to_string());
                prefs::set("exclude_masks", &masks_entry.text());
                let atomic = atomic_check.is_active();
                prefs::set("atomic_uploads", if atomic { "1" } else { "0" });
                // Apply live where possible.
                worker::KEEPALIVE_SECS
                    .store(ka.max(5) as u64, std::sync::atomic::Ordering::Relaxed);
                scp_core::set_atomic_uploads(atomic);
                state.exclude_entry.set_text(&masks_entry.text());
                state.set_status("Preferences saved");
                win.close();
            }
        ));
        win.present();
    }

    fn load_custom_commands() -> Vec<CustomCommand> {
        prefs::get("custom_commands")
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    fn save_custom_commands(cmds: &[CustomCommand]) {
        if let Ok(j) = serde_json::to_string(cmds) {
            prefs::set("custom_commands", &j);
        }
    }

    /// Run a custom command on the selected remote files. "{}" expands to their
    /// shell-quoted absolute paths; templates without it run unchanged.
    fn run_custom_command(self: &Rc<Self>, template: &str) {
        let session = self.session();
        if !session.connected.get()
            || session.creds.borrow().as_ref().map(|c| c.protocol)
                != Some(scp_core::types::Protocol::Sftp)
        {
            self.set_status("Custom commands need a connected SFTP session");
            return;
        }
        let base = session.remote_path.borrow().clone();
        let paths: String = self
            .selected_entries(false)
            .iter()
            .map(|e| shell_quote(&join_posix(&base, &e.name)))
            .collect::<Vec<_>>()
            .join(" ");
        let cmd = if template.contains("{}") {
            template.replace("{}", &paths)
        } else {
            template.to_string()
        };
        self.set_status(&format!("Executing: {cmd}…"));
        let _ = session.cmd.send(Cmd::Exec { cmd });
    }

    /// Manage and run custom remote command templates.
    fn custom_commands_dialog(self: &Rc<Self>) {
        let win = gtk::Window::builder()
            .title("Custom Commands")
            .transient_for(&self.window)
            .modal(true)
            .default_width(520)
            .default_height(360)
            .build();

        let intro = Label::builder()
            .label("Run a templated command on the selected remote file(s). \"{}\" expands to their shell-quoted paths.")
            .xalign(0.0).wrap(true).build();
        intro.add_css_class("caption");
        intro.add_css_class("dim-label");

        let list = ListBox::new();
        list.add_css_class("boxed-list");
        let scroll = ScrolledWindow::builder().vexpand(true).child(&list).build();

        let rebuild: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let rebuild_impl: Rc<dyn Fn()> = {
            let list = list.clone();
            let state = self.clone();
            let rebuild = rebuild.clone();
            let win = win.clone();
            Rc::new(move || {
                while let Some(row) = list.first_child() {
                    list.remove(&row);
                }
                let cmds = Self::load_custom_commands();
                if cmds.is_empty() {
                    let l = Label::builder().label("No custom commands yet.").build();
                    l.add_css_class("dim-label");
                    l.set_margin_top(12);
                    l.set_margin_bottom(12);
                    list.append(&l);
                    return;
                }
                for (idx, c) in cmds.iter().enumerate() {
                    let row = GtkBox::builder()
                        .orientation(Orientation::Horizontal).spacing(8)
                        .margin_top(4).margin_bottom(4).margin_start(8).margin_end(8)
                        .build();
                    let info = GtkBox::builder()
                        .orientation(Orientation::Vertical).hexpand(true).build();
                    let name_l = Label::builder().label(&c.name).xalign(0.0).build();
                    let tmpl_l = Label::builder().label(&c.template).xalign(0.0).build();
                    tmpl_l.add_css_class("caption");
                    tmpl_l.add_css_class("dim-label");
                    tmpl_l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    info.append(&name_l);
                    info.append(&tmpl_l);
                    let run = Button::with_label("Run");
                    run.add_css_class("suggested-action");
                    let template = c.template.clone();
                    let st = state.clone();
                    let w = win.clone();
                    run.connect_clicked(move |_| {
                        st.run_custom_command(&template);
                        w.close();
                    });
                    let del = Button::from_icon_name("user-trash-symbolic");
                    del.add_css_class("flat");
                    let rb = rebuild.clone();
                    del.connect_clicked(move |_| {
                        let mut cmds = Self::load_custom_commands();
                        if idx < cmds.len() {
                            cmds.remove(idx);
                            Self::save_custom_commands(&cmds);
                        }
                        if let Some(f) = rb.borrow().clone() {
                            f();
                        }
                    });
                    row.append(&info);
                    row.append(&run);
                    row.append(&del);
                    list.append(&row);
                }
            })
        };
        *rebuild.borrow_mut() = Some(rebuild_impl.clone());
        rebuild_impl();

        // Add row.
        let name_entry = GtkEntry::builder().placeholder_text("Name").max_width_chars(14).build();
        let tmpl_entry = GtkEntry::builder()
            .placeholder_text("Command (use {} for files)").hexpand(true).build();
        let add = Button::with_label("Add");
        {
            let rb = rebuild_impl.clone();
            let name_entry = name_entry.clone();
            let tmpl_entry = tmpl_entry.clone();
            add.connect_clicked(move |_| {
                let name = name_entry.text().trim().to_string();
                let template = tmpl_entry.text().trim().to_string();
                if name.is_empty() || template.is_empty() {
                    return;
                }
                let mut cmds = Self::load_custom_commands();
                cmds.push(CustomCommand { name, template });
                Self::save_custom_commands(&cmds);
                name_entry.set_text("");
                tmpl_entry.set_text("");
                rb();
            });
        }
        let add_row = GtkBox::builder().orientation(Orientation::Horizontal).spacing(6).build();
        add_row.append(&name_entry);
        add_row.append(&tmpl_entry);
        add_row.append(&add);

        let close = Button::with_label("Close");
        close.connect_clicked(glib::clone!(#[weak] win, move |_| win.close()));
        let btns = GtkBox::builder()
            .orientation(Orientation::Horizontal).halign(gtk::Align::End).build();
        btns.append(&close);

        let vbox = GtkBox::builder()
            .orientation(Orientation::Vertical).spacing(8)
            .margin_top(12).margin_bottom(12).margin_start(12).margin_end(12)
            .build();
        vbox.append(&intro);
        vbox.append(&scroll);
        vbox.append(&add_row);
        vbox.append(&btns);
        win.set_child(Some(&vbox));
        win.present();
    }

    /// View and forget SCP Commander's trusted SSH host keys (its own store).
    fn known_hosts_dialog(self: &Rc<Self>) {
        let win = gtk::Window::builder()
            .title("Known Hosts")
            .transient_for(&self.login_window)
            .modal(true)
            .default_width(440)
            .default_height(360)
            .build();

        let intro = Label::builder()
            .label(
                "Keys SCP Commander has accepted. Forget one to be re-prompted on the \
                 next connection. Your system ~/.ssh/known_hosts is not shown or modified.")
            .xalign(0.0)
            .wrap(true)
            .build();
        intro.add_css_class("caption");
        intro.add_css_class("dim-label");

        let list = ListBox::new();
        list.add_css_class("boxed-list");
        let scroll = ScrolledWindow::builder().vexpand(true).child(&list).build();

        // Self-referential rebuild closure so a Forget button can refresh.
        let rebuild: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let rebuild_impl: Rc<dyn Fn()> = {
            let list = list.clone();
            let state = self.clone();
            let rebuild = rebuild.clone();
            Rc::new(move || {
                while let Some(row) = list.first_child() {
                    list.remove(&row);
                }
                let hosts = scp_core::sftp::list_known_hosts();
                if hosts.is_empty() {
                    let l = Label::builder().label("No trusted keys yet.").build();
                    l.add_css_class("dim-label");
                    l.set_margin_top(12);
                    l.set_margin_bottom(12);
                    list.append(&l);
                    return;
                }
                for h in hosts {
                    let row = GtkBox::builder()
                        .orientation(Orientation::Horizontal).spacing(8)
                        .margin_top(4).margin_bottom(4).margin_start(8).margin_end(8)
                        .build();
                    let info = GtkBox::builder()
                        .orientation(Orientation::Vertical).hexpand(true).build();
                    let host_l = Label::builder().label(&h.host).xalign(0.0).build();
                    let type_l = Label::builder().label(&h.key_type).xalign(0.0).build();
                    type_l.add_css_class("caption");
                    type_l.add_css_class("dim-label");
                    info.append(&host_l);
                    info.append(&type_l);
                    let forget = Button::with_label("Forget");
                    forget.add_css_class("destructive-action");
                    let host = h.host.clone();
                    let st = state.clone();
                    let rb = rebuild.clone();
                    forget.connect_clicked(move |_| {
                        let _ = scp_core::sftp::remove_known_host(&host);
                        st.set_status(&format!("Forgot host key for {host}"));
                        if let Some(f) = rb.borrow().clone() {
                            f();
                        }
                    });
                    row.append(&info);
                    row.append(&forget);
                    list.append(&row);
                }
            })
        };
        *rebuild.borrow_mut() = Some(rebuild_impl.clone());
        rebuild_impl();

        let close = Button::with_label("Close");
        close.connect_clicked(glib::clone!(#[weak] win, move |_| win.close()));
        let btns = GtkBox::builder()
            .orientation(Orientation::Horizontal).halign(gtk::Align::End)
            .margin_top(6).build();
        btns.append(&close);

        let vbox = GtkBox::builder()
            .orientation(Orientation::Vertical).spacing(8)
            .margin_top(12).margin_bottom(12).margin_start(12).margin_end(12)
            .build();
        vbox.append(&intro);
        vbox.append(&scroll);
        vbox.append(&btns);
        win.set_child(Some(&vbox));
        win.present();
    }

    fn refresh_sites_list(&self) {
        while let Some(row) = self.sites_list.first_child() {
            self.sites_list.remove(&row);
        }
        for site in &self.sites.borrow().sites {
            let label = Label::builder()
                .label(format!(
                    "{}\n{}",
                    site.display_name(),
                    PROTO_LABELS[site.proto as usize % PROTO_LABELS.len()]
                ))
                .xalign(0.0)
                .margin_top(4)
                .margin_bottom(4)
                .margin_start(6)
                .build();
            self.sites_list.append(&label);
        }
    }

    // -- Worker events ------------------------------------------------------

    /// Handle an event from `session`'s worker. Pane/status updates only
    /// apply when that session is the active tab; caches always update.
    /// Events from the dedicated transfer worker only drive transfer rows.
    fn handle_event(self: &Rc<Self>, session: &Rc<Session>, event: Event, from_transfer: bool) {
        let is_active = Rc::ptr_eq(session, &self.session());
        if from_transfer {
            match &event {
                Event::Progress { .. }
                | Event::FileStart { .. }
                | Event::FileDone { .. }
                | Event::Done { .. }
                | Event::Cancelled { .. }
                | Event::Retrying { .. }
                | Event::Failed { .. } => {}
                Event::Error(m) => {
                    self.set_status(&format!("Transfer error: {m}"));
                    return;
                }
                // Connected/Listed etc. from the transfer connection are
                // bookkeeping only — never touch the panes.
                _ => return,
            }
        }
        match event {
            Event::Connected { path, entries } | Event::Listed { path, entries } => {
                let first_connect = !session.connected.get();
                session.connected.set(true);
                if first_connect && !from_transfer {
                    // Connect all pool workers. They establish their own
                    // independent connections in the background.
                    if let Some(creds) = session.creds.borrow().clone() {
                        session.xfer_pool.connect(creds);
                    }
                    *session.home_path.borrow_mut() = path.clone();
                }
                // Record navigation history (skip refreshes, initial connect,
                // and Back/Forward moves which manage the stacks themselves).
                {
                    let old = session.remote_path.borrow().clone();
                    if self.remote_hist_suppress.get() {
                        self.remote_hist_suppress.set(false);
                    } else if !first_connect && old != path && is_active {
                        let mut hist = self.remote_hist.borrow_mut();
                        hist.0.push(old);
                        hist.1.clear();
                    }
                }
                // Entries arrive already sorted from the worker thread
                // (folders first, then case-insensitive by name).
                let count = entries.len();
                *session.remote_path.borrow_mut() = path.clone();
                *session.cache.borrow_mut() = entries.clone();
                // Retitle on every (re)connect, not just the first one - a
                // tab reused for a different server kept its stale title.
                let title = self
                    .pending_connect
                    .borrow()
                    .as_ref()
                    .map(|(c, _)| Self::session_label(c))
                    .unwrap_or_default();
                if !title.is_empty() && *session.title.borrow() != title {
                    *session.title.borrow_mut() = title;
                    self.refresh_tabs();
                }
                if first_connect && self.remember_pw_check.is_active() {
                    if let Some(creds) = session.creds.borrow().clone() {
                        if let Auth::Password(ref pw) = creds.auth {
                            if !pw.is_empty() {
                                let proto_label = PROTO_LABELS[creds.protocol as usize % PROTO_LABELS.len()];
                                let account = secrets::account(
                                    proto_label,
                                    &creds.username,
                                    &creds.host,
                                    &creds.port.to_string(),
                                );
                                let _ = secrets::save(&account, pw);
                            }
                        }
                    }
                }
                if is_active {
                    self.remote.show(&entries, &path, self.show_hidden.get());
                    self.set_status(&format!("{path} ({count} items)"));
                    if first_connect || self.login_window.is_visible() {
                        self.login_window.set_visible(false);
                        self.update_transfer_title();
                    }
                }
            }
            Event::HostKeyUnknown { fingerprint } => {
                self.hostkey_label.set_text(&format!(
                    "New server — host key fingerprint: {fingerprint}. Trust it?"
                ));
                *self.pending_fingerprint.borrow_mut() = Some(fingerprint);
                self.hostkey_bar.set_visible(true);
                self.set_status("Server key not recognized — confirm fingerprint to connect");
            }
            Event::Progress { id, done, total } => {
                if let Some(row) = self.transfer_rows.borrow_mut().get_mut(&id) {
                    // Smoothed speed + ETA, WinSCP-style.
                    let now = std::time::Instant::now();
                    if let Some(at) = row.last_at {
                        let dt = now.duration_since(at).as_secs_f64();
                        if dt >= 0.5 {
                            let delta = done.saturating_sub(row.last_done) as f64;
                            let inst = delta / dt;
                            row.speed = if row.speed == 0.0 {
                                inst
                            } else {
                                row.speed * 0.7 + inst * 0.3
                            };
                            row.last_at = Some(now);
                            row.last_done = done;
                        }
                    } else {
                        row.last_at = Some(now);
                        row.last_done = done;
                    }
                    // "17% Uploading" headline + bytes on the bar.
                    let op = if row.download { "Downloading" } else { "Uploading" };
                    if total > 0 {
                        let frac = (done as f64 / total as f64).min(1.0);
                        row.bar.set_fraction(frac);
                        row.title.set_text(&format!("{}% {op}", (frac * 100.0).round() as u32));
                        row.bar.set_text(Some(&format!(
                            "{} / {}", human_size(done), human_size(total))));
                    } else {
                        row.bar.pulse();
                        row.title.set_text(op);
                        row.bar.set_text(Some(&human_size(done)));
                    }
                    // "Time left … · Time elapsed … · Speed …" detail line.
                    let el = row.started.elapsed().as_secs();
                    let mut detail = String::new();
                    if row.speed > 1.0 && total > done {
                        let secs = ((total - done) as f64 / row.speed) as u64;
                        detail.push_str(&format!("Time left {}:{:02}  ·  ", secs / 60, secs % 60));
                    }
                    detail.push_str(&format!(
                        "Time elapsed {}:{:02}:{:02}", el / 3600, (el / 60) % 60, el % 60));
                    if row.speed > 1.0 {
                        detail.push_str(&format!("  ·  Speed {}/s", human_size(row.speed as u64)));
                    }
                    row.detail.set_text(&detail);
                }
            }
            Event::FileStart { id, file, total } => {
                if let Some(row) = self.transfer_rows.borrow().get(&id) {
                    row.bar.set_fraction(0.0);
                    row.file_label.set_text(&format!("File:  {file}"));
                    let _ = total;
                }
            }
            Event::FileDone { id } => {
                if let Some(row) = self.transfer_rows.borrow_mut().get_mut(&id) {
                    row.files_done += 1;
                }
            }
            Event::Done { id, name, bytes, download } => {
                let files_done = self
                    .transfer_rows
                    .borrow()
                    .get(&id)
                    .map(|r| r.files_done)
                    .unwrap_or(0);
                let text = if files_done > 0 {
                    format!("done — {files_done} file(s)")
                } else {
                    format!("done — {}", human_size(bytes))
                };
                self.finish_row(id, &text, true);

                // View flow: temp download done — show the preview, drop the temp.
                if let Some((vname, vlocal)) = self.view_pending.borrow_mut().remove(&id) {
                    viewer_dialog(&self.window, &vname, &read_preview(&vlocal));
                    let _ = std::fs::remove_file(&vlocal);
                }

                // Edit flow: the temp download finished — open it and watch.
                if let Some((remote, local)) = self.edit_pending.borrow_mut().remove(&id) {
                    let mtime = std::fs::metadata(&local)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    // Use the editor command set in Preferences, else the
                    // system default app for the file's type.
                    let opened = if let Some(editor) = prefs::get("editor") {
                        std::process::Command::new(&editor)
                            .arg(&local)
                            .spawn()
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    } else {
                        let uri = format!("file://{}", local.display());
                        gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
                            .map_err(|e| e.to_string())
                    };
                    if let Err(e) = opened {
                        self.set_status(&format!("Could not open editor: {e}"));
                    } else {
                        self.set_status(&format!("Editing {name} — saves upload automatically"));
                    }
                    self.edits.borrow_mut().push(EditWatch {
                        remote,
                        local,
                        last_mtime: mtime,
                        cmd: session.cmd.clone(),
                    });
                    return;
                }

                // F6 move: the copy succeeded — delete the source side.
                if let Some(source) = self.pending_move.borrow_mut().remove(&id) {
                    match source {
                        MoveSource::Local { path, is_dir } => {
                            let outcome = if is_dir {
                                std::fs::remove_dir_all(&path)
                            } else {
                                std::fs::remove_file(&path)
                            };
                            if let Err(e) = outcome {
                                self.set_status(&format!("Move: could not delete source: {e}"));
                            }
                            self.load_local();
                        }
                        MoveSource::Remote { path, is_dir } => {
                            let _ = session.cmd.send(Cmd::Delete { path, is_dir });
                        }
                    }
                }

                self.set_status(&format!(
                    "{} {name}",
                    if download { "Downloaded" } else { "Uploaded" }
                ));
                if download {
                    self.load_local();
                } else if session.connected.get() {
                    // Refresh the owning session's listing, whichever tab it is.
                    let path = session.remote_path.borrow().clone();
                    let _ = session.cmd.send(Cmd::List { path });
                }
            }
            Event::Cancelled { id, name } => {
                self.finish_row(id, "cancelled", false);
                self.pending_move.borrow_mut().remove(&id);
                self.edit_pending.borrow_mut().remove(&id);
                self.set_status(&format!("Cancelled {name}"));
            }
            Event::Retrying { id, attempt } => {
                // Transient network error; the worker is re-attempting in place.
                if let Some(row) = self.transfer_rows.borrow().get(&id) {
                    row.title.set_text(&format!("Network error — retrying ({attempt}/3)…"));
                }
                self.set_status(&format!("Network error — retrying ({attempt}/3)…"));
            }
            Event::Failed { id, message, network: _ } => {
                // The worker already exhausted auto-retries for transient errors.
                self.finish_row(id, &format!("failed: {message}"), false);
                self.pending_move.borrow_mut().remove(&id);
                self.edit_pending.borrow_mut().remove(&id);
                self.set_status(&format!("Error: {message}"));
            }
            Event::OpOk { message } => {
                self.set_status(&message);
                if session.connected.get() {
                    let path = session.remote_path.borrow().clone();
                    let _ = session.cmd.send(Cmd::List { path });
                }
            }
            Event::SyncPlanReady { download, local, remote, plan } => {
                if plan.items.is_empty() && plan.dirs.is_empty() {
                    self.set_status("Sync preview: nothing to copy — already in sync");
                } else {
                    sync_preview_dialog(self, download, local, remote, plan);
                }
            }
            Event::FindResults { base, mask, hits } => {
                self.set_status(&format!("Find: {} match(es) for {mask}", hits.len()));
                find_results_dialog(self, &base, &mask, hits);
            }
            Event::ExecResult { exit_code, stdout, stderr } => {
                self.set_status(&format!("Command exited {exit_code}"));
                exec_result_dialog(&self.window, exit_code, &stdout, &stderr);
            }
            Event::Error(message) => {
                self.set_status(&format!("Error: {message}"));
                // A failed typed navigation must not leave the path bar
                // showing a directory we never entered.
                if is_active {
                    self.remote
                        .path_entry
                        .set_text(&session.remote_path.borrow());
                }
                // Offer reconnect when a browse error hits an established session.
                if !from_transfer && session.connected.get() {
                    reconnect_dialog(self, session, &message);
                }
            }
        }
    }
}

impl Pane {
    fn entry_at(&self, index: u32) -> Option<Entry> {
        self.entries.borrow().get(index as usize).cloned()
    }

    fn show(&self, entries: &[Entry], path: &str, show_hidden: bool) {
        self.path_entry.set_text(path);
        self.model.remove_all();
        let parent_entry = Entry {
            name: "..".into(),
            is_dir: true,
            size: 0,
            mtime: None::<i64>,
            perms: None,
            is_symlink: false,
            uid: None,
            gid: None,
        };
        let mut visible: Vec<Entry> = vec![parent_entry.clone()];
        visible.extend(
            entries
                .iter()
                .filter(|e| show_hidden || !e.name.starts_with('.'))
                .cloned(),
        );
        for e in &visible {
            self.model.append(&glib::BoxedAnyObject::new(e.clone()));
        }
        *self.entries.borrow_mut() = visible;
    }
}

// ---------------------------------------------------------------------------
// UI assembly

/// Spawn a session: its own worker thread plus a main-loop pump feeding
/// events (tagged with the session) into the shared handler.
fn create_session(state: &Rc<App>) -> Rc<Session> {
    let (event_tx, event_rx) = async_channel::unbounded::<Event>();
    let cmd = worker::spawn(event_tx);
    let (xfer_tx, xfer_rx) = async_channel::unbounded::<Event>();
    let pool_size = prefs::get_int("pool_size", pool::DEFAULT_POOL_SIZE as i64) as usize;
    let xfer_pool = pool::TransferPool::new(xfer_tx, pool_size);
    let session = Rc::new(Session {
        cmd,
        xfer_pool,
        creds: RefCell::new(None),
        remote_path: RefCell::new("/".to_string()),
        connected: Cell::new(false),
        cache: RefCell::new(Vec::new()),
        title: RefCell::new("New Session".to_string()),
        home_path: RefCell::new("/".to_string()),
    });
    glib::spawn_future_local({
        let state = state.clone();
        let session = session.clone();
        async move {
            while let Ok(event) = event_rx.recv().await {
                state.handle_event(&session, event, false);
            }
        }
    });
    glib::spawn_future_local({
        let state = state.clone();
        let session = session.clone();
        async move {
            while let Ok(event) = xfer_rx.recv().await {
                state.handle_event(&session, event, true);
            }
        }
    });
    session
}

fn build_ui(app: &Application, open_uri: Option<&str>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("SCP Commander")
        .default_width(1120)
        .default_height(640)
        .build();

    // Session form (lives in the WinSCP-style Login dialog) ------------------
    let proto_dd = DropDown::from_strings(&PROTO_LABELS);
    let auth_dd = DropDown::from_strings(&AUTH_LABELS);
    let user_entry = GtkEntry::builder().build();
    let host_entry = GtkEntry::builder().hexpand(true).build();
    let port_entry = GtkEntry::builder().text("22").max_width_chars(6).width_chars(6).build();
    let pass_entry = PasswordEntry::builder().show_peek_icon(true).hexpand(true).build();
    let remember_pw_check = gtk::CheckButton::with_label("Remember password");
    let key_entry = GtkEntry::builder().hexpand(true).build();
    let key_browse = Button::from_icon_name("document-open-symbolic");
    key_browse.set_tooltip_text(Some("Choose a private key"));
    let bucket_entry = GtkEntry::builder().build();
    let region_entry = GtkEntry::builder().placeholder_text("us-east-1").build();

    let form_label = |text: &str| {
        let l = Label::builder().label(text).xalign(0.0).build();
        l.add_css_class("dim-label");
        l
    };
    let proto_label = form_label("File protocol:");
    let auth_label = form_label("Authentication:");
    let host_label = form_label("Host name:");
    let port_label = form_label("Port number:");
    let user_label = form_label("User name:");
    let pass_label = form_label("Password:");
    let key_label = form_label("Private key:");
    let bucket_label = form_label("Bucket:");
    let region_label = form_label("Region:");

    let key_row = GtkBox::builder().orientation(Orientation::Horizontal).spacing(4).build();
    key_row.append(&key_entry);
    key_row.append(&key_browse);

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .build();
    form.attach(&proto_label, 0, 0, 1, 1);
    form.attach(&proto_dd, 1, 0, 1, 1);
    form.attach(&auth_label, 0, 1, 1, 1);
    form.attach(&auth_dd, 1, 1, 1, 1);
    form.attach(&host_label, 0, 2, 1, 1);
    form.attach(&host_entry, 1, 2, 2, 1);
    form.attach(&port_label, 3, 2, 1, 1);
    form.attach(&port_entry, 4, 2, 1, 1);
    form.attach(&user_label, 0, 3, 1, 1);
    form.attach(&user_entry, 1, 3, 1, 1);
    form.attach(&pass_label, 0, 4, 1, 1);
    form.attach(&pass_entry, 1, 4, 2, 1);
    form.attach(&remember_pw_check, 1, 5, 2, 1);
    form.attach(&key_label, 0, 6, 1, 1);
    form.attach(&key_row, 1, 6, 2, 1);
    form.attach(&bucket_label, 0, 7, 1, 1);
    form.attach(&bucket_entry, 1, 7, 1, 1);
    form.attach(&region_label, 0, 8, 1, 1);
    form.attach(&region_entry, 1, 8, 1, 1);
    let insecure_label = Label::builder()
        .label("\u{26a0} Plain FTP sends your password and data unencrypted \u{2014} prefer SFTP or FTPS.")
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    insecure_label.add_css_class("warning");
    form.attach(&insecure_label, 0, 9, 5, 1);

    // The pickers drive the default port, field visibility, and label text.
    let update_form = {
        let proto_dd = proto_dd.clone();
        let auth_dd = auth_dd.clone();
        let port_entry = port_entry.clone();
        let auth_label = auth_label.clone();
        let auth_dd2 = auth_dd.clone();
        let key_label = key_label.clone();
        let key_row = key_row.clone();
        let pass_label = pass_label.clone();
        let pass_entry = pass_entry.clone();
        let host_label = host_label.clone();
        let user_label = user_label.clone();
        let bucket_label = bucket_label.clone();
        let bucket_entry = bucket_entry.clone();
        let region_label = region_label.clone();
        let region_entry = region_entry.clone();
        let insecure_label = insecure_label.clone();
        let host_for_warn = host_entry.clone();
        let last_proto = std::cell::Cell::new(u32::MAX);
        Rc::new(move || {
            let selected = proto_dd.selected();
            let is_s3 = selected == 3;
            let is_sftp = selected == 0;
            let auth = if is_sftp { auth_dd.selected() } else { 0 };
            // Only reset the port when the protocol actually changed - the
            // auth dropdown also runs this hook and must not clobber a
            // custom port the user typed.
            if last_proto.get() != selected {
                last_proto.set(selected);
                port_entry.set_text(
                    &Credentials::default_port(proto_from_index(selected)).to_string(),
                );
            }
            auth_label.set_visible(is_sftp);
            auth_dd2.set_visible(is_sftp);
            let show_key = is_sftp && auth == 1;
            key_label.set_visible(show_key);
            key_row.set_visible(show_key);
            let show_pass = !(is_sftp && auth == 2);
            pass_label.set_visible(show_pass);
            pass_entry.set_visible(show_pass);
            pass_label.set_text(if is_s3 {
                "Secret key:"
            } else if show_key {
                "Passphrase:"
            } else {
                "Password:"
            });
            host_label.set_text(if is_s3 { "Endpoint (blank = AWS):" } else { "Host name:" });
            user_label.set_text(if is_s3 { "Access key:" } else { "User name:" });
            bucket_label.set_visible(is_s3);
            bucket_entry.set_visible(is_s3);
            region_label.set_visible(is_s3);
            region_entry.set_visible(is_s3);
            // Plain FTP, or an explicit http:// S3 endpoint: cleartext.
            let host_text = host_for_warn.text().to_string();
            let insecure = selected == 1 || (is_s3 && host_text.starts_with("http://"));
            insecure_label.set_text(if is_s3 {
                "\u{26a0} http:// endpoint sends credentials and data unencrypted."
            } else {
                "\u{26a0} Plain FTP sends your password and data unencrypted \u{2014} prefer SFTP or FTPS."
            });
            insecure_label.set_visible(insecure);
        })
    };
    let hook = update_form.clone();
    proto_dd.connect_selected_notify(move |_| hook());
    let hook = update_form.clone();
    auth_dd.connect_selected_notify(move |_| hook());
    let hook = update_form.clone();
    host_entry.connect_changed(move |_| hook());

    // Auto-fill password from keyring when credentials are fully typed.
    {
        let host_e = host_entry.clone();
        let user_e = user_entry.clone();
        let port_e = port_entry.clone();
        let proto_d = proto_dd.clone();
        let auth_d = auth_dd.clone();
        let pass_e = pass_entry.clone();
        let remember = remember_pw_check.clone();
        let try_fill = Rc::new(move || {
            let proto_idx = proto_d.selected() as usize;
            let is_password_auth = proto_idx != 0 || auth_d.selected() == 0;
            if !is_password_auth { return; }
            let account = secrets::account(
                PROTO_LABELS[proto_idx % PROTO_LABELS.len()],
                &user_e.text(),
                &host_e.text(),
                &port_e.text(),
            );
            if let Some(pw) = secrets::load(&account) {
                pass_e.set_text(&pw);
                remember.set_active(true);
            }
        });
        let f = try_fill.clone();
        host_entry.connect_changed(move |_| f());
        let f = try_fill.clone();
        user_entry.connect_changed(move |_| f());
        let f = try_fill.clone();
        port_entry.connect_changed(move |_| f());
    }

    // Login dialog buttons + main-window toolbar ------------------------------
    let login_btn = Button::with_label("Login");
    login_btn.add_css_class("suggested-action");
    let close_btn = Button::with_label("Close");
    let new_session_btn = Button::with_label("New Session…");
    let sync_up_btn = Button::from_icon_name("go-up-symbolic");
    sync_up_btn.set_tooltip_text(Some("Sync local → remote (upload changes)"));
    let sync_down_btn = Button::from_icon_name("go-down-symbolic");
    sync_down_btn.set_tooltip_text(Some("Sync remote → local (download changes)"));
    let keep_toggle = gtk::ToggleButton::new();
    keep_toggle.set_icon_name("emblem-synchronizing-symbolic");
    keep_toggle.set_tooltip_text(Some(
        "Keep remote directory up to date: auto-push local changes to the current remote dir"));

    let main_toolbar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    let exclude_entry = GtkEntry::builder()
        .placeholder_text("exclude: *.tmp; .git/")
        .max_width_chars(22)
        .tooltip_text("Exclusion masks for folder transfers and sync")
        .build();
    // Pre-fill with the default masks set in Preferences.
    if let Some(masks) = prefs::get("exclude_masks") {
        exclude_entry.set_text(&masks);
    }
    let find_btn = Button::from_icon_name("system-search-symbolic");
    find_btn.set_tooltip_text(Some("Find remote files (mask, e.g. *.log)"));
    let terminal_btn = Button::from_icon_name("utilities-terminal-symbolic");
    terminal_btn.set_tooltip_text(Some("Open SSH session in a terminal"));
    let exec_btn = Button::from_icon_name("utilities-terminal-symbolic");
    exec_btn.set_tooltip_text(Some("Execute remote command (SFTP)"));
    let mirror_check = gtk::CheckButton::with_label("Mirror");
    mirror_check.set_tooltip_text(Some("Mirror mode: delete destination items with no source counterpart on sync"));
    let hidden_btn = gtk::ToggleButton::new();
    hidden_btn.set_icon_name("view-reveal-symbolic");
    hidden_btn.set_tooltip_text(Some("Show hidden files"));
    let hint = Label::builder()
        .label("F5 copy · F6 move · F2 rename · Tab panes")
        .hexpand(true)
        .xalign(1.0)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");

    let help_btn = Button::from_icon_name("help-browser-symbolic");
    help_btn.set_tooltip_text(Some("Help"));

    main_toolbar.append(&new_session_btn);
    main_toolbar.append(&gtk::Separator::new(Orientation::Vertical));
    main_toolbar.append(&sync_up_btn);
    main_toolbar.append(&sync_down_btn);
    main_toolbar.append(&keep_toggle);
    main_toolbar.append(&find_btn);
    main_toolbar.append(&terminal_btn);
    main_toolbar.append(&exec_btn);
    main_toolbar.append(&mirror_check);
    main_toolbar.append(&gtk::Separator::new(Orientation::Vertical));
    main_toolbar.append(&hidden_btn);
    main_toolbar.append(&exclude_entry);
    main_toolbar.append(&hint);
    main_toolbar.append(&help_btn);

    // Panes ------------------------------------------------------------------
    let local_hook: MenuHook = Rc::new(RefCell::new(None));
    let remote_hook: MenuHook = Rc::new(RefCell::new(None));
    let (local_widget, local_pane, local_view, local_header) =
        make_pane("Local", &local_hook, false);
    let (remote_widget, remote_pane, remote_view, remote_header) =
        make_pane("Remote", &remote_hook, true);

    let panes = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_start(6)
        .margin_end(6)
        .vexpand(true)
        .homogeneous(true)
        .build();
    panes.append(&local_widget);
    panes.append(&remote_widget);

    // Host key trust bar ------------------------------------------------------
    let hostkey_label = Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .selectable(true)
        .build();
    let trust_btn = Button::with_label("Trust & Connect");
    trust_btn.add_css_class("destructive-action");
    let hostkey_cancel_btn = Button::with_label("Cancel");
    let hostkey_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(6)
        .margin_end(6)
        .visible(false)
        .build();
    hostkey_bar.add_css_class("card");
    hostkey_bar.append(&hostkey_label);
    hostkey_bar.append(&trust_btn);
    hostkey_bar.append(&hostkey_cancel_btn);

    // Transfers panel ----------------------------------------------------------
    let transfers_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .build();
    let transfers_header = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .margin_start(6)
        .margin_end(6)
        .build();
    let transfers_title = Label::builder().label("Transfers").xalign(0.0).hexpand(true).build();
    transfers_title.add_css_class("heading");
    let clear_btn = Button::with_label("Clear finished");
    clear_btn.add_css_class("flat");
    let cancel_all_btn = Button::with_label("Cancel all");
    cancel_all_btn.add_css_class("flat");
    transfers_header.append(&transfers_title);
    transfers_header.append(&cancel_all_btn);
    transfers_header.append(&clear_btn);

    let transfers_scroll = ScrolledWindow::builder()
        .max_content_height(130)
        .propagate_natural_height(true)
        .child(&transfers_box)
        .build();
    let transfers_panel = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .build();
    transfers_panel.append(&transfers_header);
    transfers_panel.append(&gtk::Separator::new(Orientation::Horizontal));
    transfers_panel.append(&transfers_scroll);

    let transfers_window = gtk::Window::builder()
        .title("Transfer Queue")
        .transient_for(&window)
        .default_width(500)
        .default_height(260)
        .hide_on_close(true)
        .child(&transfers_panel)
        .build();
    transfers_window.set_visible(false);

    let status = Label::builder()
        .xalign(0.0)
        .label("Not connected")
        .margin_start(6)
        .margin_end(6)
        .margin_top(2)
        .margin_bottom(4)
        .build();

    // Bottom command bar -------------------------------------------------------
    let cmd_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .margin_start(6)
        .margin_end(6)
        .margin_top(3)
        .margin_bottom(3)
        .build();

    // Synchronize menu button
    let sync_menu = gio::Menu::new();
    sync_menu.append(Some("Local → Remote (upload changes)"), Some("win.sync-up"));
    sync_menu.append(Some("Remote → Local (download changes)"), Some("win.sync-down"));
    let sync_btn = gtk::MenuButton::builder()
        .label("Synchronize")
        .menu_model(&sync_menu)
        .build();
    sync_btn.set_tooltip_text(Some("Synchronize panes"));
    cmd_bar.append(&sync_btn);

    // Separator
    let sep1 = gtk::Separator::new(Orientation::Vertical);
    sep1.set_margin_start(4);
    sep1.set_margin_end(4);
    cmd_bar.append(&sep1);

    // Queue button
    let queue_btn = Button::with_label("Queue");
    queue_btn.set_tooltip_text(Some("Show/hide the transfer queue"));
    cmd_bar.append(&queue_btn);

    // Separator
    let sep2 = gtk::Separator::new(Orientation::Vertical);
    sep2.set_margin_start(4);
    sep2.set_margin_end(4);
    cmd_bar.append(&sep2);

    // Transfer Settings: speed-limit dropdown wired to the worker throttle.
    let speed_lbl = Label::builder().label("Speed:").xalign(0.0).build();
    speed_lbl.add_css_class("dim-label");
    cmd_bar.append(&speed_lbl);
    const SPEED_CHOICES: [(&str, u64); 5] = [
        ("Unlimited", 0),
        ("100 KiB/s", 100),
        ("500 KiB/s", 500),
        ("1 MiB/s", 1024),
        ("5 MiB/s", 5120),
    ];
    let speed_dd = DropDown::from_strings(
        &SPEED_CHOICES.iter().map(|(l, _)| *l).collect::<Vec<_>>());
    // Restore the persisted cap.
    let saved_kbs = load_column_widths().get("transfer.speed_kbs").copied().unwrap_or(0) as u64;
    worker::SPEED_LIMIT_KBS.store(saved_kbs, std::sync::atomic::Ordering::Relaxed);
    // Apply the configured keepalive interval to the worker threads.
    worker::KEEPALIVE_SECS.store(
        prefs::get_int("keepalive_secs", 30).max(5) as u64,
        std::sync::atomic::Ordering::Relaxed);
    scp_core::set_atomic_uploads(prefs::get_int("atomic_uploads", 1) != 0);
    if let Some(idx) = SPEED_CHOICES.iter().position(|(_, k)| *k == saved_kbs) {
        speed_dd.set_selected(idx as u32);
    }
    speed_dd.connect_selected_notify(move |dd| {
        let kbs = SPEED_CHOICES[dd.selected() as usize].1;
        worker::SPEED_LIMIT_KBS.store(kbs, std::sync::atomic::Ordering::Relaxed);
        save_column_width("transfer.speed_kbs", kbs as i32);
    });
    cmd_bar.append(&speed_dd);

    // Sites sidebar ------------------------------------------------------------
    // WinSCP behavior: single click selects/edits, double click logs in.
    let sites_list = ListBox::builder()
        .selection_mode(SelectionMode::Single)
        .activate_on_single_click(false)
        .build();
    sites_list.add_css_class("navigation-sidebar");
    let sites_header = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .margin_start(8)
        .margin_end(4)
        .margin_top(6)
        .margin_bottom(2)
        .build();
    let sites_title = Label::builder().label("Sites").xalign(0.0).hexpand(true).build();
    sites_title.add_css_class("heading");
    let save_site_btn = Button::from_icon_name("list-add-symbolic");
    save_site_btn.add_css_class("flat");
    save_site_btn.set_tooltip_text(Some("Save current connection"));
    sites_header.append(&sites_title);
    sites_header.append(&save_site_btn);

    let sidebar = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .width_request(200)
        .build();
    sidebar.append(&sites_header);
    sidebar.append(&ScrolledWindow::builder().vexpand(true).child(&sites_list).build());

    // Login dialog (WinSCP-style): sites left, session form right ---------------
    let login_window = gtk::Window::builder()
        .title("Login")
        .modal(true)
        .transient_for(&window)
        .default_width(740)
        .default_height(460)
        .hide_on_close(true)
        .build();

    let login_main = GtkBox::builder().orientation(Orientation::Horizontal).build();
    login_main.append(&sidebar);
    login_main.append(&gtk::Separator::new(Orientation::Vertical));
    login_main.append(&form);

    // WinSCP-style Tools dropdown (bottom-left of the Login dialog).
    let tools_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .build();
    let tools_popover = Popover::builder().child(&tools_box).has_arrow(true).build();
    let tools_btn = gtk::MenuButton::builder()
        .label("Tools")
        .popover(&tools_popover)
        .build();
    let import_btn = Button::with_label("Import sites…");
    import_btn.add_css_class("flat");
    let export_btn = Button::with_label("Export sites…");
    export_btn.add_css_class("flat");
    let winscp_btn = Button::with_label("Import from WinSCP INI…");
    winscp_btn.add_css_class("flat");
    let sshcfg_btn = Button::with_label("Import from ~/.ssh/config");
    sshcfg_btn.add_css_class("flat");
    let prefs_btn = Button::with_label("Preferences…");
    prefs_btn.add_css_class("flat");
    let knownhosts_btn = Button::with_label("Manage known hosts…");
    knownhosts_btn.add_css_class("flat");
    let customcmd_btn = Button::with_label("Custom commands…");
    customcmd_btn.add_css_class("flat");
    for b in [
        &import_btn, &winscp_btn, &sshcfg_btn, &export_btn,
        &customcmd_btn, &knownhosts_btn, &prefs_btn,
    ] {
        if let Some(child) = b.child().and_downcast::<Label>() {
            child.set_xalign(0.0);
        }
        tools_box.append(b);
    }

    let login_buttons = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .build();
    login_buttons.append(&tools_btn);
    let login_spacer = GtkBox::builder().hexpand(true).build();
    login_buttons.append(&login_spacer);
    login_buttons.append(&close_btn);
    login_buttons.append(&login_btn);

    let login_content = GtkBox::builder().orientation(Orientation::Vertical).build();
    login_content.append(&login_main);
    login_content.append(&hostkey_bar);
    login_content.append(&gtk::Separator::new(Orientation::Horizontal));
    login_content.append(&login_buttons);
    login_window.set_child(Some(&login_content));
    login_window.set_default_widget(Some(&login_btn));

    // Root layout ---------------------------------------------------------------
    let tabs_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(2)
        .margin_bottom(2)
        .build();

    // Menu bar (WinSCP layout: Left, Mark, Files, Commands, Tabs, Options, Right, Help)
    let menubar_model = gio::Menu::new();
    {
        let pane_menu = |prefix: &str| {
            let m = gio::Menu::new();
            m.append(Some("Go Up"), Some(&format!("win.{prefix}-up")));
            m.append(Some("Back"), Some(&format!("win.{prefix}-back")));
            m.append(Some("Forward"), Some(&format!("win.{prefix}-forward")));
            m.append(Some("Home"), Some(&format!("win.{prefix}-home")));
            m.append(Some("Refresh"), Some(&format!("win.{prefix}-refresh")));
            m
        };
        menubar_model.append_submenu(Some("Left"), &pane_menu("left"));

        let mark = gio::Menu::new();
        mark.append(Some("Select All"), Some("win.mark-all"));
        mark.append(Some("Unselect All"), Some("win.mark-none"));
        mark.append(Some("Invert Selection"), Some("win.mark-invert"));
        menubar_model.append_submenu(Some("Mark"), &mark);

        let files = gio::Menu::new();
        files.append(Some("Transfer (F5)"), Some("win.files-transfer"));
        files.append(Some("Move (F6)"), Some("win.files-move"));
        files.append(Some("Rename (F2)"), Some("win.files-rename"));
        files.append(Some("Edit"), Some("win.files-edit"));
        files.append(Some("Delete (F8)"), Some("win.files-delete"));
        files.append(Some("New Folder (F7)"), Some("win.files-newfolder"));
        files.append(Some("Properties (F9)"), Some("win.files-properties"));
        menubar_model.append_submenu(Some("Files"), &files);

        let commands = gio::Menu::new();
        commands.append(Some("Synchronize Local → Remote"), Some("win.sync-up"));
        commands.append(Some("Synchronize Remote → Local"), Some("win.sync-down"));
        commands.append(Some("Find Files…"), Some("win.cmd-find"));
        commands.append(Some("Execute Command…"), Some("win.cmd-exec"));
        commands.append(Some("Open Terminal"), Some("win.cmd-terminal"));
        commands.append(Some("Show Transfer Queue"), Some("win.cmd-queue"));
        commands.append(Some("Session Log"), Some("win.cmd-log"));
        menubar_model.append_submenu(Some("Commands"), &commands);

        let tabs = gio::Menu::new();
        tabs.append(Some("New Tab"), Some("win.tab-new"));
        tabs.append(Some("Close Tab"), Some("win.tab-close"));
        menubar_model.append_submenu(Some("Tabs"), &tabs);

        let options = gio::Menu::new();
        options.append(Some("Toggle Hidden Files"), Some("win.opt-hidden"));
        options.append(Some("Toggle Mirror Mode"), Some("win.opt-mirror"));
        options.append(Some("Toggle Synchronized Browsing"), Some("win.opt-syncbrowse"));
        menubar_model.append_submenu(Some("Options"), &options);

        menubar_model.append_submenu(Some("Right"), &pane_menu("right"));

        let help = gio::Menu::new();
        help.append(Some("SCP Commander Help"), Some("win.help-show"));
        menubar_model.append_submenu(Some("Help"), &help);
    }
    let menubar = gtk::PopoverMenuBar::from_model(Some(&menubar_model));

    let content = GtkBox::builder().orientation(Orientation::Vertical).build();
    content.append(&menubar);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&main_toolbar);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&tabs_box);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&panes);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&cmd_bar);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&status);

    let root = content;

    let state = Rc::new(App {
        window: window.clone(),
        login_window: login_window.clone(),
        sessions: RefCell::new(Vec::new()),
        active_tab: Cell::new(0),
        tabs_box,
        local: local_pane,
        remote: remote_pane,
        local_path: RefCell::new(glib::home_dir()),
        status,
        transfers_window,
        transfers_box,
        transfers_panel,
        transfer_rows: RefCell::new(HashMap::new()),
        next_id: RefCell::new(0),
        proto_dd,
        auth_dd,
        host_entry,
        port_entry,
        user_entry,
        pass_entry,
        remember_pw_check,
        key_entry,
        bucket_entry,
        region_entry,
        hostkey_bar,
        hostkey_label,
        pending_connect: RefCell::new(None),
        pending_fingerprint: RefCell::new(None),
        exclude_entry,
        show_hidden: Cell::new(false),
        mirror_sync: Cell::new(false),
        sync_browse: Cell::new(false),
        focused_local: Cell::new(true),
        type_buf: RefCell::new(String::new()),
        type_at: Cell::new(None),
        local_hist: RefCell::new((Vec::new(), Vec::new())),
        remote_hist: RefCell::new((Vec::new(), Vec::new())),
        remote_hist_suppress: Cell::new(false),
        pending_move: RefCell::new(HashMap::new()),
        local_menu_target: RefCell::new(None),
        remote_menu_target: RefCell::new(None),
        sites_menu_index: Cell::new(0),
        edit_pending: RefCell::new(HashMap::new()),
        edits: RefCell::new(Vec::new()),
        view_pending: RefCell::new(HashMap::new()),
        sites: RefCell::new(SitesStore::load()),
        sites_list,
        log_buf: RefCell::new(Vec::new()),
        quit_confirmed: Cell::new(false),
        local_monitor: RefCell::new(None),
        local_reload_pending: Cell::new(false),
        keep_pair: RefCell::new(None),
        keep_monitor: RefCell::new(None),
        keep_pending: Cell::new(false),
        me: RefCell::new(Weak::new()),
    });
    // Stash a weak self so &self methods can hand owned handles to callbacks.
    *state.me.borrow_mut() = Rc::downgrade(&state);

    // First session tab.
    let first = create_session(&state);
    state.sessions.borrow_mut().push(first);
    state.refresh_tabs();

    update_form();
    state.load_local();
    state.refresh_sites_list();
    state.restore_queue();

    // Context menus -------------------------------------------------------------
    setup_context_menu(&state, &local_view, &local_hook, true);
    setup_context_menu(&state, &remote_view, &remote_hook, false);

    // Wire signals ----------------------------------------------------------------
    login_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.connect_clicked()
    ));
    close_btn.connect_clicked(glib::clone!(
        #[strong] login_window,
        move |_| login_window.set_visible(false)
    ));
    new_session_btn.connect_clicked(glib::clone!(
        #[strong] login_window,
        move |_| login_window.present()
    ));
    import_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.import_sites();
        }
    ));
    export_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.export_sites();
        }
    ));
    winscp_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.import_winscp();
        }
    ));
    sshcfg_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.import_ssh_config();
        }
    ));
    prefs_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.preferences_dialog();
        }
    ));
    knownhosts_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.known_hosts_dialog();
        }
    ));
    customcmd_btn.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] tools_popover,
        move |_| {
            tools_popover.popdown();
            state.custom_commands_dialog();
        }
    ));
    sync_up_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.sync(false)
    ));
    sync_down_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.sync(true)
    ));
    keep_toggle.connect_toggled(glib::clone!(
        #[strong] state,
        move |btn| {
            let on = state.keep_pair.borrow().is_some();
            // Only act when the toggle's new state disagrees with reality —
            // toggle_keep flips it; sync the button back if it couldn't start.
            if btn.is_active() != on {
                state.toggle_keep_up_to_date();
                btn.set_active(state.keep_pair.borrow().is_some());
            }
        }
    ));
    find_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| {
            if !state.session().connected.get() {
                state.set_status("Connect first to search");
                return;
            }
            let st = state.clone();
            prompt(&state.window, "Find files (mask, e.g. *.log)", "*", move |mask| {
                if mask.is_empty() {
                    return;
                }
                let base = st.session().remote_path.borrow().clone();
                st.set_status(&format!("Searching {base} for {mask}..."));
                let _ = st.session().cmd.send(Cmd::Find { base, mask });
            });
        }
    ));
    terminal_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.open_terminal()
    ));
    exec_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.menu_exec_command()
    ));
    mirror_check.connect_toggled(glib::clone!(
        #[strong] state,
        move |btn| state.mirror_sync.set(btn.is_active())
    ));
    help_btn.connect_clicked(glib::clone!(
        #[strong] window,
        move |_| show_help_dialog(&window)
    ));
    cancel_all_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.cancel_all()
    ));
    queue_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.transfers_window.present()
    ));
    // Window actions: bottom command bar + menu bar
    {
        let act = |name: &str, f: Box<dyn Fn() + 'static>| {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(move |_, _| f());
            window.add_action(&a);
        };
        macro_rules! action {
            ($name:expr, $state:ident, $body:expr) => {{
                let $state = state.clone();
                act($name, Box::new(move || $body));
            }};
        }

        action!("sync-up", st, st.sync(false));
        action!("sync-down", st, st.sync(true));

        // Left / Right pane navigation
        action!("left-up", st, st.local_up());
        action!("left-back", st, st.go_back_local());
        action!("left-forward", st, st.go_forward_local());
        action!("left-home", st, st.go_home_local());
        action!("left-refresh", st, st.load_local());
        action!("right-up", st, st.remote_up());
        action!("right-back", st, st.go_back_remote());
        action!("right-forward", st, st.go_forward_remote());
        action!("right-home", st, st.go_home_remote());
        action!("right-refresh", st, st.refresh_remote());

        // Mark
        action!("mark-all", st, st.mark_select_all());
        action!("mark-none", st, st.mark_unselect_all());
        action!("mark-invert", st, st.mark_invert());

        // Files (act on the focused pane's selection)
        action!("files-transfer", st, st.transfer_selected());
        action!("files-move", st, st.move_selected());
        action!("files-rename", st, {
            let local = st.focused_local.get();
            if st.select_for_menu(local) { st.menu_rename(local); }
        });
        action!("files-edit", st, {
            if st.select_for_menu(false) { st.menu_edit(); }
        });
        action!("files-delete", st, {
            let local = st.focused_local.get();
            if st.select_for_menu(local) { st.menu_delete(local); }
        });
        action!("files-newfolder", st, st.new_folder(st.focused_local.get()));
        action!("files-properties", st, {
            let local = st.focused_local.get();
            if st.select_for_menu(local) { st.menu_properties(local); }
        });

        // Commands
        action!("cmd-exec", st, st.menu_exec_command());
        action!("cmd-terminal", st, st.open_terminal());
        action!("cmd-queue", st, st.transfers_window.present());
        action!("cmd-log", st, session_log_dialog(&st));
        {
            let find_btn = find_btn.clone();
            act("cmd-find", Box::new(move || find_btn.emit_clicked()));
        }

        // Tabs
        action!("tab-new", st, st.new_tab());
        action!("tab-close", st, st.close_tab(st.active_tab.get()));

        // Options: flip the existing toggle widgets so their handlers run.
        {
            let hidden_btn = hidden_btn.clone();
            act("opt-hidden", Box::new(move || hidden_btn.set_active(!hidden_btn.is_active())));
        }
        {
            let mirror_check = mirror_check.clone();
            act("opt-mirror", Box::new(move || mirror_check.set_active(!mirror_check.is_active())));
        }
        action!("opt-syncbrowse", st, {
            let on = !st.sync_browse.get();
            st.sync_browse.set(on);
            st.set_status(if on {
                "Synchronized browsing on"
            } else {
                "Synchronized browsing off"
            });
        });

        // Help
        {
            let window = window.clone();
            act("help-show", Box::new(move || show_help_dialog(&window)));
        }
    }
    clear_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.clear_finished()
    ));
    trust_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.trust_host_key()
    ));
    hostkey_cancel_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| {
            state.hostkey_bar.set_visible(false);
            *state.pending_fingerprint.borrow_mut() = None;
            state.set_status("Connection cancelled");
        }
    ));
    save_site_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.begin_save_site()
    ));
    // Single click: edit (fill the form). Double click / Enter: login.
    state.sites_list.connect_row_selected(glib::clone!(
        #[strong] state,
        move |_, row| {
            if let Some(row) = row {
                state.load_site(row.index().max(0) as usize);
            }
        }
    ));
    state.sites_list.connect_row_activated(glib::clone!(
        #[strong] state,
        move |_, row| state.login_site(row.index().max(0) as usize)
    ));

    // Folder headers, WinSCP-style ("Work/web1" gets a "Work" header).
    state.sites_list.set_header_func(glib::clone!(
        #[strong] state,
        move |row, before| {
            let sites = &state.sites.borrow().sites;
            let folder = sites
                .get(row.index().max(0) as usize)
                .and_then(|s| s.folder().map(str::to_string));
            let prev_folder = before
                .and_then(|b| sites.get(b.index().max(0) as usize))
                .and_then(|s| s.folder().map(str::to_string));
            if folder.is_some() && folder != prev_folder {
                let label = Label::builder()
                    .label(folder.as_deref().unwrap_or_default())
                    .xalign(0.0)
                    .margin_start(6)
                    .margin_top(6)
                    .build();
                label.add_css_class("heading");
                label.add_css_class("dim-label");
                row.set_header(Some(&label));
            } else {
                row.set_header(gtk::Widget::NONE);
            }
        }
    ));

    // Right-click context menu: Login / Edit / Rename / Delete.
    let sites_menu_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .build();
    let sites_popover = Popover::builder().child(&sites_menu_box).has_arrow(false).build();
    sites_popover.set_parent(&state.sites_list);
    let add_site_item = |label: &str, destructive: bool, action: Box<dyn Fn(usize)>| {
        let btn = Button::with_label(label);
        btn.add_css_class("flat");
        if destructive {
            btn.add_css_class("destructive-action");
        }
        if let Some(child) = btn.child().and_downcast::<Label>() {
            child.set_xalign(0.0);
        }
        let pop = sites_popover.clone();
        let s = state.clone();
        btn.connect_clicked(move |_| {
            pop.popdown();
            action(s.sites_menu_index.get());
        });
        sites_menu_box.append(&btn);
    };
    {
        let s = state.clone();
        add_site_item("Login", false, Box::new(move |i| s.login_site(i)));
    }
    {
        let s = state.clone();
        add_site_item("Edit", false, Box::new(move |i| s.load_site(i)));
    }
    {
        let s = state.clone();
        add_site_item("Rename…", false, Box::new(move |i| s.rename_site(i)));
    }
    {
        let s = state.clone();
        add_site_item("Delete", true, Box::new(move |i| s.delete_site(i)));
    }
    let sites_click = gtk::GestureClick::builder().button(3).build();
    sites_click.connect_pressed(glib::clone!(
        #[strong] state,
        #[strong] sites_popover,
        move |gesture, _, x, y| {
            let list = gesture.widget().and_downcast::<ListBox>();
            if let Some(row) = list.and_then(|l| l.row_at_y(y as i32)) {
                state.sites_menu_index.set(row.index().max(0) as usize);
                sites_popover
                    .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                sites_popover.popup();
            }
        }
    ));
    state.sites_list.add_controller(sites_click);

    key_browse.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| {
            let dialog = gtk::FileDialog::new();
            let key_entry = state.key_entry.clone();
            dialog.open(
                Some(&state.window),
                gio::Cancellable::NONE,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            key_entry.set_text(&path.to_string_lossy());
                        }
                    }
                },
            );
        }
    ));

    local_view.connect_activate(glib::clone!(
        #[strong] state,
        move |_, position| state.open_local(position)
    ));
    remote_view.connect_activate(glib::clone!(
        #[strong] state,
        move |_, position| state.open_remote(position)
    ));
    build_pane_toolbar(&state, &local_header, true);
    build_pane_toolbar(&state, &remote_header, false);

    // Editable path bars (type a path, press Enter).
    state.local.path_entry.connect_activate(glib::clone!(
        #[strong] state,
        move |entry| state.navigate_local(&entry.text())
    ));
    state.remote.path_entry.connect_activate(glib::clone!(
        #[strong] state,
        move |entry| state.navigate_remote(&entry.text())
    ));

    // Hidden-file toggle re-renders both panes from their full caches.
    hidden_btn.connect_toggled(glib::clone!(
        #[strong] state,
        move |btn| {
            state.show_hidden.set(btn.is_active());
            state.load_local();
            let session = state.session();
            let path = session.remote_path.borrow().clone();
            let cache = session.cache.borrow().clone();
            state.remote.show(&cache, &path, state.show_hidden.get());
        }
    ));

    // Type-ahead: letters jump to the first matching row in the pane.
    for (view, is_local) in [(&local_view, true), (&remote_view, false)] {
        let key_ctl = gtk::EventControllerKey::new();
        key_ctl.connect_key_pressed(glib::clone!(
            #[strong] state,
            move |_, keyval, _, modifier| {
                if modifier.contains(gdk::ModifierType::CONTROL_MASK)
                    || modifier.contains(gdk::ModifierType::ALT_MASK)
                {
                    return glib::Propagation::Proceed;
                }
                if let Some(c) = keyval.to_unicode() {
                    if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                        state.type_ahead(is_local, c);
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
        ));
        view.add_controller(key_ctl);
    }

    // Click-to-focus for the keyboard commander.
    for (view, is_local) in [(&local_view, true), (&remote_view, false)] {
        let focus_click = gtk::GestureClick::new();
        focus_click.connect_pressed(glib::clone!(
            #[strong] state,
            move |_, _, _, _| state.set_focus(is_local)
        ));
        view.add_controller(focus_click);
    }

    // Keyboard commander: F5 copy, F6 move, F2 rename, F3 view, Del, Backspace, Tab.
    let local_view_for_keys = local_view.clone();
    let remote_view_for_keys = remote_view.clone();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[strong] state,
        move |_, key, _, _| {
            // Don't steal keys while typing in an entry.
            let editing_text = gtk::prelude::RootExt::focus(&state.window)
                .is_some_and(|w| {
                    w.is::<gtk::Text>() || w.is::<GtkEntry>() || w.is::<PasswordEntry>()
                });
            if editing_text {
                return glib::Propagation::Proceed;
            }
            let local = state.focused_local.get();
            match key {
                gdk::Key::F5 => {
                    state.transfer_selected();
                    glib::Propagation::Stop
                }
                gdk::Key::F6 => {
                    state.move_selected();
                    glib::Propagation::Stop
                }
                gdk::Key::F2 => {
                    if state.select_for_menu(local) {
                        state.menu_rename(local);
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::F3 => {
                    if state.select_for_menu(local) {
                        state.menu_view(local);
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Delete => {
                    if state.select_for_menu(local) {
                        state.menu_delete(local);
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::BackSpace => {
                    if local {
                        state.local_up();
                    } else {
                        state.remote_up();
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    state.set_focus(!local);
                    // Move real keyboard focus too, so arrows/Enter and the
                    // visible selection follow the commander's focused pane.
                    if local {
                        remote_view_for_keys.grab_focus();
                    } else {
                        local_view_for_keys.grab_focus();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    window.add_controller(keys);

    // Drag and drop between panes -----------------------------------------------
    add_drag_source(&local_view, "local", &state.local);
    add_drag_source(&remote_view, "remote", &state.remote);

    let local_drop = DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
    local_drop.connect_drop(glib::clone!(
        #[strong] state,
        move |_, value, _, _| {
            if let Some(names) = value
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("remote:").map(str::to_string))
            {
                let mut any = false;
                for name in names.lines() {
                    if let Some(entry) =
                        state.remote.entries.borrow().iter().find(|e| e.name == name).cloned()
                    {
                        state.download(&entry);
                        any = true;
                    }
                }
                return any;
            }
            false
        }
    ));
    local_view.add_controller(local_drop);

    let remote_drop = DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
    remote_drop.connect_drop(glib::clone!(
        #[strong] state,
        move |_, value, _, _| {
            if let Some(names) = value
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("local:").map(str::to_string))
            {
                let mut any = false;
                for name in names.lines() {
                    if let Some(entry) =
                        state.local.entries.borrow().iter().find(|e| e.name == name).cloned()
                    {
                        state.upload(&entry);
                        any = true;
                    }
                }
                return any;
            }
            false
        }
    ));
    remote_view.add_controller(remote_drop);

    // Files dropped from a file manager (Nautilus/Finder) upload directly.
    let uri_drop = DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    uri_drop.connect_drop(glib::clone!(
        #[strong] state,
        move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else { return false };
            if !state.session().connected.get() {
                state.set_status("Connect first to upload");
                return false;
            }
            let mut any = false;
            for file in files.files() {
                let Some(path) = file.path() else { continue };
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned())
                else {
                    continue;
                };
                let remote = join_posix(&state.session().remote_path.borrow(), &name);
                if path.is_dir() {
                    let (id, cancel, pause) = state.add_transfer(
                        &format!("{name}/"), false, 0, &path.display().to_string(), &remote);
                    let _ = state.session().xfer_pool.send(Cmd::UploadDir {
                        id,
                        name,
                        local: path,
                        remote,
                        excludes: state.exclude_masks(),
                        overwrite: 0,
                        cancel,
                        pause,
                    });
                } else {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let (id, cancel, pause) = state.add_transfer(
                        &name, false, size, &path.display().to_string(), &remote);
                    let _ = state.session().xfer_pool.send(Cmd::Upload {
                        id,
                        name,
                        local: path,
                        remote,
                        resume: false,
                        cancel,
                        pause,
                    });
                }
                any = true;
            }
            any
        }
    ));
    remote_view.add_controller(uri_drop);

    // Edit-in-editor mtime polling -------------------------------------------------
    glib::timeout_add_seconds_local(
        2,
        glib::clone!(
            #[strong] state,
            move || {
                state.poll_edits();
                glib::ControlFlow::Continue
            }
        ),
    );

    // Save the workspace (open tabs + paths) when the window closes.
    window.connect_close_request(glib::clone!(
        #[strong] state,
        move |win| {
            // Re-offer whatever didn't finish (failed rows too) next launch.
            state.save_queue();
            // Quit guard: don't silently kill running transfers.
            let active = state.active_transfers();
            if active > 0 && !state.quit_confirmed.get() {
                let msg = if active == 1 {
                    "1 transfer is still running".to_string()
                } else {
                    format!("{active} transfers are still running")
                };
                let detail = if active == 1 {
                    "Quitting now will cancel it."
                } else {
                    "Quitting now will cancel them."
                };
                let dialog = gtk::AlertDialog::builder()
                    .message(msg)
                    .detail(detail)
                    .buttons(["Keep Transferring", "Quit Anyway"])
                    .cancel_button(0)
                    .default_button(0)
                    .build();
                dialog.choose(Some(win), gio::Cancellable::NONE, glib::clone!(
                    #[strong] state,
                    move |result| {
                        if result == Ok(1) {
                            state.cancel_all();
                            state.quit_confirmed.set(true);
                            state.window.close();
                        }
                    }
                ));
                return glib::Propagation::Stop;
            }
            state.save_workspace();
            glib::Propagation::Proceed
        }
    ));

    window.set_child(Some(&root));
    window.present();

    // Restore last session's tabs; fall back to the Login dialog.
    if !state.restore_workspace() {
        login_window.present();
    }

    // Connect to a URL: the `open` signal's URI takes priority, else scan argv.
    let url_args: Vec<String> = open_uri
        .map(str::to_string)
        .into_iter()
        .chain(std::env::args().skip(1))
        .collect();
    for arg in url_args {
        if let Some(rest) = arg.strip_prefix("sftp://").map(|r| ("sftp", r))
            .or_else(|| arg.strip_prefix("ftps://").map(|r| ("ftps", r)))
            .or_else(|| arg.strip_prefix("ftp://").map(|r| ("ftp", r)))
            .or_else(|| arg.strip_prefix("s3://").map(|r| ("s3", r)))
        {
            let (scheme, authority) = rest;
            let proto_idx: u32 = match scheme {
                "sftp" => 0,
                "ftp" => 1,
                "ftps" => 2,
                "s3" => 3,
                _ => 0,
            };
            state.proto_dd.set_selected(proto_idx);
            // authority is [user[:pass]@]host[:port][/path]
            let authority = authority.split('/').next().unwrap_or("");
            let (userinfo, hostport) = if let Some(at) = authority.rfind('@') {
                (&authority[..at], &authority[at + 1..])
            } else {
                ("", authority)
            };
            let (host, port) = if let Some(c) = hostport.rfind(':') {
                (&hostport[..c], &hostport[c + 1..])
            } else {
                (hostport, "")
            };
            let (user, pass) = if let Some(c) = userinfo.find(':') {
                (&userinfo[..c], &userinfo[c + 1..])
            } else {
                (userinfo, "")
            };
            state.host_entry.set_text(host);
            if !port.is_empty() { state.port_entry.set_text(port); }
            if !user.is_empty() { state.user_entry.set_text(user); }
            if !pass.is_empty() { state.pass_entry.set_text(pass); }
            state.connect_clicked();
            break;
        }
    }
}

/// WinSCP-style per-pane command toolbar appended to the pane header:
/// up · refresh · new folder · transfer · edit (remote) · delete.
fn build_pane_toolbar(state: &Rc<App>, header: &GtkBox, local_pane: bool) {
    let tool = |icon: &str, tip: &str| {
        let b = Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_tooltip_text(Some(tip));
        header.append(&b);
        b
    };

    let back = tool("go-previous-symbolic", "Back");
    back.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.go_back_local() } else { state.go_back_remote() }
    ));

    let forward = tool("go-next-symbolic", "Forward");
    forward.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.go_forward_local() } else { state.go_forward_remote() }
    ));

    let up = tool("go-up-symbolic", "Parent directory");
    up.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.local_up() } else { state.remote_up() }
    ));

    let home = tool("go-home-symbolic", "Home directory");
    home.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.go_home_local() } else { state.go_home_remote() }
    ));

    let refresh = tool("view-refresh-symbolic", "Refresh");
    refresh.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.load_local() } else { state.refresh_remote() }
    ));

    // Directory bookmarks: star popover rebuilt each time it opens.
    let bm_box = GtkBox::builder().orientation(Orientation::Vertical).spacing(2).build();
    let bm_pop = Popover::builder().child(&bm_box).build();
    let bm_btn = gtk::MenuButton::builder()
        .icon_name("starred-symbolic")
        .popover(&bm_pop)
        .build();
    bm_btn.add_css_class("flat");
    bm_btn.set_tooltip_text(Some("Directory bookmarks"));
    header.append(&bm_btn);
    let bm_kind: &'static str = if local_pane { "local" } else { "remote" };
    bm_pop.connect_show(glib::clone!(
        #[strong] state,
        #[strong] bm_box,
        #[strong] bm_pop,
        move |_| {
            while let Some(child) = bm_box.first_child() {
                bm_box.remove(&child);
            }
            let current = if local_pane {
                state.local_path.borrow().display().to_string()
            } else {
                state.session().remote_path.borrow().clone()
            };
            let bookmarks = load_bookmarks(bm_kind);
            for b in &bookmarks {
                let btn = Button::with_label(b);
                btn.add_css_class("flat");
                if let Some(child) = btn.child().and_downcast::<Label>() {
                    child.set_xalign(0.0);
                }
                let target = b.clone();
                btn.connect_clicked(glib::clone!(
                    #[strong] state,
                    #[strong] bm_pop,
                    move |_| {
                        bm_pop.popdown();
                        if local_pane {
                            state.navigate_local(&target);
                        } else {
                            state.navigate_remote(&target);
                        }
                    }
                ));
                bm_box.append(&btn);
            }
            if !bookmarks.is_empty() {
                bm_box.append(&gtk::Separator::new(Orientation::Horizontal));
            }
            let toggle = Button::with_label(if bookmarks.contains(&current) {
                "Remove bookmark"
            } else {
                "Bookmark this directory"
            });
            toggle.add_css_class("flat");
            toggle.connect_clicked(glib::clone!(
                #[strong] bm_pop,
                move |_| {
                    bm_pop.popdown();
                    let mut list = load_bookmarks(bm_kind);
                    if let Some(pos) = list.iter().position(|b| *b == current) {
                        list.remove(pos);
                    } else {
                        list.push(current.clone());
                    }
                    save_bookmarks(bm_kind, &list);
                }
            ));
            bm_box.append(&toggle);
        }
    ));

    let newf = tool("folder-new-symbolic", "New folder");
    newf.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.new_folder(local_pane)
    ));

    header.append(&gtk::Separator::new(Orientation::Vertical));

    let transfer = tool(
        if local_pane { "send-to-symbolic" } else { "document-save-symbolic" },
        if local_pane { "Upload" } else { "Download" },
    );
    transfer.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| {
            if state.select_for_menu(local_pane) {
                state.menu_transfer(local_pane);
            }
        }
    ));

    if !local_pane {
        let edit = tool("document-edit-symbolic", "Edit (auto-upload on save)");
        edit.connect_clicked(glib::clone!(
            #[strong] state,
            move |_| {
                if state.select_for_menu(false) {
                    state.menu_edit();
                }
            }
        ));
    }

    let del = tool("user-trash-symbolic", "Delete");
    del.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| {
            if state.select_for_menu(local_pane) {
                state.menu_delete(local_pane);
            }
        }
    ));

    let props = tool("document-properties-symbolic", "Properties");
    props.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| {
            if state.select_for_menu(local_pane) {
                state.menu_properties(local_pane);
            }
        }
    ));
}

/// Attach the right-click menu gesture to a row cell widget.
fn add_menu_gesture(widget: &impl IsA<gtk::Widget>, item: &ListItem, view: &ColumnView, hook: &MenuHook) {
    let gesture = gtk::GestureClick::builder().button(3).build();
    let widget_c = widget.clone().upcast::<gtk::Widget>();
    gesture.connect_pressed(glib::clone!(
        #[strong] hook,
        #[weak] view,
        #[weak] item,
        #[weak] widget_c,
        move |_, _, x, y| {
            let point = widget_c.compute_point(
                &view,
                &gtk::graphene::Point::new(x as f32, y as f32),
            );
            if let (Some(p), Some(cb)) = (point, hook.borrow().as_ref()) {
                cb(item.position(), p.x() as f64, p.y() as f64);
            }
        }
    ));
    widget.add_controller(gesture);
}

/// A text column whose cell content is rendered from the row's Entry.
fn text_column(
    title: &str,
    xalign: f32,
    view: &ColumnView,
    hook: &MenuHook,
    render: Rc<dyn Fn(&Entry) -> String>,
) -> ColumnViewColumn {
    let factory = SignalListItemFactory::new();
    factory.connect_setup(glib::clone!(
        #[strong] hook,
        #[weak] view,
        move |_, item| {
            let item = item.downcast_ref::<ListItem>().unwrap().clone();
            let label = Label::builder().xalign(xalign).build();
            label.add_css_class("numeric");
            item.set_child(Some(&label));
            add_menu_gesture(&label, &item, &view, &hook);
        }
    ));
    factory.connect_bind(glib::clone!(
        #[strong] render,
        move |_, item| {
            let item = item.downcast_ref::<ListItem>().unwrap();
            let label = item.child().and_downcast::<Label>().unwrap();
            let text = item
                .item()
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|o| render(&o.borrow::<Entry>()))
                .unwrap_or_default();
            label.set_text(&text);
        }
    ));
    let col = ColumnViewColumn::new(Some(title), Some(factory));
    col.set_resizable(true); // drag the header divider to resize, WinSCP-style
    col
}

/// First 256 KB of a file as text, or a placeholder for binary/unreadable.
fn read_preview(path: &Path) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return "(unreadable)".into() };
    let mut buf = vec![0u8; 256 * 1024];
    let n = f.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8(buf)
        .unwrap_or_else(|e| format!("(binary file — {} bytes; no text preview)", e.as_bytes().len()))
}

/// Read-only text viewer (F3) — WinSCP's internal viewer.
fn viewer_dialog(parent: &ApplicationWindow, name: &str, text: &str) {
    let win = gtk::Window::builder()
        .title(format!("View — {name}"))
        .transient_for(parent)
        .default_width(700)
        .default_height(520)
        .build();
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .left_margin(8)
        .right_margin(8)
        .top_margin(6)
        .build();
    view.buffer().set_text(text);
    let scroll = ScrolledWindow::builder().vexpand(true).child(&view).build();
    win.set_child(Some(&scroll));
    win.present();
}

/// WinSCP-style session log: every status line, timestamped, in a window.
fn session_log_dialog(state: &Rc<App>) {
    let win = gtk::Window::builder()
        .title("Session Log")
        .transient_for(&state.window)
        .default_width(560)
        .default_height(320)
        .build();
    let text = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .left_margin(8)
        .right_margin(8)
        .top_margin(6)
        .build();
    text.buffer().set_text(&state.log_buf.borrow().join("\n"));
    let scroll = ScrolledWindow::builder().vexpand(true).child(&text).build();
    // Scroll to the newest line.
    let mut end = text.buffer().end_iter();
    text.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);

    let clear = Button::with_label("Clear");
    clear.connect_clicked(glib::clone!(
        #[strong] state,
        #[strong] text,
        move |_| {
            state.log_buf.borrow_mut().clear();
            text.buffer().set_text("");
        }
    ));
    let btns = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .halign(gtk::Align::End)
        .margin_top(6)
        .margin_bottom(6)
        .margin_end(8)
        .build();
    btns.append(&clear);

    let vbox = GtkBox::builder().orientation(Orientation::Vertical).build();
    vbox.append(&scroll);
    vbox.append(&btns);
    win.set_child(Some(&vbox));
    win.present();
}

/// Bookmark store: one "kind\tpath" line per bookmark under the config dir.
fn bookmarks_conf_path() -> PathBuf {
    let dir = glib::user_config_dir().join("scp-commander");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("bookmarks.conf")
}

fn load_bookmarks(kind: &str) -> Vec<String> {
    std::fs::read_to_string(bookmarks_conf_path())
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let (k, v) = l.split_once('\t')?;
                    (k == kind).then(|| v.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn save_bookmarks(kind: &str, list: &[String]) {
    let other: Vec<String> = std::fs::read_to_string(bookmarks_conf_path())
        .map(|s| {
            s.lines()
                .filter(|l| l.split_once('\t').map(|(k, _)| k != kind).unwrap_or(false))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut lines = other;
    lines.extend(list.iter().map(|b| format!("{kind}\t{b}")));
    let _ = std::fs::write(bookmarks_conf_path(), lines.join("\n") + "\n");
}

/// Column-width store: plain "pane.column=px" lines under the user config dir.
fn columns_conf_path() -> PathBuf {
    let dir = glib::user_config_dir().join("scp-commander");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("columns.conf")
}

fn load_column_widths() -> HashMap<String, i32> {
    std::fs::read_to_string(columns_conf_path())
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    Some((k.trim().to_string(), v.trim().parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn save_column_width(key: &str, width: i32) {
    let mut all = load_column_widths();
    if all.get(key) == Some(&width) {
        return; // resize notifications fire continuously during a drag
    }
    all.insert(key.to_string(), width);
    let mut lines: Vec<String> = all.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    let _ = std::fs::write(columns_conf_path(), lines.join("\n") + "\n");
}

/// Build a titled pane with WinSCP-style columns (Name | Size | Changed
/// [| Rights]), a header (title + action buttons appended later by build_ui),
/// and a path label. Rows get a right-click gesture firing the pane's
/// MenuHook with (row index, x, y) in view coordinates.
fn make_pane(
    title: &str,
    hook: &MenuHook,
    show_rights: bool,
) -> (GtkBox, Pane, ColumnView, GtkBox) {
    let model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = MultiSelection::new(Some(model.clone()));
    let view = ColumnView::new(Some(selection.clone()));
    view.add_css_class("data-table");

    // Name column: icon + label.
    let name_factory = SignalListItemFactory::new();
    name_factory.connect_setup(glib::clone!(
        #[strong] hook,
        #[weak] view,
        move |_, item| {
            let item = item.downcast_ref::<ListItem>().unwrap().clone();
            let row = GtkBox::builder()
                .orientation(Orientation::Horizontal)
                .spacing(6)
                .build();
            let icon = gtk::Image::new();
            let label = Label::builder().xalign(0.0).build();
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&icon);
            row.append(&label);
            item.set_child(Some(&row));
            add_menu_gesture(&row, &item, &view, &hook);
        }
    ));
    name_factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<ListItem>().unwrap();
        let row = item.child().and_downcast::<GtkBox>().unwrap();
        let icon = row.first_child().and_downcast::<gtk::Image>().unwrap();
        let label = row.last_child().and_downcast::<Label>().unwrap();
        if let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() {
            let entry = obj.borrow::<Entry>();
            icon.set_icon_name(Some(if entry.is_symlink {
                "emblem-symbolic-link"
            } else if entry.is_dir {
                "folder-symbolic"
            } else {
                "text-x-generic-symbolic"
            }));
            label.set_text(&entry.name);
        }
    });
    let name_col = ColumnViewColumn::new(Some("Name"), Some(name_factory));
    name_col.set_expand(true);
    name_col.set_resizable(true);
    view.append_column(&name_col);

    let size_col = text_column(
        "Size",
        1.0,
        &view,
        hook,
        Rc::new(|e: &Entry| if e.is_dir { String::new() } else { human_size(e.size) }),
    );
    size_col.set_fixed_width(85);
    view.append_column(&size_col);

    let type_col = text_column(
        "Type",
        0.0,
        &view,
        hook,
        Rc::new(|e: &Entry| type_description(e)),
    );
    type_col.set_fixed_width(130);
    view.append_column(&type_col);

    let changed_col = text_column(
        "Changed",
        0.0,
        &view,
        hook,
        Rc::new(|e: &Entry| {
            e.mtime
                .and_then(|m| glib::DateTime::from_unix_local(m).ok())
                .and_then(|dt| dt.format("%d.%m.%Y %H:%M").ok())
                .map(|s| s.to_string())
                .unwrap_or_default()
        }),
    );
    changed_col.set_fixed_width(130);
    view.append_column(&changed_col);

    let mut cols: Vec<(&'static str, ColumnViewColumn)> = vec![
        ("name", name_col.clone()),
        ("size", size_col.clone()),
        ("type", type_col.clone()),
        ("changed", changed_col.clone()),
    ];

    if show_rights {
        let owner_col = text_column(
            "Owner",
            1.0,
            &view,
            hook,
            Rc::new(|e: &Entry| e.uid.map(|u| u.to_string()).unwrap_or_default()),
        );
        owner_col.set_fixed_width(54);
        view.append_column(&owner_col);

        let group_col = text_column(
            "Group",
            1.0,
            &view,
            hook,
            Rc::new(|e: &Entry| e.gid.map(|g| g.to_string()).unwrap_or_default()),
        );
        group_col.set_fixed_width(54);
        view.append_column(&group_col);

        let rights_col = text_column(
            "Rights",
            0.0,
            &view,
            hook,
            Rc::new(|e: &Entry| e.perms.clone().unwrap_or_default()),
        );
        rights_col.set_fixed_width(95);
        view.append_column(&rights_col);

        cols.push(("owner", owner_col));
        cols.push(("group", group_col));
        cols.push(("rights", rights_col));
    }

    // Restore saved widths and persist any drag-to-resize (WinSCP remembers
    // column layout per pane).
    {
        let kind = title.to_lowercase();
        let saved = load_column_widths();
        for (col_name, col) in &cols {
            let key = format!("{kind}.{col_name}");
            if let Some(w) = saved.get(&key) {
                if *w >= 40 {
                    col.set_fixed_width(*w);
                }
            }
            col.connect_fixed_width_notify(move |c| {
                let w = c.fixed_width();
                if w >= 40 {
                    save_column_width(&key, w);
                }
            });
        }
    }

    view.set_single_click_activate(false);

    // ── Row 1: title + action buttons (built_ui appends buttons to `header`) ─
    let header = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(2)
        .margin_top(2)
        .margin_start(4)
        .margin_end(4)
        .build();
    let title_label = Label::builder().label(title).xalign(0.0).build();
    title_label.add_css_class("heading");
    title_label.set_margin_end(6);
    header.append(&title_label);

    // ── Row 2: WinSCP-style address bar ──────────────────────────────────────
    let path_entry = GtkEntry::builder().hexpand(true).build();
    path_entry.add_css_class("monospace");

    let folder_icon = gtk::Image::builder()
        .icon_name("folder-symbolic")
        .icon_size(gtk::IconSize::Normal)
        .build();
    folder_icon.add_css_class("dim-label");

    let addr_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .margin_top(2)
        .margin_bottom(2)
        .margin_start(4)
        .margin_end(4)
        .build();
    addr_bar.append(&folder_icon);
    addr_bar.append(&path_entry);
    addr_bar.add_css_class("card");

    let scroller = ScrolledWindow::builder().vexpand(true).child(&view).build();

    // WinSCP-style status line under the list: item count or selection size.
    let info_label = Label::builder().xalign(0.0).label("0 items").build();
    info_label.add_css_class("caption");
    info_label.add_css_class("dim-label");
    info_label.set_margin_start(6);
    info_label.set_margin_top(2);
    info_label.set_margin_bottom(2);
    info_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    selection.connect_selection_changed(glib::clone!(
        #[weak] model,
        #[weak] info_label,
        move |sel, _, _| info_label.set_text(&pane_summary(sel, &model)),
    ));
    model.connect_items_changed(glib::clone!(
        #[weak] selection,
        #[weak] info_label,
        move |model, _, _, _| info_label.set_text(&pane_summary(&selection, model)),
    ));

    let pane_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .build();
    pane_box.append(&header);
    pane_box.append(&addr_bar);
    pane_box.append(&scroller);
    pane_box.append(&info_label);

    let pane = Pane {
        model,
        selection,
        entries: Rc::new(RefCell::new(Vec::new())),
        path_entry,
        info_label,
    };
    (pane_box, pane, view, header)
}

/// Status-line text: total item count, or the count + total size of the
/// current selection.
fn pane_summary(selection: &MultiSelection, model: &gio::ListStore) -> String {
    use gtk::prelude::SelectionModelExt;
    let total = model.n_items();
    let bits = selection.selection();
    let n_sel = bits.size();
    if n_sel == 0 {
        return format!("{total} item{}", if total == 1 { "" } else { "s" });
    }
    let mut bytes = 0u64;
    for i in 0..n_sel as u32 {
        let pos = bits.nth(i);
        if let Some(obj) = model.item(pos).and_downcast::<glib::BoxedAnyObject>() {
            let entry = obj.borrow::<Entry>();
            if !entry.is_dir {
                bytes += entry.size;
            }
        }
    }
    if bytes > 0 {
        format!("{n_sel} of {total} selected · {}", human_size(bytes))
    } else {
        format!("{n_sel} of {total} selected")
    }
}

/// Wire a pane's right-click context menu: a Popover of action buttons
/// anchored at the click position, acting on the clicked row.
fn setup_context_menu(state: &Rc<App>, view: &ColumnView, hook: &MenuHook, local_pane: bool) {
    let menu_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .build();
    let popover = Popover::builder().child(&menu_box).has_arrow(false).build();
    popover.set_parent(view);

    let add_item = |label: &str, destructive: bool, action: Box<dyn Fn()>| {
        let btn = Button::with_label(label);
        btn.add_css_class("flat");
        if destructive {
            btn.add_css_class("destructive-action");
        }
        if let Some(child) = btn.child().and_downcast::<Label>() {
            child.set_xalign(0.0);
        }
        let pop = popover.clone();
        btn.connect_clicked(move |_| {
            pop.popdown();
            action();
        });
        menu_box.append(&btn);
    };

    {
        let s = state.clone();
        add_item("Open", false, Box::new(move || s.menu_open(local_pane)));
    }
    {
        let s = state.clone();
        let label = if local_pane { "Upload →" } else { "← Download" };
        add_item(label, false, Box::new(move || s.menu_transfer(local_pane)));
    }
    {
        let s = state.clone();
        add_item("View (F3)", false, Box::new(move || s.menu_view(local_pane)));
    }
    if !local_pane {
        let s = state.clone();
        add_item(
            "Edit (auto-upload on save)",
            false,
            Box::new(move || s.menu_edit()),
        );
        let s = state.clone();
        add_item("Duplicate…", false, Box::new(move || s.menu_copy_file()));
        let s = state.clone();
        add_item("Execute command…", false, Box::new(move || s.menu_exec_command()));
        let s = state.clone();
        add_item("Copy URL", false, Box::new(move || s.copy_remote_url()));
    }
    {
        let s = state.clone();
        add_item("Copy path", false, Box::new(move || s.menu_copy_path(local_pane)));
    }
    if local_pane {
        let s = state.clone();
        add_item("Show in Files", false, Box::new(move || s.menu_show_in_files()));
    }
    {
        let s = state.clone();
        add_item("Rename…", false, Box::new(move || s.menu_rename(local_pane)));
    }
    {
        let s = state.clone();
        add_item("Properties…", false, Box::new(move || s.menu_properties(local_pane)));
    }
    {
        let s = state.clone();
        add_item("Delete", true, Box::new(move || s.menu_delete(local_pane)));
    }

    let s = state.clone();
    *hook.borrow_mut() = Some(Box::new(move |index, x, y| {
        let pane = if local_pane { &s.local } else { &s.remote };
        // Right-click selects the row unless it's already in the selection
        // (so batch actions can target multiple rows).
        if !pane.selection.is_selected(index) {
            pane.selection.select_item(index, true);
        }
        let entry = pane.entry_at(index);
        if local_pane {
            *s.local_menu_target.borrow_mut() = entry;
            s.set_focus(true);
        } else {
            *s.remote_menu_target.borrow_mut() = entry;
            s.set_focus(false);
        }
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }));
}

/// Reconnect dialog: network error on an active session.
/// Shows the error message with a countdown Reconnect button (auto-fires at 0).
fn reconnect_dialog(state: &Rc<App>, session: &Rc<Session>, message: &str) {
    let win = gtk::Window::builder()
        .title("Network Error")
        .transient_for(&state.window)
        .modal(true)
        .resizable(false)
        .default_width(420)
        .build();

    let vbox = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(20)
        .margin_bottom(16)
        .margin_start(20)
        .margin_end(20)
        .build();

    // Icon + message row
    let msg_row = GtkBox::builder().orientation(Orientation::Horizontal).spacing(12).build();
    let icon = gtk::Image::from_icon_name("network-error-symbolic");
    icon.set_pixel_size(40);
    let msg_lbl = Label::builder()
        .label(&format!("Network error:\n{message}"))
        .wrap(true)
        .xalign(0.0)
        .max_width_chars(50)
        .build();
    msg_row.append(&icon);
    msg_row.append(&msg_lbl);
    vbox.append(&msg_row);
    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    // Buttons row
    let btn_row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();

    let cancel_btn = Button::with_label("Cancel");
    let reconnect_btn = Button::with_label("Reconnect (30 s)");
    reconnect_btn.add_css_class("suggested-action");

    btn_row.append(&cancel_btn);
    btn_row.append(&gtk::Box::builder().hexpand(true).build()); // spacer
    btn_row.append(&reconnect_btn);
    vbox.append(&btn_row);
    win.set_child(Some(&vbox));

    // Countdown
    let seconds = Rc::new(Cell::new(30u32));
    let reconnect_btn_c = reconnect_btn.clone();
    let state_c = state.clone();
    let session_c = session.clone();
    let win_c = win.clone();

    let do_reconnect = {
        let state = state_c.clone();
        let session = session_c.clone();
        let win = win_c.clone();
        move || {
            win.close();
            state.do_connect(session.clone());
        }
    };

    let do_reconnect_btn = do_reconnect.clone();
    reconnect_btn.connect_clicked(move |_| do_reconnect_btn());
    cancel_btn.connect_clicked({
        let win = win_c.clone();
        move |_| win.close()
    });

    glib::timeout_add_seconds_local(1, move || {
        let s = seconds.get().saturating_sub(1);
        seconds.set(s);
        if s == 0 {
            do_reconnect();
            glib::ControlFlow::Break
        } else {
            reconnect_btn_c.set_label(&format!("Reconnect ({s} s)"));
            glib::ControlFlow::Continue
        }
    });

    win.present();
}

/// WinSCP's "Save session as site" dialog: site name (Folder/Name groups)
/// plus an explicit opt-in checkbox for password storage.
fn save_site_dialog(
    parent: &ApplicationWindow,
    default_name: &str,
    can_save_password: bool,
    on_ok: impl Fn(String, bool) + 'static,
) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Save session as site")
        .default_width(340)
        .resizable(false)
        .build();
    let entry = GtkEntry::builder()
        .text(default_name)
        .activates_default(true)
        .build();
    let hint = Label::builder()
        .label("Use Folder/Name to group sites into a folder")
        .xalign(0.0)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    let save_pw = gtk::CheckButton::with_label("Save password in keyring");
    save_pw.set_sensitive(can_save_password);

    let ok = Button::with_label("Save");
    ok.add_css_class("suggested-action");
    let cancel = Button::with_label("Cancel");
    let buttons = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&cancel);
    buttons.append(&ok);

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&entry);
    content.append(&hint);
    content.append(&save_pw);
    content.append(&buttons);
    win.set_child(Some(&content));
    win.set_default_widget(Some(&ok));

    let on_ok = Rc::new(on_ok);
    {
        let win = win.clone();
        let entry = entry.clone();
        let save_pw = save_pw.clone();
        let on_ok = on_ok.clone();
        ok.connect_clicked(move |_| {
            on_ok(entry.text().to_string(), save_pw.is_active());
            win.close();
        });
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    win.present();
}

/// WinSCP-style Properties dialog: file info plus an rwx checkbox grid.
fn properties_dialog(
    parent: &ApplicationWindow,
    entry: &Entry,
    location: &str,
    current_mode: Option<u32>,
    can_chmod: bool,
    on_apply: impl Fn(u32) + 'static,
) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(format!("{} Properties", entry.name))
        .default_width(360)
        .resizable(false)
        .build();

    let info = gtk::Grid::builder().row_spacing(6).column_spacing(12).build();
    let info_label = |text: &str| {
        let l = Label::builder().label(text).xalign(0.0).build();
        l.add_css_class("dim-label");
        l
    };
    let value_label = |text: &str| {
        let l = Label::builder().label(text).xalign(0.0).build();
        l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        l.set_max_width_chars(34);
        l
    };
    let mut row = 0;
    info.attach(&info_label("Location:"), 0, row, 1, 1);
    info.attach(&value_label(location), 1, row, 1, 1);
    row += 1;
    info.attach(&info_label("Type:"), 0, row, 1, 1);
    info.attach(&value_label(&type_description(entry)), 1, row, 1, 1);
    row += 1;
    if !entry.is_dir {
        info.attach(&info_label("Size:"), 0, row, 1, 1);
        info.attach(&value_label(&format!("{} bytes", entry.size)), 1, row, 1, 1);
        row += 1;
    }
    if let Some(changed) = entry
        .mtime
        .and_then(|m| glib::DateTime::from_unix_local(m).ok())
        .and_then(|dt| dt.format("%d.%m.%Y %H:%M").ok())
    {
        info.attach(&info_label("Changed:"), 0, row, 1, 1);
        info.attach(&value_label(&changed), 1, row, 1, 1);
    }

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();
    content.append(&info);

    let mut checks: Vec<gtk::CheckButton> = Vec::new();
    let octal = Label::builder().xalign(0.0).build();
    octal.add_css_class("dim-label");
    octal.add_css_class("caption");
    if can_chmod {
        content.append(&gtk::Separator::new(Orientation::Horizontal));
        let rights_title = Label::builder().label("Rights").xalign(0.0).build();
        rights_title.add_css_class("heading");
        content.append(&rights_title);

        let grid = gtk::Grid::builder().row_spacing(4).column_spacing(14).build();
        for (col, name) in ["Read", "Write", "Execute"].iter().enumerate() {
            grid.attach(&info_label(name), col as i32 + 1, 0, 1, 1);
        }
        let mode = current_mode.unwrap_or(0);
        for (group, name) in ["Owner", "Group", "Others"].iter().enumerate() {
            grid.attach(&info_label(name), 0, group as i32 + 1, 1, 1);
            for bit in 0..3 {
                let check = gtk::CheckButton::new();
                let index = group * 3 + bit;
                check.set_active(mode & (1 << (8 - index)) != 0);
                grid.attach(&check, bit as i32 + 1, group as i32 + 1, 1, 1);
                checks.push(check);
            }
        }
        content.append(&grid);
        content.append(&octal);

        // Keep the octal readout in sync with the checkboxes.
        let update_octal = {
            let checks = checks.clone();
            let octal = octal.clone();
            Rc::new(move || {
                let mode = checks.iter().enumerate().fold(0u32, |acc, (i, c)| {
                    if c.is_active() { acc | (1 << (8 - i)) } else { acc }
                });
                octal.set_text(&format!("Octal: {mode:03o}"));
            })
        };
        update_octal();
        for check in &checks {
            let hook = update_octal.clone();
            check.connect_toggled(move |_| hook());
        }
    }

    let close = Button::with_label("Close");
    let buttons = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&close);
    if can_chmod {
        let apply = Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        let checks = checks.clone();
        let win2 = win.clone();
        apply.connect_clicked(move |_| {
            let mode = checks.iter().enumerate().fold(0u32, |acc, (i, c)| {
                if c.is_active() { acc | (1 << (8 - i)) } else { acc }
            });
            on_apply(mode);
            win2.close();
        });
        buttons.append(&apply);
    }
    content.append(&buttons);

    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    win.set_child(Some(&content));
    win.present();
}

/// WinSCP-style synchronization checklist: per-file checkboxes, copy only
/// what the user approves.
fn sync_preview_dialog(
    state: &Rc<App>,
    download: bool,
    local_root: std::path::PathBuf,
    remote_root: String,
    plan: scp_core::ops::SyncPlan,
) {
    let win = gtk::Window::builder()
        .transient_for(&state.window)
        .modal(true)
        .title(format!(
            "Synchronize {} — preview",
            if download { "remote → local" } else { "local → remote" }
        ))
        .default_width(520)
        .default_height(420)
        .build();

    let list = GtkBox::builder().orientation(Orientation::Vertical).spacing(2).build();
    let mut checks: Vec<(gtk::CheckButton, String, u64)> = Vec::new();
    for item in &plan.items {
        let check = gtk::CheckButton::builder()
            .active(true)
            .label(format!(
                "{}  ({}, {})",
                item.rel,
                human_size(item.size),
                item.reason.label()
            ))
            .build();
        list.append(&check);
        checks.push((check, item.rel.clone(), item.size));
    }
    let scroller = ScrolledWindow::builder().vexpand(true).child(&list).build();

    let delete_note = if plan.deletes.is_empty() {
        String::new()
    } else {
        format!(", {} to delete (mirror)", plan.deletes.len())
    };
    let summary = Label::builder()
        .label(format!(
            "{} file(s) to copy, {} folder(s) to create{delete_note}",
            plan.items.len(),
            plan.dirs.len()
        ))
        .xalign(0.0)
        .build();
    summary.add_css_class("dim-label");

    let apply = Button::with_label("Synchronize");
    apply.add_css_class("suggested-action");
    let cancel = Button::with_label("Cancel");
    let buttons = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&cancel);
    buttons.append(&apply);

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .build();
    content.append(&summary);
    content.append(&scroller);
    content.append(&buttons);
    win.set_child(Some(&content));

    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    {
        let state = state.clone();
        let win = win.clone();
        let dirs = plan.dirs.clone();
        let deletes = plan.deletes.clone();
        apply.connect_clicked(move |_| {
            let selected: Vec<(String, u64)> = checks
                .iter()
                .filter(|(c, _, _)| c.is_active())
                .map(|(_, rel, size)| (rel.clone(), *size))
                .collect();
            state.run_sync_items(
                download, &local_root, &remote_root, dirs.clone(), selected, deletes.clone(),
            );
            win.close();
        });
    }
    win.present();
}

/// Results of a remote Find: activate a row to jump to its directory.
fn find_results_dialog(state: &Rc<App>, base: &str, mask: &str, hits: Vec<(String, Entry)>) {
    let win = gtk::Window::builder()
        .transient_for(&state.window)
        .title(format!("Find \"{mask}\" under {base} — {} match(es)", hits.len()))
        .default_width(560)
        .default_height(420)
        .build();
    let list = ListBox::builder().selection_mode(SelectionMode::None).build();
    for (path, e) in &hits {
        let label = Label::builder()
            .label(if e.is_dir {
                format!("{path}/")
            } else {
                format!("{path}  ({})", human_size(e.size))
            })
            .xalign(0.0)
            .margin_top(2)
            .margin_bottom(2)
            .margin_start(6)
            .build();
        list.append(&label);
    }
    let paths: Vec<String> = hits.iter().map(|(p, _)| p.clone()).collect();
    list.set_activate_on_single_click(false);
    {
        let state = state.clone();
        let win = win.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(path) = paths.get(row.index().max(0) as usize) {
                let parent = parent_posix(path);
                let _ = state.session().cmd.send(Cmd::List { path: parent });
                win.close();
            }
        });
    }
    let hint = Label::builder()
        .label("Double-click a result to open its directory")
        .xalign(0.0)
        .margin_start(8)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    let content = GtkBox::builder().orientation(Orientation::Vertical).spacing(4).build();
    content.append(&hint);
    content.append(&ScrolledWindow::builder().vexpand(true).child(&list).build());
    win.set_child(Some(&content));
    win.present();
}

/// Show the output of a remote exec command.
fn exec_result_dialog(parent: &ApplicationWindow, exit_code: i32, stdout: &str, stderr: &str) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .title(format!("Command result — exit {exit_code}"))
        .default_width(600)
        .default_height(400)
        .build();

    let vbox = GtkBox::builder().orientation(Orientation::Vertical).spacing(6).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();

    let add_section = |label_text: &str, body: &str| -> GtkBox {
        let section = GtkBox::builder().orientation(Orientation::Vertical).spacing(2).build();
        let lbl = Label::builder().label(label_text).xalign(0.0).build();
        lbl.add_css_class("heading");
        let tv = gtk::TextView::builder()
            .editable(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .build();
        tv.buffer().set_text(body);
        let sw = ScrolledWindow::builder().vexpand(true).min_content_height(80).child(&tv).build();
        section.append(&lbl);
        section.append(&sw);
        section
    };

    let exit_label_text = if exit_code == 0 {
        "Exit code: 0 (success)".to_string()
    } else {
        format!("Exit code: {exit_code}")
    };
    let exit_lbl = Label::builder().label(&exit_label_text).xalign(0.0).build();
    if exit_code != 0 { exit_lbl.add_css_class("error"); }
    vbox.append(&exit_lbl);
    if !stdout.is_empty() { vbox.append(&add_section("stdout", stdout)); }
    if !stderr.is_empty() { vbox.append(&add_section("stderr", stderr)); }
    if stdout.is_empty() && stderr.is_empty() {
        let empty = Label::builder().label("(no output)").xalign(0.0).build();
        empty.add_css_class("dim-label");
        vbox.append(&empty);
    }
    let close = Button::with_label("Close");
    close.add_css_class("suggested-action");
    close.set_halign(gtk::Align::End);
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    vbox.append(&close);
    win.set_child(Some(&vbox));
    win.present();
}

/// In-app help window.
fn show_help_dialog(parent: &ApplicationWindow) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .title("SCP Commander — Help")
        .default_width(620)
        .default_height(560)
        .modal(false)
        .build();

    let vbox = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .build();

    // ── Interface overview ────────────────────────────────────────────────────
    let overview_label = Label::builder()
        .label("Interface overview")
        .xalign(0.0)
        .margin_top(12)
        .margin_bottom(4)
        .margin_start(16)
        .margin_end(16)
        .build();
    overview_label.add_css_class("heading");
    vbox.append(&overview_label);

    let diagram_text = "\
① Toolbar      New Session · Show hidden · Sync · Execute · Find · Terminal · Exclude · Help
② Session tabs  Each tab = independent connection.  Click + for a new session, × to close.
③ Pane header   Title · ↑ parent · ↻ refresh · ↑/↓ transfer · ✏ edit · 📁 new folder · 🗑 delete
④ Address bar   Shows the current path. Click to edit, press Enter to navigate.
⑤ Column header Name · Size · Type · Changed · (Owner · Rights on remote pane)";

    let diagram = Label::builder()
        .label(diagram_text)
        .xalign(0.0)
        .margin_start(16)
        .margin_end(16)
        .margin_bottom(8)
        .build();
    diagram.add_css_class("monospace");
    vbox.append(&diagram);

    // ── Icons reference ───────────────────────────────────────────────────────
    let icons_label = Label::builder()
        .label("Icons reference")
        .xalign(0.0)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(16)
        .margin_end(16)
        .build();
    icons_label.add_css_class("heading");
    vbox.append(&icons_label);

    let icon_sections: &[(&str, &[(&str, &str, &str)])] = &[
        ("Main toolbar", &[
            ("network-server-symbolic",          "New Session",      "Open the Login dialog to connect to a server or switch sites."),
            ("view-reveal-symbolic",             "Show hidden",      "Toggle files whose names start with a dot (hidden files)."),
            ("go-up-symbolic",                   "Sync upload",      "Synchronise local → remote (upload changes)."),
            ("go-down-symbolic",                 "Sync download",    "Synchronise remote → local (download changes)."),
            ("system-search-symbolic",           "Find files",       "Search the remote directory recursively by name mask (e.g. *.log)."),
            ("utilities-terminal-symbolic",      "Open terminal",    "Open an SSH session to the current host in your system terminal."),
            ("utilities-terminal-symbolic",      "Execute command",  "Run a shell command on the remote server (SFTP only)."),
            ("help-browser-symbolic",            "Help",             "Open this help window."),
        ]),
        ("Pane header — both panes", &[
            ("go-up-symbolic",                   "Parent directory", "Navigate up one level (same as the .. row or Backspace)."),
            ("view-refresh-symbolic",            "Refresh",          "Reload the current directory listing."),
            ("folder-new-symbolic",              "New folder",       "Create a new folder in the current directory."),
            ("edit-delete-symbolic",             "Delete",           "Delete selected file(s) or folder(s). Folders removed recursively."),
        ]),
        ("Pane header — local pane only", &[
            ("go-up-symbolic",                   "Upload (F5)",      "Copy selected local items to the current remote directory."),
        ]),
        ("Pane header — remote pane only", &[
            ("go-down-symbolic",                 "Download (F5)",    "Copy selected remote items to the current local directory."),
            ("document-edit-symbolic",           "Edit",             "Download the file, open in your editor, auto-upload on every save."),
        ]),
        ("File list — row icons", &[
            ("folder-symbolic",                  "Directory",        "A folder — double-click to navigate into it."),
            ("text-x-generic-symbolic",          "File",             "A regular file — double-click to transfer it."),
            ("emblem-symbolic-link",             "Symlink",          "A symbolic link."),
            ("go-up-symbolic",                   ".. (parent)",      "Top row in every listing — double-click to go up one level."),
        ]),
    ];

    let icons_grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(10)
        .margin_start(16)
        .margin_end(16)
        .margin_bottom(8)
        .build();

    let mut row_idx = 0i32;
    for (group_title, rows) in icon_sections {
        let grp_lbl = Label::builder().label(*group_title).xalign(0.0).margin_top(6).build();
        grp_lbl.add_css_class("dim-label");
        icons_grid.attach(&grp_lbl, 0, row_idx, 4, 1);
        row_idx += 1;
        for (icon_name, name, desc) in *rows {
            let img = gtk::Image::builder()
                .icon_name(*icon_name)
                .icon_size(gtk::IconSize::Normal)
                .halign(gtk::Align::Center)
                .build();
            let name_lbl = Label::builder().label(*name).xalign(0.0).build();
            name_lbl.add_css_class("caption");
            let desc_lbl = Label::builder()
                .label(*desc)
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build();
            desc_lbl.add_css_class("dim-label");
            icons_grid.attach(&img,      0, row_idx, 1, 1);
            icons_grid.attach(&name_lbl, 1, row_idx, 1, 1);
            icons_grid.attach(&desc_lbl, 2, row_idx, 1, 1);
            row_idx += 1;
        }
    }
    vbox.append(&icons_grid);

    let sep = gtk::Separator::new(Orientation::Horizontal);
    sep.set_margin_top(4);
    sep.set_margin_bottom(4);
    vbox.append(&sep);

    let sections: &[(&str, &[(&str, &str)])] = &[
        ("Connecting", &[
            ("1. Open the Login dialog", "It appears on launch or via the Login button in the toolbar."),
            ("2. Fill in credentials", "Choose protocol (SFTP · FTP · FTPS · S3), host, port, and credentials. SFTP supports password, key file, or ssh-agent auth."),
            ("3. Remember password", "Tick the checkbox to store the password in the system keyring. Next time you type the same host + user, it fills in automatically."),
            ("4. Click Login", "If the server's host key is new, review the fingerprint and click Trust & Connect."),
        ]),
        ("Saving sites", &[
            ("Save site…", "Click in the Login dialog to bookmark credentials. Use Folder/Name to group them. Double-click a saved site to connect instantly. Right-click to rename or delete."),
        ]),
        ("Browsing", &[
            ("Left pane", "Your local filesystem."),
            ("Right pane", "The remote server."),
            ("Double-click folder", "Navigate into it."),
            (".. row / Backspace", "Go up one level."),
            ("Path bar", "Type a path and press Enter to jump directly."),
            ("Column header", "Click to sort by Name, Size, Type, Changed, or Rights."),
            ("Eye icon", "Toggle hidden files (names starting with .)."),
            ("Filter box", "Type to narrow the visible listing by name."),
        ]),
        ("Transferring files", &[
            ("F5 or toolbar button", "Copy selected items to the other pane."),
            ("F6", "Move selected items (copy then delete source)."),
            ("Drag and drop", "Drag files between the two panes."),
            ("Overwrite prompt", "If the destination has a file with the same name you get Overwrite / Skip / Cancel."),
        ]),
        ("Keyboard shortcuts", &[
            ("F5", "Copy (transfer) selected items."),
            ("F6", "Move selected items."),
            ("F2", "Rename selected item."),
            ("Delete", "Delete selected item(s)."),
            ("Backspace", "Navigate to parent directory."),
            ("Tab", "Switch focus between left and right pane."),
            ("Enter", "Open folder / transfer file."),
        ]),
        ("Directory sync", &[
            ("↑ / ↓ sync buttons", "Synchronise a local/remote directory pair. A preview checklist shows what will be copied or deleted. Tick Mirror to also delete destination items with no source counterpart."),
        ]),
        ("Find files", &[
            ("🔍 button", "Search the current remote directory recursively by name mask (e.g. *.log). Double-click a result to navigate to its directory."),
        ]),
        ("Remote editing", &[
            ("Right-click → Edit", "Downloads the file to a temp location and opens it in your editor. Every save auto-uploads."),
        ]),
        ("Transfer queue", &[
            ("Bottom panel", "Shows all active and completed transfers with progress, speed, and ETA. Each transfer has its own × cancel button. Cancel All stops everything at once."),
        ]),
    ];

    let scroll = ScrolledWindow::builder().vexpand(true).build();
    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .build();

    for (section_title, rows) in sections {
        let section_label = Label::builder()
            .label(*section_title)
            .xalign(0.0)
            .margin_top(12)
            .margin_bottom(4)
            .build();
        section_label.add_css_class("heading");
        content.append(&section_label);

        let grid = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(12)
            .build();
        for (i, (key, val)) in rows.iter().enumerate() {
            let key_lbl = Label::builder()
                .label(*key)
                .xalign(0.0)
                .valign(gtk::Align::Start)
                .build();
            key_lbl.add_css_class("dim-label");
            let val_lbl = Label::builder()
                .label(*val)
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build();
            grid.attach(&key_lbl, 0, i as i32, 1, 1);
            grid.attach(&val_lbl, 1, i as i32, 1, 1);
        }
        content.append(&grid);
        let sep = gtk::Separator::new(Orientation::Horizontal);
        sep.set_margin_top(8);
        content.append(&sep);
    }

    scroll.set_child(Some(&content));
    vbox.append(&scroll);

    let close_btn = Button::with_label("Close");
    close_btn.add_css_class("suggested-action");
    let btn_row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .halign(gtk::Align::End)
        .margin_top(8)
        .margin_bottom(8)
        .margin_end(12)
        .spacing(8)
        .build();
    btn_row.append(&close_btn);
    vbox.append(&btn_row);

    win.set_child(Some(&vbox));

    let win_clone = win.clone();
    close_btn.connect_clicked(move |_| win_clone.close());

    win.present();
}

/// Small modal text prompt; calls `on_ok` with the entered string.
fn prompt(
    parent: &ApplicationWindow,
    title: &str,
    initial: &str,
    on_ok: impl Fn(String) + 'static,
) {
    let win = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(320)
        .resizable(false)
        .build();
    let entry = GtkEntry::builder().text(initial).activates_default(true).build();
    let ok = Button::with_label("OK");
    ok.add_css_class("suggested-action");
    let cancel = Button::with_label("Cancel");

    let buttons = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&cancel);
    buttons.append(&ok);

    let content = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&entry);
    content.append(&buttons);
    win.set_child(Some(&content));
    win.set_default_widget(Some(&ok));

    let on_ok = Rc::new(on_ok);
    {
        let win = win.clone();
        let entry = entry.clone();
        let on_ok = on_ok.clone();
        ok.connect_clicked(move |_| {
            on_ok(entry.text().to_string());
            win.close();
        });
    }
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    win.present();
}

/// Drag from a pane: payload is "<kind>:<name>" for the selected row.
fn add_drag_source(view: &ColumnView, kind: &'static str, pane: &Pane) {
    let drag = DragSource::builder().actions(gdk::DragAction::COPY).build();
    let entries = pane.entries.clone();
    let selection = pane.selection.clone();
    let path_entry = pane.path_entry.clone();
    drag.connect_prepare(move |_, _, _| {
        let bitset = selection.selection();
        let entries = entries.borrow();
        let names: Vec<String> = (0..bitset.size())
            .filter_map(|i| entries.get(bitset.nth(i as u32) as usize))
            .map(|e| e.name.clone())
            .collect();
        if names.is_empty() {
            return None;
        }
        // Inter-pane payload ("local:name\n…") — parsed by the other pane.
        let inter = gdk::ContentProvider::for_value(
            &format!("{kind}:{}", names.join("\n")).to_value());
        // Local items also export real file:// URIs so they can be dragged
        // straight into Nautilus/the desktop. (Remote items have no local file
        // to promise — GTK4 has no async file-promise like macOS, so dragging
        // a remote file out isn't offered; use Download instead.)
        if kind == "local" {
            let base = PathBuf::from(path_entry.text().as_str());
            let uris: String = names
                .iter()
                .map(|n| format!("{}\r\n", gio::File::for_path(base.join(n)).uri()))
                .collect();
            let uri_provider = gdk::ContentProvider::for_bytes(
                "text/uri-list", &glib::Bytes::from(uris.as_bytes()));
            return Some(gdk::ContentProvider::new_union(&[inter, uri_provider]));
        }
        Some(inter)
    });
    view.add_controller(drag);
}

/// WinSCP-style "Type" column, from the shared-mime-info database.
fn type_description(e: &Entry) -> String {
    if e.is_dir {
        return gio::functions::content_type_get_description("inode/directory").to_string();
    }
    let (ct, _uncertain) = gio::functions::content_type_guess(Some(e.name.as_str()), &[]);
    gio::functions::content_type_get_description(&ct).to_string()
}

/// Parse "rwxr-xr-x" into permission bits (0o755).
fn parse_mode(perms: &str) -> Option<u32> {
    if perms.len() != 9 {
        return None;
    }
    let mut mode = 0u32;
    for (i, c) in perms.chars().enumerate() {
        if c != '-' {
            mode |= 1 << (8 - i);
        }
    }
    Some(mode)
}

fn join_posix(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Single-quote a string for a POSIX shell (wrap in '', escaping any quotes).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn parent_posix(path: &str) -> String {
    if path == "/" {
        return "/".into();
    }
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".into(),
        Some(idx) => trimmed[..idx].to_string(),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
