//! Webcam barcode scanning via nokhwa + rxing (ZXING port).
//!
//! On Linux this uses V4L2. We enumerate `/dev/video*` and nokhwa's device
//! query, then try each numeric index so Alpine/ARM setups that are not
//! `/dev/video0` still work. Live RGB frames can be forwarded to the UI;
//! decoding runs on the same capture loop.
//!
//! Startup probe is optimistic when video nodes exist: the Scan button stays
//! enabled and open failures are reported at scan time with actionable copy.
//! Set `LIBREFOOD_CAMERA_DEBUG=1` for stderr enumeration / open details.

use nokhwa::{
    pixel_format::RgbFormat,
    query,
    utils::{
        ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
        Resolution,
    },
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

/// Result of probing for a capture device (without keeping it open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraProbe {
    /// At least one device opened successfully.
    Ready,
    /// No `/dev/video*` nodes and nokhwa found nothing.
    Missing,
    /// Nodes exist; open failed with permission denial (EACCES / video group).
    PermissionDenied { detail: String },
    /// Nodes exist; device reported busy.
    Busy { detail: String },
    /// Nodes exist; open failed for another reason (format, driver, etc.).
    /// Scan stays enabled — retry at click time.
    Present { detail: String },
}

impl CameraProbe {
    /// Whether the Scan button should be clickable.
    ///
    /// Enabled whenever a device might work — including when startup open
    /// failed — so Alpine ARM users are not stuck on a gray "blocked" button.
    /// Only [`Missing`] disables Scan.
    pub fn scan_enabled(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn scan_button_label(&self) -> &'static str {
        match self {
            Self::Ready | Self::Present { .. } | Self::Busy { .. } => "Scan with camera",
            Self::PermissionDenied { .. } => "Camera needs access — tap to retry",
            Self::Missing => "Camera unavailable — type a barcode",
        }
    }

    pub fn unavailable_message(&self) -> String {
        match self {
            Self::Ready => String::new(),
            Self::Missing => {
                "No camera detected. Type or paste a barcode instead.".into()
            }
            Self::PermissionDenied { detail } => format!(
                "Camera permission denied ({detail}). On Alpine: \
                 `sudo adduser $USER video`, then log out and back in. \
                 Typed barcodes still work."
            ),
            Self::Busy { detail } => format!(
                "Camera is busy ({detail}). Close other apps using it, then try again. \
                 Typed barcodes still work."
            ),
            Self::Present { detail } => format!(
                "Camera found but could not be opened yet ({detail}). \
                 Try Scan again, or type a barcode."
            ),
        }
    }
}

