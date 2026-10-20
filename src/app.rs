//! Main Relm4 + libadwaita window: adaptive barcode → Nutri-Score lookup.

use crate::camera::{self, CameraError};
use crate::off::{self, LookupError, ProductInfo};
use adw::prelude::AdwApplicationWindowExt;
use gtk::gdk;
use gtk::prelude::*;
use relm4::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Debug)]
pub enum Msg {
    BarcodeChanged(String),
    Lookup,
    LookupDone(Result<ProductInfo, String>),
    ScanCamera,
    PreviewFrame {
        width: u32,
        height: u32,
        rgb: Vec<u8>,
    },
    ScanDone(Result<String, String>),
    CancelScan,
    WidthChanged(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiWidth {
    Compact,
    Medium,
    Wide,
}

impl UiWidth {
    fn from_px(w: i32) -> Self {
        if w < 520 {
            Self::Compact
        } else if w < 900 {
            Self::Medium
        } else {
            Self::Wide
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Idle,
    Loading,
    Success,
    NotFound,
    Network,
    Error,
}

pub struct AppModel {
    barcode: String,
    status_kind: StatusKind,
    status_text: String,
    product: Option<ProductInfo>,
    camera_ok: bool,
    scanning: bool,
    preview: Option<gdk::Texture>,
    scan_stop: Option<Arc<AtomicBool>>,
    width_class: UiWidth,
}

impl AppModel {
    fn grade_label(&self) -> String {
        match &self.product {
            Some(p) => match p.nutriscore_grade {
                Some(g) => format!("Nutri-Score {}", g.to_ascii_uppercase()),
                None => "No Nutri-Score".into(),
            },
            None => "—".into(),
        }
    }

    fn grade_css(&self) -> &'static str {
        match &self.product {
            Some(p) => match p.nutriscore_grade {
                Some('a') | Some('A') => "nutri-a",
                Some('b') | Some('B') => "nutri-b",
                Some('c') | Some('C') => "nutri-c",
                Some('d') | Some('D') => "nutri-d",
                Some('e') | Some('E') => "nutri-e",
                _ => "nutri-none",
            },
            None => "nutri-none",
        }
    }

    fn product_name(&self) -> String {
        self.product
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    fn product_meta(&self) -> String {
        let Some(p) = &self.product else {
            return String::new();
        };
        let mut meta = format!("Barcode {}", p.barcode);
        if let Some(b) = &p.brands {
            meta.push_str(" · ");
            meta.push_str(b);
        }
        if let Some(q) = &p.quantity {
            meta.push_str(" · ");
            meta.push_str(q);
        }
        meta
    }

    fn status_css(&self) -> &'static str {
        match self.status_kind {
            StatusKind::Idle => "status-idle",
            StatusKind::Loading => "status-loading",
            StatusKind::Success => "status-ok",
            StatusKind::NotFound => "status-warn",
            StatusKind::Network | StatusKind::Error => "status-err",
        }
    }

    fn input_orientation(&self) -> gtk::Orientation {
        match self.width_class {
            UiWidth::Compact => gtk::Orientation::Vertical,
            UiWidth::Medium | UiWidth::Wide => gtk::Orientation::Horizontal,
        }
    }

    fn scan_label(&self) -> &'static str {
        if self.camera_ok {
            "Scan with camera"
        } else {
            "Camera unavailable — type a barcode"
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for AppModel {
    type Init = Option<String>;
    type Input = Msg;
    type Output = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("Librefood"),
            set_default_width: 480,
            set_default_height: 640,
            set_width_request: 360,

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                adw::HeaderBar {
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Librefood",
                        set_subtitle: "Nutri-Score via Open Food Facts",
                    },
                },

                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hexpand: true,
                    set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 18,
                        set_margin_top: 20,
                        set_margin_bottom: 28,
                        set_margin_start: 20,
                        set_margin_end: 20,
                        set_halign: gtk::Align::Fill,
                        set_valign: gtk::Align::Start,

                        gtk::Label {
                            set_label: "Librefood",
                            add_css_class: "brand-title",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                        gtk::Label {
                            set_label: "Scan or enter a barcode to see its Nutri-Score from Open Food Facts.",
                            add_css_class: "dim-label",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                            set_max_width_chars: 48,
                        },

                        #[name = "input_row"]
                        gtk::Box {
                            #[watch]
                            set_orientation: model.input_orientation(),
                            set_spacing: 10,

                            #[name = "barcode_entry"]
                            gtk::Entry {
                                set_hexpand: true,
                                set_placeholder_text: Some("EAN-13 barcode…"),
                                set_input_purpose: gtk::InputPurpose::Digits,
                                set_activates_default: true,
                                // Entry text is owned by the widget; model tracks via changed.
                                // (Do not #[watch] set_text — it fights typing/paste.)
                                connect_changed[sender] => move |entry| {
                                    sender.input(Msg::BarcodeChanged(entry.text().to_string()));
                                },
                                connect_activate[sender] => move |_| {
                                    sender.input(Msg::Lookup);
                                },
                            },

                            gtk::Button {
                                set_label: "Look up",
                                add_css_class: "suggested-action",
                                set_valign: gtk::Align::Center,
                                #[watch]
                                set_sensitive: !matches!(model.status_kind, StatusKind::Loading) && !model.scanning,
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::Lookup);
                                },
                            },
                        },

                        gtk::Button {
                            #[watch]
                            set_label: model.scan_label(),
                            set_hexpand: true,
                            #[watch]
                            set_sensitive: model.camera_ok && !model.scanning,
                            #[watch]
                            set_visible: !model.scanning,
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::ScanCamera);
                            },
                        },

                        // Live camera preview with centering guide.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 10,
                            #[watch]
                            set_visible: model.scanning,
                            add_css_class: "scan-panel",

                            gtk::Label {
                                set_label: "Center the barcode inside the guide",
                                add_css_class: "scan-hint",
                                set_halign: gtk::Align::Center,
                                set_wrap: true,
                            },

                            gtk::Overlay {
                                set_hexpand: true,
                                set_height_request: 280,
                                add_css_class: "scan-preview",

                                #[wrap(Some)]
                                set_child = &gtk::Picture {
                                    set_content_fit: gtk::ContentFit::Contain,
                                    set_can_shrink: true,
                                    set_hexpand: true,
                                    set_vexpand: true,
                                    #[watch]
                                    set_paintable: model.preview.as_ref(),
                                },

                                add_overlay = &gtk::Box {
                                    set_halign: gtk::Align::Center,
                                    set_valign: gtk::Align::Center,
                                    set_can_target: false,
                                    set_width_request: 240,
                                    set_height_request: 110,
                                    add_css_class: "scan-guide",
                                },
                            },

                            gtk::Button {
                                set_label: "Cancel scan",
                                add_css_class: "destructive-action",
                                set_hexpand: true,
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::CancelScan);
                                },
                            },
                        },

                        gtk::Label {
                            #[watch]
                            set_label: &model.status_text,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                            #[watch]
                            set_css_classes: &[model.status_css()],
                        },

                        gtk::Spinner {
                            set_halign: gtk::Align::Start,
                            #[watch]
                            set_spinning: matches!(model.status_kind, StatusKind::Loading) || model.scanning,
                            #[watch]
                            set_visible: matches!(model.status_kind, StatusKind::Loading) || model.scanning,
                        },

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            #[watch]
                            set_visible: model.product.is_some(),
                            add_css_class: "result-panel",

                            gtk::Label {
                                #[watch]
                                set_label: &model.grade_label(),
                                #[watch]
                                set_css_classes: &["nutri-badge", model.grade_css()],
                                set_halign: gtk::Align::Start,
                                set_valign: gtk::Align::Center,
                            },

                            gtk::Label {
                                #[watch]
                                set_label: &model.product_name(),
                                add_css_class: "product-name",
                                set_halign: gtk::Align::Start,
                                set_wrap: true,
                                set_xalign: 0.0,
                                set_selectable: true,
                            },

                            gtk::Label {
                                #[watch]
                                set_label: &model.product_meta(),
                                add_css_class: "dim-label",
                                set_halign: gtk::Align::Start,
                                set_wrap: true,
                                set_xalign: 0.0,
                                set_selectable: true,
                            },
                        },

                        gtk::Label {
                            set_label: "Data from Open Food Facts (openfoodfacts.org). Nutri-Score grades A–E when available.",
                            add_css_class: "caption",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                            set_margin_top: 8,
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let initial = init
            .as_deref()
            .map(off::normalize_barcode)
            .filter(|s| !s.is_empty());

        let model = AppModel {
            barcode: initial.clone().unwrap_or_default(),
            status_kind: StatusKind::Idle,
            status_text: "Enter a barcode or use the camera.".into(),
            product: None,
            camera_ok: camera::camera_available(),
            scanning: false,
            preview: None,
            scan_stop: None,
            width_class: UiWidth::Compact,
        };

        let widgets = view_output!();
        load_css();

        if let Some(code) = &initial {
            widgets.barcode_entry.set_text(code);
        }

        // Periodic width poll so layout adapts while resizing.
        {
            let sender_w = sender.clone();
            let win = root.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                sender_w.input(Msg::WidthChanged(win.width()));
                gtk::glib::ControlFlow::Continue
            });
        }

        // Auto-lookup when launched with --barcode.
        if initial.is_some() {
            let sender_l = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                sender_l.input(Msg::Lookup);
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            Msg::BarcodeChanged(text) => {
                // Avoid feedback loop from #[watch] set_text.
                if text != self.barcode {
                    self.barcode = text;
                }
            }
            Msg::Lookup => {
                let code = off::normalize_barcode(&self.barcode);
                if code.is_empty() {
                    self.status_kind = StatusKind::Error;
                    self.status_text = "Enter a barcode (digits only, e.g. EAN-13).".into();
                    self.product = None;
                    return;
                }
                self.barcode = code.clone();
                self.status_kind = StatusKind::Loading;
                self.status_text = format!("Looking up {code}…");
                self.product = None;

                let sender2 = sender.clone();
                thread::spawn(move || {
                    let result = off::lookup_product(&code).map_err(format_lookup_err);
                    sender2.input(Msg::LookupDone(result));
                });
            }
            Msg::LookupDone(result) => match result {
                Ok(info) => {
                    self.status_kind = StatusKind::Success;
                    self.status_text = if info.nutriscore_grade.is_some() {
                        "Product found.".into()
                    } else {
                        "Product found, but no Nutri-Score is published for this item.".into()
                    };
                    self.product = Some(info);
                }
                Err(msg) => {
                    let lower = msg.to_lowercase();
                    if lower.contains("not found") {
                        self.status_kind = StatusKind::NotFound;
                    } else if lower.contains("network")
                        || lower.contains("http")
                        || lower.contains("timed")
                    {
                        self.status_kind = StatusKind::Network;
                    } else {
                        self.status_kind = StatusKind::Error;
                    }
                    self.status_text = msg;
                    self.product = None;
                }
            },
            Msg::ScanCamera => {
                if self.scanning {
                    return;
                }
                if !self.camera_ok {
                    self.status_kind = StatusKind::Error;
                    self.status_text =
                        "No camera detected. Type or paste a barcode instead.".into();
                    return;
                }
                let stop = Arc::new(AtomicBool::new(false));
                self.scan_stop = Some(Arc::clone(&stop));
                self.scanning = true;
                self.preview = None;
                self.status_kind = StatusKind::Loading;
                self.status_text =
                    "Live preview on — center the barcode in the guide, then hold steady.".into();
                self.product = None;

                let sender2 = sender.clone();
                thread::spawn(move || {
                    let result = camera::scan_barcode_live(300, Arc::clone(&stop), |w, h, rgb| {
                        if !stop.load(Ordering::Relaxed) {
                            sender2.input(Msg::PreviewFrame {
                                width: w,
                                height: h,
                                rgb,
                            });
                        }
                    })
                    .map_err(format_camera_err);
                    sender2.input(Msg::ScanDone(result));
                });
            }
            Msg::PreviewFrame { width, height, rgb } => {
                if !self.scanning {
                    return;
                }
                let stride = (width as usize).saturating_mul(3);
                let needed = stride.saturating_mul(height as usize);
                if width == 0 || height == 0 || stride == 0 || rgb.len() < needed {
                    return;
                }
                let bytes = gtk::glib::Bytes::from_owned(rgb);
                let texture = gdk::MemoryTexture::new(
                    width as i32,
                    height as i32,
                    gdk::MemoryFormat::R8g8b8,
                    &bytes,
                    stride,
                );
                self.preview = Some(texture.upcast());
            }
            Msg::CancelScan => {
                if let Some(stop) = &self.scan_stop {
                    stop.store(true, Ordering::SeqCst);
                }
                self.status_text = "Cancelling scan…".into();
            }
            Msg::ScanDone(result) => {
                self.scanning = false;
                self.preview = None;
                self.scan_stop = None;
                match result {
                    Ok(code) => {
                        self.barcode = off::normalize_barcode(&code);
                        self.status_kind = StatusKind::Loading;
                        self.status_text = format!("Scanned {}. Looking up…", self.barcode);
                        let code2 = self.barcode.clone();
                        let sender2 = sender.clone();
                        thread::spawn(move || {
                            let result = off::lookup_product(&code2).map_err(format_lookup_err);
                            sender2.input(Msg::LookupDone(result));
                        });
                    }
                    Err(msg) => {
                        self.status_kind = if msg.to_lowercase().contains("cancel") {
                            StatusKind::Idle
                        } else {
                            StatusKind::Error
                        };
                        self.status_text = msg;
                    }
                }
            }
            Msg::WidthChanged(w) => {
                let next = UiWidth::from_px(w);
                if next != self.width_class {
                    self.width_class = next;
                }
            }
        }
    }
}

