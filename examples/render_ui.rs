//! Development helper: renders FlatBak's pages to PNG files.
//!
//! Run under any Wayland or X11 display, including a headless Weston:
//!
//! ```sh
//! cargo run --example render_ui -- /tmp/shots
//! ```
//!
//! Each page is driven through the real UI code and captured from the live
//! renderer, so the output shows exactly what a user would see.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use flatbak::backup::writer::{self, AppSelection, BackupRequest};
use flatbak::error::Issue;
use flatbak::flatpak::{InstalledApp, Installation, Remote};
use flatbak::ui::progress::{ProgressPage, ResultPage};
use flatbak::ui::{backup_page, restore_page, Ui};
use flatbak::util::Cancel;
use gtk::glib;

fn main() -> glib::ExitCode {
    let out_dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/flatbak-shots".to_owned()),
    );
    std::fs::create_dir_all(&out_dir).unwrap();

    let archive = make_sample_archive();

    let application = adw::Application::builder()
        .application_id("io.github.abdullahfanar.FlatBak.Render")
        .build();

    application.connect_activate(move |application| {
        let ui = Ui::build(application);
        let out_dir = out_dir.clone();
        let archive = archive.clone();

        // A short script through the flows, capturing each page once its
        // asynchronous content has arrived.
        schedule(1_200, {
            let ui = Rc::clone(&ui);
            let out = out_dir.join("01-home.png");
            move || capture(&ui, &out)
        });

        schedule(1_600, {
            let ui = Rc::clone(&ui);
            move || backup_page::start(&ui)
        });
        schedule(4_200, {
            let ui = Rc::clone(&ui);
            let out = out_dir.join("02-backup-select.png");
            move || capture(&ui, &out)
        });
        // Open the first application's row so the per-app data controls are
        // visible in the next capture.
        schedule(4_300, {
            let ui = Rc::clone(&ui);
            move || {
                expand_first_row(ui.window.upcast_ref());
            }
        });
        schedule(4_900, {
            let ui = Rc::clone(&ui);
            let out = out_dir.join("02b-backup-expanded.png");
            move || capture(&ui, &out)
        });

        schedule(5_300, {
            let ui = Rc::clone(&ui);
            move || {
                ui.go_home();
                restore_page::open(&ui, &archive);
            }
        });
        schedule(7_900, {
            let ui = Rc::clone(&ui);
            let out = out_dir.join("03-restore-select.png");
            move || capture(&ui, &out)
        });

        schedule(8_300, {
            let ui = Rc::clone(&ui);
            move || {
                ui.go_home();
                let page = ProgressPage::new("Creating Backup", "shot-progress", "Backing up applications\u{2026}");
                page.set_detail("Telegram Desktop (4 of 13)");
                page.set_progress(4, 13);
                page.log.append("com.protonvpn.www: no application data found, nothing to back up");
                page.log.append("Zen: only cached data found, nothing backed up");
                ui.push(&page.page);
            }
        });
        schedule(9_300, {
            let ui = Rc::clone(&ui);
            let out = out_dir.join("04-progress.png");
            move || capture(&ui, &out)
        });

        schedule(9_700, {
            let ui = Rc::clone(&ui);
            move || {
                ui.go_home();
                let page = ResultPage::new(
                    "Backup Complete",
                    "shot-done",
                    "dialog-warning-symbolic",
                    "Backup Complete, With Notes",
                    "13 applications \u{2022} 11 with data \u{2022} 412 MB on disk",
                );
                for issue in [
                    Issue::NoDataDirectory { app: "Proton VPN".into() },
                    Issue::MissingRemote { app: "Some App".into(), remote: "elsewhere".into() },
                ] {
                    page.log.append(&issue.to_string());
                }
                page.add_button("Show in Files", &[]);
                page.add_button("Done", &["suggested-action"]);
                ui.push(&page.page);
            }
        });
        schedule(10_700, {
            let ui = Rc::clone(&ui);
            let out = out_dir.join("05-result.png");
            move || capture(&ui, &out)
        });

        schedule(11_300, {
            let application = application.clone();
            move || application.quit()
        });
    });

    application.run_with_args::<&str>(&[])
}

/// Expands the first `AdwExpanderRow` found in the widget tree.
fn expand_first_row(root: &gtk::Widget) -> bool {
    if let Some(row) = root.downcast_ref::<adw::ExpanderRow>() {
        row.set_expanded(true);
        return true;
    }
    let mut child = root.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        if expand_first_row(&current) {
            return true;
        }
    }
    false
}

fn schedule(millis: u32, action: impl Fn() + 'static) {
    glib::timeout_add_local_once(std::time::Duration::from_millis(millis as u64), action);
}

/// Captures the window's current contents straight from its live renderer.
fn capture(ui: &Rc<Ui>, path: &PathBuf) {
    let window = &ui.window;
    let width = window.width();
    let height = window.height();
    if width == 0 || height == 0 {
        eprintln!("window not mapped yet, skipping {}", path.display());
        return;
    }

    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);

    let Some(node) = snapshot.to_node() else {
        eprintln!("nothing to render for {}", path.display());
        return;
    };
    let Some(renderer) = window.native().and_then(|native| native.renderer()) else {
        eprintln!("no renderer for {}", path.display());
        return;
    };
    let texture = renderer.render_texture(&node, None);
    match texture.save_to_png(path) {
        Ok(()) => println!("wrote {}", path.display()),
        Err(error) => eprintln!("could not write {}: {error}", path.display()),
    }
}

/// Builds a small archive so the restore page has something real to show.
fn make_sample_archive() -> PathBuf {
    let root = std::env::temp_dir().join("flatbak-render-sample");
    let data_root = root.join("varapp");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&data_root).unwrap();
    std::env::set_var("FLATBAK_DATA_ROOT", &data_root);

    let apps = [
        ("org.videolan.VLC", "VLC", "3.0.21"),
        ("org.telegram.desktop", "Telegram Desktop", "5.7.1"),
        ("com.github.tchx84.Flatseal", "Flatseal", "2.4.1"),
        ("org.gnome.Boxes", "Boxes", "49.0"),
    ];

    let mut selections = Vec::new();
    for (id, name, version) in apps {
        let dir = data_root.join(id);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config/settings"), vec![b'x'; 20_000]).unwrap();
        selections.push(AppSelection {
            app: InstalledApp {
                id: id.to_owned(),
                name: name.to_owned(),
                version: version.to_owned(),
                branch: "stable".to_owned(),
                arch: "x86_64".to_owned(),
                origin: "flathub".to_owned(),
                installation: Installation::System,
                reference: format!("app/{id}/x86_64/stable"),
                commit: "0".repeat(12),
                installed_size: Some(42_000_000),
            },
            include_data: true,
        });
    }

    let destination = root.join("sample.flatbak");
    let request = BackupRequest {
        destination: destination.clone(),
        apps: selections,
        exclude_caches: true,
        compression_level: 1,
        remotes: vec![Remote {
            name: "flathub".to_owned(),
            title: "Flathub".to_owned(),
            url: "https://dl.flathub.org/repo/".to_owned(),
            installation: Installation::System,
            disabled: false,
        }],
        flatpak_version: "1.18.2".to_owned(),
    };
    writer::create(&request, &Cancel::new(), &mut |_| {}).unwrap();
    destination
}
