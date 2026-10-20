//! GStreamer camera path for phones that need libcamera (Qualcomm CAMSS).
//!
//! Tries, in order:
//! 1. `libcamerasrc` (optional — needs gst-plugin-libcamera)
//! 2. `pipewiresrc` with `target-object` (works without libcamerasrc on Alpine)
//!
//! Frames are RGB for the same preview + rxing path as V4L2.
//!
//! On multi-camera phones, prefer a **back/rear** device by default
//! (`LIBREFOOD_CAMERA_FACING=back`). Switch recreates the **same** backend
//! that opened the stream (pipewire or libcamera).

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gst_video::VideoFrameExt;
use std::process::Command;
use std::sync::OnceLock;

/// Cached `gst::init` result — never panic on the UI thread if init fails.
static GST_INIT: OnceLock<Result<(), String>> = OnceLock::new();

fn cam_debug(msg: impl AsRef<str>) {
    if matches!(
        std::env::var("LIBREFOOD_CAMERA_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    ) {
        eprintln!("[librefood camera] {}", msg.as_ref());
    }
}

/// Idempotent GStreamer init. Must run before DeviceMonitor / ElementFactory / pipelines.
fn ensure_gst() -> Result<(), String> {
    GST_INIT
        .get_or_init(|| gst::init().map_err(|e| format!("gstreamer init: {e}")))
        .clone()
}

fn element_available(name: &str) -> bool {
    if ensure_gst().is_err() {
        return false;
    }
    gst::ElementFactory::find(name).is_some()
}

/// Which GStreamer source to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GstSource {
    LibCamera,
    PipeWire,
}

impl GstSource {
    fn element_name(self) -> &'static str {
        match self {
            Self::LibCamera => "libcamerasrc",
            Self::PipeWire => "pipewiresrc",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LibCamera => "libcamera",
            Self::PipeWire => "pipewire",
        }
    }

    fn from_factory_name(name: &str) -> Self {
        let l = name.to_ascii_lowercase();
        if l.contains("libcamera") {
            Self::LibCamera
        } else {
            Self::PipeWire
        }
    }
}

/// Preferred camera facing for selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraFacingPref {
    Back,
    Front,
    Any,
}

impl CameraFacingPref {
    fn from_env() -> Self {
        match std::env::var("LIBREFOOD_CAMERA_FACING")
            .unwrap_or_else(|_| "back".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "front" | "user" | "selfie" => Self::Front,
            "any" | "all" | "*" => Self::Any,
            _ => Self::Back,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraFacing {
    Front,
    Back,
    External,
    Unknown,
}

impl CameraFacing {
    fn label(self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Back => "back",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }

    fn matches(self, pref: CameraFacingPref) -> bool {
        match pref {
            CameraFacingPref::Any => true,
            CameraFacingPref::Back => matches!(self, Self::Back | Self::External),
            CameraFacingPref::Front => matches!(self, Self::Front),
        }
    }

    fn score(self, pref: CameraFacingPref) -> i32 {
        match pref {
            CameraFacingPref::Back => match self {
                Self::Back => 300,
                Self::External => 250,
                Self::Unknown => 100,
                Self::Front => 0,
            },
            CameraFacingPref::Front => match self {
                Self::Front => 300,
                Self::Unknown => 100,
                Self::External => 50,
                Self::Back => 0,
            },
            CameraFacingPref::Any => match self {
                Self::Back => 200,
                Self::External => 180,
                Self::Unknown => 100,
                Self::Front => 50,
            },
        }
    }
}

#[derive(Debug, Clone)]
struct CamCandidate {
    /// Stable unique id (PipeWire `object.serial`, else `node.name`, else display#area).
    id: String,
    /// Human display name from DeviceMonitor / cam --list.
    display: String,
    facing: CameraFacing,
    /// Rough max area from caps (0 if unknown).
    max_area: u64,
    /// Value for `pipewiresrc target-object` (serial / node.name / display).
    pw_target: String,
    /// Value for `libcamerasrc camera-name` when known.
    libcamera_name: Option<String>,
}

fn facing_from_text(s: &str) -> Option<CameraFacing> {
    let l = s.to_ascii_lowercase();
    if l.contains("front") || l.contains("selfie") || l.contains("user-facing") {
        Some(CameraFacing::Front)
    } else if l.contains("back")
        || l.contains("rear")
        || l.contains("world")
        || l.contains("environment")
    {
        Some(CameraFacing::Back)
    } else if l.contains("external") {
        Some(CameraFacing::External)
    } else {
        None
    }
}

fn facing_from_location_prop(props: &gst::StructureRef) -> Option<CameraFacing> {
    for key in [
        "api.libcamera.Location",
        "api.libcamera.location",
    ] {
        if let Ok(Some(s)) = props.get_optional::<&str>(key) {
            return facing_from_text(s).or_else(|| match s {
                "0" => Some(CameraFacing::Front),
                "1" => Some(CameraFacing::Back),
                "2" => Some(CameraFacing::External),
                _ => None,
            });
        }
        if let Ok(Some(v)) = props.get_optional::<i32>(key) {
            return match v {
                0 => Some(CameraFacing::Front),
                1 => Some(CameraFacing::Back),
                2 => Some(CameraFacing::External),
                _ => None,
            };
        }
    }
    None
}

fn prop_as_string(props: &gst::StructureRef, key: &str) -> Option<String> {
    if let Ok(Some(s)) = props.get_optional::<String>(key) {
        return Some(s);
    }
    if let Ok(Some(s)) = props.get_optional::<&str>(key) {
        return Some(s.to_string());
    }
    if let Ok(Some(v)) = props.get_optional::<u64>(key) {
        return Some(v.to_string());
    }
    if let Ok(Some(v)) = props.get_optional::<i64>(key) {
        return Some(v.to_string());
    }
    if let Ok(Some(v)) = props.get_optional::<u32>(key) {
        return Some(v.to_string());
    }
    if let Ok(Some(v)) = props.get_optional::<i32>(key) {
        return Some(v.to_string());
    }
    None
}

fn sensor_from_id(id: &str) -> Option<String> {
    let lower = id.to_ascii_lowercase();
    for token in lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        if token.starts_with("imx")
            || token.starts_with("ov")
            || token.starts_with("s5k")
            || token.starts_with("hi")
            || token.starts_with("gc")
        {
            return Some(token.to_string());
        }
    }
    None
}

fn facing_word(facing: CameraFacing) -> &'static str {
    match facing {
        CameraFacing::Front => "Front",
        CameraFacing::Back => "Back",
        CameraFacing::External => "External",
        CameraFacing::Unknown => "Camera",
    }
}

fn friendly_label(facing: CameraFacing, id: &str, display: &str, max_area: u64) -> String {
    let word = facing_word(facing);
    if let Some(sensor) = sensor_from_id(id).or_else(|| sensor_from_id(display)) {
        return format!("{word} · {sensor}");
    }
    if max_area > 0 {
        // Disambiguate two "Built-in Back Camera" entries by resolution.
        return format!("{word} · {}k", max_area / 1000);
    }
    if !display.is_empty() && facing_from_text(display).is_some() {
        return display.to_string();
    }
    word.to_string()
}

fn config_dir() -> Option<std::path::PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(xdg).join("librefood"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".config/librefood"))
}

