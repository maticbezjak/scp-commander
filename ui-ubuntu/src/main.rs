//! GTK4 native front-end for Ubuntu/Linux.
//!
//! Feature parity with the macOS app: dual-pane local/remote browsing with
//! navigation, protocol picker, a transfer queue with live progress, drag and
//! drop between panes, and a saved-sites sidebar.
//!
//! Build on Linux (or against Homebrew gtk4 on macOS for compile-checking):
//!   sudo apt install libgtk-4-dev build-essential
//!   cargo run -p scp-ubuntu

mod sites;
mod worker;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ColumnView, ColumnViewColumn,
    DragSource, DropDown, DropTarget, Entry as GtkEntry, Label, ListBox, ListItem, Orientation,
    PasswordEntry, ProgressBar, ScrolledWindow, SelectionMode, SignalListItemFactory,
    SingleSelection, StringList, StringObject,
};

use scp_core::types::{Auth, Credentials, Entry, HostKeyPolicy, Protocol};
use sites::{Site, SitesStore};
use worker::{Cmd, Event};

const APP_ID: &str = "net.manto.ScpCommander";

const PROTO_LABELS: [&str; 4] = ["SFTP", "FTP", "FTPS", "S3"];

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
    finished: bool,
}

struct App {
    cmd: mpsc::Sender<Cmd>,
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
    host_entry: GtkEntry,
    port_entry: GtkEntry,
    user_entry: GtkEntry,
    pass_entry: PasswordEntry,
    bucket_entry: GtkEntry,
    region_entry: GtkEntry,
    // Host key trust prompt
    hostkey_bar: GtkBox,
    hostkey_label: Label,
    pending_connect: RefCell<Option<(Credentials, String)>>,
    pending_fingerprint: RefCell<Option<String>>,
    // Sites
    sites: RefCell<SitesStore>,
    sites_list: ListBox,
}

