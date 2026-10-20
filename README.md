# Librefood

Configurable food scores from [Open Food Facts](https://world.openfoodfacts.org/) — barcode, search, or camera scan, with editable JSON scoring profiles.

Scoring details: [`docs/scoring-engine.md`](docs/scoring-engine.md).

## Build & run

```bash
git clone https://github.com/notfatnotfunny/librefood.git
cd librefood
cargo build --release
./target/release/librefood
```

Optional: `librefood --barcode 3017620422003` or `--page profiles|history|favorites`.

## Arch Linux

```bash
sudo pacman -S rust gtk4 libadwaita pkgconf
# camera (optional): v4l-utils; usermod -aG video $USER  # then re-login
```

## Alpine Linux

```bash
sudo apk add build-base cargo glib-dev pango-dev gdk-pixbuf-dev gtk4.0-dev clang23-dev libadwaita-dev
# optional camera: sudo apk add v4l-utils; sudo adduser $USER video  # then re-login
git clone https://github.com/notfatnotfunny/librefood.git
cd librefood && cargo build --release && ./target/release/librefood
```

Same list: [`alpine-dependencies.txt`](alpine-dependencies.txt).

## Camera

Needs a **capture-capable** V4L2 node (`V4L2_CAP_VIDEO_CAPTURE` — metadata-only `/dev/video*` are skipped) and `video` group access:

```bash
ls -l /dev/video*
groups   # should include `video`
# Alpine: sudo adduser $USER video   # then log out / in
LIBREFOOD_CAMERA_DEBUG=1 ./target/release/librefood
# optional: LIBREFOOD_CAMERA_DEVICE=/dev/video0
```

Optional portal stack (GNOME Snapshot-style; not required on Alpine if V4L2 works): `xdg-desktop-portal`, a desktop portal impl, `pipewire`. Librefood still captures via V4L2.

Typed barcodes always work. Window starts fullscreen/maximized; `LIBREFOOD_NO_FULLSCREEN=1` to disable.

## License

[GPL-3.0-or-later](LICENSE)