fn camera_debug_enabled() -> bool {
    matches!(
        std::env::var("LIBREFOOD_CAMERA_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

fn cam_debug(msg: impl AsRef<str>) {
    if camera_debug_enabled() {
        eprintln!("[librefood camera] {}", msg.as_ref());
    }
}

/// Probe whether at least one video capture device can be opened.
#[allow(dead_code)] // public helper; UI uses [`camera_probe`] for richer status
pub fn camera_available() -> bool {
    camera_probe().is_ready()
}

/// Detailed probe used for UI labels and status text.
pub fn camera_probe() -> CameraProbe {
    let nodes = list_dev_video_nodes();
    cam_debug(format!(
        "probe start; /dev/video* = [{}]",
        nodes
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    match open_camera() {
        Ok(mut cam) => {
            cam_debug("probe: open OK");
            let _ = cam.stop_stream();
            CameraProbe::Ready
        }
        Err(CameraError::NoCamera) => {
            cam_debug("probe: Missing");
            CameraProbe::Missing
        }
        Err(CameraError::Open(detail)) => classify_probe_failure(&detail, &nodes),
        Err(e) => classify_probe_failure(&e.to_string(), &nodes),
    }
}

fn classify_probe_failure(detail: &str, nodes: &[PathBuf]) -> CameraProbe {
    let kind = FailureKind::from_message(detail);
    cam_debug(format!("probe fail kind={kind:?}: {detail}"));
    if nodes.is_empty() && matches!(kind, FailureKind::Missing) {
        return CameraProbe::Missing;
    }
    if nodes.is_empty() && !detail.to_lowercase().contains("video") {
        // Queried indices only, nothing on disk — treat as missing.
        if matches!(kind, FailureKind::Missing) {
            return CameraProbe::Missing;
        }
    }
    match kind {
        FailureKind::Permission => CameraProbe::PermissionDenied {
            detail: detail.to_string(),
        },
        FailureKind::Busy => CameraProbe::Busy {
            detail: detail.to_string(),
        },
        FailureKind::Missing if nodes.is_empty() => CameraProbe::Missing,
        _ => {
            if nodes.is_empty() {
                CameraProbe::Missing
            } else {
                CameraProbe::Present {
                    detail: detail.to_string(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Permission,
    Busy,
    Missing,
    Other,
}

impl FailureKind {
    fn from_message(msg: &str) -> Self {
        let lower = msg.to_lowercase();
        if lower.contains("permission")
            || lower.contains("denied")
            || lower.contains("eacces")
            || lower.contains("operation not permitted")
        {
            Self::Permission
        } else if lower.contains("busy")
            || lower.contains("resource temporarily unavailable")
            || lower.contains("in use")
            || lower.contains("ebusy")
        {
            Self::Busy
        } else if (lower.contains("no ") && lower.contains("device"))
            || lower.contains("not found")
            || lower.contains("no such file")
            || lower.contains("enoent")
            || lower.contains("unable to find")
        {
            Self::Missing
        } else {
            Self::Other
        }
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

/// Whether this process looks like a Flatpak sandbox (portal path may apply).
fn running_in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Candidate camera indices: `/dev/video*` first (low numbers), then nokhwa query.
/// Path-style string indices are skipped — nokhwa's Linux backend rejects them
/// (`invalid digit found in string`).
fn candidate_indices() -> Vec<CameraIndex> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    let push =
        |idx: CameraIndex, out: &mut Vec<CameraIndex>, seen: &mut std::collections::BTreeSet<String>| {
            let key = idx.as_string();
            if seen.insert(key) {
                out.push(idx);
            }
        };

    // Prefer real V4L2 node numbers (video0 before metadata-only video1).
    for path in list_dev_video_nodes() {
        if let Some(n) = parse_video_index(&path) {
            push(CameraIndex::Index(n), &mut out, &mut seen);
        }
    }

    for api in [ApiBackend::Auto, ApiBackend::Video4Linux] {
        match query(api) {
            Ok(infos) => {
                cam_debug(format!("nokhwa query({api:?}): {} device(s)", infos.len()));
                for info in infos {
                    cam_debug(format!(
                        "  index={:?} name={} misc={}",
                        info.index(),
                        info.human_name(),
                        info.misc()
                    ));
                    push(info.index().clone(), &mut out, &mut seen);
                }
            }
            Err(e) => cam_debug(format!("nokhwa query({api:?}): {e}")),
        }
    }

    if out.is_empty() {
        for n in 0u32..8 {
            push(CameraIndex::Index(n), &mut out, &mut seen);
        }
    }

    cam_debug(format!(
        "candidates: {}",
        out.iter()
            .map(|i| i.as_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    if running_in_flatpak() {
        cam_debug("note: running under Flatpak (/.flatpak-info present)");
    }
    out
}

fn try_open_index(index: CameraIndex) -> Result<Camera, String> {
    // Alpine/ARM UVC stacks often reject AbsoluteHighestFrameRate; try several.
    let attempts = [
        RequestedFormatType::AbsoluteHighestFrameRate,
        RequestedFormatType::AbsoluteHighestResolution,
        RequestedFormatType::None,
        RequestedFormatType::HighestFrameRate(30),
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(640, 480),
            FrameFormat::MJPEG,
            30,
        )),
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(640, 480),
            FrameFormat::YUYV,
            15,
        )),
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(320, 240),
            FrameFormat::YUYV,
            30,
        )),
    ];
    let mut last = String::new();
    for fmt_ty in attempts {
        let requested = RequestedFormat::new::<RgbFormat>(fmt_ty);
        match Camera::new(index.clone(), requested) {
            Ok(cam) => {
                cam_debug(format!("open OK index={} fmt={fmt_ty:?}", index.as_string()));
                return Ok(cam);
            }
            Err(e) => {
                cam_debug(format!(
                    "open FAIL index={} fmt={fmt_ty:?}: {e}",
                    index.as_string()
                ));
                last = e.to_string();
            }
        }
    }
    Err(last)
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
        let detail = last_err.unwrap_or_else(|| "no capture device".into());
        return match FailureKind::from_message(&detail) {
            FailureKind::Missing => Err(CameraError::NoCamera),
            _ => Err(CameraError::Open(detail)),
        };
    }

    let detail = last_err.unwrap_or_else(|| {
        format!(
            "found {} video node(s) but none opened",
            nodes.len()
        )
    });
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
        match FailureKind::from_message(&detail) {
            FailureKind::Missing => Err(CameraError::NoCamera),
            _ => Err(CameraError::Open(detail)),
        }
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
        // Scan is only disabled when nothing exists.
        if !list_dev_video_nodes().is_empty() {
            assert!(
                probe.scan_enabled(),
                "video nodes present => Scan must stay enabled, got {probe:?}"
            );
        }
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

    #[test]
    fn failure_kind_classifies_messages() {
        assert_eq!(
            FailureKind::from_message("Permission denied (EACCES)"),
            FailureKind::Permission
        );
        assert_eq!(
            FailureKind::from_message("Device or resource busy"),
            FailureKind::Busy
        );
        assert_eq!(
            FailureKind::from_message("No such file or directory"),
            FailureKind::Missing
        );
        assert_eq!(
            FailureKind::from_message("Failed to Fufill"),
            FailureKind::Other
        );
    }

    #[test]
    fn probe_labels_never_say_blocked() {
        let samples = [
            CameraProbe::Ready,
            CameraProbe::Missing,
            CameraProbe::PermissionDenied {
                detail: "EACCES".into(),
            },
            CameraProbe::Busy {
                detail: "EBUSY".into(),
            },
            CameraProbe::Present {
                detail: "Failed to Fufill".into(),
            },
        ];
        for p in samples {
            let label = p.scan_button_label().to_lowercase();
            assert!(
                !label.contains("blocked"),
                "label must not say blocked: {label}"
            );
            assert!(
                !p.unavailable_message().to_lowercase().contains("blocked"),
                "status must not say blocked for {p:?}"
            );
        }
    }

    #[test]
    fn classify_present_when_nodes_exist() {
        let nodes = vec![PathBuf::from("/dev/video0")];
        let p = classify_probe_failure("Failed to Fufill; devices: /dev/video0", &nodes);
        assert!(matches!(p, CameraProbe::Present { .. }));
        assert!(p.scan_enabled());
    }

    #[test]
    fn classify_permission_when_denied() {
        let nodes = vec![PathBuf::from("/dev/video0")];
        let p = classify_probe_failure("Permission denied", &nodes);
        assert!(matches!(p, CameraProbe::PermissionDenied { .. }));
        assert!(p.scan_enabled());
    }
}
