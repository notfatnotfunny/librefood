//! Webcam barcode scanning via nokhwa + rxing (ZXING port).
//!
//! On Linux this uses V4L2. Live RGB frames can be forwarded to the UI so the
//! user can center the barcode; decoding runs on the same capture loop.

use nokhwa::{
    pixel_format::RgbFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};
use rxing::{helpers::detect_in_luma, BarcodeFormat};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CameraError {
    #[error("No camera found")]
    NoCamera,
    #[error("Camera open failed: {0}")]
    Open(String),
    #[error("Capture failed: {0}")]
    Capture(String),
    #[error("No barcode detected in frame")]
    NoBarcode,
    #[error("Scan cancelled")]
    Cancelled,
}

/// Probe whether at least one video capture device can be opened.
pub fn camera_available() -> bool {
    match open_camera() {
        Ok(mut cam) => {
            let _ = cam.stop_stream();
            true
        }
        Err(_) => false,
    }
}

fn open_camera() -> Result<Camera, CameraError> {
    let index = CameraIndex::Index(0);
    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    Camera::new(index, requested).map_err(|e| {
        let msg = e.to_string().to_lowercase();
        if msg.contains("no") || msg.contains("not found") || msg.contains("unable") {
            CameraError::NoCamera
        } else {
            CameraError::Open(e.to_string())
        }
    })
}

fn rgb_to_luma(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|p| {
            // Rec. 601 luma
            ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8
        })
        .collect()
}

fn decode_rgb(width: u32, height: u32, rgb: &[u8]) -> Option<String> {
    if rgb.len() < (width * height * 3) as usize || width == 0 || height == 0 {
        return None;
    }
    let luma = rgb_to_luma(rgb);
    // Prefer EAN-13, then fall back to any format rxing supports.
    let result = detect_in_luma(luma.clone(), width, height, Some(BarcodeFormat::EAN_13))
        .or_else(|_| detect_in_luma(luma, width, height, None))
        .ok()?;
    let text = result.getText().to_string();
    if text.chars().any(|c| c.is_ascii_digit()) {
        Some(text)
    } else {
        None
    }
}

/// Downscale RGB for UI preview (nearest-neighbor) so we do not flood the UI thread.
fn shrink_for_preview(width: u32, height: u32, rgb: &[u8], max_edge: u32) -> (u32, u32, Vec<u8>) {
    let max_dim = width.max(height);
    if max_dim == 0 || max_dim <= max_edge {
        return (width, height, rgb.to_vec());
    }
    let factor = (max_dim + max_edge - 1) / max_edge;
    let nw = (width / factor).max(1);
    let nh = (height / factor).max(1);
    let mut out = Vec::with_capacity((nw * nh * 3) as usize);
    for y in 0..nh {
        let sy = y * factor;
        for x in 0..nw {
            let sx = x * factor;
            let i = ((sy * width + sx) * 3) as usize;
            out.extend_from_slice(&rgb[i..i + 3]);
        }
    }
    (nw, nh, out)
}

/// Continuous capture with optional live RGB preview callbacks.
///
/// `on_frame` receives downscaled RGB frames for UI display (main-thread texture
/// creation happens in the caller). Stops on decode success, cancel, or frame budget.
pub fn scan_barcode_live<F>(
    max_frames: u32,
    stop: Arc<AtomicBool>,
    mut on_frame: F,
) -> Result<String, CameraError>
where
    F: FnMut(u32, u32, Vec<u8>),
{
    let mut camera = open_camera()?;
    camera
        .open_stream()
        .map_err(|e| CameraError::Open(e.to_string()))?;

    let mut last_err = CameraError::NoBarcode;

    for i in 0..max_frames {
        if stop.load(Ordering::Relaxed) {
            let _ = camera.stop_stream();
            return Err(CameraError::Cancelled);
        }

        let frame = match camera.frame() {
            Ok(f) => f,
            Err(e) => {
                last_err = CameraError::Capture(e.to_string());
                continue;
            }
        };

        let decoded = match frame.decode_image::<RgbFormat>() {
            Ok(d) => d,
            Err(e) => {
                last_err = CameraError::Capture(e.to_string());
                continue;
            }
        };

        let width = decoded.width();
        let height = decoded.height();
        let rgb = decoded.into_raw();

        // Preview ~every other frame to keep the UI responsive.
        if i % 2 == 0 {
            let (pw, ph, prgb) = shrink_for_preview(width, height, &rgb, 640);
            on_frame(pw, ph, prgb);
        }

        if let Some(code) = decode_rgb(width, height, &rgb) {
            let _ = camera.stop_stream();
            return Ok(code);
        }
    }

    let _ = camera.stop_stream();
    if stop.load(Ordering::Relaxed) {
        Err(CameraError::Cancelled)
    } else {
        Err(last_err)
    }
}