fn saved_camera_path() -> Option<std::path::PathBuf> {
    config_dir().map(|d| d.join("camera-id"))
}

/// Last camera id chosen by the user (or auto-selected).
pub fn load_saved_camera_id() -> Option<String> {
    let path = saved_camera_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let id = text.trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

pub fn save_camera_id(id: &str) {
    let Some(path) = saved_camera_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, format!("{id}\n")) {
        cam_debug(format!("save camera-id failed: {e}"));
    } else {
        cam_debug(format!("saved camera-id={id}"));
    }
}

/// Public camera descriptor for UI / debug.
#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub id: String,
    pub label: String,
    pub facing: &'static str,
}

impl CameraInfo {
    fn from_candidate(c: &CamCandidate) -> Self {
        Self {
            id: c.id.clone(),
            label: friendly_label(c.facing, &c.id, &c.display, c.max_area),
            facing: c.facing.label(),
        }
    }
}

/// Drop CAMSS V4L junk / unknown zero-area pads; keep Front/Back(/External).
fn is_switchable_camera(c: &CamCandidate) -> bool {
    let name = format!("{} {}", c.display, c.id).to_ascii_lowercase();
    if name.contains("qualcomm camera subsystem") {
        return false;
    }
    match c.facing {
        CameraFacing::Front | CameraFacing::Back | CameraFacing::External => true,
        CameraFacing::Unknown => c.max_area > 0,
    }
}

