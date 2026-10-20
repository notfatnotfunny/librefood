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
sudo pacman -S rust gtk4 libadwaita pkgconf gstreamer gst-plugins-base
# camera (optional): v4l-utils; usermod -aG video $USER  # then re-login
# phone/libcamera path: libcamera gst-plugin-libcamera pipewire
```

## Alpine Linux

```bash
sudo apk add build-base cargo glib-dev pango-dev gdk-pixbuf-dev gtk4.0-dev clang23-dev libadwaita-dev
# camera (phones / CAMSS): see Camera section below
# USB webcam only: sudo apk add v4l-utils; sudo adduser $USER video  # then re-login
git clone https://github.com/notfatnotfunny/librefood.git
cd librefood && cargo build --release && ./target/release/librefood
```

Same list: [`alpine-dependencies.txt`](alpine-dependencies.txt).

## Camera

Two capture paths:

1. **USB webcams** — raw V4L2 (MJPEG / YUYV single-plane). Needs `/dev/video*` + `video` group.
2. **Phone cameras (Qualcomm CAMSS, e.g. OnePlus fajita)** — GStreamer `libcamerasrc` (preferred) or `pipewiresrc`. Raw V4L2 STREAMON usually fails with `Broken pipe`; librefood detects CAMSS and skips retrying all nodes.

### Alpine phone (fajita / postmarketOS)

```bash
sudo apk add libcamera libcamera-tools gst-plugins-base \
  gstreamer-tools pipewire wireplumber xdg-desktop-portal xdg-desktop-portal-gtk
# libcamerasrc plugin — package name varies by Alpine release; try one of:
sudo apk add gst-plugin-libcamera || sudo apk add gst-plugins-libcamera || true
# search if needed: apk search libcamera; apk search gstreamer | grep -i camera
sudo adduser $USER video   # then log out / in
```

Build deps for GStreamer Rust crates (already covered by `glib-dev` + typically):

```bash
sudo apk add gstreamer-dev gst-plugins-base-dev
```

Verify:

```bash
gst-inspect-1.0 libcamerasrc    # should list the element
# optional: cam -l   or   libcamera-hello --list-cameras
LIBREFOOD_CAMERA_DEBUG=1 LIBREFOOD_CAMERA_BACKEND=libcamera ./target/release/librefood
```

### Arch (dev / USB)

```bash
sudo pacman -S v4l-utils gstreamer gst-plugins-base
# phone-class / libcamera path:
sudo pacman -S libcamera gst-plugin-libcamera pipewire
```

### Env knobs

| Variable | Purpose |
|---|---|
| `LIBREFOOD_CAMERA_DEBUG=1` | Log probe / backend / STREAMON / chosen facing |
| `LIBREFOOD_CAMERA_DEVICE=/dev/video0` | Force a V4L2 node (USB) |
| `LIBREFOOD_CAMERA_BACKEND=auto` | Default: libcamera first on CAMSS, else V4L2 then gst |
| `…=libcamera` / `pipewire` / `v4l2` | Force one backend |
| `LIBREFOOD_CAMERA_FACING=back` | Prefer rear cameras (default). `front` / `any` also OK |
| `LIBREFOOD_CAMERA_NAME=…` | Exact libcamera id (`cam --list` id in parentheses) |
| `LIBREFOOD_CAMERA_INDEX=0` | Among matching facing cams, pick Nth (0 = primary rear) |

On fajita (multiple cams), default is **back** (libcamera `Location`, not `/dev/video0`). During scan, **Switch camera** cycles cameras (Snapshot-style); the last choice is saved in `~/.config/librefood/camera-id`.

```bash
cam --list
LIBREFOOD_CAMERA_DEBUG=1 ./target/release/librefood
# second rear: LIBREFOOD_CAMERA_FACING=back LIBREFOOD_CAMERA_INDEX=1
# or: LIBREFOOD_CAMERA_NAME='/base/soc@0/…'
```

Optional portal packages (permission dialog for PipeWire path): `xdg-desktop-portal`, pipewire.

Typed barcodes always work. Window placement follows Fractal/Snapshot patterns: phones (Phosh / ≤1080p short edge, e.g. fajita) **maximize** into the shell workarea; desktops open a floating ~800×640 window (resizable). Escape force-maximize: `LIBREFOOD_NO_FULLSCREEN=1`.

## License

[GPL-3.0-or-later](LICENSE)
