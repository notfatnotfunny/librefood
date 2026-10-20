//! Webcam barcode scanning via nokhwa + rxing (ZXING port).
//!
//! On Linux this uses V4L2. Many `/dev/video*` nodes are metadata/controls only;
//! we ioctl QUERYCAP and keep nodes with `V4L2_CAP_VIDEO_CAPTURE` (or mplane).
//! Override with `LIBREFOOD_CAMERA_DEVICE=/dev/video0`.
//!
//! GNOME Snapshot uses xdg-desktop-portal Camera + PipeWire for capture; we keep
//! direct V4L2 (works on Alpine without portal) and optionally log portal
//! presence from the UI. Set `LIBREFOOD_CAMERA_DEBUG=1` for stderr details.

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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;
use v4l::capability::Flags as CapFlags;
use v4l::Device as V4lDevice;

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
    /// No capture-capable `/dev/video*` nodes and nokhwa found nothing.
    Missing,
    /// Capture nodes exist; open failed with permission denial.
    PermissionDenied { detail: String },
    /// Capture nodes exist; device reported busy.
    Busy { detail: String },
    /// Capture nodes exist; open failed for another reason.
    /// Scan stays enabled — retry at click time.
    Present { detail: String },
}

impl CameraProbe {
    /// Whether the Scan button should be clickable.
    ///
    /// Only [`Missing`] disables Scan — never permanently gray for open failures.
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
                 Try Scan again, set LIBREFOOD_CAMERA_DEVICE=/dev/videoN, \
                 or run with LIBREFOOD_CAMERA_DEBUG=1. Typed barcodes still work."
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
#[allow(dead_code)]
pub fn camera_available() -> bool {
    camera_probe().is_ready()
}

/// Detailed probe used for UI labels and status text.
pub fn camera_probe() -> CameraProbe {
    let capture = list_capture_video_nodes();
    let all_nodes = list_dev_video_nodes();
    cam_debug(format!(
        "probe start; all /dev/video* = [{}]; capture-capable = [{}]",
        all_nodes
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        capture
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    if let Ok(dev) = std::env::var("LIBREFOOD_CAMERA_DEVICE") {
        cam_debug(format!("LIBREFOOD_CAMERA_DEVICE={dev}"));
    }

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
        Err(CameraError::Open(detail)) => classify_probe_failure(&detail, &capture),
        Err(e) => classify_probe_failure(&e.to_string(), &capture),
    }
}

fn classify_probe_failure(detail: &str, capture_nodes: &[PathBuf]) -> CameraProbe {
    let kind = FailureKind::from_message(detail);
    cam_debug(format!("probe fail kind={kind:?}: {detail}"));
    match kind {
        FailureKind::Permission => CameraProbe::PermissionDenied {
            detail: detail.to_string(),
        },
        FailureKind::Busy => CameraProbe::Busy {
            detail: detail.to_string(),
        },
        FailureKind::Missing if capture_nodes.is_empty() => CameraProbe::Missing,
        _ => {
            if capture_nodes.is_empty() && list_dev_video_nodes().is_empty() {
                CameraProbe::Missing
            } else if capture_nodes.is_empty() {
                // Only metadata nodes — treat as missing capture.
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
        // nokhwa typo "Fufill" and correct "Fulfill" both appear in the wild.
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

/// All `/dev/videoN` paths (including metadata-only).
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
    paths.sort_by(|a, b| {
        parse_video_index(a)
            .cmp(&parse_video_index(b))
            .then(a.cmp(b))
    });
    paths
}

/// True when the node advertises video capture (not metadata-only).
fn is_v4l2_capture_node(path: &Path) -> bool {
    let Ok(dev) = V4lDevice::with_path(path) else {
        cam_debug(format!("skip {}: cannot open for QUERYCAP", path.display()));
        return false;
    };
    let Ok(caps) = dev.query_caps() else {
        cam_debug(format!("skip {}: QUERYCAP failed", path.display()));
        return false;
    };
    let f = caps.capabilities;
    let capture = f.contains(CapFlags::VIDEO_CAPTURE) || f.contains(CapFlags::VIDEO_CAPTURE_MPLANE);
    let meta = f.contains(CapFlags::META_CAPTURE);
    cam_debug(format!(
        "{}: card=\"{}\" capture={capture} meta={meta} flags={f:?}",
        path.display(),
        caps.card
    ));
    capture
}

/// Capture-capable `/dev/video*` only (skips UVC metadata nodes like video1).
fn list_capture_video_nodes() -> Vec<PathBuf> {
    list_dev_video_nodes()
        .into_iter()
        .filter(|p| is_v4l2_capture_node(p))
        .collect()
}

fn parse_video_index(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("video")?
        .parse()
        .ok()
}

fn running_in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

fn env_camera_device() -> Option<PathBuf> {
    let raw = std::env::var("LIBREFOOD_CAMERA_DEVICE").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Candidate capture indices: env override, then capture-capable `/dev/video*`,
/// then nokhwa query filtered to those indices.
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

    if let Some(path) = env_camera_device() {
        if let Some(n) = parse_video_index(&path) {
            cam_debug(format!(
                "using LIBREFOOD_CAMERA_DEVICE={} -> index {n}",
                path.display()
            ));
            push(CameraIndex::Index(n), &mut out, &mut seen);
            return out;
        }
        cam_debug(format!(
            "LIBREFOOD_CAMERA_DEVICE={} not a /dev/videoN path — ignoring",
            path.display()
        ));
    }

    let capture = list_capture_video_nodes();
    for path in &capture {
        if let Some(n) = parse_video_index(path) {
            push(CameraIndex::Index(n), &mut out, &mut seen);
        }
    }

    // nokhwa often lists metadata nodes too — only keep indices we know capture.
    let capture_idx: std::collections::BTreeSet<u32> = capture
        .iter()
        .filter_map(|p| parse_video_index(p))
        .collect();

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
                    match info.index() {
                        CameraIndex::Index(n) if capture_idx.is_empty() || capture_idx.contains(n) => {
                            push(CameraIndex::Index(*n), &mut out, &mut seen);
                        }
                        CameraIndex::Index(n) => {
                            cam_debug(format!("  skip nokhwa index {n} (not capture-capable)"));
                        }
                        other => {
                            cam_debug(format!("  skip non-numeric index {other:?}"));
                        }
                    }
                }
            }
            Err(e) => cam_debug(format!("nokhwa query({api:?}): {e}")),
        }
    }

    if out.is_empty() && !capture_idx.is_empty() {
        for n in capture_idx {
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
        cam_debug("note: Flatpak sandbox — portal Camera/PipeWire may be required (see README)");
    }
    out
}

fn try_open_index(index: CameraIndex) -> Result<Camera, String> {
    // Prefer modest Closest/None first — AbsoluteHighest* can "Failed to Fulfill"
    // when the global max FPS isn't available in MJPEG/YUYV (common on Alpine/ARM).
    let attempts = [
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(640, 480),
            FrameFormat::MJPEG,
            30,
        )),
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(640, 480),
            FrameFormat::YUYV,
            30,
        )),
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(320, 240),
            FrameFormat::YUYV,
            30,
        )),
        RequestedFormatType::None,
        RequestedFormatType::HighestFrameRate(30),
        RequestedFormatType::AbsoluteHighestFrameRate,
        RequestedFormatType::AbsoluteHighestResolution,
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
    let capture = list_capture_video_nodes();
    let all_nodes = list_dev_video_nodes();
    let candidates = candidate_indices();
    let mut errors: Vec<String> = Vec::new();
    let mut attempted = 0u32;

    if candidates.is_empty() {
        if all_nodes.is_empty() {
            return Err(CameraError::NoCamera);
        }
        return Err(CameraError::Open(format!(
            "found {} /dev/video* node(s) but none advertise V4L2 video capture \
             (only metadata/control?). Try LIBREFOOD_CAMERA_DEVICE=/dev/videoN \
             and LIBREFOOD_CAMERA_DEBUG=1. nodes: {}",
            all_nodes.len(),
            all_nodes
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    for index in candidates {
        attempted += 1;
        match try_open_index(index.clone()) {
            Ok(cam) => return Ok(cam),
            Err(e) => {
                errors.push(format!("index {}: {e}", index.as_string()));
            }
        }
    }

    let skipped_meta = all_nodes.len().saturating_sub(capture.len());
    let detail = format!(
        "tried {attempted} capture device(s); skipped {skipped_meta} non-capture node(s). {}",
        errors.join(" | ")
    );
    cam_debug(&detail);

    if capture.is_empty() && all_nodes.is_empty() {
        return Err(CameraError::NoCamera);
    }

    Err(CameraError::Open(format!(
        "{detail}. capture devices: [{}]. Hint: `groups` should include `video`; \
         close other apps using the camera; \
         override with LIBREFOOD_CAMERA_DEVICE=/dev/video0; \
         debug with LIBREFOOD_CAMERA_DEBUG=1.",
        capture
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn rgb_to_luma(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|p| {
            ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8
        })
        .collect()
}

fn decode_rgb(width: u32, height: u32, rgb: &[u8]) -> Option<String> {
    if rgb.len() < (width * height * 3) as usize || width == 0 || height == 0 {
        return None;
    }
    let luma = rgb_to_luma(rgb);
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
        let capture = list_capture_video_nodes();
        let _ = candidate_indices();
        if !capture.is_empty() {
            assert!(
                probe.scan_enabled(),
                "capture nodes present => Scan must stay enabled, got {probe:?}"
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
            FailureKind::from_message("Failed to Fulfill"),
            FailureKind::Other
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
                detail: "Failed to Fulfill".into(),
            },
        ];
        for p in samples {
            let label = p.scan_button_label().to_lowercase();
            assert!(!label.contains("blocked"), "label: {label}");
            assert!(
                !p.unavailable_message().to_lowercase().contains("blocked"),
                "status for {p:?}"
            );
        }
    }

    #[test]
    fn classify_present_when_capture_nodes_exist() {
        let nodes = vec![PathBuf::from("/dev/video0")];
        let p = classify_probe_failure("Failed to Fulfill", &nodes);
        assert!(matches!(p, CameraProbe::Present { .. }));
        assert!(p.scan_enabled());
    }

    #[test]
    fn metadata_only_nodes_are_not_capture() {
        // On this host video1 is metadata if present; capture filter must not panic.
        for path in list_dev_video_nodes() {
            let _ = is_v4l2_capture_node(&path);
        }
        // Capture list is a subset of all nodes.
        let all = list_dev_video_nodes();
        let cap = list_capture_video_nodes();
        assert!(cap.iter().all(|p| all.contains(p)));
    }
}
