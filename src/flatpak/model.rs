//! Data model for the Flatpak state FlatBak reads and writes.

use serde::{Deserialize, Serialize};

/// Which Flatpak installation an application lives in.
///
/// Flatpak supports the two well known installations plus arbitrary named ones
/// configured in `/etc/flatpak/installations.d`. FlatBak keeps whatever it
/// found so that a restore can put the application back where it was.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Installation {
    User,
    System,
    Named(String),
}

impl Installation {
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "user" => Installation::User,
            "system" | "" => Installation::System,
            other => Installation::Named(other.to_owned()),
        }
    }

    /// The value stored in the manifest and shown in the UI.
    pub fn as_str(&self) -> &str {
        match self {
            Installation::User => "user",
            Installation::System => "system",
            Installation::Named(name) => name,
        }
    }

    /// A short human readable label.
    pub fn label(&self) -> String {
        match self {
            Installation::User => "User".to_owned(),
            Installation::System => "System".to_owned(),
            Installation::Named(name) => name.clone(),
        }
    }

    /// The arguments that target this installation on the `flatpak` command line.
    pub fn cli_args(&self) -> Vec<String> {
        match self {
            Installation::User => vec!["--user".to_owned()],
            Installation::System => vec!["--system".to_owned()],
            Installation::Named(name) => vec![format!("--installation={name}")],
        }
    }

    /// True when installing here needs administrator authorisation.
    pub fn needs_authorisation(&self) -> bool {
        !matches!(self, Installation::User)
    }
}

impl From<String> for Installation {
    fn from(value: String) -> Self {
        Installation::parse(&value)
    }
}

impl From<Installation> for String {
    fn from(value: Installation) -> Self {
        value.as_str().to_owned()
    }
}

impl std::fmt::Display for Installation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An application currently installed on this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: String,
    /// Display name from the app's appdata, falling back to the ID.
    pub name: String,
    pub version: String,
    pub branch: String,
    pub arch: String,
    /// Remote the app was installed from. Empty for sideloaded apps.
    pub origin: String,
    pub installation: Installation,
    /// Full ref, e.g. `app/org.videolan.VLC/x86_64/stable`.
    pub reference: String,
    pub commit: String,
    /// Installed size in bytes, when flatpak reported one.
    pub installed_size: Option<u64>,
}

impl InstalledApp {
    /// A key that is unique even when the same app is installed both per-user
    /// and system-wide.
    pub fn key(&self) -> String {
        format!("{}@{}", self.reference, self.installation.as_str())
    }

    /// Best display name, never empty.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// True for apps with no remote, which therefore cannot be reinstalled.
    pub fn is_sideloaded(&self) -> bool {
        self.origin.is_empty() || self.origin == "-"
    }
}

/// A configured Flatpak remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub title: String,
    pub url: String,
    pub installation: Installation,
    pub disabled: bool,
}

impl Remote {
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() || self.title == "-" {
            &self.name
        } else {
            &self.title
        }
    }
}
