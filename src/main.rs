mod app;
mod camera;
mod engine;
mod library;
mod logo;
mod off;
mod product;
mod profiles;
mod score;
mod ui_profiles;

use app::{AppModel, AppPage};
use relm4::prelude::*;

fn main() {
    // Optional launch flags (stripped before GApplication):
    //   librefood --barcode 3017620422003
    //   librefood --page history|favorites|lookup
    //   LIBREFOOD_BARCODE=… / LIBREFOOD_PAGE=…
    let mut argv: Vec<String> = std::env::args().collect();
    let mut initial_barcode = std::env::var("LIBREFOOD_BARCODE").ok();
    let mut initial_page = std::env::var("LIBREFOOD_PAGE").ok();
    if let Some(i) = argv.iter().position(|a| a == "--barcode") {
        if i + 1 < argv.len() {
            initial_barcode = Some(argv[i + 1].clone());
            argv.drain(i..=i + 1);
        } else {
            argv.remove(i);
        }
    }
    if let Some(i) = argv.iter().position(|a| a == "--page") {
        if i + 1 < argv.len() {
            initial_page = Some(argv[i + 1].clone());
            argv.drain(i..=i + 1);
        } else {
            argv.remove(i);
        }
    }

    let page = match initial_page.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref()
    {
        Some("history") => AppPage::History,
        Some("favorites") | Some("favourite") | Some("favourites") => AppPage::Favorites,
        _ => AppPage::Lookup,
    };

    adw::init().expect("Failed to initialize libadwaita");
    let app = RelmApp::new("org.librefood.Librefood").with_args(argv);
    app.run::<AppModel>((initial_barcode, page));
}
