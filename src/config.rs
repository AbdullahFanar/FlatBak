//! Compile-time configuration and format constants.

/// Application ID used for GSettings, the desktop file and D-Bus.
pub const APP_ID: &str = "io.github.abdullahfanar.FlatBak";

/// Human readable application name.
pub const APP_NAME: &str = "FlatBak";

/// Crate version, surfaced in the about dialog and written into manifests.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// File extension (without dot) of backup archives.
pub const BACKUP_EXTENSION: &str = "flatbak";

/// The manifest schema version this build writes.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// The highest manifest schema version this build can read.
///
/// Archives declaring a higher version are rejected with a "please upgrade"
/// message rather than being parsed on a best-effort basis, because a future
/// version may change the meaning of existing fields.
pub const MANIFEST_FORMAT_VERSION_MAX: u32 = 1;

/// Default zstd compression level. Level 10 roughly halves application data
/// while staying fast enough to keep the UI responsive on a single thread.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 10;
