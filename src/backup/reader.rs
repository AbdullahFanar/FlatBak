//! Opening, validating and restoring a `.flatbak` archive.
//!
//! Every archive is checked in three stages, cheapest first, so a bad file is
//! rejected before anything on the system is touched:
//!
//! 1. [`Backup::open`] verifies the framing (signature, header CRC, declared
//!    lengths against the real file size) and parses the manifest.
//! 2. [`Backup::verify_payload`] hashes the compressed payload and compares it
//!    with the trailer. This reads the file but decompresses nothing.
//! 3. Extraction validates every entry path and type as it goes.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::appdata;
use crate::backup::container::{self, Header, Trailer};
use crate::backup::manifest::{AppEntry, Manifest};
use crate::backup::PAYLOAD_DATA_DIR;
use crate::error::{ArchiveError, Issue};
use crate::flatpak::{Flatpak, Installation};
use crate::util::{is_valid_app_id, parent_depth, safe_join, symlink_target_is_contained, Cancel};

/// An opened archive: framing checked, manifest parsed, payload untouched.
#[derive(Debug, Clone)]
pub struct Backup {
    pub path: PathBuf,
    pub header: Header,
    pub trailer: Trailer,
    pub manifest: Manifest,
    pub file_size: u64,
}

impl Backup {
    /// Opens and validates an archive without decompressing its payload.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow!("{} no longer exists.", path.display())
            }
            std::io::ErrorKind::PermissionDenied => {
                anyhow!("Permission denied reading {}.", path.display())
            }
            _ => anyhow!("Could not open {}: {error}", path.display()),
        })?;
        let file_size = file.metadata()?.len();
        let mut reader = BufReader::new(file);

        if file_size < container::HEADER_LEN + container::TRAILER_LEN {
            return Err(if file_size == 0 {
                anyhow!(ArchiveError::NotAFlatbakArchive(path.to_path_buf()))
            } else {
                anyhow!(ArchiveError::Truncated)
            });
        }

        let header = match container::read_header(&mut reader) {
            Ok(header) => header,
            // A bad signature almost always means the user picked the wrong
            // file, which deserves a clearer message than "damaged".
            Err(ArchiveError::Corrupt(reason)) if reason == "bad file signature" => {
                return Err(anyhow!(ArchiveError::NotAFlatbakArchive(
                    path.to_path_buf()
                )))
            }
            Err(error) => return Err(anyhow!(error)),
        };

        if header.expected_file_len() != file_size {
            return Err(if header.expected_file_len() > file_size {
                anyhow!(ArchiveError::Truncated)
            } else {
                anyhow!(ArchiveError::Corrupt(format!(
                    "the file is {} bytes longer than its header describes",
                    file_size - header.expected_file_len()
                )))
            });
        }

        let trailer = container::read_trailer(&mut reader, file_size)?;

        reader.seek(SeekFrom::Start(container::HEADER_LEN))?;
        let mut compressed = vec![0u8; header.manifest_len as usize];
        reader
            .read_exact(&mut compressed)
            .map_err(|_| ArchiveError::Truncated)?;
        let toml = zstd::decode_all(compressed.as_slice())
            .map_err(|error| ArchiveError::Corrupt(format!("manifest: {error}")))?;
        let toml = String::from_utf8(toml)
            .map_err(|_| ArchiveError::InvalidManifest("not valid UTF-8".to_owned()))?;
        let manifest = Manifest::from_toml(&toml)?;

        if manifest.flatbak.app_count != 0
            && manifest.flatbak.app_count as usize != manifest.apps.len()
        {
            return Err(anyhow!(ArchiveError::InvalidManifest(format!(
                "the manifest lists {} applications but declares {}",
                manifest.apps.len(),
                manifest.flatbak.app_count
            ))));
        }

        Ok(Self {
            path: path.to_path_buf(),
            header,
            trailer,
            manifest,
            file_size,
        })
    }

    /// Hashes the compressed payload and compares it with the trailer.
    ///
    /// Nothing is decompressed, so this is disk-bound rather than CPU-bound and
    /// worth doing up front: it means a damaged archive is rejected before any
    /// application is installed or any data directory is replaced.
    pub fn verify_payload(
        &self,
        cancel: &Cancel,
        progress: &mut impl FnMut(u64, u64),
    ) -> Result<()> {
        let mut file = BufReader::new(File::open(&self.path)?);
        file.seek(SeekFrom::Start(self.header.payload_offset()))?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut remaining = self.header.payload_len;
        let total = self.header.payload_len;
        let mut done = 0u64;

        while remaining > 0 {
            cancel.check()?;
            let want = buffer.len().min(remaining as usize);
            let read = file.read(&mut buffer[..want])?;
            if read == 0 {
                return Err(anyhow!(ArchiveError::Truncated));
            }
            hasher.update(&buffer[..read]);
            remaining -= read as u64;
            done += read as u64;
            progress(done, total);
        }

        if hasher.finalize().as_slice() != self.trailer.payload_sha256 {
            return Err(anyhow!(ArchiveError::ChecksumMismatch));
        }
        Ok(())
    }

    /// Remote names the archive needs that are not configured on this system.
    ///
    /// Only the installation the application will actually land in is
    /// considered, because a remote configured per-user does not help a
    /// system-wide install.
    pub fn missing_remotes(
        &self,
        configured: &[crate::flatpak::Remote],
        install_to_user: bool,
    ) -> Vec<MissingRemote> {
        let mut missing: Vec<MissingRemote> = Vec::new();
        for app in &self.manifest.apps {
            if app.is_sideloaded() {
                continue;
            }
            let installation = if install_to_user {
                Installation::User
            } else {
                app.installation.clone()
            };
            let present = configured.iter().any(|remote| {
                remote.name == app.origin && remote.installation == installation
            });
            if present {
                continue;
            }
            if let Some(existing) = missing
                .iter_mut()
                .find(|entry| entry.name == app.origin && entry.installation == installation)
            {
                existing.apps.push(app.display_name().to_owned());
                continue;
            }
            let recorded = self.manifest.remote(&app.origin);
            missing.push(MissingRemote {
                name: app.origin.clone(),
                url: recorded.map(|remote| remote.url.clone()).unwrap_or_default(),
                title: recorded
                    .map(|remote| remote.display_title().to_owned())
                    .unwrap_or_else(|| app.origin.clone()),
                installation,
                apps: vec![app.display_name().to_owned()],
            });
        }
        missing
    }
}

