//! GStreamer camera path for phones that need libcamera (Qualcomm CAMSS).
//!
//! Tries, in order:
//! 1. `libcamerasrc` (needs gst-plugin-libcamera / Alpine equivalent)
//! 2. `pipewiresrc` (after xdg-desktop-portal Camera grant)
//!
//! Frames are RGB for the same preview + rxing path as V4L2.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gst_video::VideoFrameExt;
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
        cam_debug("libcamerasrc not installed (apk: gst-plugin-libcamera / Arch: gst-plugin-libcamera)");
    }
    if element_available("pipewiresrc") {
        out.push(GstSource::PipeWire);
    } else {
        cam_debug("pipewiresrc not installed");
    }
    out
}

fn build_pipeline(src: GstSource) -> Result<(gst::Pipeline, gst_app::AppSink), String> {
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

    let sink = pipeline
        .by_name("sink")
        .ok_or_else(|| "appsink missing".to_string())?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "appsink cast failed".to_string())?;

    Ok((pipeline, sink))
}

/// Open a GStreamer camera pipeline and pull RGB frames.
pub struct GstCamera {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    source: GstSource,
}

impl GstCamera {
    pub fn open() -> Result<Self, String> {
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
            match Self::open_source(src) {
                Ok(cam) => {
                    cam_debug(format!("gst open OK via {}", src.label()));
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

    fn open_source(src: GstSource) -> Result<Self, String> {
        let (pipeline, sink) = build_pipeline(src)?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("set Playing: {e}"))?;

        // Wait for pipeline to settle / first buffer path ready.
        let (state_res, current, _pending) =
            pipeline.state(gst::ClockTime::from_seconds(3));
        if let Err(e) = state_res {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("state change: {e} (current={current:?})"));
        }

        // Probe one frame so we fail fast if the source is dead.
        let mut cam = Self {
            pipeline,
            sink,
            source: src,
        };
        match cam.next_rgb(gst::ClockTime::from_seconds(5)) {
            Ok(_) => {
                cam_debug(format!("gst first frame OK ({})", src.label()));
                Ok(cam)
            }
            Err(e) => {
                let _ = cam.pipeline.set_state(gst::State::Null);
                Err(e)
            }
        }
    }

    pub fn source_label(&self) -> &'static str {
        self.source.label()
    }

    pub fn next_rgb(&mut self, timeout: gst::ClockTime) -> Result<(u32, u32, Vec<u8>), String> {
        let sample = self
            .sink
            .try_pull_sample(timeout)
            .ok_or_else(|| "timeout waiting for camera frame".to_string())?;
        let caps = sample
            .caps()
            .ok_or_else(|| "sample has no caps".to_string())?;
        let info = gst_video::VideoInfo::from_caps(&caps)
            .map_err(|e| format!("VideoInfo: {e}"))?;
        let buffer = sample
            .buffer()
            .ok_or_else(|| "sample has no buffer".to_string())?;
        let frame = gst_video::VideoFrame::from_buffer_readable(buffer.copy(), &info)
            .map_err(|_| "frame map failed".to_string())?;
        let w = frame.width();
        let h = frame.height();
        let plane = frame
            .plane_data(0)
            .map_err(|e| format!("plane: {e}"))?;
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
    }
}

/// Whether GStreamer camera elements look usable on this system.
pub fn gst_camera_available() -> bool {
    if ensure_gst().is_err() {
        return false;
    }
    !candidate_sources().is_empty()
}
