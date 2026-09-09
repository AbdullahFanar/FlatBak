//! Small helpers shared by the backup, restore and UI layers.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};

/// Cooperative cancellation flag shared between the UI and worker threads.
///
/// Long running work polls this between units of work (a file, an app) so that
/// cancelling never leaves a half-written archive or a half-restored data
/// directory behind: callers unwind through the normal error path instead.
#[derive(Clone, Default, Debug)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Returns `Err(Cancelled)` if cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!(crate::error::Cancelled);
        }
        Ok(())
    }
}

/// Formats a byte count the way GLib and the `flatpak` CLI do: SI units with
/// 1000 as the base, so numbers here match what `flatpak list` reports.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Best-effort inverse of [`format_size`], used to turn the human readable
/// size column of `flatpak list` back into a byte count.
///
/// Returns `None` for anything unparseable (including flatpak's `?` placeholder
/// for sizes it does not know) so callers can treat the size as unknown rather
/// than as zero.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || text == "?" || text == "-" {
        return None;
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.trim().replace(',', ".").parse().ok()?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" | "bytes" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "pb" => 1e15,
        // Binary units, in case a future flatpak switches to them.
        "kib" => 1024.0,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some((number * multiplier).round() as u64)
}

/// Returns true if `id` is shaped like a Flatpak application ID.
///
/// This is deliberately strict: the ID is used to build filesystem paths both
/// when writing an archive and when extracting one, so anything that could
/// escape a directory (separators, `.`, `..`, empty strings) is rejected.
pub fn is_valid_app_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 255 || id == "." || id == ".." {
        return false;
    }
    if id.starts_with('.') || id.starts_with('-') {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Joins `relative` onto `base`, rejecting anything that would escape `base`.
///
/// Archive entry paths are untrusted input, so this refuses absolute paths,
/// `..` traversal, Windows-style prefixes and root components rather than
/// normalising them away. The check is lexical, which is sufficient because the
/// caller creates every intermediate directory itself and never follows a
/// symlink out of the destination tree.
pub fn safe_join(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut out = base.to_path_buf();
    let mut depth = 0usize;
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_str().unwrap_or_default();
                if text.is_empty() || text == "." || text == ".." {
                    bail!("archive entry has a suspicious path component");
                }
                out.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                bail!("archive entry path escapes the destination directory");
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("archive entry path is absolute");
            }
        }
    }
    if depth == 0 {
        bail!("archive entry path is empty");
    }
    Ok(out)
}

/// Validates a symlink target stored in an archive.
///
/// Symlinks are preserved because application data legitimately contains them,
/// but only when they resolve to somewhere inside the directory being restored.
/// `link_depth` is how many directory levels separate the link's own directory
/// from the application data root, so `0` means the link sits directly in
/// `~/.var/app/<id>/`. That depth is what a leading `..` is allowed to consume:
/// `config/theme -> ../data/theme` is fine, while `config/theme -> ../../..`
/// is not.
///
/// The check is lexical, which is sufficient because it is applied to *every*
/// symlink written: a link that cannot climb above the root cannot be used as a
/// stepping stone by a later entry either.
pub fn symlink_target_is_contained(link_depth: usize, target: &Path) -> bool {
    if target.as_os_str().is_empty() || target.is_absolute() {
        return false;
    }
    let mut depth = link_depth as i64;
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// How many directory levels sit above `relative`'s final component.
///
/// `config/prefs.js` is at depth 1, a name directly in the root is at depth 0.
pub fn parent_depth(relative: &Path) -> usize {
    relative
        .parent()
        .map(|parent| parent.components().count())
        .unwrap_or(0)
}

/// Ensures `path` carries the `.flatbak` extension.
pub fn with_backup_extension(path: PathBuf) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(crate::config::BACKUP_EXTENSION))
    {
        path
    } else {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{}", crate::config::BACKUP_EXTENSION));
        path.with_file_name(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_like_flatpak() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_000), "1.0 kB");
        assert_eq!(format_size(1_400_000_000), "1.4 GB");
        assert_eq!(format_size(420_700_000), "421 MB");
    }

    #[test]
    fn parses_sizes_round_trip() {
        assert_eq!(parse_size("1.4 GB"), Some(1_400_000_000));
        assert_eq!(parse_size("13 MB"), Some(13_000_000));
        assert_eq!(parse_size("112 kB"), Some(112_000));
        assert_eq!(parse_size("?"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("nonsense"), None);
    }

    #[test]
    fn rejects_bad_app_ids() {
        assert!(is_valid_app_id("org.videolan.VLC"));
        assert!(is_valid_app_id("io.github.kolunmi.Bazaar"));
        assert!(!is_valid_app_id(""));
        assert!(!is_valid_app_id("."));
        assert!(!is_valid_app_id(".."));
        assert!(!is_valid_app_id("../etc"));
        assert!(!is_valid_app_id("a/b"));
        assert!(!is_valid_app_id(".hidden"));
        assert!(!is_valid_app_id("with space"));
    }

    #[test]
    fn safe_join_blocks_traversal() {
        let base = Path::new("/tmp/dest");
        assert_eq!(
            safe_join(base, Path::new("config/prefs.js")).unwrap(),
            Path::new("/tmp/dest/config/prefs.js")
        );
        assert!(safe_join(base, Path::new("../escape")).is_err());
        assert!(safe_join(base, Path::new("/etc/passwd")).is_err());
        assert!(safe_join(base, Path::new("a/../../b")).is_err());
        assert!(safe_join(base, Path::new("")).is_err());
    }

    #[test]
    fn symlink_containment_respects_the_link_depth() {
        // A link sitting directly in the application root cannot use `..`.
        assert!(symlink_target_is_contained(0, Path::new("sibling")));
        assert!(symlink_target_is_contained(0, Path::new("data/file")));
        assert!(!symlink_target_is_contained(0, Path::new("..")));
        assert!(!symlink_target_is_contained(0, Path::new("../elsewhere")));

        // One level down, a single `..` is legitimate: this is the shape real
        // application data uses.
        assert!(symlink_target_is_contained(1, Path::new("../library.db")));
        assert!(symlink_target_is_contained(2, Path::new("../../library.db")));
        assert!(!symlink_target_is_contained(1, Path::new("../../escape")));

        // Interior `..` is fine as long as the walk never dips below the root.
        assert!(symlink_target_is_contained(0, Path::new("nested/deep/../file")));
        assert!(!symlink_target_is_contained(0, Path::new("nested/../../file")));

        // Absolute targets and empty targets are always refused.
        assert!(!symlink_target_is_contained(5, Path::new("/etc/passwd")));
        assert!(!symlink_target_is_contained(5, Path::new("")));
    }

    #[test]
    fn parent_depth_counts_directories() {
        assert_eq!(parent_depth(Path::new("name")), 0);
        assert_eq!(parent_depth(Path::new("config/prefs.js")), 1);
        assert_eq!(parent_depth(Path::new("data/nested/link")), 2);
    }
}
