//! Dialogs and file choosers.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config;

/// Shows a message the user has to acknowledge.
pub fn error(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(parent));
}

/// Asks a yes/no question, calling `on_confirm` if the user agrees.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm_label: &str,
    destructive: bool,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", confirm_label);
    dialog.set_response_appearance(
        "confirm",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "confirm" {
            on_confirm();
        }
    });
    dialog.present(Some(parent));
}

pub fn about(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::AboutDialog::builder()
        .application_name(config::APP_NAME)
        .application_icon(super::widgets::own_icon_name())
        .version(config::VERSION)
        .developer_name("Abdullah AL-Swedi")
        .copyright("\u{a9} 2026 Abdullah AL-Swedi")
        .license_type(gtk::License::MitX11)
        .website("https://github.com/AbdullahFanar/FlatBak")
        .issue_url("https://github.com/AbdullahFanar/FlatBak/issues")
        .comments(
            "Back up your Flatpak applications and their data, \
             then put them back after a reinstall.",
        )
        .build();
    dialog.present(Some(parent));
}

/// A file filter matching `.flatbak` archives.
fn backup_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("FlatBak Backups"));
    filter.add_pattern(&format!("*.{}", config::BACKUP_EXTENSION));
    filter
}

fn filter_model() -> gio::ListStore {
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&backup_filter());
    let all = gtk::FileFilter::new();
    all.set_name(Some("All Files"));
    all.add_pattern("*");
    filters.append(&all);
    filters
}

/// Asks where to write a new backup.
pub fn choose_destination(
    parent: &gtk::Window,
    suggested_name: &str,
    on_chosen: impl Fn(PathBuf) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Save Backup As")
        .modal(true)
        .initial_name(suggested_name)
        .filters(&filter_model())
        .default_filter(&backup_filter())
        .build();

    if let Some(home) = glib::home_dir().to_str() {
        dialog.set_initial_folder(Some(&gio::File::for_path(home)));
    }

    dialog.save(Some(parent), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                on_chosen(crate::util::with_backup_extension(path));
            }
        }
    });
}

/// Asks which backup to open.
pub fn choose_backup(parent: &gtk::Window, on_chosen: impl Fn(PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder()
        .title("Open Backup")
        .modal(true)
        .filters(&filter_model())
        .default_filter(&backup_filter())
        .build();

    dialog.open(Some(parent), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                on_chosen(path);
            }
        }
    });
}

/// Opens the file manager at the folder containing `path`.
pub fn show_in_files(parent: &gtk::Window, path: &Path) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    launcher.open_containing_folder(Some(parent), gio::Cancellable::NONE, |_| {});
}
