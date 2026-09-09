//! The backup flow: choose applications, choose a destination, review, run.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::appdata::{self, DataInfo};
use crate::backup::writer::{self, AppSelection, BackupProgress, BackupReport, BackupRequest};
use crate::config;
use crate::error::{is_cancellation, Issue};
use crate::flatpak::{InstalledApp, Remote};
use crate::recent::RecentBackup;
use crate::ui::progress::{ProgressPage, ResultPage};
use crate::ui::widgets::{self, DetailsLog};
use crate::ui::{dialogs, Ui};
use crate::util::{format_size, Cancel};

/// One row of the application list, with its live selection state.
struct AppRow {
    app: InstalledApp,
    info: DataInfo,
    row: adw::ExpanderRow,
    check: gtk::CheckButton,
    data_row: adw::SwitchRow,
}

/// State of the whole flow, shared between its pages.
struct Flow {
    ui: Rc<Ui>,
    rows: RefCell<Vec<AppRow>>,
    remotes: RefCell<Vec<Remote>>,
    exclude_caches: Cell<bool>,
    destination: RefCell<Option<PathBuf>>,
    /// Set while a programmatic update is in flight, to stop toggle handlers
    /// from recursing through their own changes.
    updating: Cell<bool>,
    summary_label: RefCell<Option<gtk::Label>>,
    continue_button: RefCell<Option<gtk::Button>>,
}

impl Flow {
    /// Everything currently selected, in list order.
    fn selections(&self) -> Vec<AppSelection> {
        self.rows
            .borrow()
            .iter()
            .filter(|row| row.check.is_active())
            .map(|row| AppSelection {
                app: row.app.clone(),
                include_data: row.data_row.is_active() && row.info.present,
            })
            .collect()
    }

    fn selected_count(&self) -> usize {
        self.rows
            .borrow()
            .iter()
            .filter(|row| row.check.is_active())
            .count()
    }

    /// Bytes of application data the current selection would write.
    fn selected_data_bytes(&self) -> u64 {
        let exclude = self.exclude_caches.get();
        self.rows
            .borrow()
            .iter()
            .filter(|row| row.check.is_active() && row.data_row.is_active())
            .map(|row| row.info.bytes_to_back_up(exclude))
            .sum()
    }

    /// Refreshes subtitles, row sensitivity and the footer summary.
    fn refresh(&self) {
        if self.updating.get() {
            return;
        }
        let exclude = self.exclude_caches.get();
        for row in self.rows.borrow().iter() {
            let selected = row.check.is_active();
            row.data_row.set_sensitive(selected && row.info.present);
            row.data_row
                .set_subtitle(&widgets::describe_data(&row.info, exclude));
            // Dim the whole row when it will not be backed up.
            if selected {
                row.row.remove_css_class("dim-label");
            } else {
                row.row.add_css_class("dim-label");
            }
        }

        let count = self.selected_count();
        let bytes = self.selected_data_bytes();
        let total = self.rows.borrow().len();
        if let Some(label) = self.summary_label.borrow().as_ref() {
            let text = if count == 0 {
                format!("No applications selected of {total}")
            } else {
                format!(
                    "{count} of {total} selected \u{2022} {} of data",
                    format_size(bytes)
                )
            };
            label.set_label(&text);
        }
        if let Some(button) = self.continue_button.borrow().as_ref() {
            button.set_sensitive(count > 0);
        }
    }
}

/// Entry point from the home page.
pub fn start(ui: &Rc<Ui>) {
    let flow = Rc::new(Flow {
        ui: Rc::clone(ui),
        rows: RefCell::new(Vec::new()),
        remotes: RefCell::new(Vec::new()),
        exclude_caches: Cell::new(true),
        destination: RefCell::new(None),
        updating: Cell::new(false),
        summary_label: RefCell::new(None),
        continue_button: RefCell::new(None),
    });

    // The list is built once the host scan finishes; until then a spinner.
    let stack = gtk::Stack::new();
    stack.add_named(&widgets::spinner("Looking for installed applications\u{2026}"), Some("loading"));

    let (bottom, summary, continue_button) = widgets::action_bar("Continue");
    *flow.summary_label.borrow_mut() = Some(summary);
    *flow.continue_button.borrow_mut() = Some(continue_button.clone());
    continue_button.set_sensitive(false);
    bottom.set_visible(false);

    let page = widgets::page("Choose Applications", "backup-select", &stack, Some(&bottom));
    ui.push(&page);

    continue_button.connect_clicked({
        let flow = Rc::clone(&flow);
        move |_| show_review(&flow)
    });

    ui.scan_host({
        let flow = Rc::clone(&flow);
        let stack = stack.clone();
        let bottom = bottom.clone();
        move |outcome| match outcome {
            Ok(scan) => {
                *flow.remotes.borrow_mut() = scan.remotes;
                let content = build_list(&flow, scan.apps);
                if stack.child_by_name("content").is_none() {
                    stack.add_named(&content, Some("content"));
                }
                stack.set_visible_child_name("content");
                bottom.set_visible(true);
                flow.refresh();
            }
            Err(message) => {
                let status = adw::StatusPage::builder()
                    .icon_name("dialog-error-symbolic")
                    .title("Could Not Read Installed Applications")
                    .description(&message)
                    .build();
                if stack.child_by_name("error").is_none() {
                    stack.add_named(&status, Some("error"));
                }
                stack.set_visible_child_name("error");
            }
        }
    });
}

