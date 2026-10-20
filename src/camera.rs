//! Webcam barcode scanning via nokhwa + rxing (ZXING port).
//!
//! On Linux this uses V4L2. We enumerate `/dev/video*` and nokhwa's device
//! query, then try each index so Alpine/ARM setups that are not `/dev/video0`
//! still work. Live RGB frames can be forwarded to the UI; decoding runs on
//! the same capture loop.

use nokhwa::{
    pixel_format::RgbFormat,
    query,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};
use rxing::{helpers::detect_in_luma, BarcodeFormat};
use std::path::PathBuf;
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

/// Result of probing for a usable capture device (without keeping it open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraProbe {
    /// At least one device opened successfully.
    Ready,
    /// No `/dev/video*` nodes and nokhwa found nothing.
    Missing,
    /// Nodes or candidates exist, but every open failed (permissions, busy, etc.).
    PresentButUnusable { detail: String },
}

impl CameraProbe {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn scan_button_label(&self) -> &'static str {
        match self {
            Self::Ready => "Scan with camera",
            Self::Missing => "Camera unavailable — type a barcode",
            Self::PresentButUnusable { .. } => "Camera blocked — type a barcode",
        }
    }

    pub fn unavailable_message(&self) -> String {
        match self {
            Self::Ready => String::new(),
            Self::Missing => {
                "No camera detected. Type or paste a barcode instead.".into()
            }
            Self::PresentButUnusable { detail } => format!(
                "Camera present but cannot be opened ({detail}). \
                 Check `video` group / device permissions, or close other apps using it. \
                 Typed barcodes still work."
            ),
        }
    }
}

/// Probe whether at least one video capture device can be opened.
#[allow(dead_code)] // public helper; UI uses [`camera_probe`] for richer status
pub fn camera_available() -> bool {
    camera_probe().is_ready()
}

/// Detailed probe used for UI labels and status text.
pub fn camera_probe() -> CameraProbe {
    match open_camera() {
        Ok(mut cam) => {
            let _ = cam.stop_stream();
            CameraProbe::Ready
        }
        Err(CameraError::NoCamera) => CameraProbe::Missing,
        Err(CameraError::Open(detail)) => CameraProbe::PresentButUnusable { detail },
        Err(e) => CameraProbe::PresentButUnusable {
            detail: e.to_string(),
        },
    }
}

/// List `/dev/videoN` paths that exist (capture nodes; metadata-only nodes may
/// still appear — open attempts filter those).
fn list_dev_video_nodes() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return paths;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(rest) = name.strip_prefix("video") {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    paths
}

fn parse_video_index(path: &std::path::Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("video")?
        .parse()
        .ok()
}

/// Candidate camera indices: nokhwa query first, then `/dev/video*`, then 0..7.
fn candidate_indices() -> Vec<CameraIndex> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    let push = |idx: CameraIndex, out: &mut Vec<CameraIndex>, seen: &mut std::collections::BTreeSet<String>| {
        let key = idx.as_string();
        if seen.insert(key) {
            out.push(idx);
        }
    };

    // Prefer Auto (platform native); fall back to explicit V4L2 on Linux.
    for api in [ApiBackend::Auto, ApiBackend::Video4Linux] {
        if let Ok(infos) = query(api) {
            for info in infos {
                push(info.index().clone(), &mut out, &mut seen);
            }
            if !out.is_empty() {
                break;
            }
        }
    }

    for path in list_dev_video_nodes() {
        if let Some(n) = parse_video_index(&path) {
            push(CameraIndex::Index(n), &mut out, &mut seen);
            // Also try path-style index for backends that accept device paths.
            push(
                CameraIndex::String(path.to_string_lossy().into_owned()),
                &mut out,
                &mut seen,
            );
        }
    }

    if out.is_empty() {
        for n in 0u32..8 {
            push(CameraIndex::Index(n), &mut out, &mut seen);
        }
    }

    out
}

fn try_open_index(index: CameraIndex) -> Result<Camera, String> {
    // Prefer a stable mid-rate request; AbsoluteHighestFrameRate can fail on
    // some V4L2 drivers (common on Alpine/ARM stacks).
    let attempts = [
        RequestedFormatType::AbsoluteHighestFrameRate,
        RequestedFormatType::None,
    ];
    let mut last = String::new();
    for fmt_ty in attempts {
        let requested = RequestedFormat::new::<RgbFormat>(fmt_ty);
        match Camera::new(index.clone(), requested) {
            Ok(cam) => return Ok(cam),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn classify_open_failure(msg: &str) -> CameraError {
    let lower = msg.to_lowercase();
    let permissionish = lower.contains("permission")
        || lower.contains("denied")
        || lower.contains("eacces")
        || lower.contains("busy")
        || lower.contains("resource temporarily unavailable")
        || lower.contains("in use")
        || lower.contains("ebusy");
    let missingish = (lower.contains("no ") && lower.contains("device"))
        || lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("enoent")
        || lower.contains("unable to find");

    if permissionish {
        CameraError::Open(msg.to_string())
    } else if missingish && list_dev_video_nodes().is_empty() {
        CameraError::NoCamera
    } else if !list_dev_video_nodes().is_empty() {
        // Nodes exist but this open failed — treat as unusable rather than missing.
        CameraError::Open(msg.to_string())
    } else if missingish {
        CameraError::NoCamera
    } else {
        CameraError::Open(msg.to_string())
    }
}

fn open_camera() -> Result<Camera, CameraError> {
    let nodes = list_dev_video_nodes();
    let candidates = candidate_indices();
    let mut last_err: Option<String> = None;
    let mut attempted = 0u32;

    for index in candidates {
        attempted += 1;
        match try_open_index(index.clone()) {
            Ok(cam) => return Ok(cam),
            Err(e) => {
                last_err = Some(format!("{}: {e}", index.as_string()));
            }
        }
    }

    if nodes.is_empty() && last_err.is_none() {
        return Err(CameraError::NoCamera);
    }

    if nodes.is_empty() && attempted > 0 {
        // Queried indices failed and no /dev/video* — truly missing / inaccessible.
        let detail = last_err.unwrap_or_else(|| "no capture device".into());
        return Err(classify_open_failure(&detail));
    }

    let detail = last_err.unwrap_or_else(|| {
        format!(
            "found {} video node(s) but none opened",
            nodes.len()
        )
    });
    // Prefer Open (permissions/busy) when nodes exist.
    if !nodes.is_empty() {
        Err(CameraError::Open(format!(
            "{detail}; devices: {}",
            nodes
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    } else {
        Err(classify_open_failure(&detail))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_does_not_panic() {
        let probe = camera_probe();
        assert_eq!(camera_available(), probe.is_ready());
        let _ = list_dev_video_nodes();
        let _ = candidate_indices();
    }

    #[test]
    fn parse_video_paths() {
        assert_eq!(
            parse_video_index(std::path::Path::new("/dev/video0")),
            Some(0)
        );
        assert_eq!(
            parse_video_index(std::path::Path::new("/dev/video12")),
            Some(12)
        );
        assert_eq!(
            parse_video_index(std::path::Path::new("/dev/video")),
            None
        );
    }
}
