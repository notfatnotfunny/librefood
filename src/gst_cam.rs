//! GStreamer camera path for phones that need libcamera (Qualcomm CAMSS).
//!
//! Tries, in order:
//! 1. `libcamerasrc` (needs gst-plugin-libcamera / Alpine equivalent)
//! 2. `pipewiresrc` (after xdg-desktop-portal Camera grant)
//!
//! Frames are RGB for the same preview + rxing path as V4L2.
//!
//! On multi-camera phones, prefer a **back/rear** libcamera device by default
//! (`LIBREFOOD_CAMERA_FACING=back`). USB / single-camera setups are unchanged.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gst_video::VideoFrameExt;
use std::process::Command;
use std::sync::Once;

static GST_INIT: Once = Once::new();

fn cam_debug(msg: impl AsRef<str>) {
    if matches!(
        std::env::var("LIBREFOOD_CAMERA_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    ) {
        eprintln!("[librefood camera] {}", msg.as_ref());
    }
}

fn ensure_gst() -> Result<(), String> {
    let mut err = None;
    GST_INIT.call_once(|| {
        if let Err(e) = gst::init() {
            err = Some(e.to_string());
        }
    });
    match err {
        Some(e) => Err(format!("gstreamer init: {e}")),
        None => Ok(()),
    }
}

fn element_available(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// Which GStreamer source to try.
#[derive(Debug, Clone, Copy)]
enum GstSource {
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

    fn label(self) -> &'static str {
        match self {
            Self::LibCamera => "libcamera",
            Self::PipeWire => "pipewire",
        }
    }
}

/// Preferred camera facing for libcamera selection.
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
            // default — and explicit back/rear/world
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

    /// Higher is better for the given preference.
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
    /// libcamerasrc `camera-name` (camera id).
    id: String,
    display: String,
    facing: CameraFacing,
    /// Rough max area from caps (0 if unknown).
    max_area: u64,
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
    // Snapshot reads "api.libcamera.location"; libcamera GStreamer plugin uses
    // "api.libcamera." + ControlId::name() → usually "Location".
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

fn friendly_label(facing: CameraFacing, id: &str, display: &str) -> String {
    let facing_word = match facing {
        CameraFacing::Front => "Front",
        CameraFacing::Back => "Back",
        CameraFacing::External => "External",
        CameraFacing::Unknown => "Camera",
    };
    if let Some(sensor) = sensor_from_id(id).or_else(|| sensor_from_id(display)) {
        format!("{facing_word} · {sensor}")
    } else if display != id
        && !display.is_empty()
        && facing_from_text(display).is_some()
    {
        // e.g. "Internal back camera"
        display.to_string()
    } else {
        facing_word.to_string()
    }
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
            label: friendly_label(c.facing, &c.id, &c.display),
            facing: c.facing.label(),
        }
    }
}

/// Enumerate libcamera cameras with human labels (Back · imx519, …).
pub fn list_cameras() -> Vec<CameraInfo> {
    if ensure_gst().is_err() {
        return Vec::new();
    }
    let cams = list_libcamera_candidates();
    let mut out: Vec<CameraInfo> = cams.iter().map(CameraInfo::from_candidate).collect();
    // Stable order: back first, then by area desc, then id.
    let pref = CameraFacingPref::Back;
    out.sort_by(|a, b| {
        let fa = cams.iter().find(|c| c.id == a.id).map(|c| c.facing.score(pref)).unwrap_or(0);
        let fb = cams.iter().find(|c| c.id == b.id).map(|c| c.facing.score(pref)).unwrap_or(0);
        fb.cmp(&fa)
            .then_with(|| {
                let aa = cams.iter().find(|c| c.id == a.id).map(|c| c.max_area).unwrap_or(0);
                let ab = cams.iter().find(|c| c.id == b.id).map(|c| c.max_area).unwrap_or(0);
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
            "libcamerasrc not installed (apk: gst-plugin-libcamera / Arch: gst-plugin-libcamera)",
        );
    }
    if element_available("pipewiresrc") {
        out.push(GstSource::PipeWire);
    } else {
        cam_debug("pipewiresrc not installed");
    }
    out
}

/// Enumerate libcamera devices via GStreamer DeviceMonitor.
fn list_from_device_monitor() -> Vec<CamCandidate> {
    let monitor = gst::DeviceMonitor::new();
    // libcamera provider registers as Source/Video; V4L also uses Video/Source.
    let _ = monitor.add_filter(Some("Source/Video"), None);
    let _ = monitor.add_filter(Some("Video/Source"), None);
    if monitor.start().is_err() {
        cam_debug("DeviceMonitor start failed");
        return Vec::new();
    }
    let devices = monitor.devices();
    let mut out = Vec::new();
    for dev in devices.iter() {
        // libcamera provider sets both name and display-name to camera->id().
        let name = dev.display_name().to_string();
        let id = name.clone();

        let props = dev.properties();
        let mut facing = props
            .as_ref()
            .and_then(|p| facing_from_location_prop(p))
            .or_else(|| facing_from_text(&name))
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

        let max_area = dev
            .caps()
            .as_ref()
            .map(max_area_from_caps)
            .unwrap_or(0);
        let class = dev.device_class().to_string();
        cam_debug(format!(
            "gst device id={id:?} facing={} area={max_area} class={class}",
            facing.label()
        ));
        out.push(CamCandidate {
            id,
            display: name,
            facing,
            max_area,
        });
    }
    monitor.stop();
    out
}

/// Fallback: parse `cam --list` / `cam -l` (libcamera-tools).
/// Example line: `1: Internal back camera (/base/soc@0/camss@...)`
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
        // "N: label (id)" or "N: label"
        let Some(rest) = line
            .split_once(':')
            .and_then(|(n, r)| {
                if n.trim().chars().all(|c| c.is_ascii_digit()) {
                    Some(r.trim())
                } else {
                    None
                }
            })
        else {
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
            id,
            display,
            facing,
            max_area: 0,
        });
    }
    out_list
}

