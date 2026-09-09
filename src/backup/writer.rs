//! Creating a `.flatbak` archive.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::appdata::{self, DataInfo};
use crate::backup::container::{self, HashingWriter, Header, Trailer};
use crate::backup::manifest::{AppEntry, Manifest, Meta, RemoteEntry};
use crate::backup::PAYLOAD_DATA_DIR;
use crate::config;
use crate::error::Issue;
use crate::flatpak::{InstalledApp, Remote};
use crate::util::{symlink_target_is_contained, Cancel};

/// One application the user chose to include.
#[derive(Debug, Clone)]
pub struct AppSelection {
    pub app: InstalledApp,
    pub include_data: bool,
}

/// Everything needed to write an archive.
#[derive(Debug, Clone)]
pub struct BackupRequest {
    pub destination: PathBuf,
    pub apps: Vec<AppSelection>,
    /// Leave `~/.var/app/<id>/cache` out of the archive.
    pub exclude_caches: bool,
    pub compression_level: i32,
    /// Remotes configured on this system; only those actually referenced by a
    /// selected application are recorded.
    pub remotes: Vec<Remote>,
    pub flatpak_version: String,
}

/// Progress messages sent to the UI while a backup runs.
#[derive(Debug, Clone)]
pub enum BackupProgress {
    /// Measuring application data before anything is written.
    Scanning { app: String, index: usize, total: usize },
    /// Started writing one application's data.
    Writing { app: String, index: usize, total: usize },
    /// Uncompressed bytes read so far, against the pre-scanned total.
    Bytes { done: u64, total: u64 },
    /// Something the user should know about, but which did not stop the backup.
    Issue(Issue),
    /// Flushing the compressor and moving the archive into place.
    Finalising,
}

/// Outcome of a completed backup.
#[derive(Debug, Clone)]
pub struct BackupReport {
    pub path: PathBuf,
    pub archive_bytes: u64,
    pub app_count: usize,
    pub data_app_count: usize,
    pub data_bytes: u64,
    pub issues: Vec<Issue>,
}

/// Deletes a partially written file unless explicitly disarmed.
///
/// An interrupted backup must never leave something that looks like a usable
/// archive, so the archive is built under a `.part` name and only renamed into
/// place once the trailer and header are both committed.
struct PartialFileGuard {
    path: PathBuf,
    armed: bool,
}

impl PartialFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartialFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A reader that yields exactly `size` bytes: short files are zero-padded and
/// long ones are truncated.
///
/// A tar header commits to a size before the data is copied. Application data
/// belonging to a running application can change size in between, which would
/// otherwise desynchronise the whole stream and corrupt every following entry.
/// Padding keeps the archive well formed; the affected file is reported.
struct ExactSizeReader<R> {
    inner: R,
    remaining: u64,
    /// Set when the source ran out early and zero padding was substituted.
    short: bool,
}

impl<R: Read> ExactSizeReader<R> {
    fn new(inner: R, size: u64) -> Self {
        Self {
            inner,
            remaining: size,
            short: false,
        }
    }
}

impl<R: Read> Read for ExactSizeReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let cap = buf.len().min(self.remaining as usize);
        match self.inner.read(&mut buf[..cap]) {
            Ok(0) => {
                // Source is shorter than advertised: pad the rest with zeros.
                self.short = true;
                buf[..cap].fill(0);
                self.remaining -= cap as u64;
                Ok(cap)
            }
            Ok(read) => {
                self.remaining -= read as u64;
                Ok(read)
            }
            Err(error) => Err(error),
        }
    }
}

