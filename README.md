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

Needs a V4L2 device (`/dev/video*`) and `video` group access:

```bash
ls -l /dev/video*
groups   # should include `video`
# Alpine: sudo adduser $USER video   # then log out / in
LIBREFOOD_CAMERA_DEBUG=1 ./target/release/librefood   # stderr probe details
```

If no camera is present the Scan button stays disabled; when a device exists Scan stays enabled and open errors show at click time. Typed barcodes always work.

## License

[GPL-3.0-or-later](LICENSE)
