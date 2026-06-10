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

mod secrets;
mod sites;
mod worker;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
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
    SingleSelection,
};

use scp_core::types::{Auth, Credentials, Entry, HostKeyPolicy, Protocol};
use sites::{Site, SitesStore};
use worker::{Cmd, Event};

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
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

// ---------------------------------------------------------------------------
// Shared UI state

/// One pane's list widgets plus the entries backing the visible rows.
struct Pane {
    model: gio::ListStore,
    selection: SingleSelection,
    entries: Rc<RefCell<Vec<Entry>>>,
    path_label: Label,
}

struct TransferRow {
    container: GtkBox,
    bar: ProgressBar,
    cancel_btn: Button,
    finished: bool,
    files_done: u32,
}

struct EditWatch {
    remote: String,
    local: PathBuf,
    last_mtime: SystemTime,
    /// Command channel of the session that opened the file.
    cmd: mpsc::Sender<Cmd>,
}

/// One server session, WinSCP-tab-style: its own worker thread, connection,
/// and cached remote listing. The remote pane shows the active session.
struct Session {
    cmd: mpsc::Sender<Cmd>,
    remote_path: RefCell<String>,
    connected: Cell<bool>,
    cache: RefCell<Vec<Entry>>,
    title: RefCell<String>,
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
    // Transfers panel
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
    key_entry: GtkEntry,
    bucket_entry: GtkEntry,
    region_entry: GtkEntry,
    // Host key trust prompt
    hostkey_bar: GtkBox,
    hostkey_label: Label,
    pending_connect: RefCell<Option<(Credentials, String)>>,
    pending_fingerprint: RefCell<Option<String>>,
    // Context menus
    local_menu_index: Cell<u32>,
    remote_menu_index: Cell<u32>,
    sites_menu_index: Cell<usize>,
    // Edit-in-editor
    edit_pending: RefCell<HashMap<u64, (String, PathBuf)>>,
    edits: RefCell<Vec<EditWatch>>,
    // Sites
    sites: RefCell<SitesStore>,
    sites_list: ListBox,
}

impl App {
    fn set_status(&self, text: &str) {
        self.status.set_text(text);
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
        self.remote.show(&cache, &path);
        let title = session.title.borrow().clone();
        self.window.set_title(Some(&if session.connected.get() {
            format!("{title} — SCP Commander")
        } else {
            "SCP Commander".to_string()
        }));
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
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        sort_entries(&mut entries);
        self.local.show(&entries, &path.to_string_lossy());
    }

    fn open_local(self: &Rc<Self>, index: u32) {
        let Some(entry) = self.local.entry_at(index) else { return };
        if entry.is_dir {
            self.local_path.borrow_mut().push(&entry.name);
            self.load_local();
        } else {
            self.upload(&entry);
        }
    }

    fn local_up(&self) {
        self.local_path.borrow_mut().pop();
        self.load_local();
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
        let path = self.session().remote_path.borrow().clone();
        *self.pending_connect.borrow_mut() = Some((creds.clone(), path.clone()));
        self.set_status("Connecting…");
        let _ = self.session().cmd.send(Cmd::Connect { creds, path });
    }

    /// "Trust & Connect" on the host key bar: retry pinned to the approved key.
    fn trust_host_key(&self) {
        let Some(fingerprint) = self.pending_fingerprint.borrow_mut().take() else { return };
        let Some((mut creds, _)) = self.pending_connect.borrow_mut().take() else { return };
        creds.host_key = HostKeyPolicy::AcceptFingerprint(fingerprint);
        self.start_connect(creds);
    }

