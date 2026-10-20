//! Main Relm4 + libadwaita window: adaptive barcode → Librefood score details.

use crate::camera::{self, CameraError};
use crate::off::{self, LookupError, ProductInfo};
use crate::score::FactorStatus;
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
    product_image: Option<gdk::Texture>,
    camera_ok: bool,
    scanning: bool,
    preview: Option<gdk::Texture>,
    scan_stop: Option<Arc<AtomicBool>>,
    width_class: UiWidth,
}

impl AppModel {
    fn product_name(&self) -> String {
        self.product
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    fn product_brand(&self) -> String {
        self.product
            .as_ref()
            .and_then(|p| p.brand_primary.clone().or_else(|| p.brands.clone()))
            .unwrap_or_default()
    }

    fn score_text(&self) -> String {
        match &self.product {
            Some(p) => format!("{}/100", p.score.score),
            None => "—/100".into(),
        }
    }

    fn score_label_text(&self) -> String {
        self.product
            .as_ref()
            .map(|p| p.score.label.to_string())
            .unwrap_or_default()
    }

    fn score_dot_classes(&self) -> [&'static str; 2] {
        match &self.product {
            Some(p) => match p.score.status {
                FactorStatus::Good => ["score-dot", "score-dot-good"],
                FactorStatus::Mediocre => ["score-dot", "score-dot-mediocre"],
                FactorStatus::Poor => ["score-dot", "score-dot-poor"],
            },
            None => ["score-dot", "score-dot-poor"],
        }
    }

    fn nutri_caption(&self) -> String {
        let Some(p) = &self.product else {
            return String::new();
        };
        let mut parts = Vec::new();
        if let Some(g) = p.nutriscore_grade {
            parts.push(format!("Nutri-Score {}", g.to_ascii_uppercase()));
        }
        parts.push(format!("Barcode {}", p.barcode));
        if let Some(q) = &p.quantity {
            parts.push(q.clone());
        }
        parts.join(" · ")
    }

    fn has_negatives(&self) -> bool {
        self.product
            .as_ref()
            .map(|p| !p.score.negatives.is_empty())
            .unwrap_or(false)
    }

    fn has_positives(&self) -> bool {
        self.product
            .as_ref()
            .map(|p| !p.score.positives.is_empty())
            .unwrap_or(false)
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

    fn header_orientation(&self) -> gtk::Orientation {
        match self.width_class {
            UiWidth::Compact => gtk::Orientation::Vertical,
            UiWidth::Medium | UiWidth::Wide => gtk::Orientation::Horizontal,
        }
    }

    fn factors_orientation(&self) -> gtk::Orientation {
        match self.width_class {
            UiWidth::Wide => gtk::Orientation::Horizontal,
            _ => gtk::Orientation::Vertical,
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

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn append_factor_rows(container: &gtk::Box, rows: &[crate::score::FactorRow]) {
    clear_box(container);
    for row in rows {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.set_hexpand(true);
        line.add_css_class("factor-row");

        let text_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_col.set_hexpand(true);
        text_col.set_halign(gtk::Align::Start);

        let title = gtk::Label::new(Some(row.title));
        title.add_css_class("factor-title");
        title.set_halign(gtk::Align::Start);
        title.set_xalign(0.0);

        let detail = gtk::Label::new(Some(&row.detail));
        detail.add_css_class("factor-detail");
        detail.set_halign(gtk::Align::Start);
        detail.set_wrap(true);
        detail.set_xalign(0.0);

        text_col.append(&title);
        text_col.append(&detail);

        let value = gtk::Label::new(Some(&row.value_label));
        value.add_css_class("factor-value");
        value.set_valign(gtk::Align::Center);

        let dot = gtk::Label::new(Some("●"));
        dot.set_valign(gtk::Align::Center);
        let (c1, c2) = match row.status {
            FactorStatus::Good => ("status-dot", "dot-good"),
            FactorStatus::Mediocre => ("status-dot", "dot-mediocre"),
            FactorStatus::Poor => ("status-dot", "dot-poor"),
        };
        dot.set_css_classes(&[c1, c2]);

        line.append(&text_col);
        line.append(&value);
        line.append(&dot);
        container.append(&line);
    }
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = Option<String>;
    type Input = Msg;
    type Output = ();
    type CommandOutput = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("Librefood"),
            set_default_width: 480,
            set_default_height: 720,
            set_width_request: 360,

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                adw::HeaderBar {
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Librefood",
                        set_subtitle: "Product score via Open Food Facts",
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
                            set_label: "Scan or enter a barcode for a detailed score from Open Food Facts data.",
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

                        // —— Score detail panel (Yuka-like layout) ——
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 16,
                            #[watch]
                            set_visible: model.product.is_some(),
                            add_css_class: "result-panel",

                            gtk::Box {
                                #[watch]
                                set_orientation: model.header_orientation(),
                                set_spacing: 16,
                                set_hexpand: true,

                                gtk::Picture {
                                    set_content_fit: gtk::ContentFit::Cover,
                                    set_can_shrink: false,
                                    set_width_request: 96,
                                    set_height_request: 96,
                                    set_halign: gtk::Align::Start,
                                    set_valign: gtk::Align::Start,
                                    set_hexpand: false,
                                    set_vexpand: false,
                                    #[watch]
                                    set_visible: model.product_image.is_some(),
                                    #[watch]
                                    set_paintable: model.product_image.as_ref(),
                                    add_css_class: "product-image",
                                },

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 4,
                                    set_hexpand: true,
                                    set_valign: gtk::Align::Center,

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
                                        set_label: &model.product_brand(),
                                        add_css_class: "product-brand",
                                        set_halign: gtk::Align::Start,
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        #[watch]
                                        set_visible: !model.product_brand().is_empty(),
                                    },

                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Horizontal,
                                        set_spacing: 10,
                                        set_margin_top: 8,
                                        set_halign: gtk::Align::Start,

                                        gtk::Label {
                                            set_label: "●",
                                            #[watch]
                                            set_css_classes: &model.score_dot_classes(),
                                            set_valign: gtk::Align::Center,
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_spacing: 0,

                                            gtk::Label {
                                                #[watch]
                                                set_label: &model.score_text(),
                                                add_css_class: "score-number",
                                                set_halign: gtk::Align::Start,
                                                set_xalign: 0.0,
                                            },
                                            gtk::Label {
                                                #[watch]
                                                set_label: &model.score_label_text(),
                                                add_css_class: "score-label",
                                                set_halign: gtk::Align::Start,
                                                set_xalign: 0.0,
                                            },
                                        },
                                    },

                                    gtk::Label {
                                        #[watch]
                                        set_label: &model.nutri_caption(),
                                        add_css_class: "dim-label",
                                        set_halign: gtk::Align::Start,
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        set_margin_top: 4,
                                        set_selectable: true,
                                    },
                                },
                            },

                            gtk::Box {
                                #[watch]
                                set_orientation: model.factors_orientation(),
                                set_spacing: 18,
                                set_hexpand: true,

                                // Negatives
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 0,
                                    set_hexpand: true,
                                    #[watch]
                                    set_visible: model.has_negatives(),

                                    gtk::Label {
                                        set_label: "Negatives",
                                        add_css_class: "section-title",
                                        set_halign: gtk::Align::Start,
                                        set_xalign: 0.0,
                                        set_margin_bottom: 4,
                                    },

                                    #[name = "negatives_list"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 0,
                                        set_hexpand: true,
                                    },
                                },

                                // Positives
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 0,
                                    set_hexpand: true,
                                    #[watch]
                                    set_visible: model.has_positives(),

                                    gtk::Label {
                                        set_label: "Positives",
                                        add_css_class: "section-title",
                                        set_halign: gtk::Align::Start,
                                        set_xalign: 0.0,
                                        set_margin_bottom: 4,
                                    },

                                    #[name = "positives_list"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 0,
                                        set_hexpand: true,
                                    },
                                },
                            },
                        },

                        gtk::Label {
                            set_label: "Scores are Librefood heuristics from Open Food Facts data (not Yuka). Nutri-Score A–E shown when published.",
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
            product_image: None,
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

        {
            let sender_w = sender.clone();
            let win = root.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                sender_w.input(Msg::WidthChanged(win.width()));
                gtk::glib::ControlFlow::Continue
            });
        }

        if initial.is_some() {
            let sender_l = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                sender_l.input(Msg::Lookup);
            });
        }

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            Msg::BarcodeChanged(text) => {
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
                    self.product_image = None;
                    clear_box(&widgets.negatives_list);
                    clear_box(&widgets.positives_list);
                } else {
                    self.barcode = code.clone();
                    self.status_kind = StatusKind::Loading;
                    self.status_text = format!("Looking up {code}…");
                    self.product = None;
                    self.product_image = None;
                    clear_box(&widgets.negatives_list);
                    clear_box(&widgets.positives_list);

                    let sender2 = sender.clone();
                    thread::spawn(move || {
                        let result = off::lookup_product(&code).map_err(format_lookup_err);
                        sender2.input(Msg::LookupDone(result));
                    });
                }
            }
            Msg::LookupDone(result) => match result {
                Ok(info) => {
                    self.status_kind = StatusKind::Success;
                    self.status_text = format!(
                        "Product found — Librefood score {}/100 ({}).",
                        info.score.score, info.score.label
                    );
                    self.product_image = texture_from_bytes(info.image_bytes.as_deref());
                    append_factor_rows(&widgets.negatives_list, &info.score.negatives);
                    append_factor_rows(&widgets.positives_list, &info.score.positives);
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
                    self.product_image = None;
                    clear_box(&widgets.negatives_list);
                    clear_box(&widgets.positives_list);
                }
            },
            Msg::ScanCamera => {
                if self.scanning {
                    // no-op
                } else if !self.camera_ok {
                    self.status_kind = StatusKind::Error;
                    self.status_text =
                        "No camera detected. Type or paste a barcode instead.".into();
                } else {
                    let stop = Arc::new(AtomicBool::new(false));
                    self.scan_stop = Some(Arc::clone(&stop));
                    self.scanning = true;
                    self.preview = None;
                    self.status_kind = StatusKind::Loading;
                    self.status_text =
                        "Live preview on — center the barcode in the guide, then hold steady."
                            .into();
                    self.product = None;
                    self.product_image = None;
                    clear_box(&widgets.negatives_list);
                    clear_box(&widgets.positives_list);

                    let sender2 = sender.clone();
                    thread::spawn(move || {
                        let result =
                            camera::scan_barcode_live(300, Arc::clone(&stop), |w, h, rgb| {
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
            }
            Msg::PreviewFrame { width, height, rgb } => {
                if self.scanning {
                    let stride = (width as usize).saturating_mul(3);
                    let needed = stride.saturating_mul(height as usize);
                    if width > 0 && height > 0 && stride > 0 && rgb.len() >= needed {
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
                }
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
        self.update_view(widgets, sender);
    }
}

fn texture_from_bytes(bytes: Option<&[u8]>) -> Option<gdk::Texture> {
    let bytes = bytes?;
    let gbytes = gtk::glib::Bytes::from_owned(bytes.to_vec());
    gdk::Texture::from_bytes(&gbytes).ok()
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
            font-size: 1.2rem;
            font-weight: 700;
        }
        .product-brand {
            font-size: 0.95rem;
            opacity: 0.65;
        }
        .product-image {
            border-radius: 10px;
            min-width: 96px;
            min-height: 96px;
            background: alpha(@card_fg_color, 0.06);
        }
        .score-number {
            font-size: 1.75rem;
            font-weight: 800;
            letter-spacing: -0.02em;
        }
        .score-label {
            font-size: 0.95rem;
            opacity: 0.7;
        }
        .score-dot {
            font-size: 1.6rem;
            line-height: 1;
        }
        .score-dot-good { color: #2e7d32; }
        .score-dot-mediocre { color: #ef6c00; }
        .score-dot-poor { color: #c62828; }
        .section-title {
            font-size: 1.05rem;
            font-weight: 700;
            margin-top: 4px;
        }
        .factor-row {
            padding: 10px 0;
            border-bottom: 1px solid alpha(@card_fg_color, 0.1);
        }
        .factor-title {
            font-weight: 650;
        }
        .factor-detail {
            font-size: 0.88rem;
            opacity: 0.65;
        }
        .factor-value {
            font-size: 0.92rem;
            opacity: 0.7;
            min-width: 4.5em;
        }
        .status-dot {
            font-size: 0.85rem;
            min-width: 1.1em;
        }
        .dot-good { color: #2e7d32; }
        .dot-mediocre { color: #ef6c00; }
        .dot-poor { color: #c62828; }
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