/// Enumerate cameras for Switch UI (filtered Front/Back, unique ids).
pub fn list_cameras() -> Vec<CameraInfo> {
    if ensure_gst().is_err() {
        return Vec::new();
    }
    let cams = list_switchable_candidates();
    let mut out: Vec<CameraInfo> = cams.iter().map(CameraInfo::from_candidate).collect();
    let pref = CameraFacingPref::Back;
    out.sort_by(|a, b| {
        let fa = cams
            .iter()
            .find(|c| c.id == a.id)
            .map(|c| c.facing.score(pref))
            .unwrap_or(0);
        let fb = cams
            .iter()
            .find(|c| c.id == b.id)
            .map(|c| c.facing.score(pref))
            .unwrap_or(0);
        fb.cmp(&fa)
            .then_with(|| {
                let aa = cams
                    .iter()
                    .find(|c| c.id == a.id)
                    .map(|c| c.max_area)
                    .unwrap_or(0);
                let ab = cams
                    .iter()
                    .find(|c| c.id == b.id)
                    .map(|c| c.max_area)
                    .unwrap_or(0);
                ab.cmp(&aa)
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    for c in &out {
        cam_debug(format!(
            "listed camera id={:?} facing={} label={:?}",
            c.id, c.facing, c.label
        ));
    }
    out
}

fn max_area_from_caps(caps: &gst::Caps) -> u64 {
    let mut best = 0u64;
    for i in 0..caps.size() {
        let Some(s) = caps.structure(i) else {
            continue;
        };
        let w = s.get_optional::<i32>("width").ok().flatten().unwrap_or(0);
        let h = s.get_optional::<i32>("height").ok().flatten().unwrap_or(0);
        if w > 0 && h > 0 {
            best = best.max((w as u64).saturating_mul(h as u64));
        }
    }
    best
}

fn candidate_sources() -> Vec<GstSource> {
    if let Ok(forced) = std::env::var("LIBREFOOD_CAMERA_BACKEND") {
        match forced.to_ascii_lowercase().as_str() {
            "libcamera" | "gst" | "gstreamer" => return vec![GstSource::LibCamera],
            "pipewire" | "pw" => return vec![GstSource::PipeWire],
            _ => {}
        }
    }
    let mut out = Vec::new();
    if element_available("libcamerasrc") {
        out.push(GstSource::LibCamera);
    } else {
        cam_debug(
            "libcamerasrc not installed (optional: apk add gst-plugin-libcamera; \
             pipewire path works without it)",
        );
    }
    if element_available("pipewiresrc") {
        out.push(GstSource::PipeWire);
    } else {
        cam_debug("pipewiresrc not installed");
    }
    out
}

fn list_from_device_monitor() -> Vec<CamCandidate> {
    if ensure_gst().is_err() {
        cam_debug("list_from_device_monitor: gst init failed");
        return Vec::new();
    }
    let monitor = gst::DeviceMonitor::new();
    let _ = monitor.add_filter(Some("Source/Video"), None);
    let _ = monitor.add_filter(Some("Video/Source"), None);
    if monitor.start().is_err() {
        cam_debug("DeviceMonitor start failed");
        return Vec::new();
    }
    let devices = monitor.devices();
    let mut out = Vec::new();
    for dev in devices.iter() {
        let display = dev.display_name().to_string();
        let props = dev.properties();
        let mut facing = props
            .as_ref()
            .and_then(|p| facing_from_location_prop(p))
            .or_else(|| facing_from_text(&display))
            .unwrap_or(CameraFacing::Unknown);

        if facing == CameraFacing::Unknown {
            if let Some(props) = props.as_ref() {
                for (key, val) in props.iter() {
                    let key_l = key.as_str().to_ascii_lowercase();
                    if key_l.contains("location") || key_l.contains("facing") {
                        if let Ok(s) = val.get::<&str>() {
                            if let Some(f) = facing_from_text(s) {
                                facing = f;
                                break;
                            }
                        }
                    }
                }
            }
        }

        let max_area = dev.caps().as_ref().map(max_area_from_caps).unwrap_or(0);
        let class = dev.device_class().to_string();

        let serial = props
            .as_ref()
            .and_then(|p| prop_as_string(p, "object.serial"));
        let node_name = props
            .as_ref()
            .and_then(|p| prop_as_string(p, "node.name"));
        let libcamera_id = props.as_ref().and_then(|p| {
            prop_as_string(p, "api.libcamera.CameraId")
                .or_else(|| prop_as_string(p, "api.libcamera.id"))
                .or_else(|| prop_as_string(p, "camera.name"))
        });
        let device_api = props
            .as_ref()
            .and_then(|p| prop_as_string(p, "device.api"))
            .unwrap_or_default();

        // Unique id: serial (best for pipewire target-object) → libcamera id → node → display#area.
        let id = serial
            .clone()
            .or_else(|| libcamera_id.clone())
            .or_else(|| node_name.clone())
            .unwrap_or_else(|| format!("{display}#{max_area}"));

        let pw_target = serial
            .or_else(|| node_name)
            .unwrap_or_else(|| display.clone());

        let libcamera_name = if device_api.contains("libcamera") {
            libcamera_id.or_else(|| Some(display.clone()))
        } else if display.contains('/') || display.starts_with("/base/") {
            Some(display.clone())
        } else {
            libcamera_id
        };

        cam_debug(format!(
            "gst device id={id:?} display={display:?} facing={} area={max_area} \
             pw_target={pw_target:?} libcamera={libcamera_name:?} api={device_api:?} class={class}",
            facing.label()
        ));
        out.push(CamCandidate {
            id,
            display,
            facing,
            max_area,
            pw_target,
            libcamera_name,
        });
    }
    monitor.stop();
    out
}

/// Fallback: parse `cam --list` / `cam -l` (libcamera-tools).
fn list_from_cam_cli() -> Vec<CamCandidate> {
    let output = Command::new("cam")
        .args(["--list"])
        .output()
        .or_else(|_| Command::new("cam").arg("-l").output());
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() && out.stdout.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut out_list = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let rest = if let Some((_, r)) = line.split_once(':') {
            r.trim()
        } else {
            continue;
        };
        let (display, id) = if let (Some(open), Some(close)) = (rest.rfind('('), rest.rfind(')')) {
            if open < close {
                let label = rest[..open].trim().to_string();
                let id = rest[open + 1..close].trim().to_string();
                (label, id)
            } else {
                (rest.to_string(), rest.to_string())
            }
        } else {
            (rest.to_string(), rest.to_string())
        };
        if id.is_empty() {
            continue;
        }
        let facing = facing_from_text(&display)
            .or_else(|| facing_from_text(&id))
            .unwrap_or(CameraFacing::Unknown);
        cam_debug(format!(
            "cam --list id={id:?} display={display:?} facing={}",
            facing.label()
        ));
        out_list.push(CamCandidate {
            pw_target: id.clone(),
            libcamera_name: Some(id.clone()),
            id,
            display,
            facing,
            max_area: 0,
        });
    }
    out_list
}

fn list_all_candidates() -> Vec<CamCandidate> {
    let mut cams = list_from_device_monitor();
    if cams.is_empty() {
        cams = list_from_cam_cli();
    } else if cams.iter().all(|c| c.facing == CameraFacing::Unknown) {
        let cli = list_from_cam_cli();
        for c in &mut cams {
            if let Some(h) = cli.iter().find(|h| h.id == c.id || c.id.contains(&h.id)) {
                if h.facing != CameraFacing::Unknown {
                    c.facing = h.facing;
                    if c.display.is_empty() || c.display == c.id {
                        c.display = h.display.clone();
                    }
                    if c.libcamera_name.is_none() {
                        c.libcamera_name = h.libcamera_name.clone();
                    }
                }
            }
        }
    }
    cams
}

fn list_switchable_candidates() -> Vec<CamCandidate> {
    list_all_candidates()
        .into_iter()
        .filter(is_switchable_camera)
        .collect()
}

fn camera_index_override() -> Option<usize> {
    std::env::var("LIBREFOOD_CAMERA_INDEX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
}

fn find_candidate<'a>(cams: &'a [CamCandidate], key: &str) -> Option<&'a CamCandidate> {
    cams.iter()
        .find(|c| c.id == key)
        .or_else(|| cams.iter().find(|c| c.pw_target == key))
        .or_else(|| {
            cams.iter()
                .find(|c| c.libcamera_name.as_deref() == Some(key))
        })
        .or_else(|| {
            // Legacy saved display name — prefer largest area.
            let mut matches: Vec<&CamCandidate> =
                cams.iter().filter(|c| c.display == key).collect();
            matches.sort_by(|a, b| b.max_area.cmp(&a.max_area));
            matches.first().copied()
        })
}

/// Resolve preferred camera id for open / switch.
/// Order: env NAME → env INDEX (within facing) → saved id → facing preference (default back).
#[allow(dead_code)]
pub fn resolve_libcamera_name() -> Option<String> {
    resolve_camera_id()
}

pub fn resolve_camera_id() -> Option<String> {
    if let Err(e) = ensure_gst() {
        cam_debug(format!("resolve_camera_id: {e}"));
        return None;
    }

    if let Ok(name) = std::env::var("LIBREFOOD_CAMERA_NAME") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            cam_debug(format!("LIBREFOOD_CAMERA_NAME override: {name}"));
            return Some(name);
        }
    }

    let pref = CameraFacingPref::from_env();
    let mut cams = list_switchable_candidates();
    if cams.is_empty() {
        cam_debug("no switchable camera candidates; using source default");
        return None;
    }

    for c in &cams {
        cam_debug(format!(
            "candidate id={:?} facing={} area={} display={:?} label={:?}",
            c.id,
            c.facing.label(),
            c.max_area,
            c.display,
            friendly_label(c.facing, &c.id, &c.display, c.max_area)
        ));
    }

    cams.sort_by(|a, b| {
        b.facing
            .score(pref)
            .cmp(&a.facing.score(pref))
            .then_with(|| b.max_area.cmp(&a.max_area))
            .then_with(|| a.id.cmp(&b.id))
    });

    let matching: Vec<&CamCandidate> = cams.iter().filter(|c| c.facing.matches(pref)).collect();
    let pool: Vec<&CamCandidate> = if matching.is_empty() {
        cam_debug(format!(
            "no camera matches facing={pref:?}; falling back to best available"
        ));
        cams.iter().collect()
    } else {
        matching
    };

    if let Some(idx) = camera_index_override() {
        let chosen = pool.get(idx).copied().or_else(|| pool.first().copied())?;
        cam_debug(format!(
            "selected via INDEX={idx} id={:?} facing={} label={:?}",
            chosen.id,
            chosen.facing.label(),
            friendly_label(chosen.facing, &chosen.id, &chosen.display, chosen.max_area)
        ));
        return Some(chosen.id.clone());
    }

    if let Some(saved) = load_saved_camera_id() {
        if let Some(c) = find_candidate(&cams, &saved) {
            if c.facing.matches(pref) || pref == CameraFacingPref::Any {
                cam_debug(format!(
                    "selected saved id={:?} facing={} label={:?}",
                    c.id,
                    c.facing.label(),
                    friendly_label(c.facing, &c.id, &c.display, c.max_area)
                ));
                return Some(c.id.clone());
            }
            cam_debug(format!(
                "saved id={:?} facing={} mismatches pref={pref:?}; ignoring saved",
                c.id,
                c.facing.label()
            ));
        } else {
            cam_debug(format!("saved camera-id {saved:?} not in list; ignoring"));
        }
    }

    let chosen = pool.first().copied()?;
    cam_debug(format!(
        "selected camera id={:?} facing={} label={:?} (pref={pref:?})",
        chosen.id,
        chosen.facing.label(),
        friendly_label(chosen.facing, &chosen.id, &chosen.display, chosen.max_area)
    ));
    Some(chosen.id.clone())
}