    fn open_remote(self: &Rc<Self>, index: u32) {
        let Some(entry) = self.remote.entry_at(index) else { return };
        if entry.is_dir {
            let path = join_posix(&self.session().remote_path.borrow(), &entry.name);
            let _ = self.session().cmd.send(Cmd::List { path });
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
    }

    fn refresh_remote(&self) {
        if self.session().connected.get() {
            let path = self.session().remote_path.borrow().clone();
            let _ = self.session().cmd.send(Cmd::List { path });
        }
    }

    // -- Transfers ----------------------------------------------------------

    fn download(self: &Rc<Self>, entry: &Entry) {
        if !self.session().connected.get() {
            return;
        }
        let remote = join_posix(&self.session().remote_path.borrow(), &entry.name);
        let local = self.local_path.borrow().join(&entry.name);
        if entry.is_dir {
            let (id, cancel) = self.add_transfer(&format!("{}/", entry.name), true, 0);
            let _ = self.session().cmd.send(Cmd::DownloadDir {
                id,
                name: entry.name.clone(),
                remote,
                local,
                cancel,
            });
        } else {
            // Resume when a smaller partial file is already present locally.
            let resume = std::fs::metadata(&local)
                .map(|m| m.len())
                .ok()
                .filter(|len| *len > 0 && *len < entry.size)
                .unwrap_or(0);
            let (id, cancel) = self.add_transfer(&entry.name, true, entry.size);
            let _ = self.session().cmd.send(Cmd::Download {
                id,
                name: entry.name.clone(),
                remote,
                local,
                resume,
                cancel,
            });
        }
    }

    fn upload(self: &Rc<Self>, entry: &Entry) {
        if !self.session().connected.get() {
            self.set_status("Connect first to upload");
            return;
        }
        let local = self.local_path.borrow().join(&entry.name);
        let remote = join_posix(&self.session().remote_path.borrow(), &entry.name);
        if entry.is_dir {
            let (id, cancel) = self.add_transfer(&format!("{}/", entry.name), false, 0);
            let _ = self.session().cmd.send(Cmd::UploadDir {
                id,
                name: entry.name.clone(),
                local,
                remote,
                cancel,
            });
        } else {
            let (id, cancel) = self.add_transfer(&entry.name, false, entry.size);
            let _ = self.session().cmd.send(Cmd::Upload {
                id,
                name: entry.name.clone(),
                local,
                remote,
                cancel,
            });
        }
    }

    fn sync(self: &Rc<Self>, download: bool) {
        if !self.session().connected.get() {
            self.set_status("Connect first to sync");
            return;
        }
        let local = self.local_path.borrow().clone();
        let remote = self.session().remote_path.borrow().clone();
        let title = format!("Sync {} {}", if download { "⬇" } else { "⬆" }, remote);
        let (id, cancel) = self.add_transfer(&title, download, 0);
        let _ = self.session().cmd.send(Cmd::Sync { id, download, local, remote, cancel });
    }

    fn add_transfer(&self, name: &str, download: bool, total: u64) -> (u64, Arc<AtomicBool>) {
        let id = {
            let mut next = self.next_id.borrow_mut();
            *next += 1;
            *next
        };
        let cancel = Arc::new(AtomicBool::new(false));

        let arrow = if download { "↓" } else { "↑" };
        let row = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .build();
        let name_label = Label::builder()
            .label(format!("{arrow} {name}"))
            .xalign(0.0)
            .width_chars(28)
            .max_width_chars(28)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let bar = ProgressBar::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .show_text(true)
            .build();
        if total > 0 {
            bar.set_text(Some(&human_size(total)));
        }
        let cancel_btn = Button::from_icon_name("process-stop-symbolic");
        cancel_btn.add_css_class("flat");
        cancel_btn.set_tooltip_text(Some("Cancel"));
        let flag = cancel.clone();
        cancel_btn.connect_clicked(move |_| flag.store(true, Ordering::Relaxed));

        row.append(&name_label);
        row.append(&bar);
        row.append(&cancel_btn);
        self.transfers_box.prepend(&row);
        self.transfers_panel.set_visible(true);

        self.transfer_rows.borrow_mut().insert(
            id,
            TransferRow {
                container: row,
                bar,
                cancel_btn,
                finished: false,
                files_done: 0,
            },
        );
        (id, cancel)
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
            self.transfers_panel.set_visible(false);
        }
    }

    fn finish_row(&self, id: u64, text: &str, full: bool) {
        if let Some(row) = self.transfer_rows.borrow_mut().get_mut(&id) {
            if full {
                row.bar.set_fraction(1.0);
            }
            row.bar.set_text(Some(text));
            row.finished = true;
            row.cancel_btn.set_visible(false);
        }
    }

    // -- Context menu actions -------------------------------------------------

    /// Point the menu index at the pane's selected row (for toolbar buttons
    /// that reuse menu actions). Returns false when nothing is selected.
    fn select_for_menu(&self, local_pane: bool) -> bool {
        let pane = if local_pane { &self.local } else { &self.remote };
        let idx = pane.selection.selected();
        if (idx as usize) >= pane.entries.borrow().len() {
            return false;
        }
        if local_pane {
            self.local_menu_index.set(idx);
        } else {
            self.remote_menu_index.set(idx);
        }
        true
    }

