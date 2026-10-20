//! Main Relm4 + libadwaita window: barcode / search → configurable Librefood score.

use crate::camera::{self, CameraError};
use crate::engine::{self, ScoreResult};
use crate::library::{FavoritesStore, HistoryStore, LibraryEntry};
use crate::logo;
use crate::off::{self, LookupError};
use crate::product::ProductModel;
use crate::profiles::ProfileStore;
use crate::score::{self, FactorStatus, ScoreSummary};
use crate::ui_profiles;
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
    SearchChanged(String),
    Lookup,
    Search,
    LookupDone(Result<ProductModel, String>),
    SearchDone(Result<Vec<off::SearchHit>, String>),
    PickSearchHit(String),
    ScanCamera,
    PreviewFrame {
        width: u32,
        height: u32,
        rgb: Vec<u8>,
    },
    ScanDone(Result<String, String>),
    CancelScan,
    WidthChanged(i32),
    ProfileSelected(u32),
    ManageProfiles,
    Rescore,
    CompareProfiles,
    ShowPage(AppPage),
    ToggleFavorite,
    OpenLibraryBarcode(String),
    ClearHistory,
    RemoveFavorite(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPage {
    Lookup,
    History,
    Favorites,
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
    search_query: String,
    status_kind: StatusKind,
    status_text: String,
    product: Option<ProductModel>,
    summary: Option<ScoreSummary>,
    last_result: Option<ScoreResult>,
    product_image: Option<gdk::Texture>,
    logo: Option<gdk::Texture>,
    camera_ok: bool,
    scanning: bool,
    preview: Option<gdk::Texture>,
    scan_stop: Option<Arc<AtomicBool>>,
    width_class: UiWidth,
    store: ProfileStore,
    profile_names: Vec<String>,
    profile_ids: Vec<String>,
    active_profile_index: u32,
    search_hits: Vec<off::SearchHit>,
    page: AppPage,
    history: HistoryStore,
    favorites: FavoritesStore,
}

impl AppModel {
    fn rebuild_profile_lists(&mut self) {
        self.profile_ids = self.store.list().iter().map(|p| p.id.clone()).collect();
        self.profile_names = self
            .store
            .list()
            .iter()
            .map(|p| {
                let mark = if p.builtin { "" } else { "★ " };
                format!("{mark}{} (v{})", p.name, p.version)
            })
            .collect();
        self.active_profile_index = self
            .profile_ids
            .iter()
            .position(|id| id == self.store.active_id())
            .unwrap_or(0) as u32;
    }

    fn rescore(&mut self) {
        let Some(product) = &self.product else {
            self.summary = None;
            self.last_result = None;
            return;
        };
        let profile = self.store.active();
        let result = engine::evaluate(product, profile);
        self.summary = Some(score::summary_from_result(&result));
        self.last_result = Some(result);
    }

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
        self.summary
            .as_ref()
            .map(|s| s.score_display.clone())
            .unwrap_or_else(|| "—".into())
    }

    fn score_label_text(&self) -> String {
        self.summary
            .as_ref()
            .map(|s| {
                if s.incomplete {
                    format!("{} (incomplete)", s.label)
                } else {
                    s.label.clone()
                }
            })
            .unwrap_or_default()
    }

    fn score_dot_classes(&self) -> [&'static str; 2] {
        match self.summary.as_ref().map(|s| s.status) {
            Some(FactorStatus::Good) => ["score-dot", "score-dot-good"],
            Some(FactorStatus::Mediocre) => ["score-dot", "score-dot-mediocre"],
            Some(FactorStatus::Poor) | None => ["score-dot", "score-dot-poor"],
        }
    }

    fn profile_caption(&self) -> String {
        let Some(s) = &self.summary else {
            return String::new();
        };
        let mut parts = vec![format!(
            "Profile “{}” (`{}`) v{} · {} pts · Librefood heuristic (not medical advice)",
            s.profile_name, s.profile_id, s.profile_version, s.score
        )];
        if let Some(p) = &self.product {
            if let Some(g) = p.nutriscore_grade {
                parts.push(format!(
                    "OFF Nutri-Score {} (source data)",
                    g.to_ascii_uppercase()
                ));
            }
            parts.push(format!("Barcode {}", p.barcode));
            if let Some(q) = &p.quantity {
                parts.push(q.clone());
            }
        }
        parts.join(" · ")
    }

    fn missing_caption(&self) -> String {
        match &self.summary {
            Some(s) if !s.missing.is_empty() => {
                format!("Missing / unavailable: {}", s.missing.join(", "))
            }
            _ => String::new(),
        }
    }

    fn has_negatives(&self) -> bool {
        self.summary
            .as_ref()
            .map(|s| !s.negatives.is_empty())
            .unwrap_or(false)
    }

    fn has_positives(&self) -> bool {
        self.summary
            .as_ref()
            .map(|s| !s.positives.is_empty())
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

    fn is_lookup_page(&self) -> bool {
        self.page == AppPage::Lookup
    }

    fn is_history_page(&self) -> bool {
        self.page == AppPage::History
    }

    fn is_favorites_page(&self) -> bool {
        self.page == AppPage::Favorites
    }

    fn nav_classes(&self, page: AppPage) -> [&'static str; 2] {
        if self.page == page {
            ["flat", "suggested-action"]
        } else {
            ["flat", "nav-inactive"]
        }
    }

    fn is_favorite(&self) -> bool {
        self.product
            .as_ref()
            .map(|p| self.favorites.contains(&p.barcode))
            .unwrap_or(false)
    }

    fn favorite_button_label(&self) -> &'static str {
        if self.is_favorite() {
            "★ Favorited"
        } else {
            "☆ Add to favorites"
        }
    }

    fn record_current_to_history(&mut self) {
        let (Some(product), Some(summary)) = (&self.product, &self.summary) else {
            return;
        };
        let entry = LibraryEntry::from_scored(product, summary);
        let _ = self.history.record(entry);
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn rebuild_history_list(
    container: &gtk::Box,
    entries: &[LibraryEntry],
    sender: &ComponentSender<AppModel>,
) {
    clear_box(container);
    if entries.is_empty() {
        let empty = gtk::Label::new(Some("No history yet — look up a product to start."));
        empty.add_css_class("dim-label");
        empty.set_halign(gtk::Align::Start);
        empty.set_xalign(0.0);
        container.append(&empty);
        return;
    }
    for entry in entries {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_hexpand(true);
        row.add_css_class("library-row");

        let btn = gtk::Button::new();
        btn.set_hexpand(true);
        btn.add_css_class("flat");
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let title = gtk::Label::new(Some(&entry.name));
        title.add_css_class("factor-title");
        title.set_halign(gtk::Align::Start);
        title.set_xalign(0.0);
        title.set_wrap(true);
        let sub = gtk::Label::new(Some(&entry.subtitle()));
        sub.add_css_class("factor-detail");
        sub.set_halign(gtk::Align::Start);
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        col.append(&title);
        col.append(&sub);
        btn.set_child(Some(&col));
        let code = entry.barcode.clone();
        let sender2 = sender.clone();
        btn.connect_clicked(move |_| {
            sender2.input(Msg::OpenLibraryBarcode(code.clone()));
        });
        row.append(&btn);
        container.append(&row);
    }
}

fn rebuild_favorites_list(
    container: &gtk::Box,
    entries: &[LibraryEntry],
    sender: &ComponentSender<AppModel>,
) {
    clear_box(container);
    if entries.is_empty() {
        let empty = gtk::Label::new(Some(
            "No favorites yet — open a product and tap “Add to favorites”.",
        ));
        empty.add_css_class("dim-label");
        empty.set_halign(gtk::Align::Start);
        empty.set_xalign(0.0);
        empty.set_wrap(true);
        container.append(&empty);
        return;
    }
    for entry in entries {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_hexpand(true);
        row.add_css_class("library-row");

        let btn = gtk::Button::new();
        btn.set_hexpand(true);
        btn.add_css_class("flat");
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let title = gtk::Label::new(Some(&entry.name));
        title.add_css_class("factor-title");
        title.set_halign(gtk::Align::Start);
        title.set_xalign(0.0);
        title.set_wrap(true);
        let sub = gtk::Label::new(Some(&entry.subtitle()));
        sub.add_css_class("factor-detail");
        sub.set_halign(gtk::Align::Start);
        sub.set_xalign(0.0);
        sub.set_wrap(true);
        col.append(&title);
        col.append(&sub);
        btn.set_child(Some(&col));
        let code = entry.barcode.clone();
        let sender2 = sender.clone();
        btn.connect_clicked(move |_| {
            sender2.input(Msg::OpenLibraryBarcode(code.clone()));
        });

        let remove = gtk::Button::with_label("Remove");
        remove.add_css_class("flat");
        let code2 = entry.barcode.clone();
        let sender3 = sender.clone();
        remove.connect_clicked(move |_| {
            sender3.input(Msg::RemoveFavorite(code2.clone()));
        });

        row.append(&btn);
        row.append(&remove);
        container.append(&row);
    }
}

fn append_factor_rows(container: &gtk::Box, rows: &[score::FactorRow]) {
    clear_box(container);
    for row in rows {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.set_hexpand(true);
        line.add_css_class("factor-row");

        let text_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_col.set_hexpand(true);
        text_col.set_halign(gtk::Align::Start);

        let title = gtk::Label::new(Some(&row.title));
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

        let delta = gtk::Label::new(Some(&format!("{:+.0}", row.weighted_delta)));
        delta.add_css_class("factor-delta");
        delta.set_valign(gtk::Align::Center);

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
        line.append(&delta);
        line.append(&dot);
        container.append(&line);
    }
}

fn append_explanation(container: &gtk::Box, lines: &[String]) {
    clear_box(container);
    for line in lines {
        let lab = gtk::Label::new(Some(line));
        lab.add_css_class("explain-line");
        lab.set_halign(gtk::Align::Start);
        lab.set_wrap(true);
        lab.set_xalign(0.0);
        lab.set_selectable(true);
        container.append(&lab);
    }
}

fn append_search_hits(
    container: &gtk::Box,
    hits: &[off::SearchHit],
    sender: &ComponentSender<AppModel>,
) {
    clear_box(container);
    for hit in hits {
        let btn = gtk::Button::new();
        btn.set_halign(gtk::Align::Fill);
        let label = if let Some(b) = &hit.brands {
            format!("{} — {} ({})", hit.name, b, hit.barcode)
        } else {
            format!("{} ({})", hit.name, hit.barcode)
        };
        btn.set_label(&label);
        let code = hit.barcode.clone();
        let sender = sender.clone();
        btn.connect_clicked(move |_| {
            sender.input(Msg::PickSearchHit(code.clone()));
        });
        container.append(&btn);
    }
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = (Option<String>, AppPage);
    type Input = Msg;
    type Output = ();
    type CommandOutput = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("Librefood"),
            set_default_width: 520,
            set_default_height: 780,
            set_width_request: 360,

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                adw::HeaderBar {
                    pack_start = &gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 0,
                        add_css_class: "linked",

                        gtk::Button {
                            set_label: "Lookup",
                            #[watch]
                            set_css_classes: &model.nav_classes(AppPage::Lookup),
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::ShowPage(AppPage::Lookup));
                            },
                        },
                        gtk::Button {
                            set_label: "History",
                            #[watch]
                            set_css_classes: &model.nav_classes(AppPage::History),
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::ShowPage(AppPage::History));
                            },
                        },
                        gtk::Button {
                            set_label: "Favorites",
                            #[watch]
                            set_css_classes: &model.nav_classes(AppPage::Favorites),
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::ShowPage(AppPage::Favorites));
                            },
                        },
                    },

                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Librefood",
                        set_subtitle: "Configurable scores via Open Food Facts",
                    },
                },

                // —— Lookup page ——
                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hexpand: true,
                    set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),
                    #[watch]
                    set_visible: model.is_lookup_page(),

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 16,
                        set_margin_top: 18,
                        set_margin_bottom: 28,
                        set_margin_start: 20,
                        set_margin_end: 20,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 12,
                            set_halign: gtk::Align::Start,

                            gtk::Picture {
                                set_content_fit: gtk::ContentFit::Contain,
                                set_can_shrink: false,
                                set_width_request: 56,
                                set_height_request: 56,
                                set_halign: gtk::Align::Start,
                                #[watch]
                                set_paintable: model.logo.as_ref(),
                                add_css_class: "app-logo",
                            },

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 2,
                                set_valign: gtk::Align::Center,

                                gtk::Label {
                                    set_label: "Librefood",
                                    add_css_class: "brand-title",
                                    set_halign: gtk::Align::Start,
                                    set_xalign: 0.0,
                                },
                                gtk::Label {
                                    set_label: "Open Food Facts data · your scoring profiles",
                                    add_css_class: "dim-label",
                                    set_halign: gtk::Align::Start,
                                    set_xalign: 0.0,
                                },
                            },
                        },

                        gtk::Box {
                            #[watch]
                            set_orientation: model.input_orientation(),
                            set_spacing: 10,

                            #[name = "barcode_entry"]
                            gtk::Entry {
                                set_hexpand: true,
                                set_placeholder_text: Some("EAN-13 barcode…"),
                                set_input_purpose: gtk::InputPurpose::Digits,
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

                        gtk::Box {
                            #[watch]
                            set_orientation: model.input_orientation(),
                            set_spacing: 10,

                            gtk::Entry {
                                set_hexpand: true,
                                set_placeholder_text: Some("Search product name…"),
                                connect_changed[sender] => move |entry| {
                                    sender.input(Msg::SearchChanged(entry.text().to_string()));
                                },
                                connect_activate[sender] => move |_| {
                                    sender.input(Msg::Search);
                                },
                            },
                            gtk::Button {
                                set_label: "Search",
                                set_valign: gtk::Align::Center,
                                #[watch]
                                set_sensitive: !matches!(model.status_kind, StatusKind::Loading),
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::Search);
                                },
                            },
                        },

                        #[name = "search_hits"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 4,
                            #[watch]
                            set_visible: !model.search_hits.is_empty(),
                        },

                        gtk::Box {
                            #[watch]
                            set_orientation: model.input_orientation(),
                            set_spacing: 10,

                            #[name = "profile_dropdown"]
                            gtk::DropDown {
                                set_hexpand: true,
                                #[watch]
                                set_selected: model.active_profile_index,
                                connect_selected_notify[sender] => move |dd| {
                                    sender.input(Msg::ProfileSelected(dd.selected()));
                                },
                            },

                            gtk::Button {
                                set_label: "Manage profiles…",
                                set_valign: gtk::Align::Center,
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::ManageProfiles);
                                },
                            },
                            gtk::Button {
                                set_label: "Compare",
                                set_valign: gtk::Align::Center,
                                #[watch]
                                set_sensitive: model.product.is_some(),
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::CompareProfiles);
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
                                set_height_request: 260,
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
                            set_spacing: 14,
                            #[watch]
                            set_visible: model.product.is_some(),
                            add_css_class: "result-panel",

                            gtk::Box {
                                #[watch]
                                set_orientation: model.header_orientation(),
                                set_spacing: 16,

                                gtk::Picture {
                                    set_content_fit: gtk::ContentFit::Cover,
                                    set_can_shrink: false,
                                    set_width_request: 96,
                                    set_height_request: 96,
                                    set_halign: gtk::Align::Start,
                                    set_hexpand: false,
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
                                        set_margin_top: 6,

                                        gtk::Label {
                                            set_label: "●",
                                            #[watch]
                                            set_css_classes: &model.score_dot_classes(),
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
                                        set_label: &model.profile_caption(),
                                        add_css_class: "dim-label",
                                        set_halign: gtk::Align::Start,
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        set_margin_top: 4,
                                        set_selectable: true,
                                    },
                                    gtk::Label {
                                        #[watch]
                                        set_label: &model.missing_caption(),
                                        add_css_class: "missing-label",
                                        set_halign: gtk::Align::Start,
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        #[watch]
                                        set_visible: !model.missing_caption().is_empty(),
                                    },

                                    gtk::Button {
                                        #[watch]
                                        set_label: model.favorite_button_label(),
                                        set_halign: gtk::Align::Start,
                                        set_margin_top: 6,
                                        connect_clicked[sender] => move |_| {
                                            sender.input(Msg::ToggleFavorite);
                                        },
                                    },
                                },
                            },

                            gtk::Box {
                                #[watch]
                                set_orientation: model.factors_orientation(),
                                set_spacing: 18,

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
                                    },
                                    #[name = "negatives_list"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 0,
                                    },
                                },

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
                                    },
                                    #[name = "positives_list"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 0,
                                    },
                                },
                            },

                            gtk::Label {
                                set_label: "Calculation steps",
                                add_css_class: "section-title",
                                set_halign: gtk::Align::Start,
                                set_xalign: 0.0,
                            },
                            #[name = "explain_list"]
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 4,
                                add_css_class: "explain-panel",
                            },
                        },

                        gtk::Label {
                            set_label: "Scores are Librefood heuristics from Open Food Facts source data and your active profile — not Yuka, not an official Nutri-Score, and not medical advice.",
                            add_css_class: "caption",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                    },
                },

                // —— History page ——
                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hexpand: true,
                    set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),
                    #[watch]
                    set_visible: model.is_history_page(),

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_margin_top: 18,
                        set_margin_bottom: 28,
                        set_margin_start: 20,
                        set_margin_end: 20,

                        gtk::Label {
                            set_label: "History",
                            add_css_class: "brand-title",
                            set_halign: gtk::Align::Start,
                            set_xalign: 0.0,
                        },
                        gtk::Label {
                            set_label: "Recent lookups and scans (newest first). Tap a row to open.",
                            add_css_class: "dim-label",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                        gtk::Button {
                            set_label: "Clear history",
                            add_css_class: "destructive-action",
                            set_halign: gtk::Align::Start,
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::ClearHistory);
                            },
                        },
                        #[name = "history_list"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 6,
                        },
                    },
                },

                // —— Favorites page ——
                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hexpand: true,
                    set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),
                    #[watch]
                    set_visible: model.is_favorites_page(),

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_margin_top: 18,
                        set_margin_bottom: 28,
                        set_margin_start: 20,
                        set_margin_end: 20,

                        gtk::Label {
                            set_label: "Favorites",
                            add_css_class: "brand-title",
                            set_halign: gtk::Align::Start,
                            set_xalign: 0.0,
                        },
                        gtk::Label {
                            set_label: "Products you starred. Tap a row to open, or remove from here.",
                            add_css_class: "dim-label",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                        #[name = "favorites_list"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 6,
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
        let (barcode_init, page_init) = init;
        let initial = barcode_init
            .as_deref()
            .map(off::normalize_barcode)
            .filter(|s| !s.is_empty());

        let store = ProfileStore::open_default().unwrap_or_else(|e| {
            eprintln!("profile store: {e}; using temp builtins only");
            let dir = std::env::temp_dir().join(format!(
                "librefood-profiles-{}",
                std::process::id()
            ));
            ProfileStore::open(dir).expect("temp profile store")
        });

        let mut model = AppModel {
            barcode: initial.clone().unwrap_or_default(),
            search_query: String::new(),
            status_kind: StatusKind::Idle,
            status_text: "Enter a barcode, search by name, or use the camera.".into(),
            product: None,
            summary: None,
            last_result: None,
            product_image: None,
            logo: logo::logo_texture(128),
            camera_ok: camera::camera_available(),
            scanning: false,
            preview: None,
            scan_stop: None,
            width_class: UiWidth::Compact,
            store,
            profile_names: vec![],
            profile_ids: vec![],
            active_profile_index: 0,
            search_hits: vec![],
            page: page_init,
            history: HistoryStore::open_default().unwrap_or_else(|e| {
                eprintln!("history store: {e}");
                let dir = std::env::temp_dir().join(format!("lf-hist-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                HistoryStore::open(dir.join("history.json"))
            }),
            favorites: FavoritesStore::open_default().unwrap_or_else(|e| {
                eprintln!("favorites store: {e}");
                let dir = std::env::temp_dir().join(format!("lf-fav-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                FavoritesStore::open(dir.join("favorites.json"))
            }),
        };
        model.rebuild_profile_lists();

        let widgets = view_output!();
        load_css();
        logo::apply_window_icon(&root);
        rebuild_history_list(&widgets.history_list, model.history.entries(), &sender);
        rebuild_favorites_list(&widgets.favorites_list, model.favorites.entries(), &sender);

        // Populate profile dropdown model
        let string_list = gtk::StringList::new(
            &model
                .profile_names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
        );
        widgets.profile_dropdown.set_model(Some(&string_list));
        widgets
            .profile_dropdown
            .set_selected(model.active_profile_index);

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
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
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
        root: &Self::Root,
    ) {
        match msg {
            Msg::BarcodeChanged(text) => {
                if text != self.barcode {
                    self.barcode = text;
                }
            }
            Msg::SearchChanged(text) => {
                self.search_query = text;
            }
            Msg::Lookup => {
                let code = off::normalize_barcode(&self.barcode);
                if code.is_empty() {
                    self.status_kind = StatusKind::Error;
                    self.status_text = "Enter a barcode (digits only).".into();
                    self.product = None;
                    self.summary = None;
                    clear_lists(widgets);
                } else {
                    self.barcode = code.clone();
                    self.status_kind = StatusKind::Loading;
                    self.status_text = format!("Looking up {code}…");
                    self.product = None;
                    self.summary = None;
                    self.product_image = None;
                    self.search_hits.clear();
                    clear_lists(widgets);
                    let sender2 = sender.clone();
                    thread::spawn(move || {
                        let result = off::lookup_product(&code).map_err(format_lookup_err);
                        sender2.input(Msg::LookupDone(result));
                    });
                }
            }
            Msg::Search => {
                let q = self.search_query.trim().to_string();
                if q.is_empty() {
                    self.status_kind = StatusKind::Error;
                    self.status_text = "Enter a search term.".into();
                } else {
                    self.status_kind = StatusKind::Loading;
                    self.status_text = format!("Searching “{q}”…");
                    let sender2 = sender.clone();
                    thread::spawn(move || {
                        let result = off::search_products(&q, 8).map_err(format_lookup_err);
                        sender2.input(Msg::SearchDone(result));
                    });
                }
            }
            Msg::SearchDone(result) => match result {
                Ok(hits) => {
                    self.status_kind = StatusKind::Success;
                    self.status_text = if hits.is_empty() {
                        "No search results.".into()
                    } else {
                        format!("{} result(s) — pick one.", hits.len())
                    };
                    self.search_hits = hits;
                    append_search_hits(&widgets.search_hits, &self.search_hits, &sender);
                }
                Err(msg) => {
                    self.status_kind = StatusKind::Error;
                    self.status_text = msg;
                    self.search_hits.clear();
                    clear_box(&widgets.search_hits);
                }
            },
            Msg::PickSearchHit(code) => {
                self.barcode = code;
                widgets.barcode_entry.set_text(&self.barcode);
                self.search_hits.clear();
                clear_box(&widgets.search_hits);
                sender.input(Msg::Lookup);
            }
            Msg::LookupDone(result) => match result {
                Ok(info) => {
                    self.status_kind = StatusKind::Success;
                    self.product_image = texture_from_bytes(info.image_bytes.as_deref());
                    self.product = Some(info);
                    self.page = AppPage::Lookup;
                    self.rescore();
                    self.record_current_to_history();
                    rebuild_history_list(
                        &widgets.history_list,
                        self.history.entries(),
                        &sender,
                    );
                    if let Some(s) = &self.summary {
                        self.status_text = format!(
                            "Product found — {} ({}) via “{}”.",
                            s.score_display, s.label, s.profile_name
                        );
                        append_factor_rows(&widgets.negatives_list, &s.negatives);
                        append_factor_rows(&widgets.positives_list, &s.positives);
                        append_explanation(&widgets.explain_list, &s.explanation_lines);
                    }
                }
                Err(msg) => {
                    let lower = msg.to_lowercase();
                    self.status_kind = if lower.contains("not found") {
                        StatusKind::NotFound
                    } else if lower.contains("network") || lower.contains("http") {
                        StatusKind::Network
                    } else {
                        StatusKind::Error
                    };
                    self.status_text = msg;
                    self.product = None;
                    self.summary = None;
                    self.product_image = None;
                    clear_lists(widgets);
                }
            },
            Msg::ProfileSelected(idx) => {
                if let Some(id) = self.profile_ids.get(idx as usize).cloned() {
                    if id != self.store.active_id() {
                        let _ = self.store.set_active(&id);
                        self.active_profile_index = idx;
                        if self.product.is_some() {
                            sender.input(Msg::Rescore);
                        }
                    }
                }
            }
            Msg::ManageProfiles => {
                let result = ui_profiles::run_manage_dialog(root, &mut self.store);
                self.rebuild_profile_lists();
                let string_list = gtk::StringList::new(
                    &self
                        .profile_names
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>(),
                );
                widgets.profile_dropdown.set_model(Some(&string_list));
                widgets
                    .profile_dropdown
                    .set_selected(self.active_profile_index);
                if result.store_changed || result.active_id != self.store.active_id() {
                    let _ = self.store.set_active(&result.active_id);
                    self.rebuild_profile_lists();
                    widgets
                        .profile_dropdown
                        .set_selected(self.active_profile_index);
                }
                if self.product.is_some() {
                    sender.input(Msg::Rescore);
                }
            }
            Msg::Rescore => {
                self.rescore();
                self.record_current_to_history();
                rebuild_history_list(&widgets.history_list, self.history.entries(), &sender);
                if let Some(s) = &self.summary {
                    self.status_text = format!(
                        "Rescored — {} ({}) via “{}”.",
                        s.score_display, s.label, s.profile_name
                    );
                    append_factor_rows(&widgets.negatives_list, &s.negatives);
                    append_factor_rows(&widgets.positives_list, &s.positives);
                    append_explanation(&widgets.explain_list, &s.explanation_lines);
                }
            }
            Msg::CompareProfiles => {
                let Some(product) = &self.product else {
                    self.status_text = "Look up a product before comparing profiles.".into();
                    self.update_view(widgets, sender);
                    return;
                };
                let mut lines = vec![
                    "Same product under each scoring profile (source OFF data unchanged):".into(),
                ];
                for p in self.store.list() {
                    let r = engine::evaluate(product, p);
                    let active = if p.id == self.store.active_id() {
                        " ← active"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "• {} v{} → {} ({}){active}",
                        p.name, p.version, r.score_display, r.label
                    ));
                }
                append_explanation(&widgets.explain_list, &lines);
                self.status_text = "Compared all profiles on the current product.".into();
            }
            Msg::ShowPage(page) => {
                self.page = page;
                if page == AppPage::History {
                    rebuild_history_list(
                        &widgets.history_list,
                        self.history.entries(),
                        &sender,
                    );
                } else if page == AppPage::Favorites {
                    rebuild_favorites_list(
                        &widgets.favorites_list,
                        self.favorites.entries(),
                        &sender,
                    );
                }
            }
            Msg::ToggleFavorite => {
                let (Some(product), Some(summary)) = (&self.product, &self.summary) else {
                    self.status_text = "Look up a product before favoriting.".into();
                    self.update_view(widgets, sender);
                    return;
                };
                let entry = LibraryEntry::from_scored(product, summary);
                match self.favorites.toggle(entry) {
                    Ok(true) => {
                        self.status_text = "Added to favorites.".into();
                    }
                    Ok(false) => {
                        self.status_text = "Removed from favorites.".into();
                    }
                    Err(e) => {
                        self.status_text = format!("Favorites error: {e}");
                    }
                }
                rebuild_favorites_list(
                    &widgets.favorites_list,
                    self.favorites.entries(),
                    &sender,
                );
            }
            Msg::OpenLibraryBarcode(code) => {
                self.page = AppPage::Lookup;
                self.barcode = off::normalize_barcode(&code);
                widgets.barcode_entry.set_text(&self.barcode);
                sender.input(Msg::Lookup);
            }
            Msg::ClearHistory => {
                if let Err(e) = self.history.clear() {
                    self.status_text = format!("Could not clear history: {e}");
                } else {
                    self.status_text = "History cleared.".into();
                }
                rebuild_history_list(&widgets.history_list, self.history.entries(), &sender);
            }
            Msg::RemoveFavorite(code) => {
                if let Err(e) = self.favorites.remove(&code) {
                    self.status_text = format!("Could not remove favorite: {e}");
                } else {
                    self.status_text = "Removed from favorites.".into();
                }
                rebuild_favorites_list(
                    &widgets.favorites_list,
                    self.favorites.entries(),
                    &sender,
                );
            }
            Msg::ScanCamera => {
                if !self.scanning {
                    if !self.camera_ok {
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
                            "Live preview on — center the barcode in the guide.".into();
                        self.product = None;
                        self.summary = None;
                        self.product_image = None;
                        clear_lists(widgets);
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
                        widgets.barcode_entry.set_text(&self.barcode);
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

fn clear_lists(widgets: &AppModelWidgets) {
    clear_box(&widgets.negatives_list);
    clear_box(&widgets.positives_list);
    clear_box(&widgets.explain_list);
    clear_box(&widgets.search_hits);
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
        LookupError::EmptyQuery => "Enter a search term.".into(),
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
        .app-logo {
            border-radius: 10px;
        }
        .product-name { font-size: 1.2rem; font-weight: 700; }
        .product-brand { font-size: 0.95rem; opacity: 0.65; }
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
        .score-label { font-size: 0.95rem; opacity: 0.7; }
        .score-dot { font-size: 1.6rem; line-height: 1; }
        .score-dot-good { color: #2e7d32; }
        .score-dot-mediocre { color: #ef6c00; }
        .score-dot-poor { color: #c62828; }
        .section-title {
            font-size: 1.05rem;
            font-weight: 700;
            margin-top: 4px;
            margin-bottom: 4px;
        }
        .factor-row {
            padding: 10px 0;
            border-bottom: 1px solid alpha(@card_fg_color, 0.1);
        }
        .factor-title { font-weight: 650; }
        .factor-detail { font-size: 0.88rem; opacity: 0.65; }
        .factor-value { font-size: 0.92rem; opacity: 0.7; min-width: 4.5em; }
        .factor-delta { font-size: 0.85rem; opacity: 0.55; min-width: 2.8em; }
        .status-dot { font-size: 0.85rem; min-width: 1.1em; }
        .dot-good { color: #2e7d32; }
        .dot-mediocre { color: #ef6c00; }
        .dot-poor { color: #c62828; }
        .dim-label { opacity: 0.75; }
        .missing-label { color: #9a5b00; font-size: 0.9rem; }
        .caption { opacity: 0.55; font-size: 0.85rem; }
        .explain-panel {
            padding: 10px;
            border-radius: 8px;
            background: alpha(@card_fg_color, 0.03);
        }
        .explain-line { font-size: 0.82rem; opacity: 0.8; }
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
        .scan-hint { font-weight: 600; opacity: 0.9; }
        .scan-preview {
            background: #121212;
            border-radius: 10px;
            min-height: 220px;
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
        .nav-inactive { opacity: 0.85; }
        .library-row {
            padding: 6px 0;
            border-bottom: 1px solid alpha(@card_fg_color, 0.08);
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