/// A remote an archive needs but which is not configured here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRemote {
    pub name: String,
    /// URL recorded in the archive; empty when the archive did not have one.
    pub url: String,
    pub title: String,
    pub installation: Installation,
    /// Applications that need it, for the explanatory text.
    pub apps: Vec<String>,
}

impl MissingRemote {
    /// True when FlatBak can add this remote unattended.
    pub fn can_add(&self) -> bool {
        !self.url.is_empty()
    }
}

/// What to do when an application already has data on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictChoice {
    /// Delete the existing data directory, then restore the archived one.
    Replace,
    /// Leave the existing data alone and restore nothing for this application.
    #[default]
    Skip,
}

/// One application the user chose to restore.
#[derive(Debug, Clone)]
pub struct RestoreSelection {
    /// Index into `manifest.apps`.
    pub index: usize,
    pub install: bool,
    pub restore_data: bool,
    pub on_conflict: ConflictChoice,
}

/// Everything needed to run a restore.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub selections: Vec<RestoreSelection>,
    /// Install everything into the user installation regardless of where it came
    /// from. Useful when the system installation is not writable.
    pub install_to_user: bool,
    /// Remotes the user agreed to create before restoring.
    pub remotes_to_add: Vec<MissingRemote>,
}

/// Progress messages sent to the UI while a restore runs.
#[derive(Debug, Clone)]
pub enum RestoreProgress {
    Verifying { done: u64, total: u64 },
    AddingRemote { name: String },
    Installing { app: String, index: usize, total: usize },
    /// A line of output from `flatpak install`.
    InstallOutput { line: String },
    ExtractingApp { app: String },
    Bytes { done: u64, total: u64 },
    Issue(Issue),
}

/// Outcome of a completed restore.
#[derive(Debug, Clone, Default)]
pub struct RestoreReport {
    pub installed: Vec<String>,
    pub data_restored: Vec<String>,
    pub issues: Vec<Issue>,
    /// True when the user cancelled after data extraction had begun, so some
    /// application data may be incomplete.
    pub interrupted_during_data: bool,
}

impl RestoreReport {
    pub fn has_problems(&self) -> bool {
        !self.issues.is_empty() || self.interrupted_during_data
    }
}