    fn menu_entry(&self, local_pane: bool) -> Option<Entry> {
        if local_pane {
            self.local.entry_at(self.local_menu_index.get())
        } else {
            self.remote.entry_at(self.remote_menu_index.get())
        }
    }

    fn menu_transfer(self: &Rc<Self>, local_pane: bool) {
        if let Some(entry) = self.menu_entry(local_pane) {
            if local_pane {
                self.upload(&entry);
            } else {
                self.download(&entry);
            }
        }
    }

    fn menu_open(self: &Rc<Self>, local_pane: bool) {
        let index = if local_pane {
            self.local_menu_index.get()
        } else {
            self.remote_menu_index.get()
        };
        if local_pane {
            self.open_local(index);
        } else {
            self.open_remote(index);
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
        let Some(entry) = self.menu_entry(local_pane) else { return };
        let state = self.clone();
        let dialog = gtk::AlertDialog::builder()
            .message(format!("Delete {}?", entry.name))
            .detail(if entry.is_dir {
                "The folder and everything inside it will be deleted."
            } else {
                "This cannot be undone."
            })
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
                if local_pane {
                    let path = state.local_path.borrow().join(&entry.name);
                    let outcome = if entry.is_dir {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    match outcome {
                        Ok(()) => state.load_local(),
                        Err(e) => state.set_status(&format!("Error: {e}")),
                    }
                } else {
                    let path = join_posix(&state.session().remote_path.borrow(), &entry.name);
                    let _ = state.session().cmd.send(Cmd::Delete { path, is_dir: entry.is_dir });
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
        let dir = glib::tmp_dir()
            .join("scp-commander-edit")
            .join(format!("{}", self.next_id.borrow()));
        if std::fs::create_dir_all(&dir).is_err() {
            self.set_status("Could not create temp directory");
            return;
        }
        let local = dir.join(&entry.name);
        let (id, cancel) = self.add_transfer(&entry.name, true, entry.size);
        self.edit_pending
            .borrow_mut()
            .insert(id, (remote.clone(), local.clone()));
        let _ = self.session().cmd.send(Cmd::Download {
            id,
            name: entry.name.clone(),
            remote,
            local,
            resume: 0,
            cancel,
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
            let (id, cancel) = self.add_transfer(&name, false, size);
            let _ = cmd.send(Cmd::Upload { id, name, local, remote, cancel });
        }
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
    fn handle_event(self: &Rc<Self>, session: &Rc<Session>, event: Event) {
        let is_active = Rc::ptr_eq(session, &self.session());
        match event {
            Event::Connected { path, entries } | Event::Listed { path, entries } => {
                let first_connect = !session.connected.get();
                session.connected.set(true);
                let mut entries = entries;
                sort_entries(&mut entries);
                let count = entries.len();
                *session.remote_path.borrow_mut() = path.clone();
                *session.cache.borrow_mut() = entries.clone();
                if first_connect {
                    let title = self
                        .pending_connect
                        .borrow()
                        .as_ref()
                        .map(|(c, _)| {
                            let target = if c.host.is_empty() {
                                c.bucket.clone().unwrap_or_default()
                            } else {
                                c.host.clone()
                            };
                            if c.username.is_empty() {
                                target
                            } else {
                                format!("{}@{}", c.username, target)
                            }
                        })
                        .unwrap_or_default();
                    *session.title.borrow_mut() = title;
                    self.refresh_tabs();
                }
                if is_active {
                    self.remote.show(&entries, &path);
                    self.set_status(&format!("{path} ({count} items)"));
                    if first_connect || self.login_window.is_visible() {
                        self.login_window.set_visible(false);
                        self.window.set_title(Some(&format!(
                            "{} — SCP Commander",
                            session.title.borrow()
                        )));
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
                if let Some(row) = self.transfer_rows.borrow().get(&id) {
                    if total > 0 {
                        row.bar.set_fraction((done as f64 / total as f64).min(1.0));
                        row.bar.set_text(Some(&format!(
                            "{} / {}",
                            human_size(done),
                            human_size(total)
                        )));
                    } else {
                        row.bar.pulse();
                        row.bar.set_text(Some(&human_size(done)));
                    }
                }
            }
            Event::FileStart { id, file, total } => {
                if let Some(row) = self.transfer_rows.borrow().get(&id) {
                    let short = file.rsplit('/').next().unwrap_or(&file);
                    row.bar.set_fraction(0.0);
                    row.bar.set_text(Some(short));
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

                // Edit flow: the temp download finished — open it and watch.
                if let Some((remote, local)) = self.edit_pending.borrow_mut().remove(&id) {
                    let mtime = std::fs::metadata(&local)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    let uri = format!("file://{}", local.display());
                    if let Err(e) =
                        gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
                    {
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
                self.set_status(&format!("Cancelled {name}"));
            }
            Event::Failed { id, message } => {
                self.finish_row(id, &format!("failed: {message}"), false);
                self.set_status(&format!("Error: {message}"));
            }
            Event::OpOk { message } => {
                self.set_status(&message);
                if session.connected.get() {
                    let path = session.remote_path.borrow().clone();
                    let _ = session.cmd.send(Cmd::List { path });
                }
            }
            Event::Error(message) => self.set_status(&format!("Error: {message}")),
        }
    }
}

impl Pane {
    fn entry_at(&self, index: u32) -> Option<Entry> {
        self.entries.borrow().get(index as usize).cloned()
    }

    fn show(&self, entries: &[Entry], path: &str) {
        self.path_label.set_text(path);
        self.model.remove_all();
        for e in entries {
            self.model.append(&glib::BoxedAnyObject::new(e.clone()));
        }
        *self.entries.borrow_mut() = entries.to_vec();
    }
}

// ---------------------------------------------------------------------------
// UI assembly

/// Spawn a session: its own worker thread plus a main-loop pump feeding
/// events (tagged with the session) into the shared handler.
fn create_session(state: &Rc<App>) -> Rc<Session> {
    let (event_tx, event_rx) = async_channel::unbounded::<Event>();
    let cmd = worker::spawn(event_tx);
    let session = Rc::new(Session {
        cmd,
        remote_path: RefCell::new("/".to_string()),
        connected: Cell::new(false),
        cache: RefCell::new(Vec::new()),
        title: RefCell::new("New Session".to_string()),
    });
    glib::spawn_future_local({
        let state = state.clone();
        let session = session.clone();
        async move {
            while let Ok(event) = event_rx.recv().await {
                state.handle_event(&session, event);
            }
        }
    });
    session
}

fn build_ui(app: &Application) {
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
    form.attach(&key_label, 0, 5, 1, 1);
    form.attach(&key_row, 1, 5, 2, 1);
    form.attach(&bucket_label, 0, 6, 1, 1);
    form.attach(&bucket_entry, 1, 6, 1, 1);
    form.attach(&region_label, 0, 7, 1, 1);
    form.attach(&region_entry, 1, 7, 1, 1);

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
        Rc::new(move || {
            let selected = proto_dd.selected();
            let is_s3 = selected == 3;
            let is_sftp = selected == 0;
            let auth = if is_sftp { auth_dd.selected() } else { 0 };
            port_entry
                .set_text(&Credentials::default_port(proto_from_index(selected)).to_string());
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
        })
    };
    let hook = update_form.clone();
    proto_dd.connect_selected_notify(move |_| hook());
    let hook = update_form.clone();
    auth_dd.connect_selected_notify(move |_| hook());

    // Login dialog buttons + main-window toolbar ------------------------------
    let login_btn = Button::with_label("Login");
    login_btn.add_css_class("suggested-action");
    let close_btn = Button::with_label("Close");
    let new_session_btn = Button::with_label("New Session…");
    let sync_up_btn = Button::from_icon_name("go-up-symbolic");
    sync_up_btn.set_tooltip_text(Some("Sync local → remote (upload changes)"));
    let sync_down_btn = Button::from_icon_name("go-down-symbolic");
    sync_down_btn.set_tooltip_text(Some("Sync remote → local (download changes)"));

    let main_toolbar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    main_toolbar.append(&new_session_btn);
    main_toolbar.append(&gtk::Separator::new(Orientation::Vertical));
    main_toolbar.append(&sync_up_btn);
    main_toolbar.append(&sync_down_btn);

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
    transfers_header.append(&transfers_title);
    transfers_header.append(&clear_btn);

    let transfers_scroll = ScrolledWindow::builder()
        .max_content_height(130)
        .propagate_natural_height(true)
        .child(&transfers_box)
        .build();
    let transfers_panel = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .visible(false)
        .build();
    transfers_panel.append(&gtk::Separator::new(Orientation::Horizontal));
    transfers_panel.append(&transfers_header);
    transfers_panel.append(&transfers_scroll);

    let status = Label::builder()
        .xalign(0.0)
        .label("Not connected")
        .margin_start(6)
        .margin_end(6)
        .margin_top(2)
        .margin_bottom(4)
        .build();

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
    for b in [&import_btn, &export_btn] {
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

    let content = GtkBox::builder().orientation(Orientation::Vertical).build();
    content.append(&main_toolbar);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&tabs_box);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&panes);
    content.append(&transfers_panel);
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
        key_entry,
        bucket_entry,
        region_entry,
        hostkey_bar,
        hostkey_label,
        pending_connect: RefCell::new(None),
        pending_fingerprint: RefCell::new(None),
        local_menu_index: Cell::new(0),
        remote_menu_index: Cell::new(0),
        sites_menu_index: Cell::new(0),
        edit_pending: RefCell::new(HashMap::new()),
        edits: RefCell::new(Vec::new()),
        sites: RefCell::new(SitesStore::load()),
        sites_list,
    });

    // First session tab.
    let first = create_session(&state);
    state.sessions.borrow_mut().push(first);
    state.refresh_tabs();

    update_form();
    state.load_local();
    state.refresh_sites_list();

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
    sync_up_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.sync(false)
    ));
    sync_down_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.sync(true)
    ));
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

    // Drag and drop between panes -----------------------------------------------
    add_drag_source(&local_view, "local", &state.local);
    add_drag_source(&remote_view, "remote", &state.remote);

    let local_drop = DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
    local_drop.connect_drop(glib::clone!(
        #[strong] state,
        move |_, value, _, _| {
            if let Some(name) = value
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("remote:").map(str::to_string))
            {
                if let Some(entry) =
                    state.remote.entries.borrow().iter().find(|e| e.name == name).cloned()
                {
                    state.download(&entry);
                    return true;
                }
            }
            false
        }
    ));
    local_view.add_controller(local_drop);

    let remote_drop = DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
    remote_drop.connect_drop(glib::clone!(
        #[strong] state,
        move |_, value, _, _| {
            if let Some(name) = value
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("local:").map(str::to_string))
            {
                if let Some(entry) =
                    state.local.entries.borrow().iter().find(|e| e.name == name).cloned()
                {
                    state.upload(&entry);
                    return true;
                }
            }
            false
        }
    ));
    remote_view.add_controller(remote_drop);

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

