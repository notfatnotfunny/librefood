//! Embedded Librefood logo (SVG) for in-app brand mark and window icon-name.

use gtk::gdk;
use gtk::gdk_pixbuf::Pixbuf;
use gtk::gio;
use gtk::prelude::*;
use relm4::gtk;

const LOGO_SVG: &[u8] = include_bytes!("../logo.svg");

/// Load the logo as a GDK texture at approximately `px` CSS pixels.
pub fn logo_texture(px: i32) -> Option<gdk::Texture> {
    let bytes = gtk::glib::Bytes::from_static(LOGO_SVG);
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    let pixbuf =
        Pixbuf::from_stream_at_scale(&stream, px, px, true, None::<&gio::Cancellable>).ok()?;
    Some(gdk::Texture::for_pixbuf(&pixbuf))
}

/// Prefer the FreeDesktop icon name when installed (Flatpak / desktop file).
pub fn apply_window_icon(win: &impl IsA<gtk::Window>) {
    win.set_icon_name(Some("org.librefood.Librefood"));
}