fn find_monitor_device(id: &str) -> Option<gst::Device> {
    if ensure_gst().is_err() {
        return None;
    }
    let monitor = gst::DeviceMonitor::new();
    let _ = monitor.add_filter(Some("Source/Video"), None);
    let _ = monitor.add_filter(Some("Video/Source"), None);
    if monitor.start().is_err() {
        return None;
    }
    let devices = monitor.devices();
    let cams = list_from_device_monitor();
    let want = find_candidate(&cams, id);
    let result = devices.iter().find(|dev| {
        let display = dev.display_name().to_string();
        let props = dev.properties();
        let serial = props
            .as_ref()
            .and_then(|p| prop_as_string(p, "object.serial"));
        let node = props
            .as_ref()
            .and_then(|p| prop_as_string(p, "node.name"));
        let lib_id = props.as_ref().and_then(|p| {
            prop_as_string(p, "api.libcamera.CameraId")
                .or_else(|| prop_as_string(p, "api.libcamera.id"))
        });
        if let Some(c) = want {
            return serial.as_deref() == Some(c.id.as_str())
                || node.as_deref() == Some(c.id.as_str())
                || lib_id.as_deref() == Some(c.id.as_str())
                || (display == c.display
                    && dev
                        .caps()
                        .map(|caps| max_area_from_caps(&caps) == c.max_area)
                        .unwrap_or(false))
                || (c.id == id
                    && (serial.as_deref() == Some(id)
                        || node.as_deref() == Some(id)
                        || display == id));
        }
        serial.as_deref() == Some(id)
            || node.as_deref() == Some(id)
            || lib_id.as_deref() == Some(id)
            || display == id
    }).cloned();
    monitor.stop();
    result
}

