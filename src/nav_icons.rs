//! Bundled symbolic SVGs for the bottom navigation bar.
//!
//! Icons are extracted to the user cache and registered with the GTK icon theme
//! so `-symbolic` recoloring follows the button’s CSS `color` (Adwaita fg).

use gtk::gdk;
use relm4::gtk;
use std::fs;
use std::path::PathBuf;

pub const SEARCH: &str = "librefood-search-symbolic";
pub const HISTORY: &str = "librefood-history-symbolic";
pub const FAVORITES: &str = "librefood-favorites-symbolic";
pub const PROFILES: &str = "librefood-profiles-symbolic";

const ICONS: &[(&str, &[u8])] = &[
    (
        SEARCH,
        include_bytes!("../data/icons/nav/librefood-search-symbolic.svg"),
    ),
    (
        HISTORY,
        include_bytes!("../data/icons/nav/librefood-history-symbolic.svg"),
    ),
    (
        FAVORITES,
        include_bytes!("../data/icons/nav/librefood-favorites-symbolic.svg"),
    ),
    (
        PROFILES,
        include_bytes!("../data/icons/nav/librefood-profiles-symbolic.svg"),
    ),
];

fn cache_dir() -> PathBuf {
    glib_user_cache().join("librefood").join("nav-icons")
}

fn glib_user_cache() -> PathBuf {
    gtk::glib::user_cache_dir()
}

/// Write bundled SVGs into the cache (if needed) and add that path to the icon theme.
pub fn install() {
    let dir = cache_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("nav icons: cache dir {}: {e}", dir.display());
        return;
    }
    for (name, bytes) in ICONS {
        let path = dir.join(format!("{name}.svg"));
        // Always refresh so updates to bundled SVGs appear without stale cache.
        if let Err(e) = fs::write(&path, bytes) {
            eprintln!("nav icons: write {}: {e}", path.display());
        }
    }
    let Some(display) = gdk::Display::default() else {
        eprintln!("nav icons: no default display");
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    theme.add_search_path(dir);
}

/// Pixel size for bottom-nav symbolic icons (CSS px).
pub const PIXEL_SIZE: i32 = 22;
