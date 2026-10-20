//! Webcam barcode scanning via raw V4L2 + optional GStreamer/libcamera.
//!
//! * **USB / simple V4L2:** single- or multi-planar ENUM_FMT → S_FMT → MMAP.
//! * **Qualcomm CAMSS phones:** raw STREAMON often returns `Broken pipe`; we
//!   fall back to GStreamer `libcamerasrc` (preferred) or `pipewiresrc`.
//!
//! Override: `LIBREFOOD_CAMERA_DEVICE=/dev/video0`
//! Backend: `LIBREFOOD_CAMERA_BACKEND=auto|v4l2|libcamera|pipewire`
//! Debug: `LIBREFOOD_CAMERA_DEBUG=1`

use image::ImageReader;
use rxing::{helpers::detect_in_luma, BarcodeFormat};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use v4l::buffer::Type as BufType;
use v4l::capability::Flags as CapFlags;
use v4l::framesize::FrameSizeEnum;
use v4l::prelude::*;
use v4l::v4l2;
use v4l::v4l_sys::{
    v4l2_buffer, v4l2_buffer__bindgen_ty_1, v4l2_fmtdesc, v4l2_format, v4l2_pix_format_mplane,
    v4l2_plane, v4l2_requestbuffers,
};
use v4l::video::Capture;
use v4l::{Format, FourCC};

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
    Uyvy,
    Vyuy,
    Yvyu,
    Grey,
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
    if capture.is_empty() && !crate::gst_cam::gst_camera_available() {
        CameraProbe::Missing
    } else if capture.is_empty() {
        CameraProbe::Present {
            detail: "no V4L2 capture nodes; GStreamer/libcamera may still work".into(),
        }
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
        "probe start; capture=[{}] camss={}",
        join_paths(&capture),
        detect_camss_system()
    ));
    // Prefer libcamera on CAMSS — raw V4L2 STREAMON is broken there.
    if detect_camss_system() || backend_prefers_gst() {
        if let Ok(cam) = crate::gst_cam::GstCamera::open() {
            cam_debug(format!("probe OK via gst ({})", cam.source_label()));
            drop(cam);
            return CameraProbe::Ready;
        }
    }
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
        Err(CameraError::NoCamera) => {
            if crate::gst_cam::GstCamera::open().is_ok() {
                CameraProbe::Ready
            } else {
                CameraProbe::Missing
            }
        }
        Err(CameraError::Open(detail)) => {
            if is_v4l_pipeline_dead(&detail) {
                if crate::gst_cam::GstCamera::open().is_ok() {
                    return CameraProbe::Ready;
                }
            }
            classify_probe_failure(&detail, &capture)
        }
        Err(e) => classify_probe_failure(&e.to_string(), &capture),
    }
}

fn backend_prefers_gst() -> bool {
    matches!(
        std::env::var("LIBREFOOD_CAMERA_BACKEND")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "libcamera" | "gst" | "gstreamer" | "pipewire" | "pw"
    )
}

fn is_v4l_pipeline_dead(msg: &str) -> bool {
    let l = msg.to_lowercase();
    l.contains("broken pipe")
        || l.contains("streamon")
        || l.contains("camss")
        || l.contains("need libcamera")
}

