//! Error types that callers need to distinguish, rather than merely display.

use std::path::PathBuf;

use thiserror::Error;

/// Returned when the user cancels a running operation.
///
/// This is an error so that cancellation unwinds through the same path as a
/// failure (cleaning up temporary files on the way out), but the UI reports it
/// as a cancellation rather than as a problem.
#[derive(Debug, Error)]
#[error("operation cancelled")]
pub struct Cancelled;

/// Reasons FlatBak cannot talk to Flatpak at all.
#[derive(Debug, Error)]
pub enum FlatpakUnavailable {
    #[error("The flatpak command was not found. Install Flatpak to use FlatBak.")]
    NotFound,
    #[error("The flatpak command failed to run: {0}")]
    Failed(String),
}

/// Reasons an archive cannot be opened or trusted.
#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("{0} is not a FlatBak backup.")]
    NotAFlatbakArchive(PathBuf),
    #[error(
        "This backup was created by a newer version of FlatBak \
         (format {found}, this version supports up to {supported}). Please update FlatBak."
    )]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("The backup file is damaged: {0}")]
    Corrupt(String),
    #[error("The backup manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("The backup file is truncated. It may have been copied or downloaded incompletely.")]
    Truncated,
    #[error(
        "The backup contents do not match its checksum. \
         The file is damaged and cannot be restored safely."
    )]
    ChecksumMismatch,
}

/// A per-application problem that does not abort the whole operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// Application data was requested but `~/.var/app/<id>` does not exist.
    NoDataDirectory { app: String },
    /// The remote an application came from is not configured on this system.
    MissingRemote { app: String, remote: String },
    /// Existing application data was left untouched at the user's request.
    DataSkipped { app: String },
    /// Installing an application failed.
    InstallFailed { app: String, message: String },
    /// Restoring an application's data failed.
    DataRestoreFailed { app: String, message: String },
    /// A file could not be read while backing up.
    ReadFailed { path: PathBuf, message: String },
    /// An archive entry was rejected for safety reasons.
    UnsafeEntry { path: String, reason: String },
    /// Anything else worth telling the user about.
    Other { message: String },
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Issue::NoDataDirectory { app } => {
                write!(f, "{app}: no application data found, nothing to back up")
            }
            Issue::MissingRemote { app, remote } => {
                write!(f, "{app}: remote \u{201c}{remote}\u{201d} is not configured")
            }
            Issue::DataSkipped { app } => write!(f, "{app}: kept existing data"),
            Issue::InstallFailed { app, message } => write!(f, "{app}: install failed \u{2014} {message}"),
            Issue::DataRestoreFailed { app, message } => {
                write!(f, "{app}: data could not be restored \u{2014} {message}")
            }
            Issue::ReadFailed { path, message } => {
                write!(f, "{}: could not be read \u{2014} {message}", path.display())
            }
            Issue::UnsafeEntry { path, reason } => {
                write!(f, "skipped unsafe archive entry {path} \u{2014} {reason}")
            }
            Issue::Other { message } => write!(f, "{message}"),
        }
    }
}

/// True if `error` is a cancellation rather than a genuine failure.
pub fn is_cancellation(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Cancelled>().is_some()
}
