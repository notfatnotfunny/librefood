//! In-page scoring-profile list + scrollable editor (narrow-screen friendly).

use crate::engine::{
    Criterion, FactorPolarity, MissingDataPolicy, ScoringProfile, ThresholdRule, Tone, When,
};
use crate::profiles::ProfileStore;
use crate::product::attrs;
use gtk::prelude::*;
use relm4::gtk;
use std::cell::RefCell;
use std::rc::Rc;

const ATTR_CHOICES: &[(&str, &str)] = &[
    (attrs::ENERGY_KCAL_100G, "Energy (kcal/100g)"),
    (attrs::SUGARS_100G, "Sugars (g/100g)"),
    (attrs::SATURATED_FAT_100G, "Saturated fat (g/100g)"),
    (attrs::SALT_100G, "Salt (g/100g)"),
    (attrs::FIBER_100G, "Fiber (g/100g)"),
    (attrs::PROTEINS_100G, "Proteins (g/100g)"),
    (attrs::ADDITIVES_N, "Additives count"),
    (attrs::NOVA_GROUP, "NOVA group"),
    (attrs::NUTRISCORE_ORDINAL, "Nutri-Score ordinal (A=5)"),
    (attrs::ORGANIC, "Organic (0/1)"),
    (attrs::ECOSCORE, "Eco-Score (0–100)"),
    (attrs::ALLERGEN_COUNT, "Allergen tag count"),
];

/// Events emitted by the Profiles page widgets.
#[derive(Debug, Clone)]
pub enum ProfilePageEvent {
    Select(String),
    /// Open the New-name dialog (non-blocking).
    New,
    /// Create after the async name dialog completes.
    NewNamed(String),
    SetActive,
    Duplicate,
    /// Duplicate after the async name dialog completes.
    DuplicateNamed(String),
    StartEdit,
    Export,
    Import,
    /// Import after the async path dialog completes.
    ImportPath(String),
    Delete,
    CancelEdit,
    SaveEdit,
}

/// Collects the edited profile from mounted editor widgets.
pub struct EditorHandle {
    collect: Rc<dyn Fn() -> Result<ScoringProfile, String>>,
}

impl EditorHandle {
    pub fn collect(&self) -> Result<ScoringProfile, String> {
        (self.collect)()
    }
}