/// Builds the application list once the scan is in.
fn build_list(flow: &Rc<Flow>, apps: Vec<InstalledApp>) -> gtk::Widget {
    let prefs = adw::PreferencesPage::new();

    // ---- Options
    let options = adw::PreferencesGroup::builder().title("Options").build();
    let caches = adw::SwitchRow::builder()
        .title("Exclude caches")
        .subtitle("Skip each application's cache folder. It is rebuilt automatically and is often the largest part.")
        .active(flow.exclude_caches.get())
        .build();
    caches.connect_active_notify({
        let flow = Rc::clone(flow);
        move |row| {
            flow.exclude_caches.set(row.is_active());
            flow.refresh();
        }
    });
    options.add(&caches);
    prefs.add(&options);

    // ---- Applications
    let group = adw::PreferencesGroup::builder()
        .title("Applications")
        .description("Choose which applications to back up, and whether to include each one's data.")
        .build();

    let select_all = gtk::Button::builder().label("All").build();
    select_all.add_css_class("flat");
    let select_none = gtk::Button::builder().label("None").build();
    select_none.add_css_class("flat");
    let header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    header_box.append(&select_all);
    header_box.append(&select_none);
    group.set_header_suffix(Some(&header_box));

    if apps.is_empty() {
        let empty = adw::ActionRow::builder()
            .title("No Flatpak applications are installed")
            .subtitle("There is nothing to back up yet.")
            .build();
        group.add(&empty);
    }

    let mut rows = Vec::with_capacity(apps.len());
    for app in apps {
        let info = appdata::inspect(&app.id);

        let mut subtitle_parts = vec![app.id.clone()];
        subtitle_parts.push(app.installation.label());
        if !app.version.is_empty() {
            subtitle_parts.push(app.version.clone());
        }

        let row = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(app.display_name()))
            .subtitle(glib::markup_escape_text(&subtitle_parts.join(" \u{2022} ")))
            .build();

        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .active(true)
            .tooltip_text("Include this application in the backup")
            .build();
        row.add_prefix(&check);
        row.add_prefix(&widgets::app_icon(&app.id));

        let data_row = adw::SwitchRow::builder()
            .title("Back up data")
            .subtitle(widgets::describe_data(&info, flow.exclude_caches.get()))
            .active(info.present)
            .sensitive(info.present)
            .build();
        row.add_row(&data_row);

        if app.is_sideloaded() {
            let warning = adw::ActionRow::builder()
                .title("Installed without a remote")
                .subtitle("FlatBak cannot reinstall this application automatically.")
                .build();
            warning.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            row.add_row(&warning);
        }

        check.connect_toggled({
            let flow = Rc::clone(flow);
            move |_| flow.refresh()
        });
        data_row.connect_active_notify({
            let flow = Rc::clone(flow);
            move |_| flow.refresh()
        });

        group.add(&row);
        rows.push(AppRow {
            app,
            info,
            row,
            check,
            data_row,
        });
    }
    *flow.rows.borrow_mut() = rows;

    let set_all = {
        let flow = Rc::clone(flow);
        move |value: bool| {
            flow.updating.set(true);
            for row in flow.rows.borrow().iter() {
                row.check.set_active(value);
            }
            flow.updating.set(false);
            flow.refresh();
        }
    };
    select_all.connect_clicked({
        let set_all = set_all.clone();
        move |_| set_all(true)
    });
    select_none.connect_clicked(move |_| set_all(false));

    prefs.add(&group);
    prefs.upcast()
}

