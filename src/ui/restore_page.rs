//! The restore flow: open an archive, resolve conflicts, reinstall, restore data.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::appdata::{self, DataInfo};
use crate::backup::manifest::AppEntry;
use crate::backup::reader::{
    self, Backup, ConflictChoice, MissingRemote, RestoreProgress, RestoreReport, RestoreRequest,
    RestoreSelection,
};
use crate::error::is_cancellation;
use crate::flatpak::Remote;
use crate::recent::RecentBackup;
use crate::ui::backup_page::fill_issues;
use crate::ui::progress::{ProgressPage, ResultPage};
use crate::ui::widgets;
use crate::ui::Ui;
use crate::util::{format_size, Cancel};

/// Choices offered when an application already has data on this system.
const CONFLICT_LABELS: [&str; 2] = ["Keep existing", "Replace"];

fn conflict_from_index(index: u32) -> ConflictChoice {
    if index == 1 {
        ConflictChoice::Replace
    } else {
        ConflictChoice::Skip
    }
}

fn conflict_to_index(choice: ConflictChoice) -> u32 {
    match choice {
        ConflictChoice::Replace => 1,
        ConflictChoice::Skip => 0,
    }
}

/// One application row in the restore list.
struct RestoreRow {
    index: usize,
    entry: AppEntry,
    /// Data already on this system for the same application.
    existing: DataInfo,
    row: adw::ExpanderRow,
    check: gtk::CheckButton,
    data_row: adw::SwitchRow,
    /// Present only when there is existing data to resolve.
    conflict_row: Option<adw::ComboRow>,
}

struct Flow {
    ui: Rc<Ui>,
    backup: Backup,
    remotes: Vec<Remote>,
    installed_ids: Vec<String>,
    rows: RefCell<Vec<RestoreRow>>,
    install_to_user: Cell<bool>,
    remote_group: RefCell<Option<adw::PreferencesGroup>>,
    remote_rows: RefCell<Vec<(MissingRemote, gtk::Switch, adw::ActionRow)>>,
    updating: Cell<bool>,
    summary_label: RefCell<Option<gtk::Label>>,
    restore_button: RefCell<Option<gtk::Button>>,
}

impl Flow {
    fn selections(&self) -> Vec<RestoreSelection> {
        self.rows
            .borrow()
            .iter()
            .filter_map(|row| {
                let install = row.check.is_active();
                let restore_data = row.data_row.is_active() && row.entry.data_included;
                if !install && !restore_data {
                    return None;
                }
                let on_conflict = row
                    .conflict_row
                    .as_ref()
                    .map(|combo| conflict_from_index(combo.selected()))
                    .unwrap_or(ConflictChoice::Replace);
                Some(RestoreSelection {
                    index: row.index,
                    install,
                    restore_data,
                    on_conflict,
                })
            })
            .collect()
    }

    /// Remotes the user ticked for creation.
    fn remotes_to_add(&self) -> Vec<MissingRemote> {
        self.remote_rows
            .borrow()
            .iter()
            .filter(|(remote, switch, _)| switch.is_active() && remote.can_add())
            .map(|(remote, _, _)| remote.clone())
            .collect()
    }

    fn refresh(&self) {
        if self.updating.get() {
            return;
        }
        let mut install_count = 0usize;
        let mut data_count = 0usize;
        let mut data_bytes = 0u64;

        for row in self.rows.borrow().iter() {
            let install = row.check.is_active();
            let can_restore_data = row.entry.data_included;
            row.data_row.set_sensitive(can_restore_data);
            if let Some(combo) = &row.conflict_row {
                combo.set_sensitive(row.data_row.is_active() && can_restore_data);
            }
            if install {
                install_count += 1;
                row.row.remove_css_class("dim-label");
            } else if row.data_row.is_active() && can_restore_data {
                row.row.remove_css_class("dim-label");
            } else {
                row.row.add_css_class("dim-label");
            }
            if row.data_row.is_active() && can_restore_data {
                data_count += 1;
                data_bytes += row.entry.data_bytes;
            }
        }

        if let Some(label) = self.summary_label.borrow().as_ref() {
            let text = if install_count == 0 && data_count == 0 {
                "Nothing selected".to_owned()
            } else {
                format!(
                    "{install_count} to install \u{2022} {data_count} with data \u{2022} {}",
                    format_size(data_bytes)
                )
            };
            label.set_label(&text);
        }
        if let Some(button) = self.restore_button.borrow().as_ref() {
            button.set_sensitive(install_count > 0 || data_count > 0);
        }
    }