/// Qualcomm CAMSS (and similar) — raw /dev/video* STREAMON needs media graph / libcamera.
fn detect_camss_system() -> bool {
    for path in list_dev_video_nodes() {
        let Ok(dev) = Device::with_path(&path) else {
            continue;
        };
        let Ok(caps) = dev.query_caps() else {
            continue;
        };
        let card = caps.card.to_ascii_lowercase();
        let bus = caps.bus.to_ascii_lowercase();
        if card.contains("camss")
            || bus.contains("camss")
            || card.contains("qualcomm camera")
            || (card.contains("qualcomm") && card.contains("camera"))
        {
            cam_debug(format!(
                "CAMSS-like device {} card=\"{}\" bus=\"{}\"",
                path.display(),
                caps.card,
                caps.bus
            ));
            return true;
        }
    }
    false
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
    let card_l = caps.card.to_ascii_lowercase();
    // Skip codec nodes (Venus encode/decode) — not cameras.
    if card_l.contains("venus") || card_l.contains("encoder") || card_l.contains("decoder") {
        cam_debug(format!("skip {}: codec card \"{}\"", path.display(), caps.card));
        return false;
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaneMode {
    /// V4L2_BUF_TYPE_VIDEO_CAPTURE
    Single,
    /// V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE (Qualcomm CAMSS, etc.)
    Mplane,
}

impl PlaneMode {
    fn buf_type(self) -> BufType {
        match self {
            Self::Single => BufType::VideoCapture,
            Self::Mplane => BufType::VideoCaptureMplane,
        }
    }
}

#[derive(Debug, Clone)]
struct NegotiatedFormat {
    width: u32,
    height: u32,
    kind: PixelKind,
    mode: PlaneMode,
}

fn fourcc_kind(fourcc: FourCC) -> Option<PixelKind> {
    match &fourcc.repr {
        b"MJPG" | b"JPEG" => Some(PixelKind::Mjpeg),
        b"YUYV" => Some(PixelKind::Yuyv),
        b"UYVY" => Some(PixelKind::Uyvy),
        b"VYUY" => Some(PixelKind::Vyuy),
        b"YVYU" => Some(PixelKind::Yvyu),
        b"GREY" | b"Y800" => Some(PixelKind::Grey),
        _ => None,
    }
}

fn preferred_fourccs() -> [FourCC; 7] {
    [
        FourCC::new(b"MJPG"),
        FourCC::new(b"JPEG"),
        FourCC::new(b"YUYV"),
        FourCC::new(b"UYVY"),
        FourCC::new(b"VYUY"),
        FourCC::new(b"YVYU"),
        FourCC::new(b"GREY"),
    ]
}

fn pick_resolution(dev: &Device, fourcc: FourCC) -> (u32, u32) {
    let target = (640u32, 480u32);
    let Ok(sizes) = dev.enum_framesizes(fourcc) else {
        return target;
    };
    let mut best: Option<(u32, u32, u64)> = None;
    for fs in sizes {
        let candidates: Vec<(u32, u32)> = match fs.size {
            FrameSizeEnum::Discrete(d) => vec![(d.width, d.height)],
            FrameSizeEnum::Stepwise(s) => {
                // Continuous/stepwise (CAMSS: 1x1–8191x8191): clamp target into range.
                let w = target.0.clamp(s.min_width.max(1), s.max_width.max(1));
                let h = target.1.clamp(s.min_height.max(1), s.max_height.max(1));
                // Keep even dims for packed YUV.
                vec![(w & !1, h & !1), (w, h)]
            }
        };
        for (w, h) in candidates {
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

fn enum_formats_for(dev: &Device, mode: PlaneMode) -> Result<Vec<FourCC>, String> {
    let mut out = Vec::new();
    let mut desc = unsafe {
        std::mem::zeroed::<v4l2_fmtdesc>()
    };
    desc.type_ = mode.buf_type() as u32;
    desc.index = 0;
    loop {
        let ret = unsafe {
            v4l2::ioctl(
                dev.handle().fd(),
                v4l2::vidioc::VIDIOC_ENUM_FMT,
                &mut desc as *mut _ as *mut std::os::raw::c_void,
            )
        };
        match ret {
            Ok(()) => {
                out.push(FourCC::from(desc.pixelformat));
                desc.index += 1;
                desc.description = unsafe { std::mem::zeroed() };
            }
            Err(_) if desc.index == 0 => return Ok(Vec::new()),
            Err(_) => break,
        }
    }
    Ok(out)
}

fn set_format_for(
    dev: &Device,
    mode: PlaneMode,
    width: u32,
    height: u32,
    fourcc: FourCC,
) -> Result<(u32, u32, FourCC), String> {
    match mode {
        PlaneMode::Single => {
            let actual = dev
                .set_format(&Format::new(width, height, fourcc))
                .map_err(|e| format!("S_FMT: {e}"))?;
            Ok((actual.width, actual.height, actual.fourcc))
        }
        PlaneMode::Mplane => {
            let mut fmt = unsafe { std::mem::zeroed::<v4l2_format>() };
            fmt.type_ = BufType::VideoCaptureMplane as u32;
            unsafe {
                fmt.fmt.pix_mp = v4l2_pix_format_mplane {
                    width,
                    height,
                    pixelformat: u32::from(fourcc),
                    field: 1, // NONE / progressive
                    colorspace: 0,
                    plane_fmt: std::mem::zeroed(),
                    num_planes: 1,
                    flags: 0,
                    __bindgen_anon_1: std::mem::zeroed(),
                    quantization: 0,
                    xfer_func: 0,
                    reserved: [0; 7],
                };
            }
            unsafe {
                v4l2::ioctl(
                    dev.handle().fd(),
                    v4l2::vidioc::VIDIOC_S_FMT,
                    &mut fmt as *mut _ as *mut std::os::raw::c_void,
                )
                .map_err(|e| format!("S_FMT mplane: {e}"))?;
            }
            let mp = unsafe { fmt.fmt.pix_mp };
            Ok((mp.width, mp.height, FourCC::from(mp.pixelformat)))
        }
    }
}

fn negotiate_format(dev: &Device) -> Result<NegotiatedFormat, String> {
    let mut last = "ENUM_FMT returned no formats".to_string();
    for mode in [PlaneMode::Single, PlaneMode::Mplane] {
        let formats = enum_formats_for(dev, mode)?;
        if formats.is_empty() {
            cam_debug(format!("ENUM_FMT {:?} → (none)", mode));
            continue;
        }
        cam_debug(format!(
            "ENUM_FMT {:?} → {}",
            mode,
            formats
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));

        let Some(fourcc) = preferred_fourccs()
            .into_iter()
            .find(|p| formats.iter().any(|f| f == p))
        else {
            last = format!(
                "no supported packed format among [{}] (need YUYV/UYVY/MJPG/GREY)",
                formats
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            continue;
        };

        let Some(kind) = fourcc_kind(fourcc) else {
            last = format!("unsupported FourCC {fourcc}");
            continue;
        };

        let (width, height) = pick_resolution(dev, fourcc);
        match set_format_for(dev, mode, width, height, fourcc) {
            Ok((w, h, actual_fcc)) => {
                let kind = fourcc_kind(actual_fcc).unwrap_or(kind);
                if w == 0 || h == 0 {
                    last = "S_FMT returned zero size".into();
                    continue;
                }
                cam_debug(format!("S_FMT {:?} → {w}x{h} {actual_fcc} ({kind:?})", mode));
                return Ok(NegotiatedFormat {
                    width: w,
                    height: h,
                    kind,
                    mode,
                });
            }
            Err(e) => {
                last = e;
            }
        }
    }
    Err(last)
}

/// MMAP capture that works for both single-plane and multiplanar buffers.
struct FrameStream {
    fd: std::os::fd::RawFd,
    mode: PlaneMode,
    mapped: Vec<(*mut u8, usize)>,
    count: u32,
    queued: bool,
}

// SAFETY: we only use mapped buffers while the device fd is open and STREAMON.
unsafe impl Send for FrameStream {}

impl FrameStream {
    fn open(dev: &Device, mode: PlaneMode, count: u32) -> Result<Self, String> {
        let fd = dev.handle().fd();
        let buf_type = mode.buf_type() as u32;
        let mut req = unsafe { std::mem::zeroed::<v4l2_requestbuffers>() };
        req.count = count;
        req.type_ = buf_type;
        req.memory = 1; // V4L2_MEMORY_MMAP
        unsafe {
            v4l2::ioctl(
                fd,
                v4l2::vidioc::VIDIOC_REQBUFS,
                &mut req as *mut _ as *mut std::os::raw::c_void,
            )
            .map_err(|e| format!("REQBUFS: {e}"))?;
        }
        if req.count == 0 {
            return Err("REQBUFS returned 0 buffers".into());
        }

        let mut mapped = Vec::new();
        for index in 0..req.count {
            let (ptr, len) = match mode {
                PlaneMode::Single => {
                    let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                    buf.type_ = buf_type;
                    buf.memory = 1;
                    buf.index = index;
                    unsafe {
                        v4l2::ioctl(
                            fd,
                            v4l2::vidioc::VIDIOC_QUERYBUF,
                            &mut buf as *mut _ as *mut std::os::raw::c_void,
                        )
                        .map_err(|e| format!("QUERYBUF: {e}"))?;
                    }
                    let offset = unsafe { buf.m.offset };
                    let len = buf.length as usize;
                    let ptr = unsafe {
                        v4l2::mmap(
                            std::ptr::null_mut(),
                            len,
                            libc::PROT_READ | libc::PROT_WRITE,
                            libc::MAP_SHARED,
                            fd,
                            offset as libc::off_t,
                        )
                        .map_err(|e| format!("mmap: {e}"))?
                    };
                    (ptr as *mut u8, len)
                }
                PlaneMode::Mplane => {
                    let mut planes = [unsafe { std::mem::zeroed::<v4l2_plane>() }; 8];
                    let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                    buf.type_ = buf_type;
                    buf.memory = 1;
                    buf.index = index;
                    buf.length = planes.len() as u32;
                    buf.m = v4l2_buffer__bindgen_ty_1 {
                        planes: planes.as_mut_ptr(),
                    };
                    unsafe {
                        v4l2::ioctl(
                            fd,
                            v4l2::vidioc::VIDIOC_QUERYBUF,
                            &mut buf as *mut _ as *mut std::os::raw::c_void,
                        )
                        .map_err(|e| format!("QUERYBUF mplane: {e}"))?;
                    }
                    let plane = planes[0];
                    let len = plane.length as usize;
                    let offset = unsafe { plane.m.mem_offset };
                    let ptr = unsafe {
                        v4l2::mmap(
                            std::ptr::null_mut(),
                            len,
                            libc::PROT_READ | libc::PROT_WRITE,
                            libc::MAP_SHARED,
                            fd,
                            offset as libc::off_t,
                        )
                        .map_err(|e| format!("mmap mplane: {e}"))?
                    };
                    (ptr as *mut u8, len)
                }
            };
            mapped.push((ptr, len));
        }

        let mut stream = Self {
            fd,
            mode,
            mapped,
            count: req.count,
            queued: false,
        };
        stream.queue_all()?;
        stream.stream_on()?;
        Ok(stream)
    }

    fn queue_all(&mut self) -> Result<(), String> {
        let buf_type = self.mode.buf_type() as u32;
        for index in 0..self.count {
            match self.mode {
                PlaneMode::Single => {
                    let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                    buf.type_ = buf_type;
                    buf.memory = 1;
                    buf.index = index;
                    unsafe {
                        v4l2::ioctl(
                            self.fd,
                            v4l2::vidioc::VIDIOC_QBUF,
                            &mut buf as *mut _ as *mut std::os::raw::c_void,
                        )
                        .map_err(|e| format!("QBUF: {e}"))?;
                    }
                }
                PlaneMode::Mplane => {
                    let mut planes = [unsafe { std::mem::zeroed::<v4l2_plane>() }; 8];
                    let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                    buf.type_ = buf_type;
                    buf.memory = 1;
                    buf.index = index;
                    buf.length = 1;
                    buf.m = v4l2_buffer__bindgen_ty_1 {
                        planes: planes.as_mut_ptr(),
                    };
                    unsafe {
                        v4l2::ioctl(
                            self.fd,
                            v4l2::vidioc::VIDIOC_QBUF,
                            &mut buf as *mut _ as *mut std::os::raw::c_void,
                        )
                        .map_err(|e| format!("QBUF mplane: {e}"))?;
                    }
                }
            }
        }
        self.queued = true;
        Ok(())
    }

    fn stream_on(&self) -> Result<(), String> {
        let mut typ = self.mode.buf_type() as u32;
        unsafe {
            v4l2::ioctl(
                self.fd,
                v4l2::vidioc::VIDIOC_STREAMON,
                &mut typ as *mut _ as *mut std::os::raw::c_void,
            )
            .map_err(|e| format!("STREAMON: {e}"))?;
        }
        Ok(())
    }

    fn stream_off(&self) {
        let mut typ = self.mode.buf_type() as u32;
        let _ = unsafe {
            v4l2::ioctl(
                self.fd,
                v4l2::vidioc::VIDIOC_STREAMOFF,
                &mut typ as *mut _ as *mut std::os::raw::c_void,
            )
        };
    }

    /// Returns a copy of the next frame bytes (bytesused).
    fn next_frame(&mut self) -> Result<Vec<u8>, String> {
        self.poll_readable(2000)?;
        let buf_type = self.mode.buf_type() as u32;
        let (index, data_offset, payload) = match self.mode {
            PlaneMode::Single => {
                let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                buf.type_ = buf_type;
                buf.memory = 1;
                unsafe {
                    v4l2::ioctl(
                        self.fd,
                        v4l2::vidioc::VIDIOC_DQBUF,
                        &mut buf as *mut _ as *mut std::os::raw::c_void,
                    )
                    .map_err(|e| format!("DQBUF: {e}"))?;
                }
                (buf.index, 0usize, buf.bytesused as usize)
            }
            PlaneMode::Mplane => {
                let mut planes = [unsafe { std::mem::zeroed::<v4l2_plane>() }; 8];
                let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                buf.type_ = buf_type;
                buf.memory = 1;
                buf.length = 1;
                buf.m = v4l2_buffer__bindgen_ty_1 {
                    planes: planes.as_mut_ptr(),
                };
                unsafe {
                    v4l2::ioctl(
                        self.fd,
                        v4l2::vidioc::VIDIOC_DQBUF,
                        &mut buf as *mut _ as *mut std::os::raw::c_void,
                    )
                    .map_err(|e| format!("DQBUF mplane: {e}"))?;
                }
                let used = planes[0].bytesused as usize;
                let offset = planes[0].data_offset as usize;
                let payload = used.saturating_sub(offset);
                (buf.index, offset, payload)
            }
        };

        let (ptr, len) = self
            .mapped
            .get(index as usize)
            .copied()
            .ok_or_else(|| format!("bad buffer index {index}"))?;
        if data_offset >= len {
            return Err("mplane data_offset past buffer".into());
        }
        let n = payload.min(len - data_offset);
        let data = unsafe { std::slice::from_raw_parts(ptr.add(data_offset), n) }.to_vec();

        // Re-queue
        match self.mode {
            PlaneMode::Single => {
                let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                buf.type_ = buf_type;
                buf.memory = 1;
                buf.index = index;
                unsafe {
                    v4l2::ioctl(
                        self.fd,
                        v4l2::vidioc::VIDIOC_QBUF,
                        &mut buf as *mut _ as *mut std::os::raw::c_void,
                    )
                    .map_err(|e| format!("QBUF: {e}"))?;
                }
            }
            PlaneMode::Mplane => {
                let mut planes = [unsafe { std::mem::zeroed::<v4l2_plane>() }; 8];
                let mut buf = unsafe { std::mem::zeroed::<v4l2_buffer>() };
                buf.type_ = buf_type;
                buf.memory = 1;
                buf.index = index;
                buf.length = 1;
                buf.m = v4l2_buffer__bindgen_ty_1 {
                    planes: planes.as_mut_ptr(),
                };
                unsafe {
                    v4l2::ioctl(
                        self.fd,
                        v4l2::vidioc::VIDIOC_QBUF,
                        &mut buf as *mut _ as *mut std::os::raw::c_void,
                    )
                    .map_err(|e| format!("QBUF mplane: {e}"))?;
                }
            }
        }
        Ok(data)
    }

    fn poll_readable(&self, timeout_ms: i32) -> Result<(), String> {
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc < 0 {
            return Err("poll failed".into());
        }
        if rc == 0 {
            return Err("timeout waiting for frame".into());
        }
        Ok(())
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        self.stream_off();
        for &(ptr, len) in &self.mapped {
            if !ptr.is_null() && len > 0 {
                let _ = unsafe { v4l2::munmap(ptr as *mut _, len) };
            }
        }
        self.mapped.clear();
    }
}

/// Try opening `path`, negotiating format, and reading one frame.
fn try_device(path: &Path) -> Result<NegotiatedFormat, String> {
    let dev = Device::with_path(path).map_err(|e| format!("open: {e}"))?;
    let fmt = negotiate_format(&dev)?;
    let mut stream = FrameStream::open(&dev, fmt.mode, 4)?;
    let _ = stream.next_frame()?;
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
    let mut broken_pipe_hits = 0u32;
    for path in &candidates {
        cam_debug(format!("trying {}", path.display()));
        match try_device(path) {
            Ok(fmt) => {
                return Ok((path.clone(), fmt));
            }
            Err(e) => {
                cam_debug(format!("fail {}: {e}", path.display()));
                let lower = e.to_lowercase();
                if lower.contains("broken pipe") || lower.contains("streamon") {
                    broken_pipe_hits += 1;
                    // CAMSS: every pad fails the same way — stop and let caller use libcamera.
                    if detect_camss_system() || broken_pipe_hits >= 2 {
                        return Err(CameraError::Open(format!(
                            "V4L2 STREAMON failed ({e}). Qualcomm CAMSS nodes need libcamera \
                             (not raw /dev/video*). Will try GStreamer libcamerasrc/pipewiresrc."
                        )));
                    }
                }
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
    } else if broken_pipe_hits > 0 || detect_camss_system() {
        " Install libcamera + GStreamer libcamerasrc (see README); \
         set LIBREFOOD_CAMERA_BACKEND=libcamera."
    } else {
        " Try LIBREFOOD_CAMERA_DEVICE=/dev/video0 and LIBREFOOD_CAMERA_DEBUG=1."
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

fn yuv_pair_to_rgb(y0: i32, y1: i32, u: i32, v: i32, out: &mut Vec<u8>) {
    let u = u - 128;
    let v = v - 128;
    for y in [y0, y1] {
        let r = (y + ((351 * v) >> 8)).clamp(0, 255) as u8;
        let g = (y - ((179 * v + 86 * u) >> 8)).clamp(0, 255) as u8;
        let b = (y + ((443 * u) >> 8)).clamp(0, 255) as u8;
        out.extend_from_slice(&[r, g, b]);
    }
}

fn packed_yuv_to_rgb(kind: PixelKind, width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>, String> {
    let need = (width as usize).saturating_mul(height as usize).saturating_mul(2);
    if data.len() < need {
        return Err(format!(
            "{kind:?} buffer too small: got {} need {need}",
            data.len()
        ));
    }
    if width % 2 != 0 {
        return Err("packed YUV width must be even".into());
    }
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for chunk in data[..need].chunks_exact(4) {
        let (y0, y1, u, v) = match kind {
            PixelKind::Yuyv => (chunk[0] as i32, chunk[2] as i32, chunk[1] as i32, chunk[3] as i32),
            PixelKind::Uyvy => (chunk[1] as i32, chunk[3] as i32, chunk[0] as i32, chunk[2] as i32),
            PixelKind::Vyuy => (chunk[1] as i32, chunk[3] as i32, chunk[2] as i32, chunk[0] as i32),
            PixelKind::Yvyu => (chunk[0] as i32, chunk[2] as i32, chunk[3] as i32, chunk[1] as i32),
            _ => unreachable!(),
        };
        yuv_pair_to_rgb(y0, y1, u, v, &mut rgb);
    }
    Ok(rgb)
}

fn grey_to_rgb(width: u32, height: u32, grey: &[u8]) -> Result<Vec<u8>, String> {
    let need = (width as usize).saturating_mul(height as usize);
    if grey.len() < need {
        return Err(format!("GREY buffer too small: got {} need {need}", grey.len()));
    }
    let mut rgb = Vec::with_capacity(need * 3);
    for &g in &grey[..need] {
        rgb.extend_from_slice(&[g, g, g]);
    }
    Ok(rgb)
}

fn frame_to_rgb(kind: PixelKind, width: u32, height: u32, buf: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    match kind {
        PixelKind::Mjpeg => mjpeg_to_rgb(buf),
        PixelKind::Yuyv | PixelKind::Uyvy | PixelKind::Vyuy | PixelKind::Yvyu => {
            Ok((width, height, packed_yuv_to_rgb(kind, width, height, buf)?))
        }
        PixelKind::Grey => Ok((width, height, grey_to_rgb(width, height, buf)?)),
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

/// `LIBREFOOD_CAMERA_ROTATE=0|90|180|270|auto` — degrees clockwise, or auto.
fn env_rotate_degrees() -> Option<u16> {
    let Ok(raw) = std::env::var("LIBREFOOD_CAMERA_ROTATE") else {
        return None;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => None,
        "0" => Some(0),
        "90" => Some(90),
        "180" => Some(180),
        "270" => Some(270),
        other => {
            cam_debug(format!("ignore invalid LIBREFOOD_CAMERA_ROTATE={other:?}"));
            None
        }
    }
}

/// `LIBREFOOD_CAMERA_MIRROR=0|1|auto` — default auto (mirror front only).
fn env_mirror(front: bool) -> bool {
    match std::env::var("LIBREFOOD_CAMERA_MIRROR")
        .unwrap_or_else(|_| "auto".into())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => front,
    }
}

fn normalize_quarter_turns(deg: u32) -> u16 {
    match deg % 360 {
        0 => 0,
        90 => 90,
        180 => 180,
        270 => 270,
        // Snap odd values to nearest quarter (sensor quirks).
        n if n < 45 || n >= 315 => 0,
        n if n < 135 => 90,
        n if n < 225 => 180,
        _ => 270,
    }
}

fn mirror_rgb_h(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgb.len()];
    for y in 0..height {
        for x in 0..width {
            let sx = width - 1 - x;
            let si = ((y * width + sx) * 3) as usize;
            let di = ((y * width + x) * 3) as usize;
            out[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
        }
    }
    out
}

/// Rotate tightly packed RGB clockwise by 0/90/180/270.
fn rotate_rgb_cw(width: u32, height: u32, rgb: &[u8], degrees: u16) -> (u32, u32, Vec<u8>) {
    let need = (width as usize).saturating_mul(height as usize).saturating_mul(3);
    if rgb.len() < need || width == 0 || height == 0 {
        return (width, height, rgb.to_vec());
    }
    match degrees {
        0 => (width, height, rgb[..need].to_vec()),
        180 => {
            let mut out = vec![0u8; need];
            for y in 0..height {
                for x in 0..width {
                    let sx = width - 1 - x;
                    let sy = height - 1 - y;
                    let si = ((sy * width + sx) * 3) as usize;
                    let di = ((y * width + x) * 3) as usize;
                    out[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
                }
            }
            (width, height, out)
        }
        90 => {
            // (x,y) → (h-1-y, x); new size h×w
            let (nw, nh) = (height, width);
            let mut out = vec![0u8; need];
            for y in 0..height {
                for x in 0..width {
                    let nx = height - 1 - y;
                    let ny = x;
                    let si = ((y * width + x) * 3) as usize;
                    let di = ((ny * nw + nx) * 3) as usize;
                    out[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
                }
            }
            (nw, nh, out)
        }
        270 => {
            // (x,y) → (y, w-1-x); new size h×w
            let (nw, nh) = (height, width);
            let mut out = vec![0u8; need];
            for y in 0..height {
                for x in 0..width {
                    let nx = y;
                    let ny = width - 1 - x;
                    let si = ((y * width + x) * 3) as usize;
                    let di = ((ny * nw + nx) * 3) as usize;
                    out[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
                }
            }
            (nw, nh, out)
        }
        other => {
            cam_debug(format!("rotate_rgb_cw: unsupported {other}, leaving unrotated"));
            (width, height, rgb[..need].to_vec())
        }
    }
}

/// Upright RGB for preview + decode. Returns (w, h, rgb, logged_once_key).
fn orient_rgb_frame(
    width: u32,
    height: u32,
    rgb: Vec<u8>,
    camera_id: Option<&str>,
    phone_camss: bool,
) -> (u32, u32, Vec<u8>, String) {
    let hint = camera_id
        .map(crate::gst_cam::orientation_hint)
        .unwrap_or(crate::gst_cam::OrientationHint {
            sensor_rotation: None,
            front: false,
        });

    let (degrees, reason) = if let Some(d) = env_rotate_degrees() {
        (d, "env")
    } else if let Some(sr) = hint.sensor_rotation {
        (normalize_quarter_turns(sr), "sensor")
    } else if phone_camss && width > height {
        // Landscape buffers on portrait phones (fajita) — Snapshot-style upright.
        (90, "camss-landscape")
    } else {
        (0, "none")
    };
    let mirror = env_mirror(hint.front);

    let (w, h, mut out) = rotate_rgb_cw(width, height, &rgb, degrees);
    if mirror {
        out = mirror_rgb_h(w, h, &out);
    }
    let key = format!("rot={degrees} mirror={mirror} reason={reason} id={camera_id:?}");
    (w, h, out, key)
}

/// Shared knobs for a live scan (Snapshot-style camera flip).
#[derive(Clone)]
pub struct LiveScanControl {
    pub stop: Arc<AtomicBool>,
    /// Set true to cycle to the next libcamera (cleared by the scan loop).
    pub cycle: Arc<AtomicBool>,
    /// True while the capture thread is tearing down / recreating the pipeline.
    pub switching: Arc<AtomicBool>,
    /// Human label of the active camera (`Back · imx519`).
    pub label: Arc<Mutex<String>>,
    /// True when more than one libcamera exists (UI can show switch).
    pub can_switch: Arc<AtomicBool>,
}

impl LiveScanControl {
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            cycle: Arc::new(AtomicBool::new(false)),
            switching: Arc::new(AtomicBool::new(false)),
            label: Arc::new(Mutex::new(String::new())),
            can_switch: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn request_cycle(&self) {
        // Mark switching immediately so the UI can ignore double-taps before the
        // capture thread tears down the pipeline.
        self.switching.store(true, Ordering::SeqCst);
        self.cycle.store(true, Ordering::SeqCst);
    }
}

/// Re-export libcamera enumeration for the UI.
pub use crate::gst_cam::list_cameras;

/// Continuous capture with optional live RGB preview callbacks.
pub fn scan_barcode_live<F>(
    max_frames: u32,
    control: LiveScanControl,
    mut on_frame: F,
) -> Result<String, CameraError>
where
    F: FnMut(u32, u32, Vec<u8>),
{
    let backend = std::env::var("LIBREFOOD_CAMERA_BACKEND")
        .unwrap_or_else(|_| "auto".into())
        .to_ascii_lowercase();
    let camss = detect_camss_system();
    let cams = crate::gst_cam::list_cameras();
    control
        .can_switch
        .store(cams.len() > 1, Ordering::SeqCst);
    cam_debug(format!(
        "scan backend={backend} camss={camss} gst_avail={} cameras={}",
        crate::gst_cam::gst_camera_available(),
        cams.len()
    ));
    for c in &cams {
        cam_debug(format!(
            "  cam id={:?} facing={} label={:?}",
            c.id, c.facing, c.label
        ));
    }

    let try_gst_first = matches!(
        backend.as_str(),
        "libcamera" | "gst" | "gstreamer" | "pipewire" | "pw"
    ) || (backend == "auto" && camss);

    if try_gst_first {
        match scan_gst_live(max_frames, &control, &mut on_frame) {
            Ok(code) => return Ok(code),
            Err(e) => {
                cam_debug(format!("gst scan failed: {e}"));
                if matches!(
                    backend.as_str(),
                    "libcamera" | "gst" | "gstreamer" | "pipewire" | "pw"
                ) {
                    return Err(e);
                }
            }
        }
    }

    if !matches!(
        backend.as_str(),
        "libcamera" | "gst" | "gstreamer" | "pipewire" | "pw"
    ) {
        match scan_v4l2_live(max_frames, Arc::clone(&control.stop), &mut on_frame) {
            Ok(code) => return Ok(code),
            Err(e) => {
                let msg = e.to_string();
                cam_debug(format!("v4l2 scan failed: {msg}"));
                if is_v4l_pipeline_dead(&msg) || camss {
                    cam_debug("falling back to GStreamer/libcamera");
                    return scan_gst_live(max_frames, &control, &mut on_frame).map_err(|ge| {
                        CameraError::Open(format!(
                            "V4L2 failed ({msg}); libcamera/GStreamer also failed ({ge}). \
                             On Alpine: apk add libcamera gst-plugins-base gst-plugin-libcamera \
                             (names vary) — see README."
                        ))
                    });
                }
                return Err(e);
            }
        }
    }

    scan_gst_live(max_frames, &control, &mut on_frame)
}

fn scan_gst_live<F>(
    max_frames: u32,
    control: &LiveScanControl,
    on_frame: &mut F,
) -> Result<String, CameraError>
where
    F: FnMut(u32, u32, Vec<u8>),
{
    let cameras = crate::gst_cam::list_cameras();
    control
        .can_switch
        .store(cameras.len() > 1, Ordering::SeqCst);

    let mut cam = crate::gst_cam::GstCamera::open().map_err(CameraError::Open)?;
    let mut current_id = cam.camera_name().map(str::to_string);
    let phone_camss = detect_camss_system();
    {
        let label = cam.camera_label();
        if let Ok(mut g) = control.label.lock() {
            *g = label.clone();
        }
        cam_debug(format!(
            "scanning via gst ({}) camera={:?} label={label}",
            cam.source_label(),
            current_id
        ));
    }

    let mut last_err = CameraError::NoBarcode;
    let mut clear_switching_on_frame = false;
    let mut last_orient_log = String::new();
    for i in 0..max_frames {
        if control.stop.load(Ordering::Relaxed) {
            return Err(CameraError::Cancelled);
        }

        // Snapshot-style flip: tear down, then recreate with the **same backend**
        // (pipewire target-object or libcamerasrc) — never require libcamerasrc
        // when the live stream was pipewire-only.
        if control.cycle.swap(false, Ordering::SeqCst) && cameras.len() > 1 {
            let old_id = current_id.clone();
            let prefer = cam.source();
            let pos = cameras
                .iter()
                .position(|c| Some(c.id.as_str()) == old_id.as_deref())
                .unwrap_or(0);
            let next = &cameras[(pos + 1) % cameras.len()];
            cam_debug(format!(
                "switch request old_id={:?} new_id={:?} label={:?} backend={} cycle={}/{}",
                old_id,
                next.id,
                next.label,
                prefer.label(),
                pos + 1,
                cameras.len()
            ));

            cam.shutdown();
            std::thread::sleep(Duration::from_millis(500));

            let opened = match crate::gst_cam::GstCamera::open_for_switch(&next.id, prefer) {
                Ok(c) => Ok(c),
                Err(e) => {
                    // Front often needs a longer settle after Wide/Back release.
                    cam_debug(format!(
                        "switch first try FAIL id={:?}: {e}; retrying",
                        next.id
                    ));
                    std::thread::sleep(Duration::from_millis(450));
                    crate::gst_cam::GstCamera::open_for_switch(&next.id, prefer).map_err(|e2| {
                        format!("{e}; retry: {e2}")
                    })
                }
            };

            match opened {
                Ok(new_cam) => {
                    cam_debug(format!(
                        "pipeline recreate OK old_id={:?} new_id={:?} via={}",
                        old_id,
                        new_cam.camera_name(),
                        new_cam.source_label()
                    ));
                    cam = new_cam;
                    current_id = cam.camera_name().map(str::to_string);
                    if let Some(id) = current_id.as_deref() {
                        crate::gst_cam::save_camera_id(id);
                    }
                    let label = cam.camera_label();
                    if let Ok(mut g) = control.label.lock() {
                        *g = label;
                    }
                    last_orient_log.clear();
                    clear_switching_on_frame = true;
                }
                Err(e) => {
                    cam_debug(format!(
                        "pipeline recreate FAIL old_id={:?} new_id={:?}: {e}",
                        old_id, next.id
                    ));
                    // Clear UI "Switching…" even if restore fails — never hang.
                    control.switching.store(false, Ordering::SeqCst);
                    clear_switching_on_frame = false;
                    match restore_gst_camera(old_id.as_deref(), prefer) {
                        Ok(c) => {
                            cam = c;
                            current_id = cam.camera_name().map(str::to_string);
                            let label = cam.camera_label();
                            if let Ok(mut g) = control.label.lock() {
                                *g = label;
                            }
                            last_err = CameraError::Open(format!("switch camera: {e}"));
                        }
                        Err(re) => {
                            return Err(CameraError::Open(format!(
                                "switch failed ({e}); could not restore camera ({re})"
                            )));
                        }
                    }
                }
            }
        }

        let (width, height, rgb) =
            match cam.next_rgb(gstreamer::ClockTime::from_mseconds(500)) {
                Ok(v) => v,
                Err(e) => {
                    last_err = CameraError::Capture(e);
                    continue;
                }
            };
        let (width, height, rgb, orient_key) = orient_rgb_frame(
            width,
            height,
            rgb,
            current_id.as_deref(),
            phone_camss,
        );
        if orient_key != last_orient_log {
            cam_debug(format!("orientation {orient_key} frame={width}x{height}"));
            last_orient_log = orient_key;
        }
        if clear_switching_on_frame {
            control.switching.store(false, Ordering::SeqCst);
            clear_switching_on_frame = false;
        }
        if i % 2 == 0 {
            let (pw, ph, prgb) = shrink_for_preview(width, height, &rgb, 640);
            on_frame(pw, ph, prgb);
        }
        if let Some(code) = decode_barcode(width, height, &rgb) {
            return Ok(code);
        }
    }
    if control.stop.load(Ordering::Relaxed) {
        Err(CameraError::Cancelled)
    } else {
        Err(last_err)
    }
}

/// Re-open after a failed mid-scan switch (same backend as before when possible).
fn restore_gst_camera(
    old_id: Option<&str>,
    prefer: crate::gst_cam::GstSource,
) -> Result<crate::gst_cam::GstCamera, CameraError> {
    if let Some(id) = old_id {
        match crate::gst_cam::GstCamera::open_for_switch(id, prefer) {
            Ok(c) => {
                cam_debug(format!(
                    "restore camera OK id={id:?} via={}",
                    c.source_label()
                ));
                return Ok(c);
            }
            Err(e) => cam_debug(format!("restore named {id:?} failed: {e}")),
        }
    }
    crate::gst_cam::GstCamera::open().map_err(CameraError::Open)
}

fn scan_v4l2_live<F>(
    max_frames: u32,
    stop: Arc<AtomicBool>,
    on_frame: &mut F,
) -> Result<String, CameraError>
where
    F: FnMut(u32, u32, Vec<u8>),
{
    let (path, negotiated) = open_first_working()?;
    cam_debug(format!(
        "scanning on {} {}x{} {:?} ({:?})",
        path.display(),
        negotiated.width,
        negotiated.height,
        negotiated.kind,
        negotiated.mode
    ));

    let dev = Device::with_path(&path).map_err(|e| CameraError::Open(e.to_string()))?;
    let fmt = negotiate_format(&dev).map_err(CameraError::Open)?;
    let mut stream = FrameStream::open(&dev, fmt.mode, 4).map_err(CameraError::Open)?;

    std::thread::sleep(Duration::from_millis(50));

    let mut last_err = CameraError::NoBarcode;
    for i in 0..max_frames {
        if stop.load(Ordering::Relaxed) {
            return Err(CameraError::Cancelled);
        }

        let buf = match stream.next_frame() {
            Ok(v) => v,
            Err(e) => {
                last_err = CameraError::Capture(e);
                continue;
            }
        };

        let (width, height, rgb) =
            match frame_to_rgb(fmt.kind, fmt.width, fmt.height, &buf) {
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
            assert!(!p.unavailable_message().to_lowercase().contains("blocked"));
        }
    }

    #[test]
    fn yuyv_roundtrip_size() {
        let w = 4u32;
        let h = 2u32;
        let yuyv = vec![16u8, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128];
        let rgb = packed_yuv_to_rgb(PixelKind::Yuyv, w, h, &yuyv).unwrap();
        assert_eq!(rgb.len(), (w * h * 3) as usize);
        let uyvy = vec![128u8, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16, 128, 16];
        let rgb2 = packed_yuv_to_rgb(PixelKind::Uyvy, w, h, &uyvy).unwrap();
        assert_eq!(rgb2.len(), (w * h * 3) as usize);
    }

    #[test]
    fn metadata_subset() {
        let all = list_dev_video_nodes();
        let cap = list_capture_video_nodes();
        assert!(cap.iter().all(|p| all.contains(p)));
    }

    #[test]
    fn rotate_rgb_cw_swaps_dimensions() {
        // 2×1: pixels [R,G] → 90° CW → 1×2 column [R,G]
        let rgb = vec![1, 0, 0, 0, 1, 0];
        let (w, h, out) = rotate_rgb_cw(2, 1, &rgb, 90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&out[0..3], &[1, 0, 0]);
        assert_eq!(&out[3..6], &[0, 1, 0]);
        let (w2, h2, back) = rotate_rgb_cw(w, h, &out, 270);
        assert_eq!((w2, h2), (2, 1));
        assert_eq!(back, rgb);
    }

    #[test]
    fn normalize_quarter_turns_snaps() {
        assert_eq!(normalize_quarter_turns(90), 90);
        assert_eq!(normalize_quarter_turns(95), 90);
        assert_eq!(normalize_quarter_turns(0), 0);
        assert_eq!(normalize_quarter_turns(270), 270);
    }
}
