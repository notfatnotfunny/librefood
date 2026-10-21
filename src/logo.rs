//! Embedded Librefood logo (SVG) for in-app brand mark and window icon-name.

use gtk::gdk;
use gtk::gdk_pixbuf::Pixbuf;
use gtk::gio;
use gtk::prelude::*;
use relm4::gtk;

const LOGO_SVG: &[u8] = include_bytes!("../logo.svg");

/// CSS pixel size of the Search hub logo frame.
pub const HUB_LOGO_CSS_PX: i32 = 96;

/// Load the logo as a GDK texture for `css_px` CSS pixels at `scale_factor`.
///
/// Renders the SVG at device pixels (css × scale), never upscales a tiny raster.
pub fn logo_texture_scaled(css_px: i32, scale_factor: i32) -> Option<gdk::Texture> {
    let scale = scale_factor.max(1);
    let css = css_px.max(1);
    // Device pixels for the widget, with light oversampling for soft downscale.
    let px = (css.saturating_mul(scale)).saturating_mul(2).max(256);
    let bytes = gtk::glib::Bytes::from_static(LOGO_SVG);
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    let pixbuf =
        Pixbuf::from_stream_at_scale(&stream, px, px, true, None::<&gio::Cancellable>).ok()?;
    Some(gdk::Texture::for_pixbuf(&pixbuf))
}

/// Convenience when the display scale is unknown yet (assumes 2× HiDPI).
pub fn logo_texture(css_px: i32) -> Option<gdk::Texture> {
    logo_texture_scaled(css_px, 2)
}

/// Prefer the FreeDesktop icon name when installed (Flatpak / desktop file).
pub fn apply_window_icon(win: &impl IsA<gtk::Window>) {
    win.set_icon_name(Some("org.librefood.Librefood"));
}