/// Creates the archive described by `request`.
pub fn create(
    request: &BackupRequest,
    cancel: &Cancel,
    progress: &mut impl FnMut(BackupProgress),
) -> Result<BackupReport> {
    let mut issues: Vec<Issue> = Vec::new();

    // ---- Phase 1: measure, so the manifest and the progress bar both know
    // what is coming before a single byte is written.
    let total_apps = request.apps.len();
    let mut entries: Vec<AppEntry> = Vec::with_capacity(total_apps);
    let mut data_sources: Vec<(usize, DataInfo)> = Vec::new();
    let mut total_data_bytes = 0u64;

    for (index, selection) in request.apps.iter().enumerate() {
        cancel.check()?;
        progress(BackupProgress::Scanning {
            app: selection.app.display_name().to_owned(),
            index,
            total: total_apps,
        });

        let mut entry = AppEntry::from_installed(&selection.app);

        if selection.app.is_sideloaded() {
            issues.push(Issue::Other {
                message: format!(
                    "{}: installed without a remote, so it cannot be reinstalled automatically",
                    selection.app.display_name()
                ),
            });
        }

        if selection.include_data {
            let info = appdata::inspect(&selection.app.id);
            if let Some(error) = &info.error {
                issues.push(Issue::ReadFailed {
                    path: appdata::data_dir(&selection.app.id),
                    message: error.clone(),
                });
            }
            let bytes = info.bytes_to_back_up(request.exclude_caches);
            if !info.present {
                issues.push(Issue::NoDataDirectory {
                    app: selection.app.display_name().to_owned(),
                });
            } else if bytes == 0 && request.exclude_caches && info.is_only_cache() {
                issues.push(Issue::Other {
                    message: format!(
                        "{}: only cached data found, nothing backed up",
                        selection.app.display_name()
                    ),
                });
            } else {
                entry.data_included = true;
                entry.data_bytes = bytes;
                entry.data_files = info.file_count;
                entry.data_path = entry.payload_prefix();
                total_data_bytes = total_data_bytes.saturating_add(bytes);
                data_sources.push((index, info));
            }
        }

        entries.push(entry);
    }

    // Record only the remotes the selected applications actually need, so a
    // restore can recreate a missing one from the archive itself.
    let needed: Vec<String> = {
        let mut names: Vec<String> = entries
            .iter()
            .filter(|entry| !entry.is_sideloaded())
            .map(|entry| entry.origin.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    };
    let remotes: Vec<RemoteEntry> = request
        .remotes
        .iter()
        .filter(|remote| needed.contains(&remote.name))
        .map(RemoteEntry::from_remote)
        .collect();
    for name in &needed {
        if !remotes.iter().any(|remote| &remote.name == name) {
            issues.push(Issue::Other {
                message: format!(
                    "the remote \u{201c}{name}\u{201d} is no longer configured, \
                     so its URL could not be recorded"
                ),
            });
        }
    }

    let data_app_count = entries.iter().filter(|entry| entry.data_included).count();
    let manifest = Manifest {
        flatbak: Meta {
            format_version: config::MANIFEST_FORMAT_VERSION,
            created_at: crate::backup::now_rfc3339(),
            created_by: format!("{} {}", config::APP_NAME, config::VERSION),
            host_arch: host_arch(),
            flatpak_version: request.flatpak_version.clone(),
            excluded_caches: request.exclude_caches,
            app_count: entries.len() as u32,
            data_bytes: total_data_bytes,
        },
        apps: entries,
        remotes,
    };

    let manifest_toml = manifest.to_toml()?;
    let manifest_compressed = zstd::encode_all(manifest_toml.as_bytes(), request.compression_level)
        .context("Could not compress the backup manifest")?;
    let manifest_len: u32 = manifest_compressed
        .len()
        .try_into()
        .context("The backup manifest is implausibly large")?;

    cancel.check()?;

    // ---- Phase 2: write the archive to a temporary neighbour of the target.
    let destination = &request.destination;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Could not create the destination folder {}", parent.display())
        })?;
    }
    let partial_path = partial_path_for(destination);
    let mut guard = PartialFileGuard::new(partial_path.clone());

    let file = File::create(&partial_path)
        .with_context(|| format!("Could not create {}", partial_path.display()))?;
    let mut file = BufWriter::new(file);

    // Reserve the header; its payload length is only known at the end.
    container::write_header(&mut file, &Header::new(manifest_len, 0))
        .context("Could not write the backup header")?;
    file.write_all(&manifest_compressed)
        .context("Could not write the backup manifest")?;

    let mut bytes_done = 0u64;
    let (payload_len, payload_digest) = {
        let mut counting = HashingWriter::new(&mut file);
        let encoder = zstd::Encoder::new(&mut counting, request.compression_level)
            .context("Could not start compression")?;
        let mut tar = tar::Builder::new(encoder);
        // Store symlinks as symlinks rather than copying their targets.
        tar.follow_symlinks(false);

        for (position, (selection_index, info)) in data_sources.iter().enumerate() {
            cancel.check()?;
            let selection = &request.apps[*selection_index];
            progress(BackupProgress::Writing {
                app: selection.app.display_name().to_owned(),
                index: position,
                total: data_sources.len(),
            });
            let _ = info;

            append_app_data(
                &mut tar,
                &selection.app.id,
                request.exclude_caches,
                cancel,
                &mut issues,
                &mut |delta| {
                    bytes_done = bytes_done.saturating_add(delta);
                    progress(BackupProgress::Bytes {
                        done: bytes_done,
                        total: total_data_bytes,
                    });
                },
            )?;
        }

        progress(BackupProgress::Finalising);
        let encoder = tar.into_inner().context("Could not finish the archive")?;
        encoder.finish().context("Could not finish compression")?;
        let (_, written, digest) = counting.finish();
        (written, digest)
    };

    file.write_all(
        &Trailer {
            payload_sha256: payload_digest,
        }
        .encode(),
    )
    .context("Could not write the backup trailer")?;

    // Patch the header now that the compressed payload length is known.
    container::write_header(&mut file, &Header::new(manifest_len, payload_len))
        .context("Could not finalise the backup header")?;

    let mut file = file
        .into_inner()
        .context("Could not flush the backup to disk")?;
    let archive_bytes = file.seek(SeekFrom::End(0))?;
    // Durability matters here: the whole point of a backup is surviving the
    // reinstall that follows.
    file.sync_all().context("Could not flush the backup to disk")?;
    drop(file);

    std::fs::rename(&partial_path, destination).with_context(|| {
        format!(
            "Could not move the finished backup to {}",
            destination.display()
        )
    })?;
    guard.disarm();

    Ok(BackupReport {
        path: destination.clone(),
        archive_bytes,
        app_count: manifest.apps.len(),
        data_app_count,
        data_bytes: total_data_bytes,
        issues,
    })
}

