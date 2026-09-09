//! The main window and the state shared across its pages.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::config;
use crate::flatpak::{Flatpak, InstalledApp, Remote};
use crate::recent::RecentStore;
use crate::util::Cancel;

use super::{dialogs, home, restore_page};

/// Everything the pages need to reach: the window, the navigation stack, the
/// Flatpak connection and the recent-backups list.
pub struct Ui {
    pub window: adw::ApplicationWindow,
    pub nav: adw::NavigationView,
    pub toasts: adw::ToastOverlay,
    pub flatpak: Flatpak,
    /// `None` until the startup probe finishes; `Some(Err)` if Flatpak is unusable.
    pub flatpak_version: RefCell<Option<Result<String, String>>>,
    pub recent: RefCell<RecentStore>,
    /// Cancellation handle for the operation currently running, if any.
    pub running: RefCell<Option<Cancel>>,
}

/// Result of the one-off scan of the host's Flatpak state.
pub struct HostScan {
    pub apps: Vec<InstalledApp>,
    pub remotes: Vec<Remote>,
}

impl Ui {
    /// Builds the window and shows the home page.
    pub fn build(application: &adw::Application) -> Rc<Self> {
        super::register_icon_paths();

        let nav = adw::NavigationView::new();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&nav));

        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title(config::APP_NAME)
            .default_width(720)
            .default_height(680)
            .width_request(360)
            .height_request(320)
            .content(&toasts)
            .build();

        let ui = Rc::new(Self {
            window,
            nav,
            toasts,
            flatpak: Flatpak::detect(),
            flatpak_version: RefCell::new(None),
            recent: RefCell::new(RecentStore::load()),
            running: RefCell::new(None),
        });

        ui.install_actions();
        let home = home::build(&ui);
        ui.nav.add(&home);

        // A running backup or restore must not be lost to a stray window close.
        ui.window.connect_close_request({
            let ui = Rc::clone(&ui);
            move |_| {
                if ui.running.borrow().is_some() {
                    ui.confirm_quit();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });

        ui.window.present();
        ui
    }

    fn install_actions(self: &Rc<Self>) {
        let about = gtk::gio::SimpleAction::new("about", None);
        about.connect_activate({
            let ui = Rc::clone(self);
            move |_, _| dialogs::about(&ui.window)
        });
        self.window.add_action(&about);

        // Used when a `.flatbak` file is opened from outside the app.
        let open_backup =
            gtk::gio::SimpleAction::new("open-backup", Some(glib::VariantTy::STRING));
        open_backup.connect_activate({
            let ui = Rc::clone(self);
            move |_, parameter| {
                let Some(path) = parameter.and_then(glib::Variant::str) else {
                    return;
                };
                ui.go_home();
                restore_page::open(&ui, std::path::Path::new(path));
            }
        });
        self.window.add_action(&open_backup);
    }

    /// Shows a transient message at the bottom of the window.
    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Shows a blocking error dialog.
    pub fn error(&self, heading: &str, body: &str) {
        dialogs::error(&self.window, heading, body);
    }

    /// Pushes a page onto the navigation stack.
    pub fn push(&self, page: &adw::NavigationPage) {
        self.nav.push(page);
    }

    /// Returns to the home page, discarding any intermediate pages.
    pub fn go_home(&self) {
        self.nav.pop_to_tag("home");
    }

    pub fn flatpak_version_string(&self) -> String {
        match self.flatpak_version.borrow().as_ref() {
            Some(Ok(version)) => version.clone(),
            _ => String::new(),
        }
    }

    /// Marks an operation as running so the window guards against closing.
    pub fn set_running(&self, cancel: Option<Cancel>) {
        *self.running.borrow_mut() = cancel;
    }

    /// Reads the host's installed applications and remotes on a worker thread.
    pub fn scan_host(self: &Rc<Self>, on_done: impl Fn(Result<HostScan, String>) + 'static) {
        let (sender, receiver) = async_channel::bounded(1);
        let flatpak = self.flatpak.clone();
        std::thread::spawn(move || {
            let outcome = flatpak
                .list_apps()
                .and_then(|apps| {
                    let remotes = flatpak.list_remotes()?;
                    Ok(HostScan { apps, remotes })
                })
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send_blocking(outcome);
        });
        glib::spawn_future_local(async move {
            if let Ok(outcome) = receiver.recv().await {
                on_done(outcome);
            }
        });
    }

    fn confirm_quit(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::builder()
            .heading("Stop the current operation?")
            .body(
                "A backup or restore is still running. \
                 Closing the window now will stop it.",
            )
            .build();
        dialog.add_response("cancel", "Keep Running");
        dialog.add_response("stop", "Stop and Close");
        dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(None, {
            let ui = Rc::clone(self);
            move |_, response| {
                if response == "stop" {
                    if let Some(cancel) = ui.running.borrow().as_ref() {
                        cancel.cancel();
                    }
                    ui.set_running(None);
                    ui.window.close();
                }
            }
        });
        dialog.present(Some(&self.window));
    }
}
