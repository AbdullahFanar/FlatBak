//! The versioned TOML manifest stored inside every `.flatbak` archive.
//!
//! # Compatibility contract
//!
//! * `flatbak.format_version` is the only field a reader may assume exists. It
//!   is checked before anything else is interpreted.
//! * Readers ignore unknown keys, so a later version can add fields without
//!   breaking this one. Every optional field therefore carries `#[serde(default)]`.
//! * A later version that needs to *change* the meaning of an existing field
//!   bumps `format_version`; this reader refuses versions it does not know
//!   rather than guessing.

use serde::{Deserialize, Serialize};

use crate::config;
use crate::error::ArchiveError;
use crate::flatpak::{InstalledApp, Installation, Remote};
use crate::util::is_valid_app_id;

/// Root of the manifest document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub flatbak: Meta,
    #[serde(default, rename = "app")]
    pub apps: Vec<AppEntry>,
    /// Remotes the backed-up applications came from, so a restore can offer to
    /// recreate one that is missing without asking the user for a URL.
    #[serde(default, rename = "remote")]
    pub remotes: Vec<RemoteEntry>,
}

/// Archive-wide metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub format_version: u32,
    /// RFC 3339 timestamp, stored as a string so an unparseable value from a
    /// future version cannot make the whole manifest unreadable.
    pub created_at: String,
    /// e.g. `FlatBak 0.1.0`.
    pub created_by: String,
    /// Architecture of the machine that made the backup, e.g. `x86_64`.
    pub host_arch: String,
    #[serde(default)]
    pub flatpak_version: String,
    /// True when `cache` subdirectories were left out of application data.
    #[serde(default)]
    pub excluded_caches: bool,
    /// Number of `[[app]]` entries, for a cheap sanity check.
    #[serde(default)]
    pub app_count: u32,
    /// Uncompressed size of all included application data.
    #[serde(default)]
    pub data_bytes: u64,
}

/// One backed-up application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub branch: String,
    pub arch: String,
    /// Remote the application was installed from.
    #[serde(default)]
    pub origin: String,
    pub installation: Installation,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub installed_size: u64,
    /// Whether this archive carries the application's data.
    pub data_included: bool,
    #[serde(default)]
    pub data_bytes: u64,
    #[serde(default)]
    pub data_files: u64,
    /// Prefix of this application's entries in the payload, `data/<id>`.
    #[serde(default)]
    pub data_path: String,
}

impl AppEntry {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// The prefix this app's payload entries must start with.
    pub fn payload_prefix(&self) -> String {
        format!("{}/{}", super::PAYLOAD_DATA_DIR, self.id)
    }

    /// True when the application has no remote and so cannot be reinstalled.
    pub fn is_sideloaded(&self) -> bool {
        self.origin.is_empty()
    }

    pub fn from_installed(app: &InstalledApp) -> Self {
        Self {
            id: app.id.clone(),
            name: app.display_name().to_owned(),
            version: app.version.clone(),
            branch: app.branch.clone(),
            arch: app.arch.clone(),
            origin: app.origin.clone(),
            installation: app.installation.clone(),
            reference: app.reference.clone(),
            commit: app.commit.clone(),
            installed_size: app.installed_size.unwrap_or(0),
            data_included: false,
            data_bytes: 0,
            data_files: 0,
            data_path: String::new(),
        }
    }
}

/// A remote referenced by at least one backed-up application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    #[serde(default)]
    pub title: String,
    pub url: String,
    pub installation: Installation,
}

impl RemoteEntry {
    pub fn from_remote(remote: &Remote) -> Self {
        Self {
            name: remote.name.clone(),
            title: remote.title.clone(),
            url: remote.url.clone(),
            installation: remote.installation.clone(),
        }
    }

    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.name
        } else {
            &self.title
        }
    }
}

