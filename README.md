# Librefood

Native Linux desktop app (Rust + GTK4 / Relm4 / libadwaita) that looks up products from the public [Open Food Facts](https://world.openfoodfacts.org/) API and scores them with **transparent, user-editable scoring profiles**.

Scan a barcode with a webcam, type/paste an EAN, or search by name. OFF source data stays separate from derived scores. Custom scores are **not** official Nutri-Score and **not** medical advice.

**App ID:** `org.librefood.Librefood`  
**Scoring docs:** [`docs/scoring-engine.md`](docs/scoring-engine.md)

## Quick install (build from source)

**Arch Linux** dependencies:

```bash
sudo pacman -S --needed gtk4 libadwaita pkgconf base-devel
# Rust: https://rustup.rs  (1.85+ recommended)
```

Clone, build, run:

```bash
git clone https://github.com/notfatnotfunny/librefood.git
cd librefood
cargo build --release
./target/release/librefood

# Or:
cargo run --release
# Optional: look up a barcode on launch
cargo run --release -- --barcode 3017620422003
```

Optional local Flatpak build: see [Flatpak](#flatpak) below.

## Requirements (other distros)

Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
```

Camera scanning uses [nokhwa](https://crates.io/crates/nokhwa) (V4L2) and [rxing](https://crates.io/crates/rxing). No Open Food Facts API key is required.

Window minimum width is ~360px; the input row stacks vertically on narrow widths.

## Usage

1. **Type or paste** a barcode (digits only; spaces are stripped) and press **Look up**, or **Search** by product name.
2. Or click **Scan with camera** — live preview with a centering guide; **Cancel scan** stops capture.
3. Pick a **scoring profile** in the Lookup dropdown, or open the **Profiles** page (nav or button) to create / duplicate / edit / import / export / restore built-ins.
4. Results show image, Librefood score, Negatives/Positives, and **calculation steps**. **Compare** scores the same product under every profile.
5. Custom profiles are saved as JSON in `~/.config/librefood/profiles/`. Built-in examples: nutritional quality, minimally processed, additive-conscious, environmental impact, blank.

### Example barcodes

| Barcode         | Notes                          |
|-----------------|--------------------------------|
| `3017620422003` | Nutella (classic test product) |
| `5449000000996` | Coca-Cola                      |
| `0000000000000` | Expected not-found             |

## Flatpak

Manifest: [`org.librefood.Librefood.yml`](org.librefood.Librefood.yml)  
Desktop / metainfo / icons live under [`data/`](data/).

### Prerequisites

```bash
# User install of Builder + runtime (no root required)
flatpak install --user flathub \
  org.flatpak.Builder \
  org.gnome.Platform//49 \
  org.gnome.Sdk//49 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08 \
  org.freedesktop.Sdk.Extension.llvm20//25.08
```

### Build, install, run (local)

```bash
cd librefood   # or your clone path

flatpak run org.flatpak.Builder --user --install --force-clean \
  build-dir org.librefood.Librefood.yml

flatpak run org.librefood.Librefood
# Optional barcode on launch
flatpak run org.librefood.Librefood --barcode 3017620422003
```

The local manifest allows network during `cargo build` so crates download inside the SDK. Camera access uses `--device=all`; network is shared for the Open Food Facts API.

### Flathub layout (prepared, not submitted)

Flathub expects an offline Cargo build:

1. Generate `cargo-sources.json` with [flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).
2. Point the module `sources` at a git tag / archive **plus** `cargo-sources.json`, and use `cargo --offline build --release --locked`.
3. Open a PR against [flathub/flathub](https://github.com/flathub/flathub) with app id `org.librefood.Librefood` when ready.

AppStream screenshots can use raw GitHub image URLs once hosted under this repo (e.g. `data/screenshots/`).

## Project layout

```
src/main.rs                         entry point
src/app.rs                          Relm4 + Adwaita adaptive UI
src/off.rs                          Open Food Facts client
src/camera.rs                       webcam capture + barcode decode
data/org.librefood.Librefood.*      desktop + AppStream metainfo
data/icons/                         hicolor icon theme
org.librefood.Librefood.yml         Flatpak manifest
```

API used: `https://world.openfoodfacts.org/api/v2/product/{barcode}.json`

## Camera caveats

- Linux uses V4L2 via [nokhwa](https://crates.io/crates/nokhwa) (`input-native` / `input-v4l`). Librefood enumerates `/dev/video*` and tries each device (not only `video0`).
- Needs permission for the `video` group (or an ACL that allows read/write on the device node).
- Decoding is best-effort: hold the code steady, fill the guide, use good lighting.
- If a camera node exists but open fails (permissions / busy), the UI says so; typed barcodes remain fully supported. Headless / VM environments often have **no camera**.

### Alpine Linux (incl. aarch64)

```sh
# Build / runtime deps (names may vary slightly by Alpine release)
apk add build-base rust cargo pkgconf \
  gtk4.0-dev libadwaita-dev \
  v4l-utils-dev v4l-utils \
  clang-libclang   # if a dependency needs libclang

# Runtime: user must reach V4L2 nodes
adduser "$USER" video
# or: setfacl -m u:"$USER":rw /dev/video0
```

`v4l2-ctl --list-devices` is useful to confirm nodes. On musl/ARM, prefer stock Alpine packages over ad-hoc x86 blobs; nokhwa’s native V4L2 backend is architecture-agnostic.

## License

MIT
