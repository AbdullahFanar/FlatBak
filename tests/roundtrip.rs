//! End-to-end test of the backup and restore cycle against a scratch data root.
//!
//! `FLATBAK_DATA_ROOT` redirects `~/.var/app` for the whole process, so this
//! file holds a single test: cargo runs each integration test file in its own
//! process, but tests within one file share it.

use std::fs;
use std::path::{Path, PathBuf};

use flatbak::appdata;
use flatbak::backup::manifest::Manifest;
use flatbak::backup::reader::{self, Backup, ConflictChoice, RestoreRequest, RestoreSelection};
use flatbak::backup::writer::{self, AppSelection, BackupRequest};
use flatbak::error::Issue;
use flatbak::flatpak::{Flatpak, InstalledApp, Installation, Remote};
use flatbak::util::Cancel;

const APP_ID: &str = "org.example.Demo";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "flatbak-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds the application data a real Flatpak app would leave behind.
fn seed_app_data(root: &Path) {
    let app = root.join(APP_ID);
    fs::create_dir_all(app.join("config")).unwrap();
    fs::create_dir_all(app.join("data/nested/deeper")).unwrap();
    fs::create_dir_all(app.join("cache/huge")).unwrap();
    // An empty directory, which a naive file-only walk would lose.
    fs::create_dir_all(app.join("data/empty-dir")).unwrap();

    fs::write(app.join("config/settings.ini"), b"[main]\nvolume=80\n").unwrap();
    fs::write(app.join("data/library.db"), vec![0xABu8; 4096]).unwrap();
    fs::write(app.join("data/nested/deeper/note.txt"), b"hello\n").unwrap();
    // Cache content, which should be excluded by default.
    fs::write(app.join("cache/huge/blob.bin"), vec![0x11u8; 8192]).unwrap();

    // A relative symlink inside the app's own data: legitimate and preserved.
    std::os::unix::fs::symlink("../library.db", app.join("data/nested/link-to-db")).unwrap();

    // An absolute symlink pointing back inside the application's own data.
    // Real application data contains these (libvirt writes its autostart links
    // this way), and they must survive a move to a machine whose home
    // directory has a different path.
    std::os::unix::fs::symlink(
        app.join("data/library.db"),
        app.join("config/absolute-link-inside"),
    )
    .unwrap();

    // An absolute symlink pointing somewhere else entirely, which cannot mean
    // anything on the restoring machine.
    std::os::unix::fs::symlink("/run/user/1000/some.socket", app.join("config/runtime-socket"))
        .unwrap();
}

fn demo_app() -> InstalledApp {
    InstalledApp {
        id: APP_ID.to_owned(),
        name: "Demo".to_owned(),
        version: "1.2.3".to_owned(),
        branch: "stable".to_owned(),
        arch: "x86_64".to_owned(),
        origin: "flathub".to_owned(),
        installation: Installation::System,
        reference: format!("app/{APP_ID}/x86_64/stable"),
        commit: "deadbeef".to_owned(),
        installed_size: Some(1_234_567),
    }
}

fn flathub() -> Remote {
    Remote {
        name: "flathub".to_owned(),
        title: "Flathub".to_owned(),
        url: "https://dl.flathub.org/repo/".to_owned(),
        installation: Installation::System,
        disabled: false,
    }
}

/// Restores with installation disabled, so no `flatpak` process is ever spawned.
fn restore_data_only(
    backup: &Backup,
    on_conflict: ConflictChoice,
) -> flatbak::backup::reader::RestoreReport {
    let request = RestoreRequest {
        selections: vec![RestoreSelection {
            index: 0,
            install: false,
            restore_data: true,
            on_conflict,
        }],
        install_to_user: false,
        remotes_to_add: Vec::new(),
    };
    reader::restore(
        backup,
        &request,
        &Flatpak::detect(),
        &Cancel::new(),
        &mut |_| {},
    )
    .expect("restore should succeed")
}

