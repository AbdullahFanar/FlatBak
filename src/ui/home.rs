//! The home page: the two primary actions and the recent backups list.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::backup;
use crate::config;
use crate::ui::{backup_page, dialogs, restore_page, widgets, Ui};
use crate::util::format_size;

/// Builds the home page and kicks off the Flatpak availability probe.
pub fn build(ui: &Rc<Ui>) -> adw::NavigationPage {
    let banner = adw::Banner::builder().revealed(false).build();

    let icon_name = widgets::own_icon_name();
    let icon = gtk::Image::builder()
        .icon_name(icon_name)
        .pixel_size(96)
        .margin_bottom(6)
        .build();
    if icon_name != config::APP_ID {
        icon.add_css_class("dim-label");
    }

    let title = gtk::Label::builder().label(config::APP_NAME).build();
    title.add_css_class("title-1");

    let subtitle = gtk::Label::builder()
        .label("Back up your Flatpak applications and their data, then put them back after a reinstall.")
        .justify(gtk::Justification::Center)
        .wrap(true)
        .max_width_chars(42)
        .build();
    subtitle.add_css_class("dim-label");

    let backup_button = gtk::Button::builder()
        .label("Create Backup")
        .halign(gtk::Align::Center)
        .build();
    backup_button.add_css_class("suggested-action");
    backup_button.add_css_class("pill");
    backup_button.set_size_request(220, -1);

    let restore_button = gtk::Button::builder()
        .label("Restore Backup")
        .halign(gtk::Align::Center)
        .build();
    restore_button.add_css_class("pill");
    restore_button.set_size_request(220, -1);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .build();
    buttons.append(&backup_button);
    buttons.append(&restore_button);

    let recent_group = adw::PreferencesGroup::builder()
        .title("Recent Backups")
        .margin_top(36)
        .visible(false)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .valign(gtk::Align::Start)
        .margin_top(36)
        .margin_bottom(36)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&icon);
    content.append(&title);
    content.append(&subtitle);
    content.append(&buttons);
    content.append(&recent_group);

    let clamp = adw::Clamp::builder()
        .maximum_size(560)
        .tightening_threshold(400)
        .child(&content)
        .build();

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    body.append(&banner);
    body.append(&scrolled);

    let menu = gtk::gio::Menu::new();
    menu.append(Some("_About FlatBak"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Main Menu")
        .primary(true)
        .build();

    let header = adw::HeaderBar::new();
    header.pack_end(&menu_button);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));

    let page = adw::NavigationPage::builder()
        .title(config::APP_NAME)
        .tag("home")
        .child(&view)
        .build();

    backup_button.connect_clicked({
        let ui = Rc::clone(ui);
        move |_| backup_page::start(&ui)
    });
    restore_button.connect_clicked({
        let ui = Rc::clone(ui);
        move |_| {
            dialogs::choose_backup(ui.window.upcast_ref(), {
                let ui = Rc::clone(&ui);
                move |path| restore_page::open(&ui, &path)
            })
        }
    });

    // The list changes after a backup is made, and files can disappear while
    // the app is open, so rebuild it each time the page comes back into view.
    page.connect_shown({
        let ui = Rc::clone(ui);
        let recent_group = recent_group.clone();
        move |_| refresh_recent(&ui, &recent_group)
    });
    refresh_recent(ui, &recent_group);

    probe(ui, &banner, &backup_button, &restore_button);

    page
}

/// Checks that Flatpak is present, disabling the actions if it is not.
fn probe(
    ui: &Rc<Ui>,
    banner: &adw::Banner,
    backup_button: &gtk::Button,
    restore_button: &gtk::Button,
) {
    backup_button.set_sensitive(false);
    restore_button.set_sensitive(false);

    let (sender, receiver) = async_channel::bounded(1);
    let flatpak = ui.flatpak.clone();
    std::thread::spawn(move || {
        let _ = sender.send_blocking(flatpak.version().map_err(|error| format!("{error:#}")));
    });

    glib::spawn_future_local({
        let ui = Rc::clone(ui);
        let banner = banner.clone();
        let backup_button = backup_button.clone();
        let restore_button = restore_button.clone();
        async move {
            let Ok(outcome) = receiver.recv().await else {
                return;
            };
            let ok = outcome.is_ok();
            match &outcome {
                Ok(_) => banner.set_revealed(false),
                Err(message) => {
                    banner.set_title(&format!("Flatpak is not available. {message}"));
                    banner.set_revealed(true);
                }
            }
            *ui.flatpak_version.borrow_mut() = Some(outcome);
            backup_button.set_sensitive(ok);
            // Opening an archive to inspect it works without Flatpak; only the
            // install step needs it, and that failure is reported per app.
            restore_button.set_sensitive(true);
        }
    });
}

/// Rebuilds the recent backups list.
fn refresh_recent(ui: &Rc<Ui>, group: &adw::PreferencesGroup) {
    // PreferencesGroup has no "remove all", so collect the rows we added.
    let mut existing: Vec<gtk::Widget> = Vec::new();
    let mut child = group.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        collect_rows(&widget, &mut existing);
    }
    for row in existing {
        if let Ok(row) = row.downcast::<adw::ActionRow>() {
            group.remove(&row);
        }
    }

    let entries = ui.recent.borrow().entries.clone();
    group.set_visible(!entries.is_empty());

    for entry in entries {
        let available = entry.is_available();
        let mut parts: Vec<String> = Vec::new();
        if entry.app_count > 0 {
            parts.push(format!(
                "{} app{}",
                entry.app_count,
                if entry.app_count == 1 { "" } else { "s" }
            ));
        }
        if entry.archive_bytes > 0 {
            parts.push(format_size(entry.archive_bytes));
        }
        if !entry.created_at.is_empty() {
            parts.push(backup::format_relative(&entry.created_at));
        }
        if !available {
            parts.push("file not found".to_owned());
        }

        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&entry.file_name()))
            .subtitle(glib::markup_escape_text(&parts.join(" \u{2022} ")))
            .activatable(available)
            .tooltip_text(entry.path.display().to_string())
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(if available {
            "document-save-symbolic"
        } else {
            "dialog-warning-symbolic"
        }));

        let forget = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Remove from this list")
            .build();
        forget.add_css_class("flat");
        forget.connect_clicked({
            let ui = Rc::clone(ui);
            let group = group.clone();
            let path = entry.path.clone();
            move |_| {
                ui.recent.borrow_mut().forget(&path);
                refresh_recent(&ui, &group);
            }
        });
        row.add_suffix(&forget);

        if available {
            row.connect_activated({
                let ui = Rc::clone(ui);
                let path: PathBuf = entry.path.clone();
                move |_| restore_page::open(&ui, &path)
            });
        } else {
            row.add_css_class("dim-label");
        }

        group.add(&row);
    }
}

/// Walks a widget subtree collecting the action rows a group contains.
fn collect_rows(widget: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    if widget.is::<adw::ActionRow>() {
        out.push(widget.clone());
        return;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        collect_rows(&current, out);
    }
}
