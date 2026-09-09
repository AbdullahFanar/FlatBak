//! FlatBak: back up and restore Flatpak applications and their user data.
//!
//! The crate is split so the parts that touch the user's system can be tested
//! without a display:
//!
//! * [`flatpak`] talks to the `flatpak` command line tool.
//! * [`appdata`] locates and measures per-application data under `~/.var/app`.
//! * [`backup`] defines the `.flatbak` container, its manifest, and the writer
//!   and reader that produce and consume archives.
//! * [`ui`] is the GTK4 / Libadwaita front end and the only part that needs a
//!   display.

pub mod appdata;
pub mod backup;
pub mod config;
pub mod error;
pub mod flatpak;
pub mod recent;
pub mod ui;
pub mod util;
