//! Development helper: a real backup and restore cycle against real data.
//!
//! Backs up the given applications from this machine's `~/.var/app`, restores
//! them into a scratch directory, and compares every file byte for byte.
//! Nothing on the system is modified: the restore never touches the real data
//! root, and applications are never installed.
//!
//! ```sh
//! cargo run --example selftest                       # a few small apps
//! cargo run --example selftest -- org.telegram.desktop
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use flatbak::appdata;
use flatbak::backup::reader::{self, Backup, ConflictChoice, RestoreRequest, RestoreSelection};
use flatbak::backup::writer::{self, AppSelection, BackupProgress, BackupRequest};
use flatbak::flatpak::Flatpak;
use flatbak::util::{format_size, Cancel};

fn main() {
    let wanted: Vec<String> = std::env::args().skip(1).collect();

    let flatpak = Flatpak::detect();
    println!("flatpak : {}", flatpak.command_line());
    let installed = match flatpak.list_apps() {
        Ok(apps) => apps,
        Err(error) => {
            eprintln!("could not list applications: {error:#}");
            std::process::exit(1);
        }
    };
    let remotes = flatpak.list_remotes().unwrap_or_default();

    // Default to whatever has data but is not enormous, so the run stays quick.
    let selected: Vec<_> = installed
        .into_iter()
        .filter(|app| {
            if wanted.is_empty() {
                let info = appdata::inspect(&app.id);
                info.present && info.bytes_to_back_up(true) < 30_000_000
            } else {
                wanted.contains(&app.id)
            }
        })
        .collect();

    if selected.is_empty() {
        eprintln!("no matching applications with data");
        std::process::exit(1);
    }

    let original_root = appdata::data_root();
    println!("source  : {}", original_root.display());
    for app in &selected {
        let info = appdata::inspect(&app.id);
        println!(
            "  {:<40} {:>9} ({} files)",
            app.id,
            format_size(info.bytes_to_back_up(true)),
            info.file_count
        );
    }

    let scratch = std::env::temp_dir().join(format!("flatbak-selftest-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let destination = scratch.join("selftest.flatbak");

    // ---- Back up, reading the machine's real application data.
    let request = BackupRequest {
        destination: destination.clone(),
        apps: selected
            .iter()
            .map(|app| AppSelection {
                app: app.clone(),
                include_data: true,
            })
            .collect(),
        exclude_caches: true,
        compression_level: flatbak::config::DEFAULT_COMPRESSION_LEVEL,
        remotes,
        flatpak_version: flatpak.version().unwrap_or_default(),
    };

    let started = std::time::Instant::now();
    let report = writer::create(&request, &Cancel::new(), &mut |update| {
        if let BackupProgress::Writing { app, index, total } = update {
            println!("  writing {} ({}/{})", app, index + 1, total);
        }
    })
    .expect("backup failed");
    println!(
        "backup  : {} in {:.1}s ({} of data \u{2192} {} archive, {:.0}% of original)",
        destination.display(),
        started.elapsed().as_secs_f64(),
        format_size(report.data_bytes),
        format_size(report.archive_bytes),
        100.0 * report.archive_bytes as f64 / report.data_bytes.max(1) as f64,
    );
    for issue in &report.issues {
        println!("  note: {issue}");
    }

    // ---- Reopen and verify.
    let backup = Backup::open(&destination).expect("could not reopen the archive");
    backup
        .verify_payload(&Cancel::new(), &mut |_, _| {})
        .expect("payload checksum failed");
    println!("verify  : ok, {} applications", backup.manifest.apps.len());

    // ---- Restore into a scratch root, leaving the real one alone.
    let restore_root = scratch.join("restored");
    std::fs::create_dir_all(&restore_root).unwrap();
    std::env::set_var("FLATBAK_DATA_ROOT", &restore_root);

    let request = RestoreRequest {
        selections: (0..backup.manifest.apps.len())
            .map(|index| RestoreSelection {
                index,
                install: false,
                restore_data: true,
                on_conflict: ConflictChoice::Replace,
            })
            .collect(),
        install_to_user: false,
        remotes_to_add: Vec::new(),
    };
    let restored = reader::restore(
        &backup,
        &request,
        &flatpak,
        &Cancel::new(),
        &mut |_| {},
    )
    .expect("restore failed");
    println!("restore : {} applications", restored.data_restored.len());
    for issue in &restored.issues {
        println!("  note: {issue}");
    }

    // ---- Compare, byte for byte.
    let mut failures = 0usize;
    let mut compared = 0usize;
    let mut skipped = 0usize;
    for app in &backup.manifest.apps {
        let before = snapshot(&original_root.join(&app.id), true);
        let after = snapshot(&restore_root.join(&app.id), false);
        for (path, kind) in &before {
            match after.get(path) {
                Some(other) if other == kind => compared += 1,
                Some(other) => {
                    println!("  MISMATCH {}/{}: {kind:?} vs {other:?}", app.id, path.display());
                    failures += 1;
                }
                None if *kind == Kind::External => {
                    skipped += 1;
                }
                None => {
                    println!("  MISSING  {}/{}", app.id, path.display());
                    failures += 1;
                }
            }
        }
        for path in after.keys() {
            if !before.contains_key(path) {
                println!("  EXTRA    {}/{}", app.id, path.display());
                failures += 1;
            }
        }
    }

    std::fs::remove_dir_all(&scratch).ok();

    println!(
        "compare : {compared} entries matched, {skipped} deliberately skipped, {failures} problems"
    );
    if failures > 0 {
        std::process::exit(1);
    }
    println!("\nOK");
}

/// What an entry is, for comparison purposes.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File(u64, [u8; 32]),
    /// A symlink, described by where it lands *within* the application's data
    /// directory. Comparing resolved targets rather than raw link text is what
    /// the format actually promises: an absolute link pointing inside the data
    /// directory is deliberately rewritten as the equivalent relative one, so
    /// that it still works on a machine with a different home directory.
    Symlink(PathBuf),
    /// A symlink pointing outside the application's data, which a backup
    /// deliberately leaves out because it cannot mean anything on another
    /// machine.
    External,
}

/// Records every entry under `root`, keyed by its path relative to it.
///
/// `skip_cache` mirrors the backup's own exclusion so the two sides line up.
fn snapshot(root: &Path, skip_cache: bool) -> BTreeMap<PathBuf, Kind> {
    use sha2::{Digest, Sha256};

    let mut out = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let child = relative.join(&name);
            if skip_cache
                && relative.as_os_str().is_empty()
                && name == std::ffi::OsStr::new(appdata::CACHE_DIR)
            {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_symlink() {
                if let Ok(target) = std::fs::read_link(&path) {
                    let kind = match resolve_within(root, &child, &target) {
                        Some(inside) => Kind::Symlink(inside),
                        None => Kind::External,
                    };
                    out.insert(child, kind);
                }
            } else if metadata.is_dir() {
                out.insert(child.clone(), Kind::Dir);
                stack.push(child);
            } else if metadata.is_file() {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let digest: [u8; 32] = Sha256::digest(&bytes).into();
                out.insert(child, Kind::File(bytes.len() as u64, digest));
            }
        }
    }
    out
}

/// Resolves a symlink target to a path within the application's data directory,
/// or `None` if it lands outside.
fn resolve_within(app_root: &Path, link_relative: &Path, target: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if target.is_absolute() {
        return target
            .strip_prefix(app_root)
            .ok()
            .map(Path::to_path_buf)
            .or_else(|| {
                let canonical = std::fs::canonicalize(app_root).ok()?;
                target.strip_prefix(&canonical).ok().map(Path::to_path_buf)
            });
    }

    let mut resolved: Vec<std::ffi::OsString> = link_relative
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect();
    for component in target.components() {
        match component {
            Component::Normal(part) => resolved.push(part.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved.iter().collect())
}
