//! Bundled symbolic SVGs for product-page factor rows (energy, sugar, …).

use crate::product::attrs;
use gtk::gdk;
use gtk::prelude::*;
use relm4::gtk;
use std::fs;
use std::path::PathBuf;

const PIXEL_SIZE: i32 = 18;

const ICONS: &[(&str, &[u8])] = &[
    (
        "librefood-stat-energy-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-energy-symbolic.svg"),
    ),
    (
        "librefood-stat-sugar-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-sugar-symbolic.svg"),
    ),
    (
        "librefood-stat-fat-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-fat-symbolic.svg"),
    ),
    (
        "librefood-stat-salt-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-salt-symbolic.svg"),
    ),
    (
        "librefood-stat-fiber-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-fiber-symbolic.svg"),
    ),
    (
        "librefood-stat-protein-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-protein-symbolic.svg"),
    ),
    (
        "librefood-stat-additives-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-additives-symbolic.svg"),
    ),
    (
        "librefood-stat-nova-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-nova-symbolic.svg"),
    ),
    (
        "librefood-stat-nutriscore-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-nutriscore-symbolic.svg"),
    ),
    (
        "librefood-stat-organic-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-organic-symbolic.svg"),
    ),
    (
        "librefood-stat-ecoscore-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-ecoscore-symbolic.svg"),
    ),
    (
        "librefood-stat-allergen-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-allergen-symbolic.svg"),
    ),
    (
        "librefood-stat-boycott-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-boycott-symbolic.svg"),
    ),
    (
        "librefood-stat-wikirate-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-wikirate-symbolic.svg"),
    ),
    (
        "librefood-stat-generic-symbolic",
        include_bytes!("../data/icons/stats/librefood-stat-generic-symbolic.svg"),
    ),
];

fn cache_dir() -> PathBuf {
    gtk::glib::user_cache_dir()
        .join("librefood")
        .join("stat-icons")
}

/// Write bundled SVGs and register with the icon theme.
pub fn install() {
    let dir = cache_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("stat icons: cache dir {}: {e}", dir.display());
        return;
    }
    for (name, bytes) in ICONS {
        let path = dir.join(format!("{name}.svg"));
        if let Err(e) = fs::write(&path, bytes) {
            eprintln!("stat icons: write {}: {e}", path.display());
        }
    }
    let Some(display) = gdk::Display::default() else {
        eprintln!("stat icons: no default display");
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    theme.add_search_path(dir);
}

/// Icon name for a scoring-attribute key.
pub fn icon_name_for_attribute(attribute: &str) -> &'static str {
    match attribute {
        attrs::ENERGY_KCAL_100G => "librefood-stat-energy-symbolic",
        attrs::SUGARS_100G => "librefood-stat-sugar-symbolic",
        attrs::SATURATED_FAT_100G => "librefood-stat-fat-symbolic",
        attrs::SALT_100G => "librefood-stat-salt-symbolic",
        attrs::FIBER_100G => "librefood-stat-fiber-symbolic",
        attrs::PROTEINS_100G => "librefood-stat-protein-symbolic",
        attrs::ADDITIVES_N => "librefood-stat-additives-symbolic",
        attrs::NOVA_GROUP => "librefood-stat-nova-symbolic",
        attrs::NUTRISCORE_ORDINAL => "librefood-stat-nutriscore-symbolic",
        attrs::ORGANIC => "librefood-stat-organic-symbolic",
        attrs::ECOSCORE => "librefood-stat-ecoscore-symbolic",
        attrs::ALLERGEN_COUNT => "librefood-stat-allergen-symbolic",
        attrs::BOYCOTT_LISTED => "librefood-stat-boycott-symbolic",
        attrs::WIKIRATE_SCORE => "librefood-stat-wikirate-symbolic",
        _ => "librefood-stat-generic-symbolic",
    }
}

/// Image widget for a factor row (symbolic, theme-recolored).
pub fn image_for_attribute(attribute: &str) -> gtk::Image {
    let img = gtk::Image::from_icon_name(icon_name_for_attribute(attribute));
    img.set_pixel_size(PIXEL_SIZE);
    img.set_valign(gtk::Align::Start);
    img.set_margin_top(2);
    img.add_css_class("factor-stat-icon");
    img
}
