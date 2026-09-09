//! The "Recent Backups" list shown on the home page.
//!
//! Stored as TOML under the XDG data directory. The list is advisory: entries
//! whose file has since been moved or deleted are shown as unavailable rather
//! than silently dropped, because a backup living on an external disk is a
//! perfectly normal thing to have.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Most entries to keep.
const MAX_ENTRIES: usize = 10;

/// One remembered backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentBackup {
    pub path: PathBuf,
    /// RFC 3339, copied from the archive's manifest.
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub app_count: u32,
    #[serde(default)]
    pub data_app_count: u32,
    #[serde(default)]
    pub archive_bytes: u64,
}

impl RecentBackup {
    /// Filename without directories, for the row title.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// True when the file is still where we left it.
    pub fn is_available(&self) -> bool {
        self.path.is_file()
    }
}

/// The persisted list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecentStore {
    #[serde(default, rename = "backup")]
    pub entries: Vec<RecentBackup>,
}

impl RecentStore {
    /// Loads the list, returning an empty one if it is missing or unreadable.
    ///
    /// A corrupt list is not worth an error dialog: the worst outcome is an
    /// empty "Recent Backups" section.
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        // Write and rename so an interrupted save cannot truncate the list.
        let temporary = path.with_extension("toml.part");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, &path)
    }

    /// Records a backup, moving it to the front if it is already known.
    pub fn remember(&mut self, entry: RecentBackup) {
        self.entries.retain(|existing| existing.path != entry.path);
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        let _ = self.save();
    }

    pub fn forget(&mut self, path: &Path) {
        self.entries.retain(|entry| entry.path != path);
        let _ = self.save();
    }
}

/// `$XDG_DATA_HOME/flatbak/recent.toml`.
fn store_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".local/share")
        });
    base.join("flatbak").join("recent.toml")
}