/// Adds one application's data directory to the tar stream.
fn append_app_data<W: Write>(
    tar: &mut tar::Builder<W>,
    app_id: &str,
    exclude_caches: bool,
    cancel: &Cancel,
    issues: &mut Vec<Issue>,
    on_bytes: &mut impl FnMut(u64),
) -> Result<()> {
    let root = appdata::data_dir(app_id);
    let prefix = PathBuf::from(PAYLOAD_DATA_DIR).join(app_id);

    // The application's own directory, so an app with only empty
    // subdirectories still round-trips.
    tar.append_dir(&prefix, &root)
        .with_context(|| format!("Could not add {} to the archive", root.display()))?;

    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        cancel.check()?;
        let current = root.join(&relative);
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) => {
                issues.push(Issue::ReadFailed {
                    path: current.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };

        for entry in entries {
            cancel.check()?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    issues.push(Issue::ReadFailed {
                        path: current.clone(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let name = entry.file_name();
            let child_relative = relative.join(&name);

            if exclude_caches
                && relative.as_os_str().is_empty()
                && name == std::ffi::OsStr::new(appdata::CACHE_DIR)
            {
                continue;
            }

            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    issues.push(Issue::ReadFailed {
                        path: path.clone(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let archive_path = prefix.join(&child_relative);

            if metadata.is_dir() {
                if let Err(error) = tar.append_dir(&archive_path, &path) {
                    issues.push(Issue::ReadFailed {
                        path: path.clone(),
                        message: error.to_string(),
                    });
                    continue;
                }
                stack.push(child_relative);
            } else if metadata.is_symlink() {
                match plan_symlink(&path, &root, &child_relative) {
                    SymlinkPlan::Store(target) => {
                        if let Err(error) =
                            append_symlink(tar, &archive_path, &target, &metadata)
                        {
                            issues.push(Issue::ReadFailed {
                                path: path.clone(),
                                message: error.to_string(),
                            });
                        }
                    }
                    SymlinkPlan::Skip(reason) => {
                        issues.push(Issue::Other {
                            message: format!("{}: skipped, {reason}", path.display()),
                        });
                    }
                }
            } else if metadata.is_file() {
                match append_regular_file(tar, &path, &archive_path, &metadata) {
                    Ok(padded) => {
                        if padded {
                            issues.push(Issue::Other {
                                message: format!(
                                    "{}: changed while being backed up and was padded to its \
                                     original size",
                                    path.display()
                                ),
                            });
                        }
                        on_bytes(metadata.len());
                    }
                    Err(error) => {
                        // Opening failed, so nothing was written to the stream
                        // and the archive stays consistent.
                        issues.push(Issue::ReadFailed {
                            path: path.clone(),
                            message: error.to_string(),
                        });
                    }
                }
            } else {
                // Sockets, FIFOs and device nodes carry no restorable state.
                issues.push(Issue::Other {
                    message: format!("{}: skipped, not a regular file", path.display()),
                });
            }
        }
    }
    Ok(())
}

/// What to do with one symlink found in application data.
enum SymlinkPlan {
    /// Store it, with this (always relative) target.
    Store(PathBuf),
    /// Leave it out, for the stated reason.
    Skip(String),
}

/// Decides how to store a symlink.
///
/// Absolute targets are the interesting case. Application data really does
/// contain them - libvirt writes its autostart links as absolute paths, for
/// instance - but an absolute path is worse than useless in a backup: the
/// machine it is restored onto may have a different user name, and therefore a
/// different home directory. So an absolute target pointing back inside the
/// application's own data directory is rewritten as a relative one, which
/// restores correctly anywhere. Anything else (a runtime socket under
/// `/run/user`, a path elsewhere on the old system) is left out and reported,
/// because it cannot be made meaningful on the new machine.
fn plan_symlink(link_path: &Path, app_root: &Path, link_relative: &Path) -> SymlinkPlan {
    let target = match std::fs::read_link(link_path) {
        Ok(target) => target,
        Err(error) => return SymlinkPlan::Skip(format!("could not be read: {error}")),
    };
    let link_dir = link_relative.parent().unwrap_or(Path::new(""));
    let depth = link_dir.components().count();

    let relative_target = if target.is_relative() {
        target
    } else {
        match strip_app_root(app_root, &target) {
            Some(inside) => relative_between(link_dir, &inside),
            None => {
                return SymlinkPlan::Skip(format!(
                    "it points to {}, outside the application's data folder",
                    target.display()
                ))
            }
        }
    };

    if symlink_target_is_contained(depth, &relative_target) {
        SymlinkPlan::Store(relative_target)
    } else {
        SymlinkPlan::Skip(format!(
            "it points to {}, outside the application's data folder",
            relative_target.display()
        ))
    }
}

/// Returns `target` relative to the application's data directory, if it is
/// inside it.
///
/// Both the plain and the canonical form of the root are tried, because a
/// distribution may present the same directory under two paths - `/home`
/// symlinked to `/var/home` being the common case.
fn strip_app_root(app_root: &Path, target: &Path) -> Option<PathBuf> {
    if let Ok(rest) = target.strip_prefix(app_root) {
        return Some(rest.to_path_buf());
    }
    let canonical = std::fs::canonicalize(app_root).ok()?;
    target.strip_prefix(&canonical).ok().map(Path::to_path_buf)
}

/// Builds the path from `from_dir` to `to`, both relative to the same root.
fn relative_between(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to.components().collect();
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();

    let mut out = PathBuf::new();
    for _ in shared..from.len() {
        out.push("..");
    }
    for component in &to[shared..] {
        out.push(component);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Appends one symlink, storing the link rather than what it points at.
fn append_symlink<W: Write>(
    tar: &mut tar::Builder<W>,
    archive_path: &Path,
    target: &Path,
    metadata: &std::fs::Metadata,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_metadata(metadata);
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("").ok();
    header.set_groupname("").ok();
    tar.append_link(&mut header, archive_path, target)?;
    Ok(())
}

/// Appends one regular file, returning true if it had to be zero-padded.
fn append_regular_file<W: Write>(
    tar: &mut tar::Builder<W>,
    path: &Path,
    archive_path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<bool> {
    // Open before touching the tar stream: a permission error must not leave a
    // header without its data.
    let file = File::open(path)?;

    let mut header = tar::Header::new_gnu();
    header.set_metadata(metadata);
    header.set_size(metadata.len());
    header.set_entry_type(tar::EntryType::Regular);
    // Ownership is meaningless across machines; a restore runs as the user.
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("").ok();
    header.set_groupname("").ok();

    let mut reader = ExactSizeReader::new(file, metadata.len());
    tar.append_data(&mut header, archive_path, &mut reader)?;
    Ok(reader.short)
}

/// `<destination>.part`, kept alongside the target so the final rename is atomic.
fn partial_path_for(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    destination.with_file_name(name)
}

/// Architecture of this machine, in Flatpak's naming.
fn host_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "x86" => "i386",
        "aarch64" => "aarch64",
        "arm" => "arm",
        other => other,
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn exact_size_reader_pads_short_sources() {
        let source: &[u8] = b"abc";
        let mut reader = ExactSizeReader::new(source, 6);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc\0\0\0");
        assert!(reader.short);
    }

    #[test]
    fn exact_size_reader_truncates_long_sources() {
        let source: &[u8] = b"abcdef";
        let mut reader = ExactSizeReader::new(source, 3);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
        assert!(!reader.short);
    }

    #[test]
    fn relative_between_walks_up_to_the_common_ancestor() {
        assert_eq!(
            relative_between(
                Path::new("config/libvirt/storage/autostart"),
                Path::new("config/libvirt/storage/boxes.xml")
            ),
            PathBuf::from("../boxes.xml")
        );
        assert_eq!(
            relative_between(Path::new("config"), Path::new("data/file")),
            PathBuf::from("../data/file")
        );
        assert_eq!(
            relative_between(Path::new(""), Path::new("data/file")),
            PathBuf::from("data/file")
        );
        assert_eq!(
            relative_between(Path::new("a/b"), Path::new("a/b")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn partial_path_sits_next_to_the_target() {
        assert_eq!(
            partial_path_for(Path::new("/backups/mine.flatbak")),
            PathBuf::from("/backups/mine.flatbak.part")
        );
    }
}