/// Restores from `backup` according to `request`.
pub fn restore(
    backup: &Backup,
    request: &RestoreRequest,
    flatpak: &Flatpak,
    cancel: &Cancel,
    progress: &mut impl FnMut(RestoreProgress),
) -> Result<RestoreReport> {
    let mut report = RestoreReport::default();

    // ---- Stage 1: prove the archive is intact before changing anything.
    backup.verify_payload(cancel, &mut |done, total| {
        progress(RestoreProgress::Verifying { done, total })
    })?;

    // ---- Stage 2: remotes the user asked us to create.
    for remote in &request.remotes_to_add {
        cancel.check()?;
        progress(RestoreProgress::AddingRemote {
            name: remote.name.clone(),
        });
        if !remote.can_add() {
            report.issues.push(Issue::Other {
                message: format!(
                    "the remote \u{201c}{}\u{201d} has no recorded URL and could not be added",
                    remote.name
                ),
            });
            continue;
        }
        if let Err(error) = flatpak.add_remote(&remote.name, &remote.url, &remote.installation) {
            report.issues.push(Issue::Other {
                message: format!("{}: {error}", remote.name),
            });
        }
    }

    // ---- Stage 3: reinstall applications.
    let to_install: Vec<&RestoreSelection> = request
        .selections
        .iter()
        .filter(|selection| selection.install)
        .collect();

    for (position, selection) in to_install.iter().enumerate() {
        cancel.check()?;
        let Some(entry) = backup.manifest.apps.get(selection.index) else {
            continue;
        };
        progress(RestoreProgress::Installing {
            app: entry.display_name().to_owned(),
            index: position,
            total: to_install.len(),
        });

        match install_one(entry, request.install_to_user, flatpak, cancel, progress) {
            Ok(()) => report.installed.push(entry.display_name().to_owned()),
            Err(error) if crate::error::is_cancellation(&error) => return Err(error),
            Err(error) => {
                let issue = Issue::InstallFailed {
                    app: entry.display_name().to_owned(),
                    message: error.to_string(),
                };
                progress(RestoreProgress::Issue(issue.clone()));
                report.issues.push(issue);
            }
        }
    }

    // ---- Stage 4: application data.
    let mut wanted: Vec<usize> = Vec::new();
    for selection in &request.selections {
        if !selection.restore_data {
            continue;
        }
        let Some(entry) = backup.manifest.apps.get(selection.index) else {
            continue;
        };
        if !entry.data_included {
            continue;
        }
        let existing = appdata::inspect(&entry.id);
        if existing.present {
            match selection.on_conflict {
                ConflictChoice::Skip => {
                    let issue = Issue::DataSkipped {
                        app: entry.display_name().to_owned(),
                    };
                    progress(RestoreProgress::Issue(issue.clone()));
                    report.issues.push(issue);
                    continue;
                }
                ConflictChoice::Replace => {
                    if let Err(error) = appdata::remove_data_dir(&entry.id, cancel) {
                        let issue = Issue::DataRestoreFailed {
                            app: entry.display_name().to_owned(),
                            message: format!("existing data could not be removed: {error}"),
                        };
                        progress(RestoreProgress::Issue(issue.clone()));
                        report.issues.push(issue);
                        continue;
                    }
                }
            }
        }
        wanted.push(selection.index);
    }

    if !wanted.is_empty() {
        let outcome = extract_data(backup, &wanted, cancel, progress, &mut report);
        match outcome {
            Ok(()) => {}
            Err(error) if crate::error::is_cancellation(&error) => {
                report.interrupted_during_data = true;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }

    Ok(report)
}

/// Reinstalls one application from its remote.
fn install_one(
    entry: &AppEntry,
    install_to_user: bool,
    flatpak: &Flatpak,
    cancel: &Cancel,
    progress: &mut impl FnMut(RestoreProgress),
) -> Result<()> {
    if entry.is_sideloaded() {
        return Err(anyhow!(
            "no remote was recorded, so it must be reinstalled by hand"
        ));
    }
    // Rebuild the ref from validated fields rather than trusting the stored
    // string: it comes from a file that may not have been written by us.
    if !is_valid_app_id(&entry.id) {
        return Err(anyhow!("the archive records an invalid application ID"));
    }
    for (label, value) in [
        ("architecture", &entry.arch),
        ("branch", &entry.branch),
        ("remote", &entry.origin),
    ] {
        if !is_safe_token(value) {
            return Err(anyhow!("the archive records an invalid {label}"));
        }
    }
    let reference = format!("app/{}/{}/{}", entry.id, entry.arch, entry.branch);
    let installation = if install_to_user {
        Installation::User
    } else {
        entry.installation.clone()
    };

    flatpak.install(
        &entry.origin,
        &reference,
        &installation,
        cancel,
        |line| {
            progress(RestoreProgress::InstallOutput {
                line: line.to_owned(),
            })
        },
    )
}

/// True for a value safe to pass as a `flatpak` argument and to embed in a ref.
///
/// Arguments are passed without a shell, so the concern is not injection but a
/// value that would be read as an option or that would break the ref's shape.
fn is_safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Streams the payload and extracts the data of the requested applications.
fn extract_data(
    backup: &Backup,
    wanted: &[usize],
    cancel: &Cancel,
    progress: &mut impl FnMut(RestoreProgress),
    report: &mut RestoreReport,
) -> Result<()> {
    let wanted_ids: Vec<&str> = wanted
        .iter()
        .filter_map(|index| backup.manifest.apps.get(*index))
        .map(|entry| entry.id.as_str())
        .collect();
    let total_bytes: u64 = wanted
        .iter()
        .filter_map(|index| backup.manifest.apps.get(*index))
        .map(|entry| entry.data_bytes)
        .sum();

    let data_root = appdata::data_root();
    std::fs::create_dir_all(&data_root).with_context(|| {
        format!(
            "Could not create the application data folder {}",
            data_root.display()
        )
    })?;

    let mut file = BufReader::new(File::open(&backup.path)?);
    file.seek(SeekFrom::Start(backup.header.payload_offset()))?;
    let limited = ReadLimit::new(file, backup.header.payload_len);
    let decoder = zstd::Decoder::new(limited).context("Could not start decompression")?;
    let mut archive = tar::Archive::new(decoder);
    // Ownership and permissions come from the running user, not the archive.
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    let mut done_bytes = 0u64;
    let mut current_app = String::new();
    let mut restored: Vec<String> = Vec::new();

    for entry in archive.entries().context("The backup payload is damaged")? {
        cancel.check()?;
        let mut entry = entry.context("The backup payload is damaged")?;

        let raw_path = match entry.path() {
            Ok(path) => path.to_path_buf(),
            Err(error) => {
                report.issues.push(Issue::UnsafeEntry {
                    path: "<unreadable>".to_owned(),
                    reason: error.to_string(),
                });
                continue;
            }
        };

        let Some((app_id, relative)) = split_payload_path(&raw_path) else {
            report.issues.push(Issue::UnsafeEntry {
                path: raw_path.display().to_string(),
                reason: "not a recognised application data path".to_owned(),
            });
            continue;
        };

        if !wanted_ids.contains(&app_id.as_str()) {
            // Not selected: skip the entry but keep reading so the stream stays
            // in step and the whole payload is consumed.
            continue;
        }

        if current_app != app_id {
            current_app = app_id.clone();
            restored.push(app_id.clone());
            progress(RestoreProgress::ExtractingApp {
                app: app_id.clone(),
            });
        }

        let app_dir = data_root.join(&app_id);
        let destination = if relative.as_os_str().is_empty() {
            app_dir.clone()
        } else {
            match safe_join(&app_dir, &relative) {
                Ok(path) => path,
                Err(error) => {
                    report.issues.push(Issue::UnsafeEntry {
                        path: raw_path.display().to_string(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            }
        };

        if let Err(error) = unpack_entry(&mut entry, &destination, &relative) {
            report.issues.push(Issue::DataRestoreFailed {
                app: app_id.clone(),
                message: format!("{}: {error}", raw_path.display()),
            });
            continue;
        }

        if entry.header().entry_type().is_file() {
            done_bytes = done_bytes.saturating_add(entry.size());
            progress(RestoreProgress::Bytes {
                done: done_bytes,
                total: total_bytes,
            });
        }
    }

    for index in wanted {
        if let Some(entry) = backup.manifest.apps.get(*index) {
            if restored.contains(&entry.id) {
                report.data_restored.push(entry.display_name().to_owned());
            } else {
                report.issues.push(Issue::DataRestoreFailed {
                    app: entry.display_name().to_owned(),
                    message: "the archive contained no data for it".to_owned(),
                });
            }
        }
    }

    Ok(())
}

/// Writes one archive entry to `destination`, rejecting anything unsafe.
///
/// `relative` is the entry's path within the application's own data directory,
/// which is what decides how far a symlink target may climb.
fn unpack_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    destination: &Path,
    relative: &Path,
) -> Result<()> {
    use tar::EntryType;

    let entry_type = entry.header().entry_type();
    match entry_type {
        EntryType::Directory => {
            replace_existing_symlink(destination)?;
            std::fs::create_dir_all(destination)?;
            Ok(())
        }
        EntryType::Regular | EntryType::Continuous => {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Never write through a symlink that happens to sit at the target.
            replace_existing_symlink(destination)?;
            entry.unpack(destination)?;
            Ok(())
        }
        EntryType::Symlink => {
            let target = entry
                .link_name()?
                .ok_or_else(|| anyhow!("symlink without a target"))?
                .to_path_buf();
            let link_dir = destination
                .parent()
                .ok_or_else(|| anyhow!("symlink has no parent directory"))?;
            if !symlink_target_is_contained(parent_depth(relative), &target) {
                return Err(anyhow!(
                    "symlink points outside the application's data directory"
                ));
            }
            std::fs::create_dir_all(link_dir)?;
            match std::fs::symlink_metadata(destination) {
                Ok(metadata) => {
                    if metadata.is_dir() {
                        std::fs::remove_dir_all(destination)?;
                    } else {
                        std::fs::remove_file(destination)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            std::os::unix::fs::symlink(&target, destination)?;
            Ok(())
        }
        // Hard links, device nodes, FIFOs and sockets are never written by
        // FlatBak, so an archive containing one is not one we made.
        other => Err(anyhow!("unsupported archive entry type {other:?}")),
    }
}

/// Removes a symlink sitting where a real file or directory should go.
fn replace_existing_symlink(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => std::fs::remove_file(path),
        _ => Ok(()),
    }
}

/// Splits `data/<app-id>/<rest>` into its application ID and remainder.
///
/// Returns `None` for anything that is not shaped like a payload path, which
/// covers absolute paths, traversal attempts and unexpected top-level names.
fn split_payload_path(path: &Path) -> Option<(String, PathBuf)> {
    use std::path::Component;

    let mut components = path.components();
    match components.next()? {
        Component::Normal(name) if name == std::ffi::OsStr::new(PAYLOAD_DATA_DIR) => {}
        _ => return None,
    }
    let app_id = match components.next()? {
        Component::Normal(name) => name.to_str()?.to_owned(),
        _ => return None,
    };
    if !is_valid_app_id(&app_id) {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            // Anything else is rejected outright rather than normalised.
            _ => return None,
        }
    }
    Some((app_id, relative))
}

/// A reader that stops after `limit` bytes, bounding the payload region.
struct ReadLimit<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> ReadLimit<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: Read> Read for ReadLimit<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let cap = buf.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buf[..cap])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_valid_payload_paths() {
        let (id, rest) = split_payload_path(Path::new("data/org.videolan.VLC/config/vlcrc")).unwrap();
        assert_eq!(id, "org.videolan.VLC");
        assert_eq!(rest, Path::new("config/vlcrc"));

        let (id, rest) = split_payload_path(Path::new("data/org.videolan.VLC")).unwrap();
        assert_eq!(id, "org.videolan.VLC");
        assert_eq!(rest, Path::new(""));
    }

    #[test]
    fn rejects_hostile_payload_paths() {
        for hostile in [
            "/etc/passwd",
            "../data/x/y",
            "data/../../etc/passwd",
            "data/../etc",
            "etc/passwd",
            "data",
            "data/../..",
            "data/..",
        ] {
            assert!(
                split_payload_path(Path::new(hostile)).is_none(),
                "should have rejected {hostile}"
            );
        }
    }

    #[test]
    fn normalises_harmless_redundancy() {
        // `Path::components` collapses repeated separators and `.` segments, so
        // these are the same contained paths written differently.
        let (id, rest) = split_payload_path(Path::new("data//org.x/y")).unwrap();
        assert_eq!((id.as_str(), rest.as_path()), ("org.x", Path::new("y")));

        let (id, rest) = split_payload_path(Path::new("data/./org.x/./y")).unwrap();
        assert_eq!((id.as_str(), rest.as_path()), ("org.x", Path::new("y")));

        // A space is not valid in an application ID.
        assert!(split_payload_path(Path::new("data/a b/y")).is_none());
    }

    #[test]
    fn safe_tokens() {
        assert!(is_safe_token("x86_64"));
        assert!(is_safe_token("stable"));
        assert!(is_safe_token("flathub"));
        assert!(!is_safe_token(""));
        assert!(!is_safe_token("--user"));
        assert!(!is_safe_token("a/b"));
        assert!(!is_safe_token("a;b"));
    }

    #[test]
    fn read_limit_bounds_the_region() {
        let data = b"abcdefghij".to_vec();
        let mut reader = ReadLimit::new(data.as_slice(), 3);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
    }
}
