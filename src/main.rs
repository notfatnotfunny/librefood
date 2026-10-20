mod app;
mod camera;
mod off;

use app::AppModel;
use relm4::prelude::*;

fn main() {
    // Optional auto-lookup on launch:
    //   librefood --barcode 3017620422003
    //   LIBREFOOD_BARCODE=3017620422003 librefood
    let mut argv: Vec<String> = std::env::args().collect();
    let mut initial_barcode = std::env::var("LIBREFOOD_BARCODE").ok();
    if let Some(i) = argv.iter().position(|a| a == "--barcode") {
        if i + 1 < argv.len() {
            initial_barcode = Some(argv[i + 1].clone());
            argv.drain(i..=i + 1);
        } else {
            argv.remove(i);
        }
    }

    adw::init().expect("Failed to initialize libadwaita");
    // Pass stripped argv so GApplication does not reject --barcode.
    let app = RelmApp::new("org.librefood.Librefood").with_args(argv);
    app.run::<AppModel>(initial_barcode);
}
