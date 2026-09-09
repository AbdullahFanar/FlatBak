//! The GTK4 / Libadwaita user interface.

mod app;
mod dialogs;
mod home;
mod widgets;

pub mod backup_page;
pub mod progress;
pub mod restore_page;

pub use app::Ui;

/// Makes Flatpak's exported application icons available to the icon theme.
///
/// Applications export their icons into their Flatpak installation's `exports`
/// directory. The user installation and the system one are already on GTK's
/// search path through `XDG_DATA_DIRS`, but the host's are not when FlatBak
/// runs inside a container, where the host filesystem appears under `/run/host`.
pub fn register_icon_paths() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    let existing = theme.search_path();

    let mut candidates: Vec<std::path::PathBuf> = vec![
        "/var/lib/flatpak/exports/share/icons".into(),
        "/run/host/var/lib/flatpak/exports/share/icons".into(),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        candidates.push(home.join(".local/share/flatpak/exports/share/icons"));
    }

    for path in candidates {
        if path.is_dir() && !existing.contains(&path) {
            theme.add_search_path(&path);
        }
    }
}