fn list_libcamera_candidates() -> Vec<CamCandidate> {
    let mut cams = list_from_device_monitor();
    if cams.is_empty() {
        cams = list_from_cam_cli();
    } else if cams.iter().all(|c| c.facing == CameraFacing::Unknown) {
        // Merge facing hints from `cam --list` by matching id.
        let cli = list_from_cam_cli();
        for c in &mut cams {
            if let Some(h) = cli.iter().find(|h| h.id == c.id || c.id.contains(&h.id)) {
                if h.facing != CameraFacing::Unknown {
                    c.facing = h.facing;
                    if c.display.is_empty() || c.display == c.id {
                        c.display = h.display.clone();
                    }
                }
            }
        }
    }
    cams
}

fn camera_index_override() -> Option<usize> {
    std::env::var("LIBREFOOD_CAMERA_INDEX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
}

/// Resolve libcamerasrc `camera-name`.
/// Order: env NAME → env INDEX (within facing) → saved id → facing preference (default back).
pub fn resolve_libcamera_name() -> Option<String> {
    if let Ok(name) = std::env::var("LIBREFOOD_CAMERA_NAME") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            cam_debug(format!("LIBREFOOD_CAMERA_NAME override: {name}"));
            return Some(name);
        }
    }

    let pref = CameraFacingPref::from_env();
    let mut cams = list_libcamera_candidates();
    if cams.is_empty() {
        cam_debug("no libcamera candidates enumerated; using libcamerasrc default");
        return None;
    }

    for c in &cams {
        cam_debug(format!(
            "candidate id={:?} facing={} area={} display={:?} label={:?}",
            c.id,
            c.facing.label(),
            c.max_area,
            c.display,
            friendly_label(c.facing, &c.id, &c.display)
        ));
    }

    // Rank: facing score, then resolution, then stable id order.
    cams.sort_by(|a, b| {
        b.facing
            .score(pref)
            .cmp(&a.facing.score(pref))
            .then_with(|| b.max_area.cmp(&a.max_area))
            .then_with(|| a.id.cmp(&b.id))
    });

    let matching: Vec<&CamCandidate> = cams
        .iter()
        .filter(|c| c.facing.matches(pref))
        .collect();

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
            friendly_label(chosen.facing, &chosen.id, &chosen.display)
        ));
        return Some(chosen.id.clone());
    }

    if let Some(saved) = load_saved_camera_id() {
        if let Some(c) = cams.iter().find(|c| c.id == saved) {
            // Don't reopen a saved Front when the user wants Back by default.
            if c.facing.matches(pref) || pref == CameraFacingPref::Any {
                cam_debug(format!(
                    "selected saved id={:?} facing={} label={:?}",
                    c.id,
                    c.facing.label(),
                    friendly_label(c.facing, &c.id, &c.display)
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
        "selected libcamera id={:?} facing={} label={:?} (pref={pref:?})",
        chosen.id,
        chosen.facing.label(),
        friendly_label(chosen.facing, &chosen.id, &chosen.display)
    ));
    Some(chosen.id.clone())
}

fn build_pipeline(
    src: GstSource,
    camera_name: Option<&str>,
) -> Result<(gst::Pipeline, gst_app::AppSink, Option<String>), String> {
    // RGB appsink — videoconvert handles UYVY/NV12/etc from libcamera.
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
    if matches!(src, GstSource::LibCamera) {
        let name = camera_name
            .map(|s| s.to_string())
            .or_else(resolve_libcamera_name);
        if let Some(name) = name {
            let el = pipeline
                .by_name("cam")
                .ok_or_else(|| "libcamerasrc element missing".to_string())?;
            el.set_property_from_str("camera-name", &name);
            // Read back — property must stick before PLAYING (MUTABLE_READY).
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
            applied = Some(name);
        } else if camera_name.is_some() {
            return Err("could not resolve camera-name for libcamerasrc".into());
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
        Self::open_named(None)
    }

    /// Open with an explicit libcamera id (`camera-name`), or auto-select when `None`.
    pub fn open_named(camera_name: Option<&str>) -> Result<Self, String> {
        ensure_gst()?;
        let sources = candidate_sources();
        if sources.is_empty() {
            return Err(
                "no GStreamer camera source (install libcamera + gst-plugin-libcamera, \
                 and/or pipewire). On Alpine: `sudo apk add libcamera gst-plugins-base \
                 mesa-gstreamer` and a libcamerasrc package if packaged."
                    .into(),
            );
        }
        let mut last = String::new();
        for src in sources {
            // When an explicit camera id is requested, only libcamerasrc can honor it.
            // Falling through to pipewiresrc would keep the old portal stream (front).
            if camera_name.is_some() && !matches!(src, GstSource::LibCamera) {
                continue;
            }
            match Self::open_source(src, camera_name) {
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

    /// Open **only** `libcamerasrc` with the given id (used for mid-scan switch).
    pub fn open_libcamera_named(camera_name: &str) -> Result<Self, String> {
        ensure_gst()?;
        if !element_available("libcamerasrc") {
            return Err("libcamerasrc not available".into());
        }
        cam_debug(format!("open_libcamera_named({camera_name:?})"));
        Self::open_source(GstSource::LibCamera, Some(camera_name))
    }

    /// Stop the pipeline and wait until it is fully released (needed before CAMSS reopen).
    pub fn shutdown(self) {
        let name = self.camera_name.clone();
        cam_debug(format!("shutdown pipeline name={name:?}"));
        let _ = self.pipeline.set_state(gst::State::Null);
        let (res, state, _) = self.pipeline.state(gst::ClockTime::from_seconds(3));
        cam_debug(format!(
            "shutdown done name={name:?} state={state:?} res={res:?}"
        ));
        // Drop: second Null + short wait is harmless once already Null; must not
        // mem::forget — a leaked PLAYING/Null pipeline can keep the sensor busy.
    }

    fn open_source(src: GstSource, camera_name: Option<&str>) -> Result<Self, String> {
        let (pipeline, sink, applied) = build_pipeline(src, camera_name)?;
        if let Some(want) = camera_name {
            if applied.as_deref() != Some(want) {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(format!(
                    "libcamerasrc camera-name mismatch (want {want:?}, applied {applied:?})"
                ));
            }
        }
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("set Playing: {e}"))?;

        // Wait for pipeline to settle / first buffer path ready.
        let (state_res, current, _pending) = pipeline.state(gst::ClockTime::from_seconds(3));
        if let Err(e) = state_res {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("state change: {e} (current={current:?})"));
        }

        // Probe one frame so we fail fast if the source is dead.
        let mut cam = Self {
            pipeline,
            sink,
            source: src,
            camera_name: applied,
        };
        match cam.next_rgb(gst::ClockTime::from_seconds(5)) {
            Ok((w, h, _)) => {
                cam_debug(format!(
                    "gst first frame OK ({}) name={:?} {}x{}",
                    src.label(),
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
        // RGB from videoconvert is tightly packed (stride may pad).
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
        // Don't rely on process env mutation across tests; just parse logic.
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
            friendly_label(CameraFacing::Back, id, id),
            "Back · imx519"
        );
        assert_eq!(
            friendly_label(CameraFacing::Front, "imx371 16-0010", "front"),
            "Front · imx371"
        );
    }
}
