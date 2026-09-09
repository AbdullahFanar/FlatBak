//! Locating and measuring per-application user data.
//!
//! Flatpak gives every application a private home under `~/.var/app/<APP_ID>`
//! with three well known subdirectories: `config` (XDG_CONFIG_HOME), `data`
//! (XDG_DATA_HOME) and `cache` (XDG_CACHE_HOME). FlatBak backs up that whole
//! directory, optionally leaving `cache` out.

use std::path::{Path, PathBuf};

use crate::util::Cancel;

/// Subdirectory holding regenerable data, excluded by default.
pub const CACHE_DIR: &str = "cache";

/// Root of per-application data for the current user.
pub fn data_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLATBAK_DATA_ROOT") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".var/app")
}

/// Data directory for one application.
pub fn data_dir(app_id: &str) -> PathBuf {
    data_root().join(app_id)
}

/// What we know about one application's data on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataInfo {
    /// True when `~/.var/app/<id>` exists and holds at least one file.
    pub present: bool,
    /// Total size of everything under the directory.
    pub total_bytes: u64,
    /// Size of the `cache` subdirectory alone.
    pub cache_bytes: u64,
    /// Number of regular files, symlinks included.
    pub file_count: u64,
    /// Set when the directory exists but could not be read.
    pub error: Option<String>,
}

impl DataInfo {
    /// Size that would actually be written, honouring the cache setting.
    pub fn bytes_to_back_up(&self, exclude_caches: bool) -> u64 {
        if exclude_caches {
            self.total_bytes.saturating_sub(self.cache_bytes)
        } else {
            self.total_bytes
        }
    }

    /// True when excluding caches would leave nothing behind.
    pub fn is_only_cache(&self) -> bool {
        self.present && self.total_bytes > 0 && self.total_bytes == self.cache_bytes
    }
}

/// Measures one application's data directory.
///
/// A missing directory is not an error: plenty of applications have never been
/// launched, and the caller reports that as a skipped item rather than a
/// failure. Unreadable subtrees are recorded in `error` and otherwise skipped so
/// that one bad directory cannot abort a whole backup.
pub fn inspect(app_id: &str) -> DataInfo {
    let dir = data_dir(app_id);
    let metadata = match std::fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(_) => return DataInfo::default(),
    };
    if !metadata.is_dir() {
        return DataInfo {
            error: Some("not a directory".to_owned()),
            ..DataInfo::default()
        };
    }

    let mut info = DataInfo::default();
    let mut first_error = None;
    let (total, files) = measure(&dir, &mut first_error);
    info.total_bytes = total;
    info.file_count = files;

    let cache = dir.join(CACHE_DIR);
    if cache.is_dir() {
        let (cache_bytes, _) = measure(&cache, &mut first_error);
        info.cache_bytes = cache_bytes;
    }

    info.present = files > 0;
    info.error = first_error;
    info
}

/// Recursively sums file sizes, recording the first read error encountered.
///
/// Symlinks are counted but never followed, which keeps the total finite even if
/// application data contains a link loop.
fn measure(dir: &Path, first_error: &mut Option<String>) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) => {
                if first_error.is_none() {
                    *first_error = Some(format!("{}: {error}", current.display()));
                }
                continue;
            }
        };
        for entry in entries.flatten() {
            let metadata = match entry.metadata().or_else(|_| entry.path().symlink_metadata()) {
                Ok(metadata) => metadata,
                Err(error) => {
                    if first_error.is_none() {
                        *first_error = Some(format!("{}: {error}", entry.path().display()));
                    }
                    continue;
                }
            };
            let file_type = entry.file_type().ok();
            if file_type.is_some_and(|t| t.is_dir()) {
                stack.push(entry.path());
            } else {
                bytes = bytes.saturating_add(metadata.len());
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// Application IDs that have data under `~/.var/app` but are not installed.
///
/// These come up after uninstalling an application without removing its data;
/// the backup page offers them so the data is not silently lost.
pub fn orphaned_data(installed: &[String]) -> Vec<String> {
    let root = data_root();
    let mut orphans = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return orphans;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if crate::util::is_valid_app_id(&name) && !installed.contains(&name) {
            orphans.push(name);
        }
    }
    orphans.sort();
    orphans
}

/// Deletes an application data directory, used by the "replace" conflict choice.
///
/// Kept separate from the extraction path so the destructive step is explicit
/// and easy to audit.
pub fn remove_data_dir(app_id: &str, cancel: &Cancel) -> std::io::Result<()> {
    cancel.check().map_err(std::io::Error::other)?;
    let dir = data_dir(app_id);
    if !crate::util::is_valid_app_id(app_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to remove data for an invalid application ID",
        ));
    }
    match std::fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&dir),
        Ok(_) => std::fs::remove_file(&dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