fn clear_container(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn profile_row_title(mark: &str, name: &str) -> String {
    format!("{mark}  {name}")
}

fn profile_row_sub(id: &str, version: u32, builtin: bool) -> String {
    let kind = if builtin { "built-in" } else { "custom" };
    format!("{id} · v{version} · {kind}")
}

/// Rebuild the selectable profile list (built-in + custom, active marked).
pub fn rebuild_profile_list(
    container: &gtk::Box,
    store: &ProfileStore,
    selected_id: Option<&str>,
    emit: &Rc<dyn Fn(ProfilePageEvent)>,
) {
    clear_container(container);
    let active = store.active_id().to_string();
    let selected = selected_id.unwrap_or(&active);

    for p in store.list() {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        row.set_hexpand(true);
        row.add_css_class("profile-row");
        if p.id == selected {
            row.add_css_class("profile-row-selected");
        }

        let mark = if p.id == active { "●" } else { "○" };
        let title = gtk::Label::new(Some(&profile_row_title(mark, &p.name)));
        title.add_css_class("profile-row-title");
        title.set_halign(gtk::Align::Start);
        title.set_xalign(0.0);
        title.set_wrap(true);

        let sub = gtk::Label::new(Some(&profile_row_sub(&p.id, p.version, p.builtin)));
        sub.add_css_class("dim-label");
        sub.set_halign(gtk::Align::Start);
        sub.set_xalign(0.0);
        sub.set_wrap(true);

        row.append(&title);
        row.append(&sub);

        let btn = gtk::Button::new();
        btn.set_child(Some(&row));
        btn.add_css_class("flat");
        btn.set_hexpand(true);
        // Widget name holds the profile id for in-place relabel.
        btn.set_widget_name(&p.id);
        let id = p.id.clone();
        let emit = Rc::clone(emit);
        btn.connect_clicked(move |_| {
            emit(ProfilePageEvent::Select(id.clone()));
        });
        container.append(&btn);
    }
}

/// Update ●/○ / selection styling without destroying buttons (Set active safe).
pub fn relabel_profile_list(
    container: &gtk::Box,
    store: &ProfileStore,
    selected_id: Option<&str>,
) {
    let active = store.active_id().to_string();
    let selected = selected_id.unwrap_or(&active);
    let mut child = container.first_child();
    while let Some(widget) = child {
        let next = widget.next_sibling();
        let Some(btn) = widget.downcast_ref::<gtk::Button>() else {
            child = next;
            continue;
        };
        let id = btn.widget_name().to_string();
        if id.is_empty() {
            child = next;
            continue;
        }
        let Some(row) = btn.child().and_then(|c| c.downcast::<gtk::Box>().ok()) else {
            child = next;
            continue;
        };
        if id == selected {
            row.add_css_class("profile-row-selected");
        } else {
            row.remove_css_class("profile-row-selected");
        }
        let mark = if id == active { "●" } else { "○" };
        let name = store
            .get(&id)
            .map(|p| p.name.as_str())
            .unwrap_or(id.as_str());
        let builtin = store.get(&id).map(|p| p.builtin).unwrap_or(false);
        let version = store.get(&id).map(|p| p.version).unwrap_or(1);
        if let Some(title) = row.first_child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
            title.set_text(&profile_row_title(mark, name));
        }
        if let Some(sub) = row
            .first_child()
            .and_then(|c| c.next_sibling())
            .and_then(|c| c.downcast::<gtk::Label>().ok())
        {
            sub.set_text(&profile_row_sub(&id, version, builtin));
        }
        child = next;
    }
}

/// Mount New / Duplicate / … action buttons into `actions`.
pub fn rebuild_profile_actions(actions: &gtk::Box, emit: &Rc<dyn Fn(ProfilePageEvent)>) {
    clear_container(actions);
    actions.set_orientation(gtk::Orientation::Vertical);
    actions.set_spacing(8);

    let flow = gtk::FlowBox::new();
    // 2-up wraps cleanly from ~360px; avoids clipped third columns on phones.
    flow.set_max_children_per_line(2);
    flow.set_min_children_per_line(1);
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_column_spacing(6);
    flow.set_row_spacing(6);
    flow.set_hexpand(true);

    let buttons: &[(&str, ProfilePageEvent, bool)] = &[
        ("New", ProfilePageEvent::New, true),
        ("Set active", ProfilePageEvent::SetActive, false),
        ("Duplicate", ProfilePageEvent::Duplicate, false),
        ("Edit", ProfilePageEvent::StartEdit, false),
        ("Export JSON", ProfilePageEvent::Export, false),
        ("Import JSON", ProfilePageEvent::Import, false),
        ("Delete", ProfilePageEvent::Delete, false),
    ];

    for (label, event, suggested) in buttons {
        let btn = gtk::Button::with_label(label);
        if *suggested {
            btn.add_css_class("suggested-action");
        }
        let emit = Rc::clone(emit);
        let event = event.clone();
        btn.connect_clicked(move |_| emit(event.clone()));
        flow.append(&btn);
    }
    actions.append(&flow);
}

