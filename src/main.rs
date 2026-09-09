//! FlatBak: back up and restore Flatpak applications and their user data.

use adw::prelude::*;
use flatbak::{config, ui};
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    let application = adw::Application::builder()
        .application_id(config::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    application.connect_activate(|application| {
        present(application);
    });

    // `flatbak some-backup.flatbak` jumps straight into that archive's
    // restore flow, which is what double-clicking one in a file manager does.
    application.connect_open(|application, files, _| {
        let window = present(application);
        if let Some(path) = files.first().and_then(|file| file.path()) {
            let target = path.to_string_lossy().to_variant();
            if let Err(error) = window.activate_action("open-backup", Some(&target)) {
                eprintln!("flatbak: could not open {}: {error}", path.display());
            }
        }
    });

    application.run()
}

/// Returns the existing window, creating it on first use.
fn present(application: &adw::Application) -> gtk::Window {
    if let Some(window) = application.active_window() {
        window.present();
        return window;
    }
    let ui = ui::Ui::build(application);
    ui.window.clone().upcast()
}
