//! GTK4 native front-end for Ubuntu/Linux.
//!
//! Dual-pane "commander" layout: local files on the left, remote on the right,
//! with a connection bar on top. It links the shared `scp-core` crate directly
//! (same workspace), so all protocol logic is shared with the macOS app.
//!
//! Build on Linux only (needs GTK4 dev libs):
//!   sudo apt install libgtk-4-dev build-essential
//!   cargo run -p scp-ubuntu

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ColumnView, ColumnViewColumn, Entry,
    Label, ListItem, Orientation, PasswordEntry, ScrolledWindow, SignalListItemFactory,
    SingleSelection, StringList,
};

use scp_core::types::{Auth, Credentials, Protocol};
use scp_core::{connect, Transport};

const APP_ID: &str = "net.manto.ScpCommander";

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    // Connection bar -------------------------------------------------------
    let host_entry = Entry::builder().placeholder_text("host").build();
    let user_entry = Entry::builder().placeholder_text("user").build();
    let pass_entry = PasswordEntry::builder().show_peek_icon(true).build();
    let path_entry = Entry::builder().text("/").hexpand(true).build();
    let connect_btn = Button::with_label("Connect");

    let conn_bar = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    conn_bar.append(&Label::new(Some("sftp://")));
    conn_bar.append(&user_entry);
    conn_bar.append(&Label::new(Some("@")));
    conn_bar.append(&host_entry);
    conn_bar.append(&pass_entry);
    conn_bar.append(&path_entry);
    conn_bar.append(&connect_btn);

    // Dual panes -----------------------------------------------------------
    let (local_pane, _local_model) = make_pane("Local");
    let (remote_pane, remote_model) = make_pane("Remote");

    let panes = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(6)
        .vexpand(true)
        .homogeneous(true)
        .build();
    panes.append(&local_pane);
    panes.append(&remote_pane);

    let status = Label::builder().xalign(0.0).label("Not connected").build();

    let root = GtkBox::builder().orientation(Orientation::Vertical).build();
    root.append(&conn_bar);
    root.append(&panes);
    root.append(&status);

    // A connection lives for the lifetime of the window.
    let session: Rc<RefCell<Option<Box<dyn Transport>>>> = Rc::new(RefCell::new(None));

    connect_btn.connect_clicked(glib::clone!(
        #[strong] session,
        #[strong] remote_model,
        #[strong] status,
        #[weak] host_entry,
        #[weak] user_entry,
        #[weak] pass_entry,
        #[weak] path_entry,
        move |_| {
            let creds = Credentials::basic(
                Protocol::Sftp,
                host_entry.text().to_string(),
                22,
                user_entry.text().to_string(),
                Auth::Password(pass_entry.text().to_string()),
            );
            match connect(&creds) {
                Ok(mut t) => {
                    let path = path_entry.text().to_string();
                    match t.list_dir(&path) {
                        Ok(mut entries) => {
                            entries.sort_by(|a, b| (b.is_dir, &a.name).cmp(&(a.is_dir, &b.name)));
                            fill_model(&remote_model, &entries);
                            status.set_text(&format!(
                                "Connected — {} ({} items)",
                                path,
                                entries.len()
                            ));
                        }
                        Err(e) => status.set_text(&format!("List failed: {e}")),
                    }
                    *session.borrow_mut() = Some(t);
                }
                Err(e) => status.set_text(&format!("Connect failed: {e}")),
            }
        }
    ));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("SCP Commander")
        .default_width(900)
        .default_height(560)
        .child(&root)
        .build();
    window.present();
}

/// Build a single titled pane containing a one-column file list.
/// Returns the pane widget and the backing string model to fill later.
fn make_pane(title: &str) -> (GtkBox, StringList) {
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
            .and_downcast::<gtk::StringObject>()
            .map(|s| s.string().to_string())
            .unwrap_or_default();
        label.set_text(&text);
    });

    let column = ColumnViewColumn::new(Some("Name"), Some(factory));
    column.set_expand(true);
    let view = ColumnView::new(Some(selection));
    view.append_column(&column);

    let scroller = ScrolledWindow::builder().vexpand(true).child(&view).build();

    let pane = GtkBox::builder().orientation(Orientation::Vertical).spacing(4).build();
    pane.append(&Label::builder().label(title).xalign(0.0).build());
    pane.append(&scroller);
    (pane, model)
}

fn fill_model(model: &StringList, entries: &[scp_core::Entry]) {
    while model.n_items() > 0 {
        model.remove(0);
    }
    for e in entries {
        let suffix = if e.is_dir { "/" } else { "" };
        model.append(&format!("{}{}", e.name, suffix));
    }
}
