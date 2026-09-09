//! The progress page shared by the backup and restore flows.

use adw::prelude::*;

use crate::ui::widgets::DetailsLog;

/// A page showing one running operation: a stage, a bar, a detail line, a
/// collapsible log and a cancel button.
pub struct ProgressPage {
    pub page: adw::NavigationPage,
    pub bar: gtk::ProgressBar,
    pub stage: gtk::Label,
    pub detail: gtk::Label,
    pub log: DetailsLog,
    pub cancel: gtk::Button,
}

impl ProgressPage {
    pub fn new(title: &str, tag: &str, stage_text: &str) -> Self {
        let stage = gtk::Label::builder()
            .label(stage_text)
            .halign(gtk::Align::Start)
            .wrap(true)
            .xalign(0.0)
            .build();
        stage.add_css_class("title-4");

        let bar = gtk::ProgressBar::builder().show_text(false).build();

        let detail = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .single_line_mode(true)
            .build();
        detail.add_css_class("dim-label");
        detail.add_css_class("caption");

        let log = DetailsLog::new("Details");

        let cancel = gtk::Button::builder()
            .label("Cancel")
            .halign(gtk::Align::Center)
            .margin_top(12)
            .build();
        cancel.add_css_class("pill");

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&stage);
        content.append(&bar);
        content.append(&detail);
        content.append(&log.revealer);
        content.append(&cancel);

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

        // No back button: leaving mid-operation would strand the worker thread.
        let header = adw::HeaderBar::builder().show_back_button(false).build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&scrolled));

        let page = adw::NavigationPage::builder()
            .title(title)
            .tag(tag)
            .can_pop(false)
            .child(&view)
            .build();

        Self {
            page,
            bar,
            stage,
            detail,
            log,
            cancel,
        }
    }

    pub fn set_stage(&self, text: &str) {
        self.stage.set_label(text);
    }

    pub fn set_detail(&self, text: &str) {
        self.detail.set_label(text);
    }

    /// Sets the bar to a known fraction, or pulses when the total is unknown.
    pub fn set_progress(&self, done: u64, total: u64) {
        if total > 0 {
            self.bar
                .set_fraction((done as f64 / total as f64).clamp(0.0, 1.0));
        } else {
            self.bar.pulse();
        }
    }

    pub fn set_fraction(&self, fraction: f64) {
        self.bar.set_fraction(fraction.clamp(0.0, 1.0));
    }

    /// Switches a cancel button into its "stopping" state.
    ///
    /// Takes the button rather than `&self` so the click handler does not have
    /// to capture the page that owns it, which would be a reference cycle.
    pub fn mark_cancelling(button: &gtk::Button) {
        button.set_sensitive(false);
        button.set_label("Cancelling\u{2026}");
    }
}

/// A page reporting the outcome of a finished operation.
pub struct ResultPage {
    pub page: adw::NavigationPage,
    pub log: DetailsLog,
    pub buttons: gtk::Box,
}

impl ResultPage {
    pub fn new(title: &str, tag: &str, icon: &str, heading: &str, description: &str) -> Self {
        let log = DetailsLog::new("Details");

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Center)
            .margin_top(12)
            .build();

        let extra = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();
        extra.append(&log.revealer);
        extra.append(&buttons);

        let status = adw::StatusPage::builder()
            .icon_name(icon)
            .title(heading)
            .description(description)
            .child(&extra)
            .build();

        let clamp = adw::Clamp::builder()
            .maximum_size(560)
            .tightening_threshold(400)
            .child(&status)
            .build();

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        let header = adw::HeaderBar::builder().show_back_button(false).build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&scrolled));

        let page = adw::NavigationPage::builder()
            .title(title)
            .tag(tag)
            .child(&view)
            .build();

        Self {
            page,
            log,
            buttons,
        }
    }

    pub fn add_button(&self, label: &str, css: &[&str]) -> gtk::Button {
        let button = gtk::Button::builder().label(label).build();
        for class in css {
            button.add_css_class(class);
        }
        button.add_css_class("pill");
        self.buttons.append(&button);
        button
    }
}