#[test]
fn backup_and_restore_round_trip() {
    let root = scratch("roundtrip");
    let data_root = root.join("varapp");
    fs::create_dir_all(&data_root).unwrap();
    std::env::set_var("FLATBAK_DATA_ROOT", &data_root);
    seed_app_data(&data_root);

    // ---- Measuring
    let info = appdata::inspect(APP_ID);
    assert!(info.present, "seeded data should be found");
    assert!(info.cache_bytes >= 8192, "cache should be measured separately");
    assert_eq!(
        info.bytes_to_back_up(true),
        info.total_bytes - info.cache_bytes,
        "excluding caches should subtract exactly the cache size"
    );

    // ---- Writing
    let destination = root.join("demo.flatbak");
    let request = BackupRequest {
        destination: destination.clone(),
        apps: vec![AppSelection {
            app: demo_app(),
            include_data: true,
        }],
        exclude_caches: true,
        compression_level: 3,
        remotes: vec![flathub()],
        flatpak_version: "1.18.2".to_owned(),
    };

    let report = writer::create(&request, &Cancel::new(), &mut |_| {}).expect("backup should work");
    assert_eq!(report.app_count, 1);
    assert_eq!(report.data_app_count, 1);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.to_string().contains("runtime-socket")),
        "the link pointing outside the data folder should be reported: {:?}",
        report.issues
    );
    assert!(destination.is_file(), "the archive should exist");
    assert!(
        !root.join("demo.flatbak.part").exists(),
        "the temporary file should be gone"
    );
    assert_eq!(report.archive_bytes, fs::metadata(&destination).unwrap().len());

    // ---- Opening
    let backup = Backup::open(&destination).expect("the archive should open");
    assert_eq!(backup.manifest.flatbak.format_version, 1);
    assert!(backup.manifest.flatbak.excluded_caches);
    assert_eq!(backup.manifest.apps.len(), 1);

    let entry = &backup.manifest.apps[0];
    assert_eq!(entry.id, APP_ID);
    assert_eq!(entry.origin, "flathub");
    assert_eq!(entry.installation, Installation::System);
    assert_eq!(entry.reference, format!("app/{APP_ID}/x86_64/stable"));
    assert!(entry.data_included);
    assert_eq!(entry.data_path, format!("data/{APP_ID}"));
    assert_eq!(entry.installed_size, 1_234_567);

    // The remote's address travels with the archive, so a restore can recreate it.
    let remote = backup.manifest.remote("flathub").expect("remote recorded");
    assert_eq!(remote.url, "https://dl.flathub.org/repo/");
    assert_eq!(backup.manifest.required_remotes(), vec!["flathub"]);

    backup
        .verify_payload(&Cancel::new(), &mut |_, _| {})
        .expect("a freshly written archive should verify");

    // ---- Restoring onto a clean system
    fs::remove_dir_all(data_root.join(APP_ID)).unwrap();
    let report = restore_data_only(&backup, ConflictChoice::Replace);
    assert_eq!(report.data_restored, vec!["Demo"]);
    assert!(
        report.issues.is_empty(),
        "clean restore should report nothing: {:?}",
        report.issues
    );

    let app = data_root.join(APP_ID);
    assert_eq!(
        fs::read(app.join("config/settings.ini")).unwrap(),
        b"[main]\nvolume=80\n"
    );
    assert_eq!(fs::read(app.join("data/library.db")).unwrap(), vec![0xABu8; 4096]);
    assert_eq!(
        fs::read(app.join("data/nested/deeper/note.txt")).unwrap(),
        b"hello\n"
    );
    assert!(
        app.join("data/empty-dir").is_dir(),
        "empty directories should survive the round trip"
    );

    let link = app.join("data/nested/link-to-db");
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(metadata.is_symlink(), "symlinks should stay symlinks");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("../library.db"));

    assert!(
        !app.join("cache").exists(),
        "excluded caches must not come back"
    );

    // The absolute link that pointed inside comes back as an equivalent
    // relative one, so it resolves correctly under a different home directory.
    let rewritten = app.join("config/absolute-link-inside");
    assert!(fs::symlink_metadata(&rewritten).unwrap().is_symlink());
    let target = fs::read_link(&rewritten).unwrap();
    assert!(
        target.is_relative(),
        "an absolute target should have been rewritten, got {}",
        target.display()
    );
    assert_eq!(target, Path::new("../data/library.db"));
    assert_eq!(
        fs::read(&rewritten).unwrap(),
        vec![0xABu8; 4096],
        "the rewritten link should resolve to the same file"
    );

    // The link into the runtime directory is left out, as reported earlier.
    assert!(
        fs::symlink_metadata(app.join("config/runtime-socket")).is_err(),
        "a link pointing outside the data folder must not be restored"
    );

    // ---- Existing data: keeping it
    fs::write(app.join("config/settings.ini"), b"LOCAL CHANGES\n").unwrap();
    let report = restore_data_only(&backup, ConflictChoice::Skip);
    assert_eq!(
        fs::read(app.join("config/settings.ini")).unwrap(),
        b"LOCAL CHANGES\n",
        "keeping existing data must not overwrite it"
    );
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue, Issue::DataSkipped { .. })),
        "skipping should be reported: {:?}",
        report.issues
    );
    assert!(report.data_restored.is_empty());

    // ---- Existing data: replacing it
    fs::write(app.join("config/stale.txt"), b"should be gone\n").unwrap();
    let report = restore_data_only(&backup, ConflictChoice::Replace);
    assert_eq!(
        fs::read(app.join("config/settings.ini")).unwrap(),
        b"[main]\nvolume=80\n",
        "replacing should restore the archived contents"
    );
    assert!(
        !app.join("config/stale.txt").exists(),
        "replacing should remove files that are not in the backup"
    );
    assert_eq!(report.data_restored, vec!["Demo"]);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn missing_data_directory_is_reported_not_fatal() {
    // A separate scratch root, but the same process-wide env var; set it to a
    // directory where the application has never run.
    let root = scratch("nodata");
    let data_root = root.join("varapp");
    fs::create_dir_all(&data_root).unwrap();

    // Point the manifest writer at an app with no data at all by using an ID
    // that was never seeded under the shared root.
    let mut app = demo_app();
    app.id = "org.example.NeverLaunched".to_owned();
    app.reference = format!("app/{}/x86_64/stable", app.id);

    let destination = root.join("nodata.flatbak");
    let request = BackupRequest {
        destination: destination.clone(),
        apps: vec![AppSelection {
            app,
            include_data: true,
        }],
        exclude_caches: true,
        compression_level: 3,
        remotes: vec![flathub()],
        flatpak_version: String::new(),
    };

    let report = writer::create(&request, &Cancel::new(), &mut |_| {}).expect("backup should work");
    assert_eq!(report.app_count, 1, "the app is still recorded");
    assert_eq!(report.data_app_count, 0, "but without data");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue, Issue::NoDataDirectory { .. })),
        "the missing directory should be reported: {:?}",
        report.issues
    );

    let backup = Backup::open(&destination).unwrap();
    assert!(!backup.manifest.apps[0].data_included);
    assert_eq!(backup.manifest.apps_with_data(), 0);

    // An archive with no payload content is still a valid archive.
    backup
        .verify_payload(&Cancel::new(), &mut |_, _| {})
        .expect("an empty payload should still verify");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn manifest_survives_a_round_trip_through_the_archive() {
    let root = scratch("manifest");
    let destination = root.join("m.flatbak");
    let request = BackupRequest {
        destination: destination.clone(),
        apps: vec![AppSelection {
            app: demo_app(),
            include_data: false,
        }],
        exclude_caches: false,
        compression_level: 1,
        remotes: vec![flathub()],
        flatpak_version: "1.18.2".to_owned(),
    };
    writer::create(&request, &Cancel::new(), &mut |_| {}).unwrap();

    let backup = Backup::open(&destination).unwrap();
    let text = backup.manifest.to_toml().unwrap();
    let reparsed = Manifest::from_toml(&text).expect("the written manifest must be re-readable");
    assert_eq!(reparsed.apps[0].id, APP_ID);
    assert_eq!(reparsed.flatbak.created_by, backup.manifest.flatbak.created_by);

    fs::remove_dir_all(&root).ok();
}