    window.set_child(Some(&root));
    window.present();
    // WinSCP opens with the Login dialog.
    login_window.present();
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

    let up = tool("go-up-symbolic", "Parent directory");
    up.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.local_up() } else { state.remote_up() }
    ));

    let refresh = tool("view-refresh-symbolic", "Refresh");
    refresh.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| if local_pane { state.load_local() } else { state.refresh_remote() }
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
    ColumnViewColumn::new(Some(title), Some(factory))
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
    let selection = SingleSelection::new(Some(model.clone()));
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

    if show_rights {
        let rights_col = text_column(
            "Rights",
            0.0,
            &view,
            hook,
            Rc::new(|e: &Entry| e.perms.clone().unwrap_or_default()),
        );
        rights_col.set_fixed_width(95);
        view.append_column(&rights_col);
    }

    view.set_single_click_activate(false);

    // WinSCP-style pane toolbar strip; build_ui appends the action buttons.
    let header = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(2)
        .build();
    let title_label = Label::builder().label(title).xalign(0.0).build();
    title_label.add_css_class("heading");
    title_label.set_margin_end(8);
    header.append(&title_label);

    let path_label = Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Start)
        .build();
    path_label.add_css_class("dim-label");
    path_label.add_css_class("caption");

    let scroller = ScrolledWindow::builder().vexpand(true).child(&view).build();

    let pane_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .build();
    pane_box.append(&header);
    pane_box.append(&path_label);
    pane_box.append(&scroller);

    let pane = Pane {
        model,
        selection,
        entries: Rc::new(RefCell::new(Vec::new())),
        path_label,
    };
    (pane_box, pane, view, header)
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
    if !local_pane {
        let s = state.clone();
        add_item(
            "Edit (auto-upload on save)",
            false,
            Box::new(move || s.menu_edit()),
        );
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
        if local_pane {
            s.local.selection.set_selected(index);
            s.local_menu_index.set(index);
        } else {
            s.remote.selection.set_selected(index);
            s.remote_menu_index.set(index);
        }
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }));
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
    drag.connect_prepare(move |_, _, _| {
        let index = selection.selected();
        let entry = entries.borrow().get(index as usize).cloned()?;
        Some(gdk::ContentProvider::for_value(
            &format!("{kind}:{}", entry.name).to_value(),
        ))
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

fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn join_posix(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
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