impl App {
    fn set_status(&self, text: &str) {
        self.status.set_text(text);
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
        let mut creds = Credentials::basic(
            protocol,
            host,
            port,
            self.user_entry.text().to_string(),
            Auth::Password(self.pass_entry.text().to_string()),
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

    // -- Transfers ----------------------------------------------------------

    fn download(self: &Rc<Self>, entry: &Entry) {
        if entry.is_dir || !*self.connected.borrow() {
            return;
        }
        let remote = join_posix(&self.remote_path.borrow(), &entry.name);
        let local = self.local_path.borrow().join(&entry.name);
        let id = self.add_transfer(&entry.name, true, entry.size);
        let _ = self.cmd.send(Cmd::Download {
            id,
            name: entry.name.clone(),
            remote,
            local,
        });
    }

    fn upload(self: &Rc<Self>, entry: &Entry) {
        if entry.is_dir {
            return;
        }
        if !*self.connected.borrow() {
            self.set_status("Connect first to upload");
            return;
        }
        let local = self.local_path.borrow().join(&entry.name);
        let remote = join_posix(&self.remote_path.borrow(), &entry.name);
        let id = self.add_transfer(&entry.name, false, entry.size);
        let _ = self.cmd.send(Cmd::Upload {
            id,
            name: entry.name.clone(),
            local,
            remote,
        });
    }

    fn add_transfer(&self, name: &str, download: bool, total: u64) -> u64 {
        let id = {
            let mut next = self.next_id.borrow_mut();
            *next += 1;
            *next
        };

        let arrow = if download { "↓" } else { "↑" };
        let row = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .build();
        let name_label = Label::builder()
            .label(format!("{arrow} {name}"))
            .xalign(0.0)
            .width_chars(24)
            .max_width_chars(24)
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
        row.append(&name_label);
        row.append(&bar);
        self.transfers_box.prepend(&row);
        self.transfers_panel.set_visible(true);

        self.transfer_rows.borrow_mut().insert(
            id,
            TransferRow {
                container: row,
                bar,
                finished: false,
            },
        );
        id
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
        self.sites.borrow_mut().add(Site {
            name: name.clone(),
            proto: self.proto_dd.selected(),
            host,
            port: self.port_entry.text().to_string(),
            user,
        });
        self.refresh_sites_list();
        self.set_status(&format!("Saved site “{name}”"));
    }

    fn load_site(&self, index: usize) {
        let Some(site) = self.sites.borrow().sites.get(index).cloned() else { return };
        self.proto_dd.set_selected(site.proto);
        self.host_entry.set_text(&site.host);
        self.port_entry.set_text(&site.port);
        self.user_entry.set_text(&site.user);
        self.pass_entry.set_text("");
        self.set_status(&format!("Loaded “{}” — enter password and Connect", site.name));
    }

    fn delete_site(&self, index: usize) {
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
                        row.bar.set_fraction(done as f64 / total as f64);
                        row.bar
                            .set_text(Some(&format!("{} / {}", human_size(done), human_size(total))));
                    } else {
                        row.bar.pulse();
                        row.bar.set_text(Some(&human_size(done)));
                    }
                }
            }
            Event::Done { id, name, bytes, download } => {
                if let Some(row) = self.transfer_rows.borrow_mut().get_mut(&id) {
                    row.bar.set_fraction(1.0);
                    row.bar.set_text(Some(&format!("done — {}", human_size(bytes))));
                    row.finished = true;
                }
                if download {
                    self.load_local();
                    self.set_status(&format!("Downloaded {name}"));
                } else {
                    let path = self.remote_path.borrow().clone();
                    let _ = self.cmd.send(Cmd::List { path });
                    self.set_status(&format!("Uploaded {name}"));
                }
            }
            Event::Failed { id, message } => {
                if let Some(row) = self.transfer_rows.borrow_mut().get_mut(&id) {
                    row.bar.set_text(Some(&format!("failed: {message}")));
                    row.finished = true;
                }
                self.set_status(&format!("Error: {message}"));
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

    // Connection bar --------------------------------------------------------
    let proto_dd = DropDown::from_strings(&PROTO_LABELS);
    let user_entry = GtkEntry::builder().placeholder_text("user").build();
    let host_entry = GtkEntry::builder().placeholder_text("host").hexpand(true).build();
    let port_entry = GtkEntry::builder().text("22").max_width_chars(5).width_chars(5).build();
    let pass_entry = PasswordEntry::builder().show_peek_icon(true).build();
    // S3 only — hidden until the picker selects S3.
    let bucket_entry = GtkEntry::builder().placeholder_text("bucket").visible(false).build();
    let region_entry = GtkEntry::builder()
        .placeholder_text("region")
        .max_width_chars(10)
        .visible(false)
        .build();
    let connect_btn = Button::with_label("Connect");
    connect_btn.add_css_class("suggested-action");

    let conn_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    conn_bar.append(&proto_dd);
    conn_bar.append(&user_entry);
    conn_bar.append(&Label::new(Some("@")));
    conn_bar.append(&host_entry);
    conn_bar.append(&Label::new(Some(":")));
    conn_bar.append(&port_entry);
    conn_bar.append(&pass_entry);
    conn_bar.append(&bucket_entry);
    conn_bar.append(&region_entry);
    conn_bar.append(&connect_btn);

    // The picker drives the default port, S3 field visibility, and placeholders.
    proto_dd.connect_selected_notify(glib::clone!(
        #[weak] port_entry,
        #[weak] bucket_entry,
        #[weak] region_entry,
        #[weak] user_entry,
        #[weak] host_entry,
        move |dd| {
            let selected = dd.selected();
            let p = Credentials::default_port(proto_from_index(selected));
            port_entry.set_text(&p.to_string());
            let is_s3 = selected == 3;
            bucket_entry.set_visible(is_s3);
            region_entry.set_visible(is_s3);
            user_entry.set_placeholder_text(Some(if is_s3 { "access key" } else { "user" }));
            host_entry.set_placeholder_text(Some(if is_s3 {
                "endpoint (blank = AWS)"
            } else {
                "host"
            }));
        }
    ));

    // Panes ------------------------------------------------------------------
    let (local_widget, local_pane, local_view) = make_pane("Local");
    let (remote_widget, remote_pane, remote_view) = make_pane("Remote");

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

    // Host key trust bar (hidden until a strict connect meets a new server) --
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

    // Transfers panel (hidden until something is queued) ---------------------
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
        .max_content_height(120)
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

    // Sites sidebar ----------------------------------------------------------
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
    sidebar.append(
        &ScrolledWindow::builder().vexpand(true).child(&sites_list).build(),
    );

    // Root layout ------------------------------------------------------------
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
        host_entry,
        port_entry,
        user_entry,
        pass_entry,
        bucket_entry,
        region_entry,
        hostkey_bar,
        hostkey_label,
        pending_connect: RefCell::new(None),
        pending_fingerprint: RefCell::new(None),
        sites: RefCell::new(SitesStore::load()),
        sites_list,
    });

    state.load_local();
    state.refresh_sites_list();

    // Wire signals -----------------------------------------------------------
    connect_btn.connect_clicked(glib::clone!(
        #[strong] state,
        move |_| state.connect_clicked()
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
    // Right-click a site to delete it.
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

    // Double-click (activate) opens dirs / starts transfers.
    local_view.connect_activate(glib::clone!(
        #[strong] state,
        move |_, position| state.open_local(position)
    ));
    remote_view.connect_activate(glib::clone!(
        #[strong] state,
        move |_, position| state.open_remote(position)
    ));

    // Up buttons live in the pane headers (created in make_pane).
    if let Some(btn) = find_up_button(&local_widget) {
        btn.connect_clicked(glib::clone!(#[strong] state, move |_| state.local_up()));
    }
    if let Some(btn) = find_up_button(&remote_widget) {
        btn.connect_clicked(glib::clone!(#[strong] state, move |_| state.remote_up()));
    }

    // Drag and drop between panes -------------------------------------------
    // Drag carries "local:<name>" / "remote:<name>"; the selection is the row
    // being dragged (press selects before the drag threshold is crossed).
    add_drag_source(&local_view, "local", &state.local);
    add_drag_source(&remote_view, "remote", &state.remote);

    let local_drop = DropTarget::new(glib::types::Type::STRING, gdk::DragAction::COPY);
    local_drop.connect_drop(glib::clone!(
        #[strong] state,
        move |_, value, _, _| {
            if let Some(name) = value.get::<String>().ok().and_then(|s| s.strip_prefix("remote:").map(str::to_string)) {
                if let Some(entry) = state.remote.entries.borrow().iter().find(|e| e.name == name).cloned() {
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
            if let Some(name) = value.get::<String>().ok().and_then(|s| s.strip_prefix("local:").map(str::to_string)) {
                if let Some(entry) = state.local.entries.borrow().iter().find(|e| e.name == name).cloned() {
                    state.upload(&entry);
                    return true;
                }
            }
            false
        }
    ));
    remote_view.add_controller(remote_drop);

    // Worker event pump -----------------------------------------------------
    glib::spawn_future_local(glib::clone!(
        #[strong] state,
        async move {
            while let Ok(event) = event_rx.recv().await {
                state.handle_event(event);
            }
        }
    ));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("SCP Commander")
        .default_width(1080)
        .default_height(620)
        .child(&root)
        .build();
    window.present();
}

/// Build a titled pane: header (title + up button), path label, file list.
fn make_pane(title: &str) -> (GtkBox, Pane, ColumnView) {
    let model = StringList::new(&[]);
    let selection = SingleSelection::new(Some(model.clone()));

    let factory = SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = Label::builder().xalign(0.0).build();
        item.downcast_ref::<ListItem>().unwrap().set_child(Some(&label));
    });
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
    let view = ColumnView::new(Some(selection.clone()));
    view.append_column(&column);
    view.set_single_click_activate(false);

    let header = GtkBox::builder().orientation(Orientation::Horizontal).build();
    let title_label = Label::builder().label(title).xalign(0.0).hexpand(true).build();
    title_label.add_css_class("heading");
    let up_btn = Button::from_icon_name("go-up-symbolic");
    up_btn.add_css_class("flat");
    up_btn.set_widget_name("up-button");
    up_btn.set_tooltip_text(Some("Parent directory"));
    header.append(&title_label);
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
    (pane_box, pane, view)
}

/// Drag from a pane: payload is "<kind>:<name>" for the selected row.
fn add_drag_source(view: &ColumnView, kind: &'static str, pane: &Pane) {
    let drag = DragSource::builder().actions(gdk::DragAction::COPY).build();
    let entries = pane.entries.clone();
    let selection = pane.selection.clone();
    drag.connect_prepare(move |_, _, _| {
        let index = selection.selected();
        let entry = entries.borrow().get(index as usize).cloned()?;
        if entry.is_dir {
            return None; // only single files transfer for now
        }
        Some(gdk::ContentProvider::for_value(
            &format!("{kind}:{}", entry.name).to_value(),
        ))
    });
    view.add_controller(drag);
}

/// Locate the "up" button planted in a pane header by make_pane.
fn find_up_button(pane_box: &GtkBox) -> Option<Button> {
    let header = pane_box.first_child()?;
    let mut child = header.first_child();
    while let Some(widget) = child {
        if widget.widget_name() == "up-button" {
            return widget.downcast::<Button>().ok();
        }
        child = widget.next_sibling();
    }
    None
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