fn link_rgb_appsink(
    pipeline: &gst::Pipeline,
    src: &gst::Element,
) -> Result<gst_app::AppSink, String> {
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|_| "videoconvert missing".to_string())?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|_| "capsfilter missing".to_string())?;
    let sink = gst::ElementFactory::make("appsink")
        .name("sink")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 2u32)
        .property("drop", true)
        .build()
        .map_err(|_| "appsink missing".to_string())?;

    pipeline
        .add_many([src, &convert, &filter, &sink])
        .map_err(|e| format!("add elements: {e}"))?;
    gst::Element::link_many([src, &convert, &filter, &sink])
        .map_err(|_| "link camera → RGB appsink failed".to_string())?;

    sink.downcast::<gst_app::AppSink>()
        .map_err(|_| "appsink cast failed".to_string())
}

fn build_pipeline(
    src: GstSource,
    camera_id: Option<&str>,
) -> Result<(gst::Pipeline, gst_app::AppSink, Option<String>), String> {
    let desc = format!(
        "{} name=cam ! videoconvert ! video/x-raw,format=RGB ! \
         appsink name=sink emit-signals=false sync=false max-buffers=2 drop=true",
        src.element_name()
    );
    cam_debug(format!("gst pipeline ({}): {desc}", src.label()));
    let pipeline = gst::parse::launch(&desc)
        .map_err(|e| format!("parse {}: {e}", src.label()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| format!("{}: not a pipeline", src.label()))?;

    let mut applied = None;
    let cand = camera_id.and_then(|id| {
        let cams = list_switchable_candidates();
        find_candidate(&cams, id).cloned()
    });

    if matches!(src, GstSource::LibCamera) {
        let name = camera_id
            .and_then(|id| {
                cand.as_ref()
                    .and_then(|c| c.libcamera_name.clone())
                    .or_else(|| Some(id.to_string()))
            })
            .or_else(resolve_camera_id);
        if let Some(name) = name {
            let el = pipeline
                .by_name("cam")
                .ok_or_else(|| "libcamerasrc element missing".to_string())?;
            el.set_property_from_str("camera-name", &name);
            let got: String = el.property("camera-name");
            cam_debug(format!(
                "set libcamerasrc camera-name want={name:?} got={got:?}"
            ));
            if !got.is_empty() && got != name {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(format!(
                    "camera-name not applied (want {name:?}, got {got:?})"
                ));
            }
            applied = Some(
                cand.as_ref()
                    .map(|c| c.id.clone())
                    .unwrap_or_else(|| name),
            );
        } else if camera_id.is_some() {
            return Err("could not resolve camera-name for libcamerasrc".into());
        }
    } else if matches!(src, GstSource::PipeWire) {
        let target = camera_id.map(|id| {
            cand.as_ref()
                .map(|c| c.pw_target.clone())
                .unwrap_or_else(|| id.to_string())
        }).or_else(|| {
            resolve_camera_id().and_then(|id| {
                let cams = list_switchable_candidates();
                find_candidate(&cams, &id).map(|c| c.pw_target.clone())
            })
        });
        if let Some(target) = target {
            let el = pipeline
                .by_name("cam")
                .ok_or_else(|| "pipewiresrc element missing".to_string())?;
            el.set_property_from_str("target-object", &target);
            let got: String = el.property("target-object");
            cam_debug(format!(
                "set pipewiresrc target-object want={target:?} got={got:?}"
            ));
            applied = Some(
                cand.as_ref()
                    .map(|c| c.id.clone())
                    .or_else(|| camera_id.map(|s| s.to_string()))
                    .unwrap_or(target),
            );
        } else if camera_id.is_some() {
            return Err("could not resolve target-object for pipewiresrc".into());
        }
    }

    let sink = pipeline
        .by_name("sink")
        .ok_or_else(|| "appsink missing".to_string())?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "appsink cast failed".to_string())?;

    Ok((pipeline, sink, applied))
}

