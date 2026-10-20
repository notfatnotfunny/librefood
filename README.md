# Librefood

Native Linux desktop app (Rust + GTK4 / Relm4 / libadwaita) that looks up a product’s **Nutri-Score** from the public [Open Food Facts](https://world.openfoodfacts.org/) API.

Scan a barcode with a webcam, or type/paste an EAN-13 (etc.) code when no camera is available.

## Requirements (Linux)

- Rust toolchain (1.85+ recommended; developed with 1.99)
- System packages:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
# Optional but recommended for webcam capture (V4L2):
sudo apt install v4l-utils
```

Camera scanning uses [nokhwa](https://crates.io/crates/nokhwa) (V4L2) and [rxing](https://crates.io/crates/rxing) (ZXING port). No Open Food Facts API key is required.

## Build & run

```bash
cd ~/librefood   # or wherever you cloned this tree
cargo run
# or optimized:
cargo run --release

# Optional: look up a barcode immediately on launch
cargo run --release -- --barcode 3017620422003
```

Window minimum width is ~360px; the input row stacks vertically on narrow widths and sits in a single row on tablet/desktop sizes.

## Usage

1. **Type or paste** a barcode (digits only; spaces are stripped) and press **Look up** or Enter.
2. Or click **Scan with camera** — the app opens `/dev/video0`, grabs frames, and decodes EAN/UPC/Code128 when possible.
3. Results show product name, brands/quantity when present, and a Nutri-Score badge **A–E** (or “No Nutri-Score” if OFF has no grade).

### Example barcodes

| Barcode       | Notes                          |
|---------------|--------------------------------|
| `3017620422003` | Nutella (classic test product) |
| `0000000000000` | Expected not-found             |

## Offline / error UX

| State        | What you see                                      |
|--------------|---------------------------------------------------|
| Loading      | Spinner + “Looking up …” / camera progress        |
| Not found    | Clear message; no result panel                    |
| Network error| Message asking to check connectivity              |
| No camera    | Scan button disabled; typed entry still works     |

## Project layout

```
src/main.rs     entry point
src/app.rs      Relm4 + Adwaita adaptive UI
src/off.rs      Open Food Facts client
src/camera.rs   webcam capture + barcode decode
```

API used: `https://world.openfoodfacts.org/api/v2/product/{barcode}.json`

## Camera caveats

- Needs a V4L2 device (typically `/dev/video0`) and permission for the `video` group.
- Decoding is best-effort: hold the code steady, fill most of the frame, use good lighting.
- Headless / VM environments often have **no camera**; typed/pasted barcodes remain fully supported.
- First open of the camera can take a second; the UI stays responsive via a background thread.

## License

MIT