impl Manifest {
    /// Parses and validates a manifest document.
    pub fn from_toml(text: &str) -> Result<Self, ArchiveError> {
        // Read the version on its own first: a manifest from a future release
        // may not fit this struct at all, and "please update" is a far better
        // message than a field-level parse error.
        #[derive(Deserialize)]
        struct VersionProbe {
            flatbak: VersionProbeMeta,
        }
        #[derive(Deserialize)]
        struct VersionProbeMeta {
            format_version: u32,
        }

        let probe: VersionProbe = toml::from_str(text)
            .map_err(|error| ArchiveError::InvalidManifest(error.message().to_owned()))?;
        if probe.flatbak.format_version > config::MANIFEST_FORMAT_VERSION_MAX {
            return Err(ArchiveError::UnsupportedVersion {
                found: probe.flatbak.format_version,
                supported: config::MANIFEST_FORMAT_VERSION_MAX,
            });
        }

        let manifest: Manifest = toml::from_str(text)
            .map_err(|error| ArchiveError::InvalidManifest(error.message().to_owned()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_toml(&self) -> Result<String, ArchiveError> {
        toml::to_string_pretty(self)
            .map_err(|error| ArchiveError::InvalidManifest(error.to_string()))
    }

    /// Rejects manifests that are self-inconsistent or that describe payload
    /// paths we would refuse to extract anyway.
    fn validate(&self) -> Result<(), ArchiveError> {
        if self.flatbak.format_version == 0 {
            return Err(ArchiveError::InvalidManifest(
                "format_version must be at least 1".to_owned(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for app in &self.apps {
            if !is_valid_app_id(&app.id) {
                return Err(ArchiveError::InvalidManifest(format!(
                    "\u{201c}{}\u{201d} is not a valid application ID",
                    app.id
                )));
            }
            if app.branch.is_empty() || app.arch.is_empty() {
                return Err(ArchiveError::InvalidManifest(format!(
                    "{} is missing its branch or architecture",
                    app.id
                )));
            }
            if !seen.insert((app.reference.clone(), app.installation.as_str().to_owned())) {
                return Err(ArchiveError::InvalidManifest(format!(
                    "{} appears twice for the same installation",
                    app.id
                )));
            }
            if app.data_included {
                let expected = app.payload_prefix();
                if app.data_path != expected {
                    return Err(ArchiveError::InvalidManifest(format!(
                        "{} declares an unexpected data path \u{201c}{}\u{201d}",
                        app.id, app.data_path
                    )));
                }
            }
        }
        for remote in &self.remotes {
            if remote.name.is_empty() {
                return Err(ArchiveError::InvalidManifest(
                    "a remote entry has no name".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// The remote entry matching `name`, if the archive recorded one.
    pub fn remote(&self, name: &str) -> Option<&RemoteEntry> {
        self.remotes.iter().find(|remote| remote.name == name)
    }

    /// Distinct remote names the archive's applications need.
    pub fn required_remotes(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .apps
            .iter()
            .filter(|app| !app.is_sideloaded())
            .map(|app| app.origin.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Total uncompressed application data the archive claims to hold.
    pub fn total_data_bytes(&self) -> u64 {
        self.apps
            .iter()
            .filter(|app| app.data_included)
            .map(|app| app.data_bytes)
            .sum()
    }

    pub fn apps_with_data(&self) -> usize {
        self.apps.iter().filter(|app| app.data_included).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            flatbak: Meta {
                format_version: 1,
                created_at: "2026-09-08T12:00:00Z".to_owned(),
                created_by: "FlatBak 0.1.0".to_owned(),
                host_arch: "x86_64".to_owned(),
                flatpak_version: "1.18.2".to_owned(),
                excluded_caches: true,
                app_count: 1,
                data_bytes: 1234,
            },
            apps: vec![AppEntry {
                id: "org.videolan.VLC".to_owned(),
                name: "VLC".to_owned(),
                version: "3.0.21".to_owned(),
                branch: "stable".to_owned(),
                arch: "x86_64".to_owned(),
                origin: "flathub".to_owned(),
                installation: Installation::System,
                reference: "app/org.videolan.VLC/x86_64/stable".to_owned(),
                commit: "abc123".to_owned(),
                installed_size: 13_000_000,
                data_included: true,
                data_bytes: 1234,
                data_files: 7,
                data_path: "data/org.videolan.VLC".to_owned(),
            }],
            remotes: vec![RemoteEntry {
                name: "flathub".to_owned(),
                title: "Flathub".to_owned(),
                url: "https://dl.flathub.org/repo/".to_owned(),
                installation: Installation::System,
            }],
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let text = sample().to_toml().unwrap();
        let parsed = Manifest::from_toml(&text).unwrap();
        assert_eq!(parsed.apps.len(), 1);
        assert_eq!(parsed.apps[0].id, "org.videolan.VLC");
        assert_eq!(parsed.apps[0].installation, Installation::System);
        assert_eq!(parsed.remotes[0].url, "https://dl.flathub.org/repo/");
        assert!(parsed.flatbak.excluded_caches);
    }

    #[test]
    fn ignores_unknown_future_keys() {
        let mut text = sample().to_toml().unwrap();
        text.push_str("\n[flatbak.future]\nsomething = true\n");
        let parsed = Manifest::from_toml(&text);
        assert!(parsed.is_ok(), "unknown keys must not break parsing: {parsed:?}");
    }

    #[test]
    fn refuses_newer_format_versions() {
        let text = "[flatbak]\nformat_version = 99\ncreated_at = \"\"\n\
                    created_by = \"\"\nhost_arch = \"\"\n";
        match Manifest::from_toml(text) {
            Err(ArchiveError::UnsupportedVersion { found, .. }) => assert_eq!(found, 99),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn rejects_traversal_in_app_id() {
        let mut manifest = sample();
        manifest.apps[0].id = "../../etc".to_owned();
        let text = toml::to_string(&manifest).unwrap();
        assert!(matches!(
            Manifest::from_toml(&text),
            Err(ArchiveError::InvalidManifest(_))
        ));
    }

    #[test]
    fn rejects_mismatched_data_path() {
        let mut manifest = sample();
        manifest.apps[0].data_path = "data/somewhere.else".to_owned();
        let text = toml::to_string(&manifest).unwrap();
        assert!(matches!(
            Manifest::from_toml(&text),
            Err(ArchiveError::InvalidManifest(_))
        ));
    }

    #[test]
    fn rejects_duplicate_apps() {
        let mut manifest = sample();
        let duplicate = manifest.apps[0].clone();
        manifest.apps.push(duplicate);
        let text = toml::to_string(&manifest).unwrap();
        assert!(matches!(
            Manifest::from_toml(&text),
            Err(ArchiveError::InvalidManifest(_))
        ));
    }
}