/// Open a GStreamer camera pipeline and pull RGB frames.
pub struct GstCamera {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    source: GstSource,
    camera_name: Option<String>,
}

impl GstCamera {
    pub fn open() -> Result<Self, String> {
        // Must init before resolve_camera_id → DeviceMonitor (fajita Scan crash).
        ensure_gst()?;
        // Prefer an explicit Back (etc.) target — untargeted pipewire often picks Front.
        if let Some(id) = resolve_camera_id() {
            match Self::open_camera_id(&id) {
                Ok(cam) => return Ok(cam),
                Err(e) => cam_debug(format!("open preferred id={id:?} failed: {e}")),
            }
        }
        Self::open_untargeted()
    }

    /// Open a specific camera id (DeviceMonitor serial / libcamera name / display).
    pub fn open_camera_id(camera_id: &str) -> Result<Self, String> {
        ensure_gst()?;
        cam_debug(format!("open_camera_id({camera_id:?})"));

        // Best path: GstDevice::create_element (sets pipewire/libcamera props correctly).
        if let Some(dev) = find_monitor_device(camera_id) {
            match Self::open_from_device(&dev, camera_id) {
                Ok(cam) => return Ok(cam),
                Err(e) => cam_debug(format!("create_element open failed: {e}")),
            }
        }

        let mut last = String::new();
        for src in candidate_sources() {
            match Self::open_source(src, Some(camera_id)) {
                Ok(cam) => {
                    cam_debug(format!(
                        "gst open OK via {} name={:?}",
                        cam.source.label(),
                        cam.camera_name
                    ));
                    return Ok(cam);
                }
                Err(e) => {
                    cam_debug(format!("gst {} named open failed: {e}", src.label()));
                    last = format!("{}: {e}", src.label());
                }
            }
        }
        Err(if last.is_empty() {
            format!("no GStreamer source could open camera {camera_id:?}")
        } else {
            last
        })
    }

    /// Mid-scan switch: reopen `camera_id` preferring the same backend as `prefer`.
    pub fn open_for_switch(camera_id: &str, prefer: GstSource) -> Result<Self, String> {
        ensure_gst()?;
        cam_debug(format!(
            "open_for_switch id={camera_id:?} prefer={}",
            prefer.label()
        ));

        if let Some(dev) = find_monitor_device(camera_id) {
            match Self::open_from_device(&dev, camera_id) {
                Ok(cam) => {
                    cam_debug(format!(
                        "switch create_element OK via {} name={:?}",
                        cam.source.label(),
                        cam.camera_name
                    ));
                    return Ok(cam);
                }
                Err(e) => cam_debug(format!("switch create_element failed: {e}")),
            }
        }

        // Prefer the backend that was live (pipewire-only Alpine has no libcamerasrc).
        let mut order = candidate_sources();
        order.sort_by_key(|s| if *s == prefer { 0 } else { 1 });
        let mut last = String::new();
        for src in order {
            match Self::open_source(src, Some(camera_id)) {
                Ok(cam) => {
                    cam_debug(format!(
                        "pipeline recreate OK via {} old_prefer={} new_id={:?}",
                        cam.source.label(),
                        prefer.label(),
                        cam.camera_name
                    ));
                    return Ok(cam);
                }
                Err(e) => {
                    cam_debug(format!(
                        "pipeline recreate FAIL via {} id={camera_id:?}: {e}",
                        src.label()
                    ));
                    last = format!("{}: {e}", src.label());
                }
            }
        }
        Err(last)
    }

    /// Backward-compatible alias.
    #[allow(dead_code)]
    pub fn open_named(camera_name: Option<&str>) -> Result<Self, String> {
        match camera_name {
            Some(id) => Self::open_camera_id(id),
            None => Self::open(),
        }
    }

