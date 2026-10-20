# Librefood

Native Linux desktop app (Rust + GTK4 / Relm4 / libadwaita) that looks up a product’s **Nutri-Score** from the public [Open Food Facts](https://world.openfoodfacts.org/) API.

Scan a barcode with a webcam (live preview + centering guide), or type/paste an EAN-13 (etc.) code when no camera is available.

**App ID:** `org.librefood.Librefood`

## Requirements (Linux)

- Rust toolchain (1.85+ recommended)
- System packages (Arch):

```bash
sudo pacman -S --needed gtk4 libadwaita pkgconf base-devel
# Optional: v4l-utils for camera debugging
```

Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
```

Camera scanning uses [nokhwa](https://crates.io/crates/nokhwa) (V4L2) and [rxing](https://crates.io/crates/rxing). No Open Food Facts API key is required.

## Build & run (native)

```bash
cd ~/Documents/codeIsFun/librefood   # or your clone path
cargo run --release

# Optional: look up a barcode immediately on launch
cargo run --release -- --barcode 3017620422003
```

Window minimum width is ~360px; the input row stacks vertically on narrow widths.

## Usage

1. **Type or paste** a barcode (digits only; spaces are stripped) and press **Look up** or Enter.
2. Or click **Scan with camera** — live preview opens with a dashed centering guide; **Cancel scan** stops capture.
3. Results show product name, brands/quantity when present, and a Nutri-Score badge **A–E**.

### Example barcodes

| Barcode         | Notes                          |
|-----------------|--------------------------------|
| `3017620422003` | Nutella (classic test product) |
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
cd ~/Documents/codeIsFun/librefood

# Build into ./build-dir and export a local repo
flatpak run org.flatpak.Builder --user --install --force-clean \
  build-dir org.librefood.Librefood.yml

# Run
flatpak run org.librefood.Librefood

# Optional barcode on launch
flatpak run org.librefood.Librefood --barcode 3017620422003
```

The local manifest allows network during `cargo build` so crates download inside the SDK. Camera access uses `--device=all`; network is shared for the Open Food Facts API.

### Flathub layout (prepared, not submitted)

Flathub expects an offline Cargo build. When you have a public GitHub repo:

1. Generate `cargo-sources.json` with [flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).
2. Point the module `sources` at a git tag / archive **plus** `cargo-sources.json`, and use `cargo --offline build --release --locked`.
3. Open a PR against [flathub/flathub](https://github.com/flathub/flathub) with app id `org.librefood.Librefood` (do **not** open until credentials / upstream repo exist).

AppStream screenshots are omitted until a public GitHub remote can host them (required HTTPS image URLs).

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

- Needs a V4L2 device (typically `/dev/video0`) and permission for the `video` group (or equivalent ACL).
- Decoding is best-effort: hold the code steady, fill the guide, use good lighting.
- Headless / VM environments often have **no camera**; typed barcodes remain fully supported.

## License

MIT