    /// Rebuilds the "Missing Remotes" group, which depends on the target
    /// installation the user has chosen.
    fn refresh_missing_remotes(self: &Rc<Self>) {
        let Some(group) = self.remote_group.borrow().clone() else {
            return;
        };
        for (_, _, row) in self.remote_rows.borrow().iter() {
            group.remove(row);
        }
        self.remote_rows.borrow_mut().clear();

        let missing = self
            .backup
            .missing_remotes(&self.remotes, self.install_to_user.get());
        group.set_visible(!missing.is_empty());

        for remote in missing {
            let apps = if remote.apps.len() <= 3 {
                remote.apps.join(", ")
            } else {
                format!(
                    "{} and {} more",
                    remote.apps[..2].join(", "),
                    remote.apps.len() - 2
                )
            };
            let subtitle = if remote.can_add() {
                format!("{}\nNeeded by {apps}", remote.url)
            } else {
                format!(
                    "No address was recorded for this remote, so it must be added by hand.\n\
                     Needed by {apps}"
                )
            };

            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&remote.title))
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(if remote.can_add() {
                "network-server-symbolic"
            } else {
                "dialog-warning-symbolic"
            }));

            // Remotes we know the address for default to being added, since
            // that is what makes the rest of the restore work.
            let switch = gtk::Switch::builder()
                .valign(gtk::Align::Center)
                .active(remote.can_add())
                .sensitive(remote.can_add())
                .tooltip_text("Add this remote before restoring")
                .build();
            row.add_suffix(&switch);

            group.add(&row);
            self.remote_rows.borrow_mut().push((remote, switch, row));
        }
    }
}

/// Opens an archive and shows the selection page.
pub fn open(ui: &Rc<Ui>, path: &Path) {
    let path = path.to_path_buf();

    // A spinner page while the archive header and manifest are read. This is
    // fast even for large archives because the manifest sits at a known offset.
    let stack = gtk::Stack::new();
    stack.add_named(&widgets::spinner("Reading backup\u{2026}"), Some("loading"));
    let (bottom, summary, restore_button) = widgets::action_bar("Restore");
    bottom.set_visible(false);
    restore_button.set_sensitive(false);

    let page = widgets::page("Restore Backup", "restore-select", &stack, Some(&bottom));
    ui.push(&page);

    let (sender, receiver) = async_channel::bounded(1);
    let flatpak = ui.flatpak.clone();
    let worker_path = path.clone();
    std::thread::spawn(move || {
        let outcome = Backup::open(&worker_path).map(|backup| {
            // Remotes and installed apps are advisory: if Flatpak is not
            // working we can still show the archive's contents.
            let remotes = flatpak.list_remotes().unwrap_or_default();
            let installed = flatpak
                .list_apps()
                .map(|apps| apps.into_iter().map(|app| app.id).collect::<Vec<_>>())
                .unwrap_or_default();
            (backup, remotes, installed)
        });
        let _ = sender.send_blocking(outcome.map_err(|error| format!("{error:#}")));
    });

    glib::spawn_future_local({
        let ui = Rc::clone(ui);
        let stack = stack.clone();
        let bottom = bottom.clone();
        let summary = summary.clone();
        let restore_button = restore_button.clone();
        let page = page.clone();
        async move {
            let Ok(outcome) = receiver.recv().await else {
                return;
            };
            match outcome {
                Ok((backup, remotes, installed_ids)) => {
                    let flow = Rc::new(Flow {
                        ui: Rc::clone(&ui),
                        backup,
                        remotes,
                        installed_ids,
                        rows: RefCell::new(Vec::new()),
                        install_to_user: Cell::new(false),
                        remote_group: RefCell::new(None),
                        remote_rows: RefCell::new(Vec::new()),
                        updating: Cell::new(false),
                        summary_label: RefCell::new(Some(summary)),
                        restore_button: RefCell::new(Some(restore_button.clone())),
                    });

                    // Keep a backup we can successfully open in the recent list,
                    // so reopening it later takes one click.
                    ui.recent.borrow_mut().remember(RecentBackup {
                        path: flow.backup.path.clone(),
                        created_at: flow.backup.manifest.flatbak.created_at.clone(),
                        app_count: flow.backup.manifest.apps.len() as u32,
                        data_app_count: flow.backup.manifest.apps_with_data() as u32,
                        archive_bytes: flow.backup.file_size,
                    });

                    let content = build_list(&flow);
                    if stack.child_by_name("content").is_none() {
                        stack.add_named(&content, Some("content"));
                    }
                    stack.set_visible_child_name("content");
                    bottom.set_visible(true);
                    flow.refresh();

                    restore_button.connect_clicked({
                        let flow = Rc::clone(&flow);
                        move |_| run(&flow)
                    });
                }
                Err(message) => {
                    // Drop straight back home: an unreadable archive leaves
                    // nothing useful on screen.
                    let _ = page;
                    ui.go_home();
                    ui.error("Cannot Open Backup", &message);
                }
            }
        }
    });
}

