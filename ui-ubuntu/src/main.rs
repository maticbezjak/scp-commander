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
    SingleSelection, StringList, StringObject,
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
    model: StringList,
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
}

/// Late-bound hook the row factories use to open the context menu.
type MenuHook = Rc<RefCell<Option<Box<dyn Fn(u32, f64, f64)>>>>;

struct App {
    cmd: mpsc::Sender<Cmd>,
    window: ApplicationWindow,
    local: Pane,
    remote: Pane,
    local_path: RefCell<PathBuf>,
    remote_path: RefCell<String>,
    connected: RefCell<bool>,
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
                        Entry {
                            name: e.file_name().to_string_lossy().into_owned(),
                            is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                            mtime: None,
                            perms: None,
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
        let path = self.remote_path.borrow().clone();
        *self.pending_connect.borrow_mut() = Some((creds.clone(), path.clone()));
        self.set_status("Connecting…");
        let _ = self.cmd.send(Cmd::Connect { creds, path });
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
            let path = join_posix(&self.remote_path.borrow(), &entry.name);
            let _ = self.cmd.send(Cmd::List { path });
        } else {
            self.download(&entry);
        }
    }

    fn remote_up(&self) {
        if !*self.connected.borrow() {
            return;
        }
        let parent = parent_posix(&self.remote_path.borrow());
        let _ = self.cmd.send(Cmd::List { path: parent });
    }

    fn refresh_remote(&self) {
        if *self.connected.borrow() {
            let path = self.remote_path.borrow().clone();
            let _ = self.cmd.send(Cmd::List { path });
        }
    }

    // -- Transfers ----------------------------------------------------------

    fn download(self: &Rc<Self>, entry: &Entry) {
        if !*self.connected.borrow() {
            return;
        }
        let remote = join_posix(&self.remote_path.borrow(), &entry.name);
        let local = self.local_path.borrow().join(&entry.name);
        if entry.is_dir {
            let (id, cancel) = self.add_transfer(&format!("{}/", entry.name), true, 0);
            let _ = self.cmd.send(Cmd::DownloadDir {
                id,
                name: entry.name.clone(),
                remote,
                local,
                cancel,
            });
        } else {
            let (id, cancel) = self.add_transfer(&entry.name, true, entry.size);
            let _ = self.cmd.send(Cmd::Download {
                id,
                name: entry.name.clone(),
                remote,
                local,
                cancel,
            });
        }
    }

    fn upload(self: &Rc<Self>, entry: &Entry) {
        if !*self.connected.borrow() {
            self.set_status("Connect first to upload");
            return;
        }
        let local = self.local_path.borrow().join(&entry.name);
        let remote = join_posix(&self.remote_path.borrow(), &entry.name);
        if entry.is_dir {
            let (id, cancel) = self.add_transfer(&format!("{}/", entry.name), false, 0);
            let _ = self.cmd.send(Cmd::UploadDir {
                id,
                name: entry.name.clone(),
                local,
                remote,
                cancel,
            });
        } else {
            let (id, cancel) = self.add_transfer(&entry.name, false, entry.size);
            let _ = self.cmd.send(Cmd::Upload {
                id,
                name: entry.name.clone(),
                local,
                remote,
                cancel,
            });
        }
    }

    fn sync(self: &Rc<Self>, download: bool) {
        if !*self.connected.borrow() {
            self.set_status("Connect first to sync");
            return;
        }
        let local = self.local_path.borrow().clone();
        let remote = self.remote_path.borrow().clone();
        let title = format!("Sync {} {}", if download { "⬇" } else { "⬆" }, remote);
        let (id, cancel) = self.add_transfer(&title, download, 0);
        let _ = self.cmd.send(Cmd::Sync { id, download, local, remote, cancel });
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
                    let base = state.remote_path.borrow().clone();
                    let _ = state.cmd.send(Cmd::Rename {
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
                    let path = join_posix(&state.remote_path.borrow(), &entry.name);
                    let _ = state.cmd.send(Cmd::Delete { path, is_dir: entry.is_dir });
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
        if !*self.connected.borrow() {
            return;
        }
        let remote = join_posix(&self.remote_path.borrow(), &entry.name);
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
        let _ = self.cmd.send(Cmd::Download {
            id,
            name: entry.name.clone(),
            remote,
            local,
            cancel,
        });
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
                let path = join_posix(&state.remote_path.borrow(), &name);
                let _ = state.cmd.send(Cmd::Mkdir { path });
            }
        });
    }

    // -- Edit watches ---------------------------------------------------------

    fn poll_edits(self: &Rc<Self>) {
        let changed: Vec<(String, PathBuf)> = {
            let mut edits = self.edits.borrow_mut();
            let mut out = Vec::new();
            for watch in edits.iter_mut() {
                let Ok(meta) = std::fs::metadata(&watch.local) else { continue };
                let Ok(mtime) = meta.modified() else { continue };
                if mtime > watch.last_mtime {
                    watch.last_mtime = mtime;
                    out.push((watch.remote.clone(), watch.local.clone()));
                }
            }
            out
        };
        for (remote, local) in changed {
            let name = local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
            let (id, cancel) = self.add_transfer(&name, false, size);
            let _ = self.cmd.send(Cmd::Upload { id, name, local, remote, cancel });
        }
    }

    // -- Sites --------------------------------------------------------------

    fn save_current_site(&self) {
        let host = self.host_entry.text().to_string();
        let user = self.user_entry.text().to_string();
        let name = if host.is_empty() {
            "New site".to_string()
        } else if user.is_empty() {
            host.clone()
        } else {
            format!("{user}@{host}")
        };
        let proto_idx = self.proto_dd.selected();
        let port = self.port_entry.text().to_string();
        self.sites.borrow_mut().add(Site {
            name: name.clone(),
            proto: proto_idx,
            host: host.clone(),
            port: port.clone(),
            user: user.clone(),
        });
        self.refresh_sites_list();

        let password = self.pass_entry.text().to_string();
        if !password.is_empty() && self.selected_auth() == 0 {
            let account = secrets::account(
                PROTO_LABELS[proto_idx as usize % 4],
                &user,
                &host,
                &port,
            );
            match secrets::save(&account, &password) {
                Ok(()) => self.set_status(&format!("Saved site “{name}” (password in keyring)")),
                Err(e) => self.set_status(&format!("Saved site “{name}” (keyring: {e})")),
            }
        } else {
            self.set_status(&format!("Saved site “{name}”"));
        }
    }

    fn load_site(&self, index: usize) {
        let Some(site) = self.sites.borrow().sites.get(index).cloned() else { return };
        self.proto_dd.set_selected(site.proto);
        self.host_entry.set_text(&site.host);
        self.port_entry.set_text(&site.port);
        self.user_entry.set_text(&site.user);
        let account = secrets::account(
            PROTO_LABELS[site.proto as usize % 4],
            &site.user,
            &site.host,
            &site.port,
        );
        if let Some(password) = secrets::load(&account) {
            self.pass_entry.set_text(&password);
            self.set_status(&format!("Loaded “{}” — password from keyring", site.name));
        } else {
            self.pass_entry.set_text("");
            self.set_status(&format!("Loaded “{}” — enter password and Connect", site.name));
        }
    }

    fn delete_site(&self, index: usize) {
        if let Some(site) = self.sites.borrow().sites.get(index).cloned() {
            secrets::delete(&secrets::account(
                PROTO_LABELS[site.proto as usize % 4],
                &site.user,
                &site.host,
                &site.port,
            ));
        }
        self.sites.borrow_mut().remove(index);
        self.refresh_sites_list();
    }

    fn refresh_sites_list(&self) {
        while let Some(row) = self.sites_list.first_child() {
            self.sites_list.remove(&row);
        }
        for site in &self.sites.borrow().sites {
            let label = Label::builder()
                .label(format!(
                    "{}\n{}",
                    site.name,
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

    fn handle_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Connected { path, entries } | Event::Listed { path, entries } => {
                *self.connected.borrow_mut() = true;
                let mut entries = entries;
                sort_entries(&mut entries);
                let count = entries.len();
                self.remote.show(&entries, &path);
                *self.remote_path.borrow_mut() = path.clone();
                self.set_status(&format!("{path} ({count} items)"));
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
                    });
                    return;
                }

                self.set_status(&format!(
                    "{} {name}",
                    if download { "Downloaded" } else { "Uploaded" }
                ));
                if download {
                    self.load_local();
                } else {
                    self.refresh_remote();
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
                self.refresh_remote();
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
        while self.model.n_items() > 0 {
            self.model.remove(0);
        }
        for e in entries {
            let line = if e.is_dir {
                format!("{}/", e.name)
            } else {
                format!("{}  ·  {}", e.name, human_size(e.size))
            };
            self.model.append(&line);
        }
        *self.entries.borrow_mut() = entries.to_vec();
    }
}

// ---------------------------------------------------------------------------
// UI assembly

fn build_ui(app: &Application) {
    let (event_tx, event_rx) = async_channel::unbounded::<Event>();
    let cmd = worker::spawn(event_tx);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("SCP Commander")
        .default_width(1120)
        .default_height(640)
        .build();

    // Connection bar --------------------------------------------------------
    let proto_dd = DropDown::from_strings(&PROTO_LABELS);
    let auth_dd = DropDown::from_strings(&AUTH_LABELS);
    let user_entry = GtkEntry::builder().placeholder_text("user").build();
    let host_entry = GtkEntry::builder().placeholder_text("host").hexpand(true).build();
    let port_entry = GtkEntry::builder().text("22").max_width_chars(5).width_chars(5).build();
    let pass_entry = PasswordEntry::builder().show_peek_icon(true).build();
    let key_entry = GtkEntry::builder()
        .placeholder_text("private key path")
        .visible(false)
        .build();
    let key_browse = Button::from_icon_name("document-open-symbolic");
    key_browse.set_visible(false);
    key_browse.set_tooltip_text(Some("Choose a private key"));
    // S3 only — hidden until the picker selects S3.
    let bucket_entry = GtkEntry::builder().placeholder_text("bucket").visible(false).build();
    let region_entry = GtkEntry::builder()
        .placeholder_text("region")
        .max_width_chars(10)
        .visible(false)
        .build();
    let connect_btn = Button::with_label("Connect");
    connect_btn.add_css_class("suggested-action");
    let sync_up_btn = Button::from_icon_name("go-up-symbolic");
    sync_up_btn.set_tooltip_text(Some("Sync local → remote (upload changes)"));
    let sync_down_btn = Button::from_icon_name("go-down-symbolic");
    sync_down_btn.set_tooltip_text(Some("Sync remote → local (download changes)"));

    let conn_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    conn_bar.append(&proto_dd);
    conn_bar.append(&auth_dd);
    conn_bar.append(&user_entry);
    conn_bar.append(&Label::new(Some("@")));
    conn_bar.append(&host_entry);
    conn_bar.append(&Label::new(Some(":")));
    conn_bar.append(&port_entry);
    conn_bar.append(&key_entry);
    conn_bar.append(&key_browse);
    conn_bar.append(&pass_entry);
    conn_bar.append(&bucket_entry);
    conn_bar.append(&region_entry);
    conn_bar.append(&connect_btn);
    conn_bar.append(&sync_up_btn);
    conn_bar.append(&sync_down_btn);

    // The pickers drive default port, S3/auth field visibility, placeholders.
    let update_form = glib::clone!(
        #[weak] proto_dd,
        #[weak] auth_dd,
        #[weak] port_entry,
        #[weak] bucket_entry,
        #[weak] region_entry,
        #[weak] user_entry,
        #[weak] host_entry,
        #[weak] key_entry,
        #[weak] key_browse,
        #[weak] pass_entry,
        move || {
            let selected = proto_dd.selected();
            let is_s3 = selected == 3;
            let is_sftp = selected == 0;
            let p = Credentials::default_port(proto_from_index(selected));
            port_entry.set_text(&p.to_string());
            bucket_entry.set_visible(is_s3);
            region_entry.set_visible(is_s3);
            auth_dd.set_visible(is_sftp);
            let auth = if is_sftp { auth_dd.selected() } else { 0 };
            key_entry.set_visible(is_sftp && auth == 1);
            key_browse.set_visible(is_sftp && auth == 1);
            pass_entry.set_visible(!(is_sftp && auth == 2));
            user_entry.set_placeholder_text(Some(if is_s3 { "access key" } else { "user" }));
            host_entry.set_placeholder_text(Some(if is_s3 {
                "endpoint (blank = AWS)"
            } else {
                "host"
            }));
        }
    );
    let hook = update_form.clone();
    proto_dd.connect_selected_notify(move |_| hook());
    let hook = update_form.clone();
    auth_dd.connect_selected_notify(move |_| hook());

    // Panes ------------------------------------------------------------------
    let local_hook: MenuHook = Rc::new(RefCell::new(None));
    let remote_hook: MenuHook = Rc::new(RefCell::new(None));
    let (local_widget, local_pane, local_view, local_up_btn, local_new_btn) =
        make_pane("Local", &local_hook);
    let (remote_widget, remote_pane, remote_view, remote_up_btn, remote_new_btn) =
        make_pane("Remote", &remote_hook);

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
    let sites_list = ListBox::builder().selection_mode(SelectionMode::None).build();
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
        .width_request(180)
        .build();
    sidebar.append(&sites_header);
    sidebar.append(&ScrolledWindow::builder().vexpand(true).child(&sites_list).build());

    // Root layout ---------------------------------------------------------------
    let content = GtkBox::builder().orientation(Orientation::Vertical).build();
    content.append(&conn_bar);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&hostkey_bar);
    content.append(&panes);
    content.append(&transfers_panel);
    content.append(&gtk::Separator::new(Orientation::Horizontal));
    content.append(&status);

    let root = GtkBox::builder().orientation(Orientation::Horizontal).build();
    root.append(&sidebar);
    root.append(&gtk::Separator::new(Orientation::Vertical));
    root.append(&content);

    let state = Rc::new(App {
        cmd,
        window: window.clone(),
        local: local_pane,
        remote: remote_pane,
        local_path: RefCell::new(glib::home_dir()),
        remote_path: RefCell::new("/".to_string()),
        connected: RefCell::new(false),
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
        edit_pending: RefCell::new(HashMap::new()),
        edits: RefCell::new(Vec::new()),
        sites: RefCell::new(SitesStore::load()),
        sites_list,
    });

    update_form();
    state.load_local();
    state.refresh_sites_list();

    // Context menus -------------------------------------------------------------
    setup_context_menu(&state, &local_view, &local_hook, true);
    setup_context_menu(&state, &remote_view, &remote_hook, false);

    // Wire signals ----------------------------------------------------------------
    connect_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.connect_clicked()
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
        move |_| state.save_current_site()
    ));
    state.sites_list.connect_row_activated(glib::clone!(
        #[strong] state,
        move |_, row| state.load_site(row.index().max(0) as usize)
    ));
    let sites_click = gtk::GestureClick::builder().button(3).build();
    sites_click.connect_pressed(glib::clone!(
        #[strong] state,
        move |gesture, _, _, y| {
            let list = gesture.widget().and_downcast::<ListBox>();
            if let Some(row) = list.and_then(|l| l.row_at_y(y as i32)) {
                state.delete_site(row.index().max(0) as usize);
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
    local_up_btn.connect_clicked(glib::clone!(#[strong] state, move |_| state.local_up()));
    remote_up_btn.connect_clicked(glib::clone!(#[strong] state, move |_| state.remote_up()));
    local_new_btn.connect_clicked(glib::clone!(#[strong] state, move |_| state.new_folder(true)));
    remote_new_btn.connect_clicked(glib::clone!(#[strong] state, move |_| state.new_folder(false)));

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

    // Worker event pump --------------------------------------------------------------
    glib::spawn_future_local(glib::clone!(
        #[strong] state,
        async move {
            while let Ok(event) = event_rx.recv().await {
                state.handle_event(event);
            }
        }
    ));

    window.set_child(Some(&root));
    window.present();
}

/// Build a titled pane: header (title + new-folder + up buttons), path label,
/// file list. Rows get a right-click gesture that fires the pane's MenuHook
/// with (row index, x, y) in view coordinates.
fn make_pane(
    title: &str,
    hook: &MenuHook,
) -> (GtkBox, Pane, ColumnView, Button, Button) {
    let model = StringList::new(&[]);
    let selection = SingleSelection::new(Some(model.clone()));
    let view = ColumnView::new(Some(selection.clone()));

    let factory = SignalListItemFactory::new();
    factory.connect_setup(glib::clone!(
        #[strong] hook,
        #[weak] view,
        move |_, item| {
            let item = item.downcast_ref::<ListItem>().unwrap().clone();
            let label = Label::builder().xalign(0.0).build();
            item.set_child(Some(&label));

            let gesture = gtk::GestureClick::builder().button(3).build();
            gesture.connect_pressed(glib::clone!(
                #[strong] hook,
                #[weak] view,
                #[weak] item,
                #[weak] label,
                move |_, _, x, y| {
                    let point = label.compute_point(
                        &view,
                        &gtk::graphene::Point::new(x as f32, y as f32),
                    );
                    if let (Some(p), Some(cb)) = (point, hook.borrow().as_ref()) {
                        cb(item.position(), p.x() as f64, p.y() as f64);
                    }
                }
            ));
            label.add_controller(gesture);
        }
    ));
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<ListItem>().unwrap();
        let label = item.child().and_downcast::<Label>().unwrap();
        let text = item
            .item()
            .and_downcast::<StringObject>()
            .map(|s| s.string().to_string())
            .unwrap_or_default();
        label.set_text(&text);
    });

    let column = ColumnViewColumn::new(Some("Name"), Some(factory));
    column.set_expand(true);
    view.append_column(&column);
    view.set_single_click_activate(false);

    let header = GtkBox::builder().orientation(Orientation::Horizontal).build();
    let title_label = Label::builder().label(title).xalign(0.0).hexpand(true).build();
    title_label.add_css_class("heading");
    let new_btn = Button::from_icon_name("folder-new-symbolic");
    new_btn.add_css_class("flat");
    new_btn.set_tooltip_text(Some("New folder"));
    let up_btn = Button::from_icon_name("go-up-symbolic");
    up_btn.add_css_class("flat");
    up_btn.set_tooltip_text(Some("Parent directory"));
    header.append(&title_label);
    header.append(&new_btn);
    header.append(&up_btn);

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
    (pane_box, pane, view, up_btn, new_btn)
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