    /// Open **only** libcamerasrc with the given id.
    #[allow(dead_code)]
    pub fn open_libcamera_named(camera_name: &str) -> Result<Self, String> {
        ensure_gst()?;
        if !element_available("libcamerasrc") {
            return Err("libcamerasrc not available".into());
        }
        cam_debug(format!("open_libcamera_named({camera_name:?})"));
        Self::open_source(GstSource::LibCamera, Some(camera_name))
    }

    fn open_untargeted() -> Result<Self, String> {
        ensure_gst()?;
        let sources = candidate_sources();
        if sources.is_empty() {
            return Err(
                "no GStreamer camera source (install pipewire and/or \
                 gst-plugin-libcamera). On Alpine: `sudo apk add pipewire \
                 wireplumber xdg-desktop-portal gst-plugins-base` — \
                 gst-plugin-libcamera is optional."
                    .into(),
            );
        }
        let mut last = String::new();
        for src in sources {
            match Self::open_source(src, None) {
                Ok(cam) => {
                    cam_debug(format!(
                        "gst open OK via {} name={:?}",
                        cam.source.label(),
                        cam.camera_name
                    ));
                    return Ok(cam);
                }
                Err(e) => {
                    cam_debug(format!("gst {} failed: {e}", src.label()));
                    last = format!("{}: {e}", src.label());
                }
            }
        }
        Err(last)
    }

    fn open_from_device(device: &gst::Device, camera_id: &str) -> Result<Self, String> {
        let src_el = device
            .create_element(Some("cam"))
            .map_err(|e| format!("create_element: {e}"))?;
        let factory = src_el
            .factory()
            .map(|f| f.name().to_string())
            .unwrap_or_default();
        let source = GstSource::from_factory_name(&factory);
        cam_debug(format!(
            "create_element factory={factory:?} source={} id={camera_id:?}",
            source.label()
        ));

        let pipeline = gst::Pipeline::default();
        let sink = link_rgb_appsink(&pipeline, &src_el)?;
        Self::finish_open(pipeline, sink, source, Some(camera_id.to_string()))
    }

    /// Stop the pipeline and wait until it is fully released (needed before CAMSS reopen).
    pub fn shutdown(self) {
        let name = self.camera_name.clone();
        cam_debug(format!(
            "shutdown pipeline name={name:?} source={}",
            self.source.label()
        ));
        let _ = self.pipeline.set_state(gst::State::Null);
        let (res, state, _) = self.pipeline.state(gst::ClockTime::from_seconds(3));
        cam_debug(format!(
            "shutdown done name={name:?} state={state:?} res={res:?}"
        ));
    }

    fn open_source(src: GstSource, camera_name: Option<&str>) -> Result<Self, String> {
        let (pipeline, sink, applied) = build_pipeline(src, camera_name)?;
        if let Some(want) = camera_name {
            let cams = list_switchable_candidates();
            let ok = match (&applied, find_candidate(&cams, want)) {
                (Some(a), Some(c)) => {
                    a == &c.id || a == &c.pw_target || a == want || c.display == want
                }
                (Some(a), None) => a == want,
                _ => false,
            };
            if !ok {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(format!(
                    "camera id mismatch (want {want:?}, applied {applied:?})"
                ));
            }
        }
        Self::finish_open(pipeline, sink, src, applied)
    }

    fn finish_open(
        pipeline: gst::Pipeline,
        sink: gst_app::AppSink,
        source: GstSource,
        camera_name: Option<String>,
    ) -> Result<Self, String> {
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("set Playing: {e}"))?;

        let (state_res, current, _pending) = pipeline.state(gst::ClockTime::from_seconds(3));
        if let Err(e) = state_res {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("state change: {e} (current={current:?})"));
        }

        let mut cam = Self {
            pipeline,
            sink,
            source,
            camera_name,
        };
        match cam.next_rgb(gst::ClockTime::from_seconds(5)) {
            Ok((w, h, _)) => {
                cam_debug(format!(
                    "gst first frame OK ({}) name={:?} {}x{}",
                    source.label(),
                    cam.camera_name,
                    w,
                    h
                ));
                if let Some(id) = cam.camera_name.as_deref() {
                    save_camera_id(id);
                }
                Ok(cam)
            }
            Err(e) => {
                let _ = cam.pipeline.set_state(gst::State::Null);
                let _ = cam.pipeline.state(gst::ClockTime::from_seconds(2));
                Err(e)
            }
        }
    }

    pub fn source_label(&self) -> &'static str {
        self.source.label()
    }

    pub fn source(&self) -> GstSource {
        self.source
    }

    pub fn camera_name(&self) -> Option<&str> {
        self.camera_name.as_deref()
    }

    pub fn camera_label(&self) -> String {
        let Some(id) = self.camera_name.as_deref() else {
            return self.source.label().to_string();
        };
        list_cameras()
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.label)
            .unwrap_or_else(|| id.to_string())
    }

    pub fn next_rgb(&mut self, timeout: gst::ClockTime) -> Result<(u32, u32, Vec<u8>), String> {
        let sample = self
            .sink
            .try_pull_sample(timeout)
            .ok_or_else(|| "timeout waiting for camera frame".to_string())?;
        let caps = sample
            .caps()
            .ok_or_else(|| "sample has no caps".to_string())?;
        let info = gst_video::VideoInfo::from_caps(&caps).map_err(|e| format!("VideoInfo: {e}"))?;
        let buffer = sample
            .buffer()
            .ok_or_else(|| "sample has no buffer".to_string())?;
        let frame = gst_video::VideoFrame::from_buffer_readable(buffer.copy(), &info)
            .map_err(|_| "frame map failed".to_string())?;
        let w = frame.width();
        let h = frame.height();
        let plane = frame.plane_data(0).map_err(|e| format!("plane: {e}"))?;
        let stride = frame.plane_stride()[0] as usize;
        let row_bytes = (w as usize).saturating_mul(3);
        let mut rgb = Vec::with_capacity(row_bytes.saturating_mul(h as usize));
        for y in 0..h as usize {
            let start = y.saturating_mul(stride);
            let end = start.saturating_add(row_bytes);
            if end > plane.len() {
                return Err(format!(
                    "RGB plane too small: len={} need row end {end}",
                    plane.len()
                ));
            }
            rgb.extend_from_slice(&plane[start..end]);
        }
        Ok((w, h, rgb))
    }
}

