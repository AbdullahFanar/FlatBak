//! Small widget builders shared between the backup and restore pages.

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::appdata::DataInfo;
use crate::util::format_size;

/// Wraps `child` in the standard page scaffolding: a header bar with a back
/// button plus an optional bottom bar.
pub fn page(
    title: &str,
    tag: &str,
    child: &impl IsA<gtk::Widget>,
    bottom: Option<&gtk::Widget>,
) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(child));
    if let Some(bottom) = bottom {
        view.add_bottom_bar(bottom);
    }

    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&view)
        .build()
}

/// A bottom action bar: a summary label on the left, a primary button on the right.
pub fn action_bar(button_label: &str) -> (gtk::Widget, gtk::Label, gtk::Button) {
    // `halign: Start` would make the label report a tiny natural width and wrap
    // long before it needs to; filling the space it is given and aligning the
    // text inside it keeps the summary on one line until the window is narrow.
    let label = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Center)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .build();
    label.add_css_class("dim-label");

    let button = gtk::Button::builder()
        .label(button_label)
        .halign(gtk::Align::End)
        .build();
    button.add_css_class("suggested-action");
    button.add_css_class("pill");

    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    label.set_hexpand(true);
    box_.append(&label);
    box_.append(&button);

    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .tightening_threshold(400)
        .child(&box_)
        .build();

    (clamp.upcast(), label, button)
}

/// The icon name to use for FlatBak itself.
///
/// The application's own icon is only in the theme once it has been installed,
/// so a build run straight from `cargo` falls back to a stock icon rather than
/// showing the "missing image" placeholder.
pub fn own_icon_name() -> &'static str {
    let installed = gdk::Display::default()
        .map(|display| gtk::IconTheme::for_display(&display).has_icon(crate::config::APP_ID))
        .unwrap_or(false);
    if installed {
        crate::config::APP_ID
    } else {
        "drive-harddisk-symbolic"
    }
}

/// An icon for one application, falling back to a generic symbol.
///
/// Flatpak exports application icons into the icon theme search path, so a
/// lookup by application ID finds the real icon for most apps.
pub fn app_icon(app_id: &str) -> gtk::Image {
    let icon = gtk::Image::builder().pixel_size(32).build();
    let has_icon = gdk::Display::default()
        .map(|display| gtk::IconTheme::for_display(&display).has_icon(app_id))
        .unwrap_or(false);
    if has_icon {
        icon.set_icon_name(Some(app_id));
    } else {
        icon.set_icon_name(Some("application-x-executable-symbolic"));
        icon.add_css_class("dim-label");
    }
    icon
}

/// A page shown while something is loading.
pub fn spinner(message: &str) -> gtk::Widget {
    let spinner = adw::Spinner::new();
    spinner.set_size_request(48, 48);

    let label = gtk::Label::builder().label(message).build();
    label.add_css_class("dim-label");

    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .vexpand(true)
        .build();
    box_.append(&spinner);
    box_.append(&label);
    box_.upcast()
}

/// Human readable description of an application's data on disk.
pub fn describe_data(info: &DataInfo, exclude_caches: bool) -> String {
    if !info.present {
        return "No application data found".to_owned();
    }
    let bytes = info.bytes_to_back_up(exclude_caches);
    if exclude_caches && info.cache_bytes > 0 {
        format!(
            "{} \u{2022} {} of cache excluded",
            format_size(bytes),
            format_size(info.cache_bytes)
        )
    } else {
        format_size(bytes)
    }
}

/// A collapsible details pane used by the progress and result pages.
pub struct DetailsLog {
    pub revealer: gtk::Revealer,
    view: gtk::TextView,
    count: std::cell::Cell<usize>,
}

impl DetailsLog {
    pub fn new(title: &str) -> Self {
        let view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .build();

        let scrolled = gtk::ScrolledWindow::builder()
            .min_content_height(140)
            .max_content_height(240)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&view)
            .build();

        let frame = gtk::Frame::builder().child(&scrolled).build();

        let heading = gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .build();
        heading.add_css_class("heading");

        let box_ = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        box_.append(&heading);
        box_.append(&frame);

        let revealer = gtk::Revealer::builder()
            .child(&box_)
            .reveal_child(false)
            .build();

        Self {
            revealer,
            view,
            count: std::cell::Cell::new(0),
        }
    }

    /// Appends a line, revealing the pane on the first one.
    pub fn append(&self, line: &str) {
        let buffer = self.view.buffer();
        let mut end = buffer.end_iter();
        if self.count.get() > 0 {
            buffer.insert(&mut end, "\n");
        }
        buffer.insert(&mut end, line);
        self.count.set(self.count.get() + 1);
        if !self.revealer.reveals_child() {
            self.revealer.set_reveal_child(true);
        }

        // Keep the newest line in view, but only after the next layout pass:
        // scrolling a view that has not been allocated yet computes the
        // position from a zero-sized widget and leaves the first lines hidden.
        let view = self.view.clone();
        glib::idle_add_local_once(move || {
            let buffer = view.buffer();
            let mark = buffer.create_mark(None, &buffer.end_iter(), false);
            view.scroll_mark_onscreen(&mark);
            buffer.delete_mark(&mark);
        });
    }
}