/// The review page: destination plus a summary of what will be written.
fn show_review(flow: &Rc<Flow>) {
    let prefs = adw::PreferencesPage::new();

    let destination_group = adw::PreferencesGroup::builder().title("Destination").build();
    let destination_row = adw::ActionRow::builder()
        .title("Choose where to save")
        .subtitle("No file chosen yet")
        .activatable(true)
        .build();
    destination_row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
    destination_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    destination_group.add(&destination_row);
    prefs.add(&destination_group);

    let selections = flow.selections();
    let with_data = selections.iter().filter(|s| s.include_data).count();
    let data_bytes = flow.selected_data_bytes();

    let summary_group = adw::PreferencesGroup::builder().title("Summary").build();
    for (title, value) in [
        ("Applications", format!("{}", selections.len())),
        ("Including data", format!("{with_data}")),
        ("Application data", format_size(data_bytes)),
        (
            "Caches",
            if flow.exclude_caches.get() {
                "Excluded".to_owned()
            } else {
                "Included".to_owned()
            },
        ),
    ] {
        let row = adw::ActionRow::builder().title(title).build();
        let label = gtk::Label::builder().label(&value).build();
        label.add_css_class("dim-label");
        row.add_suffix(&label);
        summary_group.add(&row);
    }

    let note = adw::ActionRow::builder()
        .title("Compressed size will be smaller")
        .subtitle("Application data is compressed with Zstandard as it is written.")
        .build();
    note.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
    summary_group.add(&note);
    prefs.add(&summary_group);

    let (bottom, summary_label, create_button) = widgets::action_bar("Create Backup");
    create_button.set_sensitive(false);
    summary_label.set_label("Choose a destination to continue");

    let page = widgets::page("Review Backup", "backup-review", &prefs, Some(&bottom));

    // Pre-fill a sensible file name including today's date.
    let suggested = format!(
        "flatpaks-{}.{}",
        chrono::Local::now().format("%Y-%m-%d"),
        config::BACKUP_EXTENSION
    );

    let apply_destination = {
        let destination_row = destination_row.clone();
        let create_button = create_button.clone();
        let summary_label = summary_label.clone();
        let flow = Rc::clone(flow);
        move |path: PathBuf| {
            destination_row.set_title(&glib::markup_escape_text(
                &path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            ));
            destination_row.set_subtitle(&glib::markup_escape_text(
                &path
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default(),
            ));
            *flow.destination.borrow_mut() = Some(path);
            create_button.set_sensitive(true);
            summary_label.set_label("Ready to create the backup");
        }
    };

    destination_row.connect_activated({
        let flow = Rc::clone(flow);
        let suggested = suggested.clone();
        let apply = apply_destination.clone();
        move |_| {
            dialogs::choose_destination(flow.ui.window.upcast_ref(), &suggested, {
                let apply = apply.clone();
                move |path| apply(path.clone())
            })
        }
    });

    // Re-apply a destination already chosen on an earlier visit to this page.
    if let Some(existing) = flow.destination.borrow().clone() {
        apply_destination(existing);
    }

    create_button.connect_clicked({
        let flow = Rc::clone(flow);
        move |_| {
            let Some(destination) = flow.destination.borrow().clone() else {
                return;
            };
            if destination.exists() {
                dialogs::confirm(
                    flow.ui.window.upcast_ref::<gtk::Widget>(),
                    "Replace existing file?",
                    &format!("{} already exists and will be overwritten.", destination.display()),
                    "Replace",
                    true,
                    {
                        let flow = Rc::clone(&flow);
                        move || run(&flow)
                    },
                );
            } else {
                run(&flow);
            }
        }
    });

    flow.ui.push(&page);
}