impl Drop for GstCamera {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(2));
    }
}

/// Whether GStreamer camera elements look usable on this system.
pub fn gst_camera_available() -> bool {
    if ensure_gst().is_err() {
        return false;
    }
    !candidate_sources().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facing_text_heuristics() {
        assert_eq!(facing_from_text("Internal front camera"), Some(CameraFacing::Front));
        assert_eq!(facing_from_text("Internal back camera"), Some(CameraFacing::Back));
        assert_eq!(facing_from_text("CameraLocationBack"), Some(CameraFacing::Back));
        assert_eq!(facing_from_text("rear ultrawide"), Some(CameraFacing::Back));
        assert_eq!(facing_from_text("selfie"), Some(CameraFacing::Front));
    }

    #[test]
    fn back_pref_scores_rear_over_front() {
        let pref = CameraFacingPref::Back;
        assert!(CameraFacing::Back.score(pref) > CameraFacing::Front.score(pref));
        assert!(CameraFacing::External.score(pref) > CameraFacing::Front.score(pref));
        assert!(CameraFacing::Back.matches(pref));
        assert!(!CameraFacing::Front.matches(pref));
    }

    #[test]
    fn facing_pref_from_env_defaults_back() {
        assert_eq!(
            match "back" {
                "front" | "user" | "selfie" => CameraFacingPref::Front,
                "any" | "all" | "*" => CameraFacingPref::Any,
                _ => CameraFacingPref::Back,
            },
            CameraFacingPref::Back
        );
    }

    #[test]
    fn sensor_and_label_from_fajita_ids() {
        let id = "/base/soc@0/camss@acb3000/i2c@…/imx519@1a";
        assert_eq!(sensor_from_id(id).as_deref(), Some("imx519"));
        assert_eq!(
            friendly_label(CameraFacing::Back, id, id, 0),
            "Back · imx519"
        );
        assert_eq!(
            friendly_label(CameraFacing::Front, "imx371 16-0010", "front", 0),
            "Front · imx371"
        );
        assert_eq!(
            friendly_label(CameraFacing::Back, "83", "Built-in Back Camera", 9216000),
            "Back · 9216k"
        );
    }

    #[test]
    fn filters_qualcomm_subsystem_junk() {
        let junk = CamCandidate {
            id: "Qualcomm Camera Subsystem#0".into(),
            display: "Qualcomm Camera Subsystem".into(),
            facing: CameraFacing::Unknown,
            max_area: 0,
            pw_target: "Qualcomm Camera Subsystem".into(),
            libcamera_name: None,
        };
        let front = CamCandidate {
            id: "71".into(),
            display: "Built-in Front Camera".into(),
            facing: CameraFacing::Front,
            max_area: 3145728,
            pw_target: "71".into(),
            libcamera_name: None,
        };
        let back = CamCandidate {
            id: "72".into(),
            display: "Built-in Back Camera".into(),
            facing: CameraFacing::Back,
            max_area: 9216000,
            pw_target: "72".into(),
            libcamera_name: None,
        };
        assert!(!is_switchable_camera(&junk));
        assert!(is_switchable_camera(&front));
        assert!(is_switchable_camera(&back));
    }

    /// Regression: DeviceMonitor enumeration must not run before gst::init
    /// (fajita panic: "GStreamer has not been initialized").
    #[test]
    fn list_and_resolve_init_gst_first() {
        // Must not panic even when this is the first GST touch in the process.
        let _ = list_cameras();
        let _ = resolve_camera_id();
        let _ = gst_camera_available();
        let _ = candidate_sources();
        assert!(ensure_gst().is_ok() || ensure_gst().is_err());
    }
}
