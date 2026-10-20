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
    OpenProfilesPage,
    Profiles(ui_profiles::ProfilePageEvent),
    /// Rebuild Profiles list/actions after the current update returns (avoids GTK freezes).
    RefreshProfilesUi,
    /// Apply profile DropDown model outside a Profiles update.
    SyncProfileDropdown,
    /// In-place ●/○ / selection CSS refresh (no widget destroy).
    RelabelProfiles,
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
    Profiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiWidth {
    Compact,
    Medium,
    Wide,
}

impl UiWidth {
    fn from_px(w: i32) -> Self {
        // Fractal collapses its split view at max-width: 600sp.
        if w < 600 {
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
    camera_probe: camera::CameraProbe,
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
    selected_profile_id: Option<String>,
    profiles_status: String,
    editing_profile: bool,
    profile_editor: Option<ui_profiles::EditorHandle>,
    /// Ignore DropDown notify while we rebuild its model programmatically.
    suppress_profile_dropdown: bool,
    /// Steps remaining for LIBREFOOD_SMOKE_SET_ACTIVE regression.
    smoke_set_active_left: Option<u32>,
    smoke_tick: u32,
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
        self.camera_probe.scan_button_label()
    }

    fn camera_ok(&self) -> bool {
        // Enable Scan whenever a device may exist; only Missing grays the button.
        self.camera_probe.scan_enabled()
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

    fn is_profiles_list(&self) -> bool {
        self.page == AppPage::Profiles && !self.editing_profile
    }

    fn is_profiles_editor(&self) -> bool {
        self.page == AppPage::Profiles && self.editing_profile
    }

    fn nav_classes(&self, page: AppPage) -> [&'static str; 3] {
        if self.page == page {
            ["flat", "suggested-action", "bottom-nav-btn"]
        } else {
            ["flat", "nav-inactive", "bottom-nav-btn"]
        }
    }

    fn page_subtitle(&self) -> &'static str {
        match self.page {
            AppPage::Lookup => "Lookup",
            AppPage::History => "History",
            AppPage::Favorites => "Favorites",
            AppPage::Profiles => "Profiles",
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

/// Defer profile UI events until after the emitting widget's signal returns.
/// Rebuilding lists/actions synchronously from a button click destroys that
/// button mid-handler and freezes GTK (Force Quit / Wait).
fn profiles_emit(
    sender: &ComponentSender<AppModel>,
) -> std::rc::Rc<dyn Fn(ui_profiles::ProfilePageEvent)> {
    let sender = sender.clone();
    std::rc::Rc::new(move |ev| {
        let sender = sender.clone();
        gtk::glib::idle_add_local_once(move || {
            sender.input(Msg::Profiles(ev));
        });
    })
}

fn refresh_profiles_page(
    widgets: &AppModelWidgets,
    model: &AppModel,
    sender: &ComponentSender<AppModel>,
) {
    let emit = profiles_emit(sender);
    ui_profiles::rebuild_profile_list(
        &widgets.profiles_list,
        &model.store,
        model.selected_profile_id.as_deref(),
        &emit,
    );
    // Actions are static — only build once (or when empty). Rebuilding FlowBox
    // on every Set active races GTK layout and freezes the UI.
    if widgets.profiles_actions.first_child().is_none() {
        ui_profiles::rebuild_profile_actions(&widgets.profiles_actions, &emit);
    }
}

fn sync_profile_dropdown(widgets: &AppModelWidgets, model: &mut AppModel) {
    model.suppress_profile_dropdown = true;
    // Only rebuild the StringList when names/ids actually changed.
    let n = widgets
        .profile_dropdown
        .model()
        .map(|m| m.n_items())
        .unwrap_or(0);
    if n != model.profile_names.len() as u32 {
        let string_list = gtk::StringList::new(
            &model
                .profile_names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
        );
        widgets.profile_dropdown.set_model(Some(&string_list));
    }
    if widgets.profile_dropdown.selected() != model.active_profile_index {
        widgets
            .profile_dropdown
            .set_selected(model.active_profile_index);
    }
    model.suppress_profile_dropdown = false;
}

fn schedule_profiles_ui_refresh(sender: &ComponentSender<AppModel>) {
    queue_msg(sender, Msg::RefreshProfilesUi);
}

/// Never call `sender.input` directly from inside `update` — Relm4 may re-enter
/// while `widgets` is still borrowed and freeze the GTK main loop.
fn queue_msg(sender: &ComponentSender<AppModel>, msg: Msg) {
    let sender = sender.clone();
    gtk::glib::idle_add_local_once(move || {
        sender.input(msg);
    });
}

fn handle_profiles_event(
    model: &mut AppModel,
    widgets: &AppModelWidgets,
    root: &adw::ApplicationWindow,
    sender: ComponentSender<AppModel>,
    ev: ui_profiles::ProfilePageEvent,
) {
    use ui_profiles::ProfilePageEvent;
    let emit = profiles_emit(&sender);

    match ev {
        ProfilePageEvent::Select(id) => {
            model.selected_profile_id = Some(id);
            queue_msg(&sender, Msg::RelabelProfiles);
        }
        ProfilePageEvent::New => {
            let sender = sender.clone();
            ui_profiles::ask_text_async(
                root,
                "New scoring profile",
                "Name",
                "My scoring profile",
                move |name| {
                    if let Some(name) = name {
                        sender.input(Msg::Profiles(ProfilePageEvent::NewNamed(name)));
                    }
                },
            );
        }
        ProfilePageEvent::NewNamed(name) => {
            match model.store.create_new(&name) {
                Ok(created) => {
                    model.selected_profile_id = Some(created.id.clone());
                    model.rebuild_profile_lists();
                    sync_profile_dropdown(widgets, model);
                    model.profiles_status =
                        format!("Created “{}” — opening editor.", created.name);
                    if let Some(profile) = model.store.get(&created.id).cloned() {
                        model.editing_profile = true;
                        model.profile_editor = Some(ui_profiles::mount_profile_editor(
                            &widgets.profiles_editor_body,
                            &widgets.profiles_editor_footer,
                            profile,
                            &emit,
                        ));
                    }
                    schedule_profiles_ui_refresh(&sender);
                }
                Err(e) => model.profiles_status = e,
            }
        }
        ProfilePageEvent::SetActive => {
            let Some(id) = model.selected_profile_id.clone() else {
                model.profiles_status = "Select a profile first.".into();
                return;
            };
            match model.store.set_active(&id) {
                Ok(()) => {
                    model.rebuild_profile_lists();
                    model.profiles_status = format!("Active profile: {id}");
                    queue_msg(&sender, Msg::RelabelProfiles);
                    queue_msg(&sender, Msg::SyncProfileDropdown);
                    if model.product.is_some() {
                        queue_msg(&sender, Msg::Rescore);
                    }
                }
                Err(e) => model.profiles_status = e,
            }
        }
        ProfilePageEvent::Duplicate => {
            let Some(id) = model.selected_profile_id.clone() else {
                model.profiles_status = "Select a profile first.".into();
                return;
            };
            let default_name = format!(
                "{} (copy)",
                model
                    .store
                    .get(&id)
                    .map(|p| p.name.as_str())
                    .unwrap_or("Profile")
            );
            let sender = sender.clone();
            ui_profiles::ask_text_async(
                root,
                "Duplicate profile",
                "Name",
                &default_name,
                move |name| {
                    if let Some(name) = name {
                        sender.input(Msg::Profiles(ProfilePageEvent::DuplicateNamed(name)));
                    }
                },
            );
        }
        ProfilePageEvent::DuplicateNamed(name) => {
            let Some(id) = model.selected_profile_id.clone() else {
                model.profiles_status = "Select a profile first.".into();
                return;
            };
            match model.store.duplicate(&id, &name) {
                Ok(p) => {
                    let _ = model.store.set_active(&p.id);
                    model.selected_profile_id = Some(p.id.clone());
                    model.rebuild_profile_lists();
                    sync_profile_dropdown(widgets, model);
                    model.profiles_status = format!("Duplicated as {}", p.id);
                    schedule_profiles_ui_refresh(&sender);
                }
                Err(e) => model.profiles_status = e,
            }
        }
        ProfilePageEvent::StartEdit => {
            let Some(id) = model.selected_profile_id.clone() else {
                model.profiles_status = "Select a profile first.".into();
                return;
            };
            let Some(profile) = model.store.get(&id).cloned() else {
                model.profiles_status = format!("Unknown profile `{id}`");
                return;
            };
            model.editing_profile = true;
            model.profile_editor = Some(ui_profiles::mount_profile_editor(
                &widgets.profiles_editor_body,
                &widgets.profiles_editor_footer,
                profile,
                &emit,
            ));
            model.profiles_status = "Editing — Save or Cancel below.".into();
        }
        ProfilePageEvent::Export => {
            let Some(id) = model.selected_profile_id.clone() else {
                model.profiles_status = "Select a profile first.".into();
                return;
            };
            match model.store.export_json(&id) {
                Ok(json) => {
                    let path = model.store.root().join(format!("{id}-export.json"));
                    match std::fs::write(&path, json) {
                        Ok(()) => {
                            model.profiles_status = format!("Wrote {}", path.display());
                        }
                        Err(e) => model.profiles_status = e.to_string(),
                    }
                }
                Err(e) => model.profiles_status = e,
            }
        }
        ProfilePageEvent::Import => {
            let sender = sender.clone();
            ui_profiles::ask_path_async(root, move |path| {
                if let Some(path) = path {
                    sender.input(Msg::Profiles(ProfilePageEvent::ImportPath(path)));
                }
            });
        }
        ProfilePageEvent::ImportPath(path) => match std::fs::read_to_string(path.trim()) {
            Ok(text) => match model.store.import_json(&text) {
                Ok(p) => {
                    model.selected_profile_id = Some(p.id.clone());
                    model.rebuild_profile_lists();
                    sync_profile_dropdown(widgets, model);
                    model.profiles_status = format!("Imported {}", p.id);
                    schedule_profiles_ui_refresh(&sender);
                }
                Err(e) => model.profiles_status = e,
            },
            Err(e) => model.profiles_status = e.to_string(),
        },
        ProfilePageEvent::Delete => {
            let Some(id) = model.selected_profile_id.clone() else {
                model.profiles_status = "Select a profile first.".into();
                return;
            };
            match model.store.delete(&id) {
                Ok(()) => {
                    model.selected_profile_id = Some(model.store.active_id().to_string());
                    model.rebuild_profile_lists();
                    sync_profile_dropdown(widgets, model);
                    model.profiles_status = "Removed user override / custom profile.".into();
                    schedule_profiles_ui_refresh(&sender);
                }
                Err(e) => model.profiles_status = e,
            }
        }
        ProfilePageEvent::RestoreBuiltins => match model.store.restore_builtins() {
            Ok(()) => {
                model.selected_profile_id = Some(model.store.active_id().to_string());
                model.rebuild_profile_lists();
                sync_profile_dropdown(widgets, model);
                model.profiles_status =
                    "Built-in profiles restored from embedded defaults (user customs kept)."
                        .into();
                schedule_profiles_ui_refresh(&sender);
            }
            Err(e) => model.profiles_status = e,
        },
        ProfilePageEvent::CancelEdit => {
            model.editing_profile = false;
            model.profile_editor = None;
            ui_profiles::clear_editor(
                &widgets.profiles_editor_body,
                &widgets.profiles_editor_footer,
            );
            model.profiles_status = "Edit cancelled.".into();
            schedule_profiles_ui_refresh(&sender);
        }
        ProfilePageEvent::SaveEdit => {
            let Some(editor) = model.profile_editor.as_ref() else {
                model.profiles_status = "No editor open.".into();
                return;
            };
            match editor.collect() {
                Ok(profile) => {
                    let id = profile.id.clone();
                    match model.store.save(profile) {
                        Ok(()) => {
                            let _ = model.store.set_active(&id);
                            model.selected_profile_id = Some(id.clone());
                            model.rebuild_profile_lists();
                            sync_profile_dropdown(widgets, model);
                            model.editing_profile = false;
                            model.profile_editor = None;
                            ui_profiles::clear_editor(
                                &widgets.profiles_editor_body,
                                &widgets.profiles_editor_footer,
                            );
                            model.profiles_status = format!("Saved `{id}`.");
                            schedule_profiles_ui_refresh(&sender);
                            if model.product.is_some() {
                                queue_msg(&sender, Msg::Rescore);
                            }
                        }
                        Err(e) => model.profiles_status = e,
                    }
                }
                Err(e) => model.profiles_status = e,
            }
        }
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
            // Snapshot-like soft defaults; apply_window_placement clamps to the monitor.
            set_default_width: 800,
            set_default_height: 640,
            // Fractal minimums (width-request: 360, height-request: 294).
            set_width_request: 360,
            set_height_request: 294,

            #[wrap(Some)]
            set_content = &adw::ToolbarView {
                set_top_bar_style: adw::ToolbarStyle::Flat,
                set_bottom_bar_style: adw::ToolbarStyle::Raised,

                add_top_bar = &adw::HeaderBar {
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Librefood",
                        #[watch]
                        set_subtitle: model.page_subtitle(),
                    },
                },

                #[wrap(Some)]
                set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

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
                                set_label: "Profiles",
                                set_valign: gtk::Align::Center,
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::OpenProfilesPage);
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
                            set_sensitive: model.camera_ok() && !model.scanning,
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
                                // Hide hint/guide chrome until a real frame arrives (no empty flash).
                                #[watch]
                                set_visible: model.preview.is_some(),
                            },

                            gtk::Overlay {
                                set_hexpand: true,
                                set_height_request: 260,
                                add_css_class: "scan-preview",
                                #[watch]
                                set_visible: model.preview.is_some(),

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

                // —— Profiles page (list) ——
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_vexpand: true,
                    set_hexpand: true,
                    #[watch]
                    set_visible: model.is_profiles_list(),

                    gtk::ScrolledWindow {
                        set_vexpand: true,
                        set_hexpand: true,
                        set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            set_margin_top: 18,
                            set_margin_bottom: 12,
                            set_margin_start: 16,
                            set_margin_end: 16,

                            gtk::Label {
                                set_label: "Profiles",
                                add_css_class: "brand-title",
                                set_halign: gtk::Align::Start,
                                set_xalign: 0.0,
                            },
                            gtk::Label {
                                set_label: "Declarative JSON scoring profiles — not medical advice. Select one, then Edit on this page.",
                                add_css_class: "dim-label",
                                set_halign: gtk::Align::Start,
                                set_wrap: true,
                                set_wrap_mode: gtk::pango::WrapMode::WordChar,
                                set_ellipsize: gtk::pango::EllipsizeMode::None,
                                set_xalign: 0.0,
                            },
                            gtk::Expander {
                                set_label: Some("How scoring works"),

                                #[wrap(Some)]
                                set_child = &gtk::Label {
                                    set_label: crate::engine::ENGINE_NOTES.trim(),
                                    add_css_class: "dim-label",
                                    set_wrap: true,
                                    set_xalign: 0.0,
                                    set_selectable: true,
                                },
                            },
                            #[name = "profiles_list"]
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 4,
                            },
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 8,
                        set_margin_start: 16,
                        set_margin_end: 16,
                        set_margin_bottom: 16,
                        add_css_class: "profiles-actions-bar",

                        #[name = "profiles_actions"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 8,
                        },
                        gtk::Label {
                            #[watch]
                            set_label: &model.profiles_status,
                            add_css_class: "dim-label",
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                    },
                },

                // —— Profiles page (editor) ——
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_vexpand: true,
                    set_hexpand: true,
                    #[watch]
                    set_visible: model.is_profiles_editor(),

                    gtk::ScrolledWindow {
                        set_vexpand: true,
                        set_hexpand: true,
                        set_policy: (gtk::PolicyType::Never, gtk::PolicyType::Automatic),

                        #[name = "profiles_editor_body"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            set_margin_top: 12,
                            set_margin_bottom: 8,
                            set_margin_start: 14,
                            set_margin_end: 14,
                        },
                    },

                    #[name = "profiles_editor_footer"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 8,
                        set_margin_start: 14,
                        set_margin_end: 14,
                        set_margin_bottom: 12,
                    },
                },
                },

                add_bottom_bar = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_homogeneous: true,
                    set_spacing: 2,
                    set_hexpand: true,
                    add_css_class: "bottom-nav",

                    gtk::Button {
                        set_label: "Lookup",
                        set_hexpand: true,
                        #[watch]
                        set_css_classes: &model.nav_classes(AppPage::Lookup),
                        connect_clicked[sender] => move |_| {
                            sender.input(Msg::ShowPage(AppPage::Lookup));
                        },
                    },
                    gtk::Button {
                        set_label: "History",
                        set_hexpand: true,
                        #[watch]
                        set_css_classes: &model.nav_classes(AppPage::History),
                        connect_clicked[sender] => move |_| {
                            sender.input(Msg::ShowPage(AppPage::History));
                        },
                    },
                    gtk::Button {
                        set_label: "Favorites",
                        set_hexpand: true,
                        #[watch]
                        set_css_classes: &model.nav_classes(AppPage::Favorites),
                        connect_clicked[sender] => move |_| {
                            sender.input(Msg::ShowPage(AppPage::Favorites));
                        },
                    },
                    gtk::Button {
                        set_label: "Profiles",
                        set_hexpand: true,
                        #[watch]
                        set_css_classes: &model.nav_classes(AppPage::Profiles),
                        connect_clicked[sender] => move |_| {
                            sender.input(Msg::ShowPage(AppPage::Profiles));
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
            // Quick list only — full open is deferred (avoids init hang / D-Bus lock).
            camera_probe: camera::camera_probe_quick(),
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
            selected_profile_id: None,
            profiles_status: "Select a profile, then Edit — editor opens full-page.".into(),
            editing_profile: false,
            profile_editor: None,
            suppress_profile_dropdown: false,
            smoke_set_active_left: if std::env::var("LIBREFOOD_SMOKE_SET_ACTIVE")
                .ok()
                .as_deref()
                == Some("1")
            {
                Some(4)
            } else {
                None
            },
            smoke_tick: 0,
        };
        model.rebuild_profile_lists();
        model.selected_profile_id = Some(model.store.active_id().to_string());

        let widgets = view_output!();
        load_css();
        logo::apply_window_icon(&root);
        apply_window_placement(&root);
        // Wayland/Phosh often ignore maximize until the surface is mapped —
        // re-apply on map and once shortly after (same as before).
        {
            let win = root.clone();
            win.connect_map(|w| {
                apply_window_placement(w);
            });
            let win2 = root.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(150),
                move || {
                    apply_window_placement(&win2);
                },
            );
        }
        rebuild_history_list(&widgets.history_list, model.history.entries(), &sender);
        rebuild_favorites_list(&widgets.favorites_list, model.favorites.entries(), &sender);
        refresh_profiles_page(&widgets, &model, &sender);
        if model.page == AppPage::Profiles {
            if std::env::var("LIBREFOOD_EDIT_PROFILE").ok().as_deref() == Some("1") {
                // Defer: open editor after first frame for screenshots.
                let sender2 = sender.clone();
                gtk::glib::idle_add_local_once(move || {
                    sender2.input(Msg::Profiles(ui_profiles::ProfilePageEvent::StartEdit));
                });
            }
            if std::env::var("LIBREFOOD_SMOKE_SET_ACTIVE").ok().as_deref() == Some("1") {
                eprintln!(
                    "SMOKE_SET_ACTIVE_START profiles={} page={:?}",
                    model.store.list().len(),
                    model.page
                );
            }
        }

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

        // Optional headless-ish screenshot for docs (LIBREFOOD_SCREENSHOT=/path.png).
        if let Ok(path) = std::env::var("LIBREFOOD_SCREENSHOT") {
            let delay_ms: u64 = std::env::var("LIBREFOOD_SCREENSHOT_DELAY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(900);
            let win = root.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(delay_ms),
                move || {
                    let paintable = gtk::WidgetPaintable::new(Some(&win));
                    let img = gdk::prelude::PaintableExt::current_image(&paintable);
                    if let Ok(tex) = img.downcast::<gdk::Texture>() {
                        if let Err(e) = tex.save_to_png(&path) {
                            eprintln!("screenshot save failed: {e}");
                        } else {
                            eprintln!("screenshot wrote {path}");
                        }
                    } else {
                        eprintln!("screenshot: widget had no texture yet");
                    }
                    std::process::exit(0);
                },
            );
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
                if !self.suppress_profile_dropdown {
                    if let Some(id) = self.profile_ids.get(idx as usize).cloned() {
                        if id != self.store.active_id() {
                            let _ = self.store.set_active(&id);
                            self.active_profile_index = idx;
                            if self.product.is_some() {
                                queue_msg(&sender, Msg::Rescore);
                            }
                        }
                    }
                }
            }
            Msg::OpenProfilesPage => {
                self.editing_profile = false;
                self.profile_editor = None;
                ui_profiles::clear_editor(&widgets.profiles_editor_body, &widgets.profiles_editor_footer);
                queue_msg(&sender, Msg::ShowPage(AppPage::Profiles));
            }
            Msg::Profiles(ev) => {
                handle_profiles_event(self, widgets, root, sender.clone(), ev);
            }
            Msg::RefreshProfilesUi => {
                refresh_profiles_page(widgets, self, &sender);
            }
            Msg::SyncProfileDropdown => {
                sync_profile_dropdown(widgets, self);
            }
            Msg::RelabelProfiles => {
                ui_profiles::relabel_profile_list(
                    &widgets.profiles_list,
                    &self.store,
                    self.selected_profile_id.as_deref(),
                );
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
                if page != AppPage::Profiles {
                    self.editing_profile = false;
                    self.profile_editor = None;
                    ui_profiles::clear_editor(
                        &widgets.profiles_editor_body,
                        &widgets.profiles_editor_footer,
                    );
                }
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
                } else if page == AppPage::Profiles {
                    if self.selected_profile_id.is_none() {
                        self.selected_profile_id = Some(self.store.active_id().to_string());
                    }
                    refresh_profiles_page(widgets, self, &sender);
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
                    // Best-effort portal ping; capture uses V4L2 and/or libcamera/GStreamer.
                    try_xdg_camera_portal_hint();
                    // Re-probe so a re-login / unplugged camera is reflected.
                    self.camera_probe = camera::camera_probe();
                    if !self.camera_ok() {
                        self.status_kind = StatusKind::Error;
                        self.status_text = self.camera_probe.unavailable_message();
                    } else {
                        let stop = Arc::new(AtomicBool::new(false));
                        self.scan_stop = Some(Arc::clone(&stop));
                        self.scanning = true;
                        self.preview = None;
                        self.status_kind = StatusKind::Loading;
                        self.status_text = "Opening camera…".into();
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
                        let first = self.preview.is_none();
                        let bytes = gtk::glib::Bytes::from_owned(rgb);
                        let texture = gdk::MemoryTexture::new(
                            width as i32,
                            height as i32,
                            gdk::MemoryFormat::R8g8b8,
                            &bytes,
                            stride,
                        );
                        self.preview = Some(texture.upcast());
                        if first {
                            self.status_text =
                                "Live preview on — center the barcode in the guide.".into();
                        }
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
                // Drive Set active hang smoke off the existing width poller (proves
                // the main loop is alive and exercises the real Profiles path).
                if let Some(left) = self.smoke_set_active_left {
                    if left == 0 {
                        eprintln!("SMOKE_SET_ACTIVE_OK");
                        std::process::exit(0);
                    }
                    self.smoke_tick = self.smoke_tick.wrapping_add(1);
                    // Fire every ~4th poll (~1s) so GTK can settle.
                    if self.smoke_tick > 4 && self.smoke_tick % 4 == 0 {
                        let ids = self.profile_ids.clone();
                        if ids.is_empty() {
                            eprintln!("SMOKE_SET_ACTIVE_FAIL empty profiles");
                            std::process::exit(1);
                        }
                        let idx = (4 - left) as usize % ids.len();
                        let id = ids[idx].clone();
                        eprintln!("SMOKE_SET_ACTIVE_STEP left={left} id={id}");
                        self.selected_profile_id = Some(id.clone());
                        self.smoke_set_active_left = Some(left - 1);
                        queue_msg(
                            &sender,
                            Msg::Profiles(ui_profiles::ProfilePageEvent::Select(id)),
                        );
                        queue_msg(
                            &sender,
                            Msg::Profiles(ui_profiles::ProfilePageEvent::SetActive),
                        );
                    }
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
            let lower = msg.to_lowercase();
            if lower.contains("permission")
                || lower.contains("denied")
                || lower.contains("eacces")
            {
                format!(
                    "Camera permission denied. On Alpine: \
                     `sudo adduser $USER video`, then re-login. Or type a barcode."
                )
            } else if lower.contains("busy") || lower.contains("ebusy") {
                format!("Camera busy. Close other apps using it, or type a barcode.")
            } else {
                // msg is already a compact summary from camera::open_first_working
                format!("Could not open camera. {msg} Or type a barcode.")
            }
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

/// Whether this looks like a phone/handheld session (Phosh etc.).
fn is_mobile_desktop_env() -> bool {
    let desk = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_DESKTOP").unwrap_or_default();
    let combined = format!("{desk};{session}").to_ascii_lowercase();
    combined.contains("phosh")
        || combined.contains("plasma-mobile")
        || combined.contains("lomiri")
        || combined.contains("mobile")
}

/// Handheld / phone-class display (e.g. OnePlus fajita 1920×1080 @ 16:9).
///
/// Used only to decide maximize-on-launch. Fractal/Snapshot do **not** detect
/// phones for fullscreen; Phosh fills the workarea when the window is maximized.
fn is_handheld_display(mon_w: i32, mon_h: i32, scale: i32, n_monitors: u32) -> bool {
    let short = mon_w.min(mon_h);
    let long = mon_w.max(mon_h);
    let single = n_monitors <= 1;
    if is_mobile_desktop_env() {
        return true;
    }
    // Classic small phones / very short workareas
    if short <= 900 || mon_h <= 700 {
        return true;
    }
    // 1080p-class single panel (fajita landscape 1920×1080 or portrait 1080×1920).
    // Desktop 1080p users: set LIBREFOOD_NO_FULLSCREEN=1 to keep a floating window.
    if single && short <= 1080 && long <= 2400 {
        return true;
    }
    if single && short <= 1080 && scale >= 2 {
        return true;
    }
    false
}

/// Soft default size inside the monitor (never ≥ display geom).
///
/// Snapshot ships 800×640; Fractal UI uses 900×850. We prefer Snapshot’s
/// smaller default so ultrawide/short phones never open “too wide/tall”
/// before maximize applies.
fn clamp_default_size(mon_w: i32, mon_h: i32, prefer_w: i32, prefer_h: i32) -> (i32, i32) {
    let max_w = ((mon_w as f64) * 0.92).round() as i32;
    let max_h = ((mon_h as f64) * 0.92).round() as i32;
    let min_w = 360.min(mon_w).max(1);
    let min_h = 294.min(mon_h).max(1);
    let w = prefer_w.clamp(min_w, max_w.max(min_w)).min(mon_w);
    let h = prefer_h.clamp(min_h, max_h.max(min_h)).min(mon_h);
    (w, h)
}

/// Place the window inside the current monitor (Fractal/Snapshot patterns).
///
/// * **Mobile / Phosh / small single panel:** `maximize()` into the workarea
///   (Snapshot restores maximized; neither Fractal nor Snapshot auto-fullscreen
///   on launch).
/// * **Desktop:** floating default ~800×640, resizable — no forced maximize.
/// * Defaults always clamped **below** monitor size so a delayed maximize never
///   leaves an oversized floating window.
///
/// Escape: `LIBREFOOD_NO_FULLSCREEN=1` skips maximize. Tests: `LIBREFOOD_WIDTH`/`HEIGHT`.
fn apply_window_placement(win: &adw::ApplicationWindow) {
    let Some(display) = gdk::Display::default() else {
        win.set_default_size(800, 640);
        return;
    };

    let monitors = display.monitors();
    let n_monitors = monitors.n_items();
    let monitor = win
        .surface()
        .and_then(|s| display.monitor_at_surface(&s))
        .or_else(|| {
            monitors
                .item(0)
                .and_then(|o| o.downcast::<gdk::Monitor>().ok())
        });

    let Some(monitor) = monitor else {
        win.set_default_size(800, 640);
        return;
    };

    let geom = monitor.geometry();
    let scale = monitor.scale_factor().max(1);
    // GDK geometry is in application pixels — never request larger.
    let mon_w = geom.width().max(360);
    let mon_h = geom.height().max(294);

    if let (Ok(w), Ok(h)) = (
        std::env::var("LIBREFOOD_WIDTH"),
        std::env::var("LIBREFOOD_HEIGHT"),
    ) {
        if let (Ok(w), Ok(h)) = (w.parse::<i32>(), h.parse::<i32>()) {
            let (cw, ch) = clamp_default_size(mon_w, mon_h, w, h);
            win.set_default_size(cw, ch);
            return;
        }
    }

    let handheld = is_handheld_display(mon_w, mon_h, scale, n_monitors);

    // Snapshot defaults (800×640). On very short landscape phones, prefer a
    // portrait-ish fallback so a failed maximize is still usable.
    let (prefer_w, prefer_h) = if handheld && mon_h < mon_w && mon_h <= 700 {
        (360.min(mon_w), mon_h.min(640))
    } else if handheld {
        (mon_w.min(800), mon_h.min(640))
    } else {
        (800, 640)
    };
    let (def_w, def_h) = clamp_default_size(mon_w, mon_h, prefer_w, prefer_h);
    win.set_default_size(def_w, def_h);
    win.set_size_request(360.min(mon_w), 294.min(mon_h));

    let no_fs = matches!(
        std::env::var("LIBREFOOD_NO_FULLSCREEN").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    );
    if no_fs {
        return;
    }

    if handheld {
        // Phosh / mobile: fill the shell workarea (panel stays). True
        // fullscreen is opt-in elsewhere; Fractal/Snapshot do not launch FS.
        win.maximize();
    }
    // Desktop: leave floating at the soft default — resizable, not forced FS.
}

/// Best-effort xdg-desktop-portal Camera presence check (Snapshot uses portal
/// + PipeWire for capture). We still open via V4L2; this only logs / warms
/// the portal when available.
fn try_xdg_camera_portal_hint() {
    use gtk::gio;
    use gtk::gio::prelude::*;
    use gtk::glib;

    let debug = matches!(
        std::env::var("LIBREFOOD_CAMERA_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    );

    let Ok(proxy) = gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::NONE,
        None,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Camera",
        gio::Cancellable::NONE,
    ) else {
        if debug {
            eprintln!("[librefood camera] xdg-desktop-portal Camera interface unavailable (OK on Alpine — using V4L2)");
        }
        return;
    };

    if let Some(v) = proxy.cached_property("IsCameraPresent") {
        if debug {
            eprintln!("[librefood camera] portal IsCameraPresent={v}");
        }
    }

    // Fire AccessCamera with empty options; ignore failures (no portal / denied).
    let opts = glib::VariantDict::new(None).end();
    let args = glib::Variant::tuple_from_iter(std::iter::once(opts));
    match proxy.call_sync(
        "AccessCamera",
        Some(&args),
        gio::DBusCallFlags::NONE,
        1500,
        gio::Cancellable::NONE,
    ) {
        Ok(ret) => {
            if debug {
                eprintln!("[librefood camera] portal AccessCamera started: {ret}");
            }
        }
        Err(e) => {
            if debug {
                eprintln!("[librefood camera] portal AccessCamera: {e} (falling back to V4L2)");
            }
        }
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
        .bottom-nav {
            padding: 4px 4px 6px;
            min-height: 48px;
        }
        .bottom-nav-btn {
            min-height: 44px;
            padding: 8px 2px;
            font-size: 0.82rem;
            font-weight: 600;
        }
        .library-row {
            padding: 6px 0;
            border-bottom: 1px solid alpha(@card_fg_color, 0.08);
        }
        .profile-row {
            padding: 8px 4px;
        }
        .profile-row-title { font-weight: 650; }
        .profile-row-selected {
            background: alpha(@accent_bg_color, 0.18);
            border-radius: 8px;
        }
        .profiles-actions-bar {
            padding-top: 8px;
            border-top: 1px solid alpha(@card_fg_color, 0.1);
        }
        .profiles-editor-footer {
            padding-top: 8px;
            border-top: 1px solid alpha(@card_fg_color, 0.12);
            background: @window_bg_color;
        }
        .criterion-card {
            margin-bottom: 4px;
        }
        .field-label {
            font-size: 0.85rem;
            font-weight: 600;
            opacity: 0.8;
            margin-top: 2px;
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod window_placement_tests {
    use super::{clamp_default_size, is_handheld_display};

    #[test]
    fn fajita_1080p_16x9_is_handheld() {
        // Physical or logical 1920×1080 landscape (OnePlus fajita).
        assert!(is_handheld_display(1920, 1080, 1, 1));
        assert!(is_handheld_display(1920, 1080, 2, 1));
        assert!(is_handheld_display(1080, 1920, 3, 1)); // portrait
        // Logical size after scale (960×540) still handheld via short≤900.
        assert!(is_handheld_display(960, 540, 1, 1));
    }

    #[test]
    fn large_desktop_not_handheld_by_size_alone() {
        assert!(!is_handheld_display(2560, 1440, 1, 1));
        assert!(!is_handheld_display(3840, 2160, 1, 2));
    }

    #[test]
    fn clamp_default_stays_inside_monitor() {
        let (w, h) = clamp_default_size(1920, 1080, 800, 640);
        assert!(w <= 1920 && h <= 1080);
        assert!(w <= ((1920.0 * 0.92) as i32) + 1);
        assert!(h <= ((1080.0 * 0.92) as i32) + 1);
        // Never request larger than the display even if prefer is huge.
        let (w2, h2) = clamp_default_size(800, 480, 2000, 2000);
        assert!(w2 <= 800 && h2 <= 480);
        assert!(w2 >= 360.min(800));
        assert!(h2 >= 294.min(480));
    }
}