/// Starts the backup on a worker thread and shows the progress page.
fn run(flow: &Rc<Flow>) {
    let Some(destination) = flow.destination.borrow().clone() else {
        return;
    };
    let selections = flow.selections();
    if selections.is_empty() {
        return;
    }

    let request = BackupRequest {
        destination,
        apps: selections,
        exclude_caches: flow.exclude_caches.get(),
        compression_level: config::DEFAULT_COMPRESSION_LEVEL,
        remotes: flow.remotes.borrow().clone(),
        flatpak_version: flow.ui.flatpak_version_string(),
    };

    let progress_page = Rc::new(ProgressPage::new(
        "Creating Backup",
        "backup-progress",
        "Preparing\u{2026}",
    ));
    flow.ui.push(&progress_page.page);

    let cancel = Cancel::new();
    flow.ui.set_running(Some(cancel.clone()));
    progress_page.cancel.connect_clicked({
        let cancel = cancel.clone();
        move |button| {
            cancel.cancel();
            ProgressPage::mark_cancelling(button);
        }
    });

    /// What the worker thread reports back.
    enum Message {
        Progress(BackupProgress),
        Finished(Outcome),
    }
    enum Outcome {
        Done(BackupReport),
        Cancelled,
        Failed(String),
    }

    let (sender, receiver) = async_channel::unbounded::<Message>();
    {
        let cancel = cancel.clone();
        let sender = sender.clone();
        std::thread::spawn(move || {
            // Byte updates arrive per file; throttle them so a directory with
            // tens of thousands of small files cannot swamp the main loop.
            let mut last_bytes_sent = Instant::now() - Duration::from_secs(1);
            let mut report = |update: BackupProgress| {
                if let BackupProgress::Bytes { done, total } = update {
                    if last_bytes_sent.elapsed() < Duration::from_millis(60) && done < total {
                        return;
                    }
                    last_bytes_sent = Instant::now();
                }
                let _ = sender.send_blocking(Message::Progress(update));
            };

            let outcome = match writer::create(&request, &cancel, &mut report) {
                Ok(report) => Outcome::Done(report),
                Err(error) if is_cancellation(&error) => Outcome::Cancelled,
                Err(error) => Outcome::Failed(format!("{error:#}")),
            };
            let _ = sender.send_blocking(Message::Finished(outcome));
        });
    }

    glib::spawn_future_local({
        let flow = Rc::clone(flow);
        let progress_page = Rc::clone(&progress_page);
        async move {
            while let Ok(message) = receiver.recv().await {
                match message {
                    Message::Progress(update) => apply_progress(&progress_page, update),
                    Message::Finished(outcome) => {
                        flow.ui.set_running(None);
                        match outcome {
                            Outcome::Done(report) => show_result(&flow, report),
                            Outcome::Cancelled => {
                                flow.ui.go_home();
                                flow.ui.toast("Backup cancelled");
                            }
                            Outcome::Failed(message) => {
                                flow.ui.go_home();
                                flow.ui.error("Backup Failed", &message);
                            }
                        }
                        break;
                    }
                }
            }
        }
    });
}

fn apply_progress(page: &ProgressPage, update: BackupProgress) {
    match update {
        BackupProgress::Scanning { app, index, total } => {
            page.set_stage("Measuring application data\u{2026}");
            page.set_detail(&app);
            page.set_progress(index as u64, total as u64);
        }
        BackupProgress::Writing { app, index, total } => {
            page.set_stage("Backing up applications\u{2026}");
            page.set_detail(&format!("{} ({} of {})", app, index + 1, total));
        }
        BackupProgress::Bytes { done, total } => {
            page.set_progress(done, total);
            if total > 0 {
                page.set_detail(&format!(
                    "{} of {}",
                    format_size(done),
                    format_size(total)
                ));
            }
        }
        BackupProgress::Issue(issue) => page.log.append(&issue.to_string()),
        BackupProgress::Finalising => {
            page.set_stage("Finishing the archive\u{2026}");
            page.set_fraction(1.0);
            page.set_detail("Flushing compressed data to disk");
        }
    }
}

/// The final page, plus the recent-backups bookkeeping.
fn show_result(flow: &Rc<Flow>, report: BackupReport) {
    flow.ui.recent.borrow_mut().remember(RecentBackup {
        path: report.path.clone(),
        created_at: crate::backup::now_rfc3339(),
        app_count: report.app_count as u32,
        data_app_count: report.data_app_count as u32,
        archive_bytes: report.archive_bytes,
    });

    let warnings = report.issues.len();
    let description = format!(
        "{} application{} \u{2022} {} with data \u{2022} {} on disk",
        report.app_count,
        if report.app_count == 1 { "" } else { "s" },
        report.data_app_count,
        format_size(report.archive_bytes),
    );

    let page = ResultPage::new(
        "Backup Complete",
        "backup-done",
        if warnings == 0 {
            "object-select-symbolic"
        } else {
            "dialog-warning-symbolic"
        },
        if warnings == 0 {
            "Backup Complete"
        } else {
            "Backup Complete, With Notes"
        },
        &description,
    );

    fill_issues(&page.log, &report.issues);

    let show = page.add_button("Show in Files", &[]);
    show.connect_clicked({
        let ui = Rc::clone(&flow.ui);
        let path = report.path.clone();
        move |_| dialogs::show_in_files(ui.window.upcast_ref(), &path)
    });

    let done = page.add_button("Done", &["suggested-action"]);
    done.connect_clicked({
        let ui = Rc::clone(&flow.ui);
        move |_| ui.go_home()
    });

    flow.ui.push(&page.page);
}

/// Writes the issue list into a details pane, grouping repetitive entries.
pub fn fill_issues(log: &DetailsLog, issues: &[Issue]) {
    const MAX_LINES: usize = 200;
    for issue in issues.iter().take(MAX_LINES) {
        log.append(&issue.to_string());
    }
    if issues.len() > MAX_LINES {
        log.append(&format!(
            "\u{2026} and {} more",
            issues.len() - MAX_LINES
        ));
    }
}