/// Mount a full-page scrollable editor into `body`, sticky Save/Cancel into `footer`.
pub fn mount_profile_editor(
    body: &gtk::Box,
    footer: &gtk::Box,
    mut profile: ScoringProfile,
    emit: &Rc<dyn Fn(ProfilePageEvent)>,
) -> EditorHandle {
    clear_container(body);
    clear_container(footer);

    // Built-ins are opened only after the caller refused Edit; keep as custom copy
    // only if somehow reached (Duplicate is the supported path).
    debug_assert!(
        !profile.builtin,
        "built-in profiles must not open in the editor"
    );
    profile.builtin = false;

    body.set_orientation(gtk::Orientation::Vertical);
    body.set_spacing(12);
    body.set_margin_top(4);
    body.set_margin_bottom(8);
    body.set_margin_start(2);
    body.set_margin_end(2);

    let heading = gtk::Label::new(Some("Edit profile"));
    heading.add_css_class("brand-title");
    heading.set_halign(gtk::Align::Start);
    heading.set_xalign(0.0);
    body.append(&heading);

    let id_lab = gtk::Label::new(Some(&format!("id · {}", profile.id)));
    id_lab.add_css_class("dim-label");
    id_lab.set_halign(gtk::Align::Start);
    id_lab.set_xalign(0.0);
    id_lab.set_selectable(true);
    body.append(&id_lab);

    let name = gtk::Entry::builder().text(&profile.name).hexpand(true).build();
    name.set_placeholder_text(Some("Profile name"));
    body.append(&field_label("Name"));
    body.append(&name);

    let base = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
    base.set_value(profile.base_score);
    base.set_hexpand(true);
    body.append(&field_label("Base score"));
    body.append(&base);

    let missing = gtk::DropDown::from_strings(&["ignore", "penalize", "unknown", "incomplete"]);
    missing.set_selected(match profile.missing_data {
        MissingDataPolicy::Ignore => 0,
        MissingDataPolicy::Penalize => 1,
        MissingDataPolicy::Unknown => 2,
        MissingDataPolicy::Incomplete => 3,
    });
    missing.set_hexpand(true);
    body.append(&field_label("Missing-data policy"));
    body.append(&missing);

    body.append(&field_label("Criteria (weight + two-band threshold)"));

    struct RowW {
        title: gtk::Entry,
        attr: gtk::DropDown,
        weight: gtk::SpinButton,
        low_max: gtk::SpinButton,
        low_adj: gtk::SpinButton,
        high_adj: gtk::SpinButton,
        polarity: gtk::DropDown,
        id: String,
    }
    let rows: Rc<RefCell<Vec<RowW>>> = Rc::new(RefCell::new(Vec::new()));

    for c in &profile.criteria {
        let frame = gtk::Frame::new(Some(&c.id));
        frame.add_css_class("criterion-card");
        let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
        col.set_margin_top(8);
        col.set_margin_bottom(8);
        col.set_margin_start(8);
        col.set_margin_end(8);

        let title = gtk::Entry::builder().text(&c.title).hexpand(true).build();
        let labels: Vec<&str> = ATTR_CHOICES.iter().map(|(_, l)| *l).collect();
        let attr = gtk::DropDown::from_strings(&labels);
        attr.set_selected(
            ATTR_CHOICES
                .iter()
                .position(|(k, _)| *k == c.attribute)
                .unwrap_or(0) as u32,
        );
        attr.set_hexpand(true);
        let weight = gtk::SpinButton::with_range(0.0, 5.0, 0.1);
        weight.set_value(c.weight);
        weight.set_hexpand(true);
        let polarity = gtk::DropDown::from_strings(&["negative", "positive", "neutral"]);
        polarity.set_selected(match c.polarity {
            FactorPolarity::Negative => 0,
            FactorPolarity::Positive => 1,
            FactorPolarity::Neutral => 2,
        });
        polarity.set_hexpand(true);
        let (low_max_v, low_adj_v, high_adj_v) = simplify_rules(&c.rules);
        let low_max = gtk::SpinButton::with_range(-1000.0, 1000.0, 0.5);
        low_max.set_value(low_max_v);
        low_max.set_hexpand(true);
        let low_adj = gtk::SpinButton::with_range(-50.0, 50.0, 1.0);
        low_adj.set_value(low_adj_v);
        low_adj.set_hexpand(true);
        let high_adj = gtk::SpinButton::with_range(-50.0, 50.0, 1.0);
        high_adj.set_value(high_adj_v);
        high_adj.set_hexpand(true);

        col.append(&field_label("Title"));
        col.append(&title);
        col.append(&field_label("Attribute"));
        col.append(&attr);
        col.append(&field_label("Weight"));
        col.append(&weight);
        col.append(&field_label("Polarity"));
        col.append(&polarity);
        col.append(&field_label("When value ≤"));
        col.append(&low_max);
        col.append(&field_label("Adjust (≤ band)"));
        col.append(&low_adj);
        col.append(&field_label("Adjust (otherwise)"));
        col.append(&high_adj);

        frame.set_child(Some(&col));
        body.append(&frame);
        rows.borrow_mut().push(RowW {
            title,
            attr,
            weight,
            low_max,
            low_adj,
            high_adj,
            polarity,
            id: c.id.clone(),
        });
    }

    let tip = gtk::Label::new(Some(
        "For full rule lists, caps, and category overrides: Export JSON, edit, Import.",
    ));
    tip.set_wrap(true);
    tip.set_xalign(0.0);
    tip.add_css_class("dim-label");
    body.append(&tip);

    let err = gtk::Label::new(None);
    err.set_xalign(0.0);
    err.set_wrap(true);
    err.add_css_class("status-err");
    body.append(&err);

    // Sticky footer actions
    footer.set_orientation(gtk::Orientation::Vertical);
    footer.set_spacing(8);
    footer.add_css_class("profiles-editor-footer");
    footer.set_margin_top(8);
    footer.set_margin_bottom(4);

    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_homogeneous(true);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel");
    btn_row.append(&save);
    btn_row.append(&cancel);
    footer.append(&btn_row);

    let profile_id = profile.id.clone();
    let profile_desc = profile.description.clone();
    let profile_ver = profile.version;
    let labels = profile.labels.clone();
    let caps = profile.caps.clone();
    let category_overrides = profile.category_overrides.clone();

    let collect: Rc<dyn Fn() -> Result<ScoringProfile, String>> = {
        let name = name.clone();
        let base = base.clone();
        let missing = missing.clone();
        let rows = Rc::clone(&rows);
        let err = err.clone();
        Rc::new(move || {
            let mut criteria = Vec::new();
            for r in rows.borrow().iter() {
                let title = r.title.text().to_string();
                let attribute = ATTR_CHOICES
                    .get(r.attr.selected() as usize)
                    .map(|(k, _)| (*k).to_string())
                    .unwrap_or_else(|| attrs::SUGARS_100G.to_string());
                let thr = r.low_max.value();
                let low = r.low_adj.value();
                let high = r.high_adj.value();
                let polarity = match r.polarity.selected() {
                    1 => FactorPolarity::Positive,
                    2 => FactorPolarity::Neutral,
                    _ => FactorPolarity::Negative,
                };
                criteria.push(Criterion {
                    id: r.id.clone(),
                    title,
                    attribute,
                    weight: r.weight.value(),
                    polarity,
                    rules: vec![
                        ThresholdRule {
                            when: When::Lte { value: thr },
                            adjust: low,
                            label: format!("≤ {thr}"),
                            tone: if low >= 0.0 {
                                Tone::Good
                            } else {
                                Tone::Mediocre
                            },
                        },
                        ThresholdRule {
                            when: When::Gt { value: thr },
                            adjust: high,
                            label: format!("> {thr}"),
                            tone: if high >= 0.0 {
                                Tone::Good
                            } else {
                                Tone::Poor
                            },
                        },
                    ],
                    missing_policy: None,
                    missing_penalty: None,
                });
            }
            let missing_data = match missing.selected() {
                1 => MissingDataPolicy::Penalize,
                2 => MissingDataPolicy::Unknown,
                3 => MissingDataPolicy::Incomplete,
                _ => MissingDataPolicy::Ignore,
            };
            let out = ScoringProfile {
                id: profile_id.clone(),
                name: name.text().to_string(),
                description: profile_desc.clone(),
                version: profile_ver.max(1).saturating_add(1),
                scale: Default::default(),
                base_score: base.value(),
                missing_data,
                default_missing_penalty: -5.0,
                labels: labels.clone(),
                criteria,
                caps: caps.clone(),
                category_overrides: category_overrides.clone(),
                builtin: false,
            };
            if let Err(e) = out.validate() {
                err.set_text(&e);
                return Err(e);
            }
            err.set_text("");
            Ok(out)
        })
    };

    {
        let emit = Rc::clone(emit);
        save.connect_clicked(move |_| emit(ProfilePageEvent::SaveEdit));
    }
    {
        let emit = Rc::clone(emit);
        cancel.connect_clicked(move |_| emit(ProfilePageEvent::CancelEdit));
    }

    EditorHandle { collect }
}