fn format_lookup_err(e: LookupError) -> String {
    match e {
        LookupError::NotFound => {
            "Product not found in Open Food Facts. Check the barcode or try another item.".into()
        }
        LookupError::Network(msg) => {
            format!("Network error — check your connection. ({msg})")
        }
        LookupError::BadResponse => "Unexpected response from Open Food Facts.".into(),
        LookupError::EmptyBarcode => "Enter a barcode (digits only).".into(),
    }
}

fn format_camera_err(e: CameraError) -> String {
    match e {
        CameraError::NoCamera => "No camera found. Type or paste a barcode instead.".into(),
        CameraError::Open(msg) => {
            format!("Could not open camera ({msg}). Type a barcode instead.")
        }
        CameraError::Capture(msg) => {
            format!("Camera capture failed ({msg}). Try again or type a barcode.")
        }
        CameraError::NoBarcode => {
            "No barcode detected. Center the code in the guide under good light, or type it in."
                .into()
        }
        CameraError::Cancelled => "Scan cancelled.".into(),
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
        .brand-title {
            font-size: 1.85rem;
            font-weight: 800;
            letter-spacing: -0.02em;
            color: #1a5c3a;
        }
        .product-name {
            font-size: 1.25rem;
            font-weight: 600;
        }
        .dim-label {
            opacity: 0.75;
        }
        .caption {
            opacity: 0.55;
            font-size: 0.85rem;
        }
        .result-panel {
            padding: 16px;
            border-radius: 12px;
            background: alpha(@card_fg_color, 0.04);
            border: 1px solid alpha(@card_fg_color, 0.08);
        }
        .scan-panel {
            padding: 12px;
            border-radius: 12px;
            background: alpha(@card_fg_color, 0.03);
            border: 1px solid alpha(@card_fg_color, 0.08);
        }
        .scan-hint {
            font-weight: 600;
            opacity: 0.9;
        }
        .scan-preview {
            background: #121212;
            border-radius: 10px;
            min-height: 240px;
        }
        .scan-guide {
            border: 2px dashed alpha(#ffffff, 0.75);
            border-radius: 8px;
            background: transparent;
        }
        .nutri-badge {
            font-size: 1.35rem;
            font-weight: 800;
            padding: 10px 16px;
            border-radius: 10px;
            min-width: 140px;
        }
        .nutri-a { background: #038141; color: white; }
        .nutri-b { background: #85bb2f; color: white; }
        .nutri-c { background: #fecb02; color: #222; }
        .nutri-d { background: #ee8100; color: white; }
        .nutri-e { background: #e63e11; color: white; }
        .nutri-none { background: alpha(@card_fg_color, 0.12); }
        .status-ok { color: #1a5c3a; }
        .status-warn { color: #9a5b00; }
        .status-err { color: #a40000; }
        .status-loading { opacity: 0.85; }
        .status-idle { opacity: 0.8; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