fn build_list(flow: &Rc<Flow>) -> gtk::Widget {
    let prefs = adw::PreferencesPage::new();
    let manifest = &flow.backup.manifest;

    // ---- What this archive is
    let about = adw::PreferencesGroup::builder().title("This Backup").build();
    for (title, value) in [
        (
            "Created",
            crate::backup::format_timestamp(&manifest.flatbak.created_at),
        ),
        ("Applications", format!("{}", manifest.apps.len())),
        (
            "With data",
            format!(
                "{} \u{2022} {}",
                manifest.apps_with_data(),
                format_size(manifest.total_data_bytes())
            ),
        ),
        ("Archive size", format_size(flow.backup.file_size)),
        ("Made by", manifest.flatbak.created_by.clone()),
    ] {
        let row = adw::ActionRow::builder().title(title).build();
        // These values are short; wrapping only squeezes them into a
        // ragged column against the row's title.
        let label = gtk::Label::builder()
            .label(&value)
            .xalign(1.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        label.add_css_class("dim-label");
        row.add_suffix(&label);
        about.add(&row);
    }
    if manifest.flatbak.host_arch != std::env::consts::ARCH
        && !manifest.flatbak.host_arch.is_empty()
    {
        let row = adw::ActionRow::builder()
            .title("Different architecture")
            .subtitle(format!(
                "This backup was made on {}. Some applications may not be available here.",
                manifest.flatbak.host_arch
            ))
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        about.add(&row);
    }
    prefs.add(&about);

    // ---- Missing remotes
    let remote_group = adw::PreferencesGroup::builder()
        .title("Missing Remotes")
        .description(
            "These remotes are not set up on this system. \
             Applications from them cannot be installed until they are added.",
        )
        .visible(false)
        .build();
    *flow.remote_group.borrow_mut() = Some(remote_group.clone());
    prefs.add(&remote_group);

    // ---- Options
    let options = adw::PreferencesGroup::builder().title("Options").build();

    let user_row = adw::SwitchRow::builder()
        .title("Install for this user only")
        .subtitle(
            "Ignore where each application was installed before. \
             Use this if the system-wide installation is not writable.",
        )
        .active(false)
        .build();
    user_row.connect_active_notify({
        let flow = Rc::clone(flow);
        move |row| {
            flow.install_to_user.set(row.is_active());
            flow.refresh_missing_remotes();
        }
    });
    options.add(&user_row);

    let model = gtk::StringList::new(&CONFLICT_LABELS);
    let default_conflict = adw::ComboRow::builder()
        .title("If data already exists")
        .subtitle("Each application can override this.")
        .model(&model)
        .selected(conflict_to_index(ConflictChoice::default()))
        .build();
    options.add(&default_conflict);
    prefs.add(&options);

    // ---- Applications
    let group = adw::PreferencesGroup::builder()
        .title("Applications")
        .description("Choose what to reinstall, and whose data to put back.")
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

    let mut rows = Vec::new();
    for (index, entry) in manifest.apps.iter().enumerate() {
        let existing = appdata::inspect(&entry.id);
        let already_installed = flow.installed_ids.contains(&entry.id);

        let mut subtitle_parts = vec![entry.id.clone()];
        subtitle_parts.push(entry.installation.label());
        if !entry.version.is_empty() {
            subtitle_parts.push(entry.version.clone());
        }
        if already_installed {
            subtitle_parts.push("already installed".to_owned());
        }

        let row = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(entry.display_name()))
            .subtitle(glib::markup_escape_text(&subtitle_parts.join(" \u{2022} ")))
            .build();

        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            // Nothing to do for an app that is already here; the user can still
            // tick it to force an update.
            .active(!already_installed)
            .tooltip_text("Reinstall this application")
            .build();
        row.add_prefix(&check);
        row.add_prefix(&widgets::app_icon(&entry.id));

        let data_subtitle = if entry.data_included {
            format!(
                "{} in this backup",
                format_size(entry.data_bytes)
            )
        } else {
            "No data was included for this application".to_owned()
        };
        let data_row = adw::SwitchRow::builder()
            .title("Restore data")
            .subtitle(&data_subtitle)
            .active(entry.data_included)
            .sensitive(entry.data_included)
            .build();
        row.add_row(&data_row);

        // Existing data needs an explicit decision; default to keeping it.
        let conflict_row = if existing.present && entry.data_included {
            let combo = adw::ComboRow::builder()
                .title("Existing data")
                .subtitle(format!(
                    "{} already here",
                    format_size(existing.total_bytes)
                ))
                .model(&gtk::StringList::new(&CONFLICT_LABELS))
                .selected(conflict_to_index(ConflictChoice::default()))
                .build();
            row.add_row(&combo);
            Some(combo)
        } else {
            None
        };

        if entry.is_sideloaded() {
            let warning = adw::ActionRow::builder()
                .title("No remote recorded")
                .subtitle("This application must be reinstalled by hand.")
                .build();
            warning.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            row.add_row(&warning);
            check.set_active(false);
            check.set_sensitive(false);
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
        rows.push(RestoreRow {
            index,
            entry: entry.clone(),
            existing,
            row,
            check,
            data_row,
            conflict_row,
        });
    }
    *flow.rows.borrow_mut() = rows;

    // The global choice drives every per-application combo.
    default_conflict.connect_selected_notify({
        let flow = Rc::clone(flow);
        move |combo| {
            let selected = combo.selected();
            flow.updating.set(true);
            for row in flow.rows.borrow().iter() {
                if let Some(per_app) = &row.conflict_row {
                    per_app.set_selected(selected);
                }
            }
            flow.updating.set(false);
            flow.refresh();
        }
    });

    let set_all = {
        let flow = Rc::clone(flow);
        move |value: bool| {
            flow.updating.set(true);
            for row in flow.rows.borrow().iter() {
                if row.check.is_sensitive() {
                    row.check.set_active(value);
                }
                if row.entry.data_included {
                    row.data_row.set_active(value);
                }
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
    flow.refresh_missing_remotes();
    prefs.upcast()
}

/// Starts the restore on a worker thread.
fn run(flow: &Rc<Flow>) {
    let selections = flow.selections();
    if selections.is_empty() {
        return;
    }

    // Warn once, clearly, if any existing data is about to be deleted.
    let replacing: Vec<String> = flow
        .rows
        .borrow()
        .iter()
        .filter(|row| {
            row.existing.present
                && row.data_row.is_active()
                && row.entry.data_included
                && row
                    .conflict_row
                    .as_ref()
                    .is_some_and(|combo| conflict_from_index(combo.selected()) == ConflictChoice::Replace)
        })
        .map(|row| row.entry.display_name().to_owned())
        .collect();

    if replacing.is_empty() {
        start_worker(flow);
        return;
    }

    let list = if replacing.len() <= 5 {
        replacing.join("\n")
    } else {
        format!(
            "{}\nand {} more",
            replacing[..4].join("\n"),
            replacing.len() - 4
        )
    };
    crate::ui::dialogs::confirm(
        flow.ui.window.upcast_ref::<gtk::Widget>(),
        "Replace existing application data?",
        &format!(
            "The current data for these applications will be deleted and \
             replaced with the data from the backup. This cannot be undone.\n\n{list}"
        ),
        "Replace",
        true,
        {
            let flow = Rc::clone(flow);
            move || start_worker(&flow)
        },
    );
}

fn start_worker(flow: &Rc<Flow>) {
    let request = RestoreRequest {
        selections: flow.selections(),
        install_to_user: flow.install_to_user.get(),
        remotes_to_add: flow.remotes_to_add(),
    };

    let progress_page = Rc::new(ProgressPage::new(
        "Restoring",
        "restore-progress",
        "Checking the backup\u{2026}",
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

    enum Message {
        Progress(RestoreProgress),
        Finished(Outcome),
    }
    enum Outcome {
        Done(RestoreReport),
        Cancelled,
        Failed(String),
    }

    let (sender, receiver) = async_channel::unbounded::<Message>();
    {
        let backup = flow.backup.clone();
        let flatpak = flow.ui.flatpak.clone();
        let cancel = cancel.clone();
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut last_sent = Instant::now() - Duration::from_secs(1);
            let mut report = |update: RestoreProgress| {
                // Verification and extraction produce updates far faster than
                // the UI can use them; installs are chatty too.
                let throttled = matches!(
                    update,
                    RestoreProgress::Verifying { .. }
                        | RestoreProgress::Bytes { .. }
                        | RestoreProgress::InstallOutput { .. }
                );
                if throttled {
                    if last_sent.elapsed() < Duration::from_millis(60) {
                        return;
                    }
                    last_sent = Instant::now();
                }
                let _ = sender.send_blocking(Message::Progress(update));
            };

            let outcome = match reader::restore(&backup, &request, &flatpak, &cancel, &mut report) {
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
                                flow.ui.toast("Restore cancelled");
                            }
                            Outcome::Failed(message) => {
                                flow.ui.go_home();
                                flow.ui.error("Restore Failed", &message);
                            }
                        }
                        break;
                    }
                }
            }
        }
    });
}