fn field_label(text: &str) -> gtk::Label {
    let lab = gtk::Label::new(Some(text));
    lab.set_halign(gtk::Align::Start);
    lab.set_xalign(0.0);
    lab.add_css_class("field-label");
    lab
}

/// Non-blocking text prompt — never pumps/`recv` on the UI thread.
pub fn ask_text_async(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    placeholder: &str,
    initial: &str,
    on_done: impl FnOnce(Option<String>) + 'static,
) {
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(320)
        .resizable(true)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(14);
    root.set_margin_bottom(14);
    root.set_margin_start(14);
    root.set_margin_end(14);

    let entry = gtk::Entry::builder()
        .text(initial)
        .placeholder_text(placeholder)
        .hexpand(true)
        .build();
    root.append(&entry);

    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_homogeneous(true);
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel");
    btn_row.append(&ok);
    btn_row.append(&cancel);
    root.append(&btn_row);
    dialog.set_child(Some(&root));

    let done = Rc::new(RefCell::new(Some(on_done)));
    let finish: Rc<dyn Fn(Option<String>)> = {
        let done = Rc::clone(&done);
        let dialog = dialog.clone();
        Rc::new(move |value: Option<String>| {
            if let Some(cb) = done.borrow_mut().take() {
                cb(value);
            }
            dialog.destroy();
        })
    };

    ok.connect_clicked({
        let entry = entry.clone();
        let finish = Rc::clone(&finish);
        move |_| {
            let t = entry.text().to_string();
            finish(if t.trim().is_empty() { None } else { Some(t) });
        }
    });
    cancel.connect_clicked({
        let finish = Rc::clone(&finish);
        move |_| finish(None)
    });
    dialog.connect_close_request({
        let finish = Rc::clone(&finish);
        move |_| {
            finish(None);
            gtk::glib::Propagation::Proceed
        }
    });
    entry.connect_activate({
        let ok = ok.clone();
        move |_| ok.emit_clicked()
    });

    dialog.present();
    entry.grab_focus();
}

/// Path prompt for Import JSON (async).
pub fn ask_path_async(
    parent: &impl IsA<gtk::Window>,
    on_done: impl FnOnce(Option<String>) + 'static,
) {
    ask_text_async(
        parent,
        "Import profile JSON",
        "Absolute path to .json",
        "",
        on_done,
    )
}

fn simplify_rules(rules: &[ThresholdRule]) -> (f64, f64, f64) {
    let mut low_max = 5.0;
    let mut low_adj = 0.0;
    let mut high_adj = 0.0;
    if let Some(r0) = rules.first() {
        if let When::Lte { value } = &r0.when {
            low_max = *value;
            low_adj = r0.adjust;
        } else if matches!(r0.when, When::Always) {
            low_adj = r0.adjust;
            high_adj = r0.adjust;
        }
    }
    if let Some(r1) = rules.get(1) {
        high_adj = r1.adjust;
    }
    for r in rules {
        if let When::Gt { .. } | When::Gte { .. } = r.when {
            high_adj = r.adjust;
        }
    }
    (low_max, low_adj, high_adj)
}

/// Clear editor containers when leaving edit mode.
pub fn clear_editor(body: &gtk::Box, footer: &gtk::Box) {
    clear_container(body);
    clear_container(footer);
}
