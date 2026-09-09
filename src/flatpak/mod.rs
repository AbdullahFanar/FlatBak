//! Discovery of, and operations on, the host's Flatpak installations.

mod cli;
mod model;

pub use cli::Flatpak;
pub use model::{InstalledApp, Installation, Remote};