fn apply_progress(page: &ProgressPage, update: RestoreProgress) {
    match update {
        RestoreProgress::Verifying { done, total } => {
            page.set_stage("Checking the backup\u{2026}");
            page.set_progress(done, total);
            page.set_detail(&format!(
                "Verified {} of {}",
                format_size(done),
                format_size(total)
            ));
        }
        RestoreProgress::AddingRemote { name } => {
            page.set_stage("Adding remotes\u{2026}");
            page.set_detail(&name);
            page.log.append(&format!("Added the remote \u{201c}{name}\u{201d}"));
        }
        RestoreProgress::Installing { app, index, total } => {
            page.set_stage("Reinstalling applications\u{2026}");
            page.set_detail(&format!("{} ({} of {})", app, index + 1, total));
            page.set_progress(index as u64, total as u64);
        }
        RestoreProgress::InstallOutput { line } => page.set_detail(&line),
        RestoreProgress::ExtractingApp { app } => {
            page.set_stage("Restoring application data\u{2026}");
            page.set_detail(&app);
        }
        RestoreProgress::Bytes { done, total } => {
            page.set_progress(done, total);
            page.set_detail(&format!(
                "{} of {}",
                format_size(done),
                format_size(total)
            ));
        }
        RestoreProgress::Issue(issue) => page.log.append(&issue.to_string()),
    }
}

fn show_result(flow: &Rc<Flow>, report: RestoreReport) {
    let problems = report.has_problems();
    let mut description_parts = vec![format!(
        "{} application{} installed",
        report.installed.len(),
        if report.installed.len() == 1 { "" } else { "s" }
    )];
    description_parts.push(format!("{} restored with data", report.data_restored.len()));

    let page = ResultPage::new(
        "Restore Finished",
        "restore-done",
        if problems {
            "dialog-warning-symbolic"
        } else {
            "object-select-symbolic"
        },
        if problems {
            "Restore Finished, With Notes"
        } else {
            "Restore Complete"
        },
        &description_parts.join(" \u{2022} "),
    );

    if report.interrupted_during_data {
        page.log.append(
            "The restore was stopped while application data was being written, \
             so some data may be incomplete.",
        );
    }
    fill_issues(&page.log, &report.issues);

    let done = page.add_button("Done", &["suggested-action"]);
    done.connect_clicked({
        let ui = Rc::clone(&flow.ui);
        move |_| ui.go_home()
    });

    flow.ui.push(&page.page);
}
