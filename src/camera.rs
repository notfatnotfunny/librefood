//! Webcam barcode scanning via raw V4L2 (`v4l` crate) + rxing.
//!
//! Alpine/ARM stacks often make nokhwa fail with
//! `CameraFormat: Failed to Fulfill` even on capture-capable nodes. We open
//! devices ourselves: QUERYCAP → ENUM_FMT → S_FMT → MMAP stream, decode
//! MJPEG/YUYV to RGB, then barcode-detect.
//!
//! Override: `LIBREFOOD_CAMERA_DEVICE=/dev/video0`
//! Debug: `LIBREFOOD_CAMERA_DEBUG=1`

use image::ImageReader;
use rxing::{helpers::detect_in_luma, BarcodeFormat};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use v4l::buffer::Type as BufType;
use v4l::capability::Flags as CapFlags;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;
use v4l::{Format, FourCC};
use v4l::framesize::FrameSizeEnum;

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
    Ready,
    Missing,
    PermissionDenied { detail: String },
    Busy { detail: String },
    /// Capture nodes exist; open deferred / previous soft failure.
    Present { detail: String },
}

impl CameraProbe {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PixelKind {
    Mjpeg,
    Yuyv,
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

#[allow(dead_code)]
pub fn camera_available() -> bool {
    camera_probe().is_ready()
}

/// Fast startup probe: list capture nodes only — does not stream.
pub fn camera_probe_quick() -> CameraProbe {
    let capture = list_capture_video_nodes();
    let all_nodes = list_dev_video_nodes();
    cam_debug(format!(
        "quick probe; all=[{}] capture=[{}]",
        join_paths(&all_nodes),
        join_paths(&capture)
    ));
    if capture.is_empty() {
        CameraProbe::Missing
    } else {
        CameraProbe::Present {
            detail: format!(
                "found {} capture device(s); open deferred until Scan",
                capture.len()
            ),
        }
    }
}

/// Full probe: try to open + grab one frame (used at Scan time).
pub fn camera_probe() -> CameraProbe {
    let capture = list_capture_video_nodes();
    cam_debug(format!(
        "probe start; capture=[{}]",
        join_paths(&capture)
    ));
    match open_first_working() {
        Ok((path, fmt)) => {
            cam_debug(format!(
                "probe OK {} {}x{} {:?}",
                path.display(),
                fmt.width,
                fmt.height,
                fmt.kind
            ));
            CameraProbe::Ready
        }
        Err(CameraError::NoCamera) => CameraProbe::Missing,
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
        _ if capture_nodes.is_empty() => CameraProbe::Missing,
        _ => CameraProbe::Present {
            detail: detail.to_string(),
        },
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

fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

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

fn is_v4l2_capture_node(path: &Path) -> bool {
    let Ok(dev) = Device::with_path(path) else {
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

fn env_camera_device() -> Option<PathBuf> {
    let raw = std::env::var("LIBREFOOD_CAMERA_DEVICE").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn candidate_paths() -> Vec<PathBuf> {
    if let Some(path) = env_camera_device() {
        cam_debug(format!("LIBREFOOD_CAMERA_DEVICE={}", path.display()));
        return vec![path];
    }
    list_capture_video_nodes()
}

#[derive(Debug, Clone)]
struct NegotiatedFormat {
    width: u32,
    height: u32,
    kind: PixelKind,
}

fn fourcc_kind(fourcc: FourCC) -> Option<PixelKind> {
    if fourcc == FourCC::new(b"MJPG") || fourcc == FourCC::new(b"JPEG") {
        Some(PixelKind::Mjpeg)
    } else if fourcc == FourCC::new(b"YUYV") {
        Some(PixelKind::Yuyv)
    } else {
        None
    }
}

fn pick_resolution(dev: &Device, fourcc: FourCC) -> (u32, u32) {
    let target = (640u32, 480u32);
    let Ok(sizes) = dev.enum_framesizes(fourcc) else {
        return target;
    };
    let mut best: Option<(u32, u32, u64)> = None;
    for fs in sizes {
        let discrete: Vec<(u32, u32)> = match fs.size {
            FrameSizeEnum::Discrete(d) => vec![(d.width, d.height)],
            FrameSizeEnum::Stepwise(s) => {
                // Prefer mid-ish size within stepwise range.
                vec![
                    (s.min_width.max(320).min(s.max_width), s.min_height.max(240).min(s.max_height)),
                    (
                        ((s.min_width + s.max_width) / 2).max(s.min_width),
                        ((s.min_height + s.max_height) / 2).max(s.min_height),
                    ),
                ]
            }
        };
        for (w, h) in discrete {
            if w == 0 || h == 0 {
                continue;
            }
            let score = (w as i64 - target.0 as i64).unsigned_abs()
                + (h as i64 - target.1 as i64).unsigned_abs();
            match best {
                None => best = Some((w, h, score)),
                Some((_, _, s)) if score < s => best = Some((w, h, score)),
                _ => {}
            }
        }
    }
    best.map(|(w, h, _)| (w, h)).unwrap_or(target)
}

fn negotiate_format(dev: &Device) -> Result<NegotiatedFormat, String> {
    let formats = dev
        .enum_formats()
        .map_err(|e| format!("ENUM_FMT: {e}"))?;
    if formats.is_empty() {
        return Err("ENUM_FMT returned no formats".into());
    }
    cam_debug(format!(
        "formats: {}",
        formats
            .iter()
            .map(|f| format!("{} ({})", f.fourcc, f.description))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let prefer = [FourCC::new(b"MJPG"), FourCC::new(b"JPEG"), FourCC::new(b"YUYV")];
    let fourcc = prefer
        .into_iter()
        .find(|p| formats.iter().any(|f| f.fourcc == *p))
        .or_else(|| formats.first().map(|f| f.fourcc))
        .ok_or_else(|| "no usable FourCC".to_string())?;

    let kind = fourcc_kind(fourcc).ok_or_else(|| {
        format!("unsupported FourCC {fourcc} (need MJPG/YUYV)")
    })?;

    let (width, height) = pick_resolution(dev, fourcc);
    let requested = Format::new(width, height, fourcc);
    let actual = dev
        .set_format(&requested)
        .map_err(|e| format!("S_FMT {width}x{height} {fourcc}: {e}"))?;

    let kind = fourcc_kind(actual.fourcc).unwrap_or(kind);
    cam_debug(format!(
        "S_FMT -> {}x{} {} ({kind:?})",
        actual.width, actual.height, actual.fourcc
    ));
    if actual.width == 0 || actual.height == 0 {
        return Err("S_FMT returned zero size".into());
    }
    Ok(NegotiatedFormat {
        width: actual.width,
        height: actual.height,
        kind,
    })
}

/// Try opening `path`, negotiating format, and reading one frame.
fn try_device(path: &Path) -> Result<NegotiatedFormat, String> {
    let mut dev = Device::with_path(path).map_err(|e| format!("open: {e}"))?;
    let fmt = negotiate_format(&dev)?;
    let mut stream = MmapStream::with_buffers(&mut dev, BufType::VideoCapture, 4)
        .map_err(|e| format!("REQBUFS/mmap: {e}"))?;
    let _ = stream.next().map_err(|e| format!("first frame: {e}"))?;
    Ok(fmt)
}

fn open_first_working() -> Result<(PathBuf, NegotiatedFormat), CameraError> {
    let candidates = candidate_paths();
    let all = list_dev_video_nodes();
    if candidates.is_empty() {
        if all.is_empty() {
            return Err(CameraError::NoCamera);
        }
        return Err(CameraError::Open(format!(
            "found {} /dev/video* node(s) but none advertise video capture. nodes: [{}]",
            all.len(),
            join_paths(&all)
        )));
    }

    let mut failures: Vec<(String, FailureKind)> = Vec::new();
    for path in &candidates {
        cam_debug(format!("trying {}", path.display()));
        match try_device(path) {
            Ok(fmt) => {
                return Ok((path.clone(), fmt));
            }
            Err(e) => {
                cam_debug(format!("fail {}: {e}", path.display()));
                failures.push((format!("{}: {e}", path.display()), FailureKind::from_message(&e)));
            }
        }
    }

    // Compact error: group identical messages instead of 12× spam.
    let n = failures.len();
    let mut by_msg: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut perm = 0usize;
    let mut busy = 0usize;
    for (msg, kind) in &failures {
        // Strip path prefix for grouping: "/dev/videoN: rest"
        let rest = msg
            .split_once(": ")
            .map(|(_, r)| r.to_string())
            .unwrap_or_else(|| msg.clone());
        *by_msg.entry(rest).or_default() += 1;
        match kind {
            FailureKind::Permission => perm += 1,
            FailureKind::Busy => busy += 1,
            _ => {}
        }
    }
    let summary = by_msg
        .into_iter()
        .map(|(m, c)| {
            if c == 1 {
                m
            } else {
                format!("{m} (×{c})")
            }
        })
        .collect::<Vec<_>>()
        .join("; ");

    let hint = if perm > 0 {
        " Permission denied — add user to `video` group and re-login."
    } else if busy > 0 {
        " Device busy — close other camera apps."
    } else {
        " Try LIBREFOOD_CAMERA_DEVICE=/dev/video0 and LIBREFOOD_CAMERA_DEBUG=1; \
         check `v4l2-ctl --list-formats-ext -d /dev/video0`."
    };

    Err(CameraError::Open(format!(
        "tried {n} capture device(s). {summary}.{hint}"
    )))
}

fn mjpeg_to_rgb(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| format!("jpeg probe: {e}"))?;
    let img = reader
        .decode()
        .map_err(|e| format!("jpeg decode: {e}"))?
        .to_rgb8();
    Ok((img.width(), img.height(), img.into_raw()))
}

fn yuyv_to_rgb(width: u32, height: u32, yuyv: &[u8]) -> Result<Vec<u8>, String> {
    let need = (width as usize).saturating_mul(height as usize).saturating_mul(2);
    if yuyv.len() < need {
        return Err(format!(
            "YUYV buffer too small: got {} need {need}",
            yuyv.len()
        ));
    }
    if width % 2 != 0 {
        return Err("YUYV width must be even".into());
    }
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for chunk in yuyv[..need].chunks_exact(4) {
        let y0 = chunk[0] as i32;
        let u = chunk[1] as i32 - 128;
        let y1 = chunk[2] as i32;
        let v = chunk[3] as i32 - 128;
        for y in [y0, y1] {
            let r = (y + ((351 * v) >> 8)).clamp(0, 255) as u8;
            let g = (y - ((179 * v + 86 * u) >> 8)).clamp(0, 255) as u8;
            let b = (y + ((443 * u) >> 8)).clamp(0, 255) as u8;
            rgb.extend_from_slice(&[r, g, b]);
        }
    }
    Ok(rgb)
}

fn frame_to_rgb(kind: PixelKind, width: u32, height: u32, buf: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    match kind {
        PixelKind::Mjpeg => mjpeg_to_rgb(buf),
        PixelKind::Yuyv => Ok((width, height, yuyv_to_rgb(width, height, buf)?)),
    }
}

fn rgb_to_luma(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
        .collect()
}

fn decode_barcode(width: u32, height: u32, rgb: &[u8]) -> Option<String> {
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
    let (path, negotiated) = open_first_working()?;
    cam_debug(format!(
        "scanning on {} {}x{} {:?}",
        path.display(),
        negotiated.width,
        negotiated.height,
        negotiated.kind
    ));

    let mut dev = Device::with_path(&path).map_err(|e| CameraError::Open(e.to_string()))?;
    // Re-apply format after reopen.
    let _ = negotiate_format(&dev).map_err(CameraError::Open)?;
    let mut stream = MmapStream::with_buffers(&mut dev, BufType::VideoCapture, 4)
        .map_err(|e| CameraError::Open(format!("mmap stream: {e}")))?;

    // Brief warm-up; ignore first couple of failures on flaky USB cams.
    std::thread::sleep(Duration::from_millis(50));

    let mut last_err = CameraError::NoBarcode;
    for i in 0..max_frames {
        if stop.load(Ordering::Relaxed) {
            return Err(CameraError::Cancelled);
        }

        let (buf, _meta) = match stream.next() {
            Ok(v) => v,
            Err(e) => {
                last_err = CameraError::Capture(e.to_string());
                continue;
            }
        };

        let (width, height, rgb) =
            match frame_to_rgb(negotiated.kind, negotiated.width, negotiated.height, buf) {
                Ok(v) => v,
                Err(e) => {
                    last_err = CameraError::Capture(e);
                    continue;
                }
            };

        if i % 2 == 0 {
            let (pw, ph, prgb) = shrink_for_preview(width, height, &rgb, 640);
            on_frame(pw, ph, prgb);
        }

        if let Some(code) = decode_barcode(width, height, &rgb) {
            return Ok(code);
        }
    }

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
        let probe = camera_probe_quick();
        let capture = list_capture_video_nodes();
        if !capture.is_empty() {
            assert!(probe.scan_enabled());
        }
        // Full probe opens hardware — only assert it does not panic.
        let _ = camera_probe();
    }

    #[test]
    fn quick_probe_does_not_open() {
        let q = camera_probe_quick();
        let capture = list_capture_video_nodes();
        if capture.is_empty() {
            assert!(!q.scan_enabled());
        } else {
            assert!(q.scan_enabled());
            assert!(matches!(q, CameraProbe::Present { .. }));
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
    }

    #[test]
    fn probe_labels_never_say_blocked() {
        for p in [
            CameraProbe::Ready,
            CameraProbe::Missing,
            CameraProbe::PermissionDenied {
                detail: "EACCES".into(),
            },
            CameraProbe::Busy {
                detail: "EBUSY".into(),
            },
            CameraProbe::Present {
                detail: "fail".into(),
            },
        ] {
            assert!(!p.scan_button_label().to_lowercase().contains("blocked"));
            assert!(!p.unavailable_message().to_lowercase().contains("blocked"));
        }
    }

    #[test]
    fn yuyv_roundtrip_size() {
        let w = 4u32;
        let h = 2u32;
        // 4*2*2 = 16 bytes YUYV
        let yuyv = vec![16u8, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128];
        let rgb = yuyv_to_rgb(w, h, &yuyv).unwrap();
        assert_eq!(rgb.len(), (w * h * 3) as usize);
    }

    #[test]
    fn metadata_subset() {
        let all = list_dev_video_nodes();
        let cap = list_capture_video_nodes();
        assert!(cap.iter().all(|p| all.contains(p)));
    }
}
