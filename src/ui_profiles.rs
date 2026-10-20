//! Imperative GTK dialogs for scoring-profile management.

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

pub struct ProfileDialogResult {
    pub active_id: String,
    pub store_changed: bool,
}

fn pump_until(rx: &std::sync::mpsc::Receiver<()>) {
    while rx.try_recv().is_err() {
        while gtk::glib::MainContext::default().iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

/// List / activate / duplicate / delete / import / export / edit profiles.
pub fn run_manage_dialog(
    parent: &impl IsA<gtk::Window>,
    store: &mut ProfileStore,
) -> ProfileDialogResult {
    let dialog = gtk::Window::builder()
        .title("Scoring profiles")
        .transient_for(parent)
        .modal(true)
        .default_width(540)
        .default_height(580)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let hint = gtk::Label::new(Some(
        "Declarative JSON profiles — not medical advice. Duplicate a built-in to customize.",
    ));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    root.append(&hint);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&list)
        .build();
    root.append(&scroll);

    let id_order: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let refresh = {
        let list = list.clone();
        let id_order = Rc::clone(&id_order);
        move |store: &ProfileStore| {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let mut ids = Vec::new();
            let active = store.active_id().to_string();
            for (i, p) in store.list().iter().enumerate() {
                let row = gtk::ListBoxRow::new();
                let mark = if p.id == active { "● " } else { "○ " };
                let builtin = if p.builtin { " (built-in)" } else { "" };
                let label = gtk::Label::new(Some(&format!(
                    "{mark}{} — {} v{}{builtin}",
                    p.name, p.id, p.version
                )));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                row.set_child(Some(&label));
                list.append(&row);
                if p.id == active {
                    list.select_row(Some(&row));
                }
                ids.push(p.id.clone());
                let _ = i;
            }
            *id_order.borrow_mut() = ids;
        }
    };
    refresh(store);

    let selected_id = {
        let list = list.clone();
        let id_order = Rc::clone(&id_order);
        move || -> Option<String> {
            let row = list.selected_row()?;
            let idx = row.index();
            if idx < 0 {
                return None;
            }
            id_order.borrow().get(idx as usize).cloned()
        }
    };

    let btn_row = gtk::FlowBox::new();
    btn_row.set_max_children_per_line(4);
    btn_row.set_selection_mode(gtk::SelectionMode::None);
    let btn_new = gtk::Button::with_label("New profile…");
    btn_new.add_css_class("suggested-action");
    let btn_active = gtk::Button::with_label("Set active");
    let btn_dup = gtk::Button::with_label("Duplicate");
    let btn_edit = gtk::Button::with_label("Edit…");
    let btn_export = gtk::Button::with_label("Export JSON");
    let btn_import = gtk::Button::with_label("Import JSON");
    let btn_delete = gtk::Button::with_label("Delete override");
    let btn_close = gtk::Button::with_label("Close");
    for b in [
        &btn_new,
        &btn_active,
        &btn_dup,
        &btn_edit,
        &btn_export,
        &btn_import,
        &btn_delete,
        &btn_close,
    ] {
        btn_row.append(b);
    }
    root.append(&btn_row);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    root.append(&status);
    dialog.set_child(Some(&root));

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let changed = Rc::new(std::cell::Cell::new(false));
    // Safety: dialog is modal and store lives for the dialog lifetime on the same thread.
    let store_ptr = store as *mut ProfileStore;

    {
        let refresh = refresh.clone();
        let status = status.clone();
        let changed = Rc::clone(&changed);
        let dialog = dialog.clone();
        btn_new.connect_clicked(move |_| {
            let store = unsafe { &mut *store_ptr };
            let Some(name) = ask_text_dialog(
                &dialog,
                "New scoring profile",
                "Name",
                "My scoring profile",
            ) else {
                return;
            };
            match store.create_new(&name) {
                Ok(created) => {
                    changed.set(true);
                    // Offer immediate edit of weights / thresholds.
                    let to_edit = store.get(&created.id).cloned().unwrap_or(created.clone());
                    if let Some(edited) = edit_profile_dialog(&dialog, to_edit) {
                        match store.save(edited) {
                            Ok(()) => status.set_text(&format!(
                                "Created and saved “{}” ({}).",
                                name, created.id
                            )),
                            Err(e) => status.set_text(&format!(
                                "Created {}, but save after edit failed: {e}",
                                created.id
                            )),
                        }
                    } else {
                        status.set_text(&format!(
                            "Created “{}” ({}) — saved under ~/.config/librefood/profiles/",
                            name, created.id
                        ));
                    }
                    let _ = store.set_active(&created.id);
                    refresh(store);
                }
                Err(e) => status.set_text(&e),
            }
        });
    }
    {
        let refresh = refresh.clone();
        let selected_id = selected_id.clone();
        let status = status.clone();
        let changed = Rc::clone(&changed);
        btn_active.connect_clicked(move |_| {
            let store = unsafe { &mut *store_ptr };
            if let Some(id) = selected_id() {
                match store.set_active(&id) {
                    Ok(()) => {
                        changed.set(true);
                        refresh(store);
                        status.set_text(&format!("Active: {id}"));
                    }
                    Err(e) => status.set_text(&e),
                }
            }
        });
    }
    {
        let refresh = refresh.clone();
        let selected_id = selected_id.clone();
        let status = status.clone();
        let changed = Rc::clone(&changed);
        btn_dup.connect_clicked(move |_| {
            let store = unsafe { &mut *store_ptr };
            if let Some(id) = selected_id() {
                let name = format!(
                    "{} (copy)",
                    store.get(&id).map(|p| p.name.as_str()).unwrap_or("Profile")
                );
                match store.duplicate(&id, &name) {
                    Ok(p) => {
                        let _ = store.set_active(&p.id);
                        changed.set(true);
                        refresh(store);
                        status.set_text(&format!("Duplicated as {}", p.id));
                    }
                    Err(e) => status.set_text(&e),
                }
            }
        });
    }
    {
        let refresh = refresh.clone();
        let selected_id = selected_id.clone();
        let status = status.clone();
        let changed = Rc::clone(&changed);
        let dialog = dialog.clone();
        btn_edit.connect_clicked(move |_| {
            let store = unsafe { &mut *store_ptr };
            if let Some(id) = selected_id() {
                if let Some(profile) = store.get(&id).cloned() {
                    if let Some(edited) = edit_profile_dialog(&dialog, profile) {
                        match store.save(edited) {
                            Ok(()) => {
                                changed.set(true);
                                refresh(store);
                                status.set_text("Profile saved.");
                            }
                            Err(e) => status.set_text(&e),
                        }
                    }
                }
            }
        });
    }
    {
        let selected_id = selected_id.clone();
        let status = status.clone();
        let dialog = dialog.clone();
        btn_export.connect_clicked(move |_| {
            let store = unsafe { &*store_ptr };
            if let Some(id) = selected_id() {
                match store.export_json(&id) {
                    Ok(json) => {
                        let path = store.root().join(format!("{id}-export.json"));
                        match std::fs::write(&path, json) {
                            Ok(()) => status.set_text(&format!("Wrote {}", path.display())),
                            Err(e) => status.set_text(&e.to_string()),
                        }
                        let _ = &dialog;
                    }
                    Err(e) => status.set_text(&e),
                }
            }
        });
    }
    {
        let refresh = refresh.clone();
        let status = status.clone();
        let changed = Rc::clone(&changed);
        let dialog = dialog.clone();
        btn_import.connect_clicked(move |_| {
            let store = unsafe { &mut *store_ptr };
            // Simple import: paste path via Entry dialog
            let ask = gtk::Window::builder()
                .title("Import profile JSON")
                .transient_for(&dialog)
                .modal(true)
                .default_width(420)
                .build();
            let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
            box_.set_margin_top(12);
            box_.set_margin_bottom(12);
            box_.set_margin_start(12);
            box_.set_margin_end(12);
            let entry = gtk::Entry::builder()
                .placeholder_text("Absolute path to .json")
                .hexpand(true)
                .build();
            let ok = gtk::Button::with_label("Import");
            ok.add_css_class("suggested-action");
            box_.append(&entry);
            box_.append(&ok);
            ask.set_child(Some(&box_));
            let (txi, rxi) = std::sync::mpsc::channel::<Option<String>>();
            ok.connect_clicked({
                let ask = ask.clone();
                let entry = entry.clone();
                let txi = txi.clone();
                move |_| {
                    let _ = txi.send(Some(entry.text().to_string()));
                    ask.close();
                }
            });
            ask.connect_close_request({
                let txi = txi.clone();
                move |_| {
                    let _ = txi.send(None);
                    gtk::glib::Propagation::Proceed
                }
            });
            ask.present();
            let path = loop {
                if let Ok(v) = rxi.try_recv() {
                    break v;
                }
                while gtk::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(16));
            };
            ask.destroy();
            if let Some(path) = path {
                match std::fs::read_to_string(path.trim()) {
                    Ok(text) => match store.import_json(&text) {
                        Ok(p) => {
                            changed.set(true);
                            refresh(store);
                            status.set_text(&format!("Imported {}", p.id));
                        }
                        Err(e) => status.set_text(&e),
                    },
                    Err(e) => status.set_text(&e.to_string()),
                }
            }
        });
    }
    {
        let refresh = refresh.clone();
        let selected_id = selected_id.clone();
        let status = status.clone();
        let changed = Rc::clone(&changed);
        btn_delete.connect_clicked(move |_| {
            let store = unsafe { &mut *store_ptr };
            if let Some(id) = selected_id() {
                match store.delete(&id) {
                    Ok(()) => {
                        changed.set(true);
                        refresh(store);
                        status.set_text("Removed user override / custom profile.");
                    }
                    Err(e) => status.set_text(&e),
                }
            }
        });
    }

    btn_close.connect_clicked({
        let dialog = dialog.clone();
        let tx = tx.clone();
        move |_| {
            dialog.close();
            let _ = tx.send(());
        }
    });
    dialog.connect_close_request({
        let tx = tx.clone();
        move |_| {
            let _ = tx.send(());
            gtk::glib::Propagation::Proceed
        }
    });

    dialog.present();
    pump_until(&rx);
    dialog.destroy();

    ProfileDialogResult {
        active_id: store.active_id().to_string(),
        store_changed: changed.get(),
    }
}

fn ask_text_dialog(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    label: &str,
    initial: &str,
) -> Option<String> {
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(400)
        .build();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 10);
    box_.set_margin_top(14);
    box_.set_margin_bottom(14);
    box_.set_margin_start(14);
    box_.set_margin_end(14);
    box_.append(&gtk::Label::new(Some(label)));
    let entry = gtk::Entry::builder()
        .text(initial)
        .hexpand(true)
        .activates_default(true)
        .build();
    box_.append(&entry);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let ok = gtk::Button::with_label("Create");
    ok.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel");
    row.append(&ok);
    row.append(&cancel);
    box_.append(&row);
    dialog.set_child(Some(&box_));

    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    ok.connect_clicked({
        let dialog = dialog.clone();
        let entry = entry.clone();
        let tx = tx.clone();
        move |_| {
            let text = entry.text().trim().to_string();
            let _ = tx.send(if text.is_empty() { None } else { Some(text) });
            dialog.close();
        }
    });
    entry.connect_activate({
        let dialog = dialog.clone();
        let entry = entry.clone();
        let tx = tx.clone();
        move |_| {
            let text = entry.text().trim().to_string();
            let _ = tx.send(if text.is_empty() { None } else { Some(text) });
            dialog.close();
        }
    });
    cancel.connect_clicked({
        let dialog = dialog.clone();
        let tx = tx.clone();
        move |_| {
            let _ = tx.send(None);
            dialog.close();
        }
    });
    dialog.connect_close_request({
        let tx = tx.clone();
        move |_| {
            let _ = tx.send(None);
            gtk::glib::Propagation::Proceed
        }
    });

    dialog.present();
    entry.grab_focus();
    let mut result = None;
    loop {
        if let Ok(v) = rx.try_recv() {
            result = v;
            break;
        }
        while gtk::glib::MainContext::default().iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    dialog.destroy();
    result
}

fn edit_profile_dialog(
    parent: &impl IsA<gtk::Window>,
    mut profile: ScoringProfile,
) -> Option<ScoringProfile> {
    if profile.builtin {
        profile.id = format!("{}-custom", profile.id);
        profile.name = format!("{} (custom)", profile.name);
        profile.builtin = false;
    }

    let dialog = gtk::Window::builder()
        .title("Edit scoring profile")
        .transient_for(parent)
        .modal(true)
        .default_width(560)
        .default_height(640)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let name = gtk::Entry::builder().text(&profile.name).build();
    root.append(&gtk::Label::new(Some("Name")));
    root.append(&name);

    let base = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
    base.set_value(profile.base_score);
    root.append(&gtk::Label::new(Some("Base score")));
    root.append(&base);

    let missing = gtk::DropDown::from_strings(&["ignore", "penalize", "unknown", "incomplete"]);
    missing.set_selected(match profile.missing_data {
        MissingDataPolicy::Ignore => 0,
        MissingDataPolicy::Penalize => 1,
        MissingDataPolicy::Unknown => 2,
        MissingDataPolicy::Incomplete => 3,
    });
    root.append(&gtk::Label::new(Some("Missing-data policy")));
    root.append(&missing);

    let crit_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&crit_box)
        .build();
    root.append(&gtk::Label::new(Some(
        "Criteria: weight + two-band threshold (≤ / >)",
    )));
    root.append(&scroll);

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
    let mut rows: Vec<RowW> = Vec::new();

    for c in &profile.criteria {
        let frame = gtk::Frame::new(Some(&c.id));
        let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        col.set_margin_top(6);
        col.set_margin_bottom(6);
        col.set_margin_start(6);
        col.set_margin_end(6);

        let title = gtk::Entry::builder().text(&c.title).build();
        let labels: Vec<&str> = ATTR_CHOICES.iter().map(|(_, l)| *l).collect();
        let attr = gtk::DropDown::from_strings(&labels);
        attr.set_selected(
            ATTR_CHOICES
                .iter()
                .position(|(k, _)| *k == c.attribute)
                .unwrap_or(0) as u32,
        );
        let weight = gtk::SpinButton::with_range(0.0, 5.0, 0.1);
        weight.set_value(c.weight);
        let polarity = gtk::DropDown::from_strings(&["negative", "positive", "neutral"]);
        polarity.set_selected(match c.polarity {
            FactorPolarity::Negative => 0,
            FactorPolarity::Positive => 1,
            FactorPolarity::Neutral => 2,
        });
        let (low_max_v, low_adj_v, high_adj_v) = simplify_rules(&c.rules);
        let low_max = gtk::SpinButton::with_range(-1000.0, 1000.0, 0.5);
        low_max.set_value(low_max_v);
        let low_adj = gtk::SpinButton::with_range(-50.0, 50.0, 1.0);
        low_adj.set_value(low_adj_v);
        let high_adj = gtk::SpinButton::with_range(-50.0, 50.0, 1.0);
        high_adj.set_value(high_adj_v);

        col.append(&gtk::Label::new(Some("Title")));
        col.append(&title);
        col.append(&gtk::Label::new(Some("Attribute")));
        col.append(&attr);
        col.append(&gtk::Label::new(Some("Weight / polarity")));
        let wp = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        wp.append(&weight);
        wp.append(&polarity);
        col.append(&wp);
        let band = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        band.append(&gtk::Label::new(Some("≤")));
        band.append(&low_max);
        band.append(&gtk::Label::new(Some("adj")));
        band.append(&low_adj);
        band.append(&gtk::Label::new(Some("else")));
        band.append(&high_adj);
        col.append(&band);
        frame.set_child(Some(&col));
        crit_box.append(&frame);
        rows.push(RowW {
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
        "For full rule lists / caps / category overrides: Export JSON, edit, Import.",
    ));
    tip.set_wrap(true);
    tip.add_css_class("dim-label");
    root.append(&tip);

    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel");
    btn_row.append(&save);
    btn_row.append(&cancel);
    root.append(&btn_row);
    let err = gtk::Label::new(None);
    err.set_xalign(0.0);
    root.append(&err);
    dialog.set_child(Some(&root));

    let (tx, rx) = std::sync::mpsc::channel::<Option<ScoringProfile>>();

    {
        let name = name.clone();
        let base = base.clone();
        let missing = missing.clone();
        let err = err.clone();
        let tx = tx.clone();
        let dialog = dialog.clone();
        let profile_id = profile.id.clone();
        let profile_desc = profile.description.clone();
        let profile_ver = profile.version;
        let labels = profile.labels.clone();
        let caps = profile.caps.clone();
        let category_overrides = profile.category_overrides.clone();
        let rows_snap: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.title.clone(),
                    r.attr.clone(),
                    r.weight.clone(),
                    r.low_max.clone(),
                    r.low_adj.clone(),
                    r.high_adj.clone(),
                    r.polarity.clone(),
                    r.id.clone(),
                )
            })
            .collect();

        save.connect_clicked(move |_| {
            let mut criteria = Vec::new();
            for (title_e, attr_dd, weight_sb, low_max_sb, low_adj_sb, high_adj_sb, pol_dd, id) in
                &rows_snap
            {
                let title = title_e.text().to_string();
                let attribute = ATTR_CHOICES
                    .get(attr_dd.selected() as usize)
                    .map(|(k, _)| (*k).to_string())
                    .unwrap_or_else(|| attrs::SUGARS_100G.to_string());
                let thr = low_max_sb.value();
                let low = low_adj_sb.value();
                let high = high_adj_sb.value();
                let polarity = match pol_dd.selected() {
                    1 => FactorPolarity::Positive,
                    2 => FactorPolarity::Neutral,
                    _ => FactorPolarity::Negative,
                };
                criteria.push(Criterion {
                    id: id.clone(),
                    title,
                    attribute,
                    weight: weight_sb.value(),
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
            let mut out = ScoringProfile {
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
                return;
            }
            let _ = tx.send(Some(out));
            dialog.close();
        });
    }
    cancel.connect_clicked({
        let dialog = dialog.clone();
        let tx = tx.clone();
        move |_| {
            let _ = tx.send(None);
            dialog.close();
        }
    });
    dialog.connect_close_request({
        let tx = tx.clone();
        move |_| {
            let _ = tx.send(None);
            gtk::glib::Propagation::Proceed
        }
    });

    dialog.present();
    let mut result = None;
    loop {
        if let Ok(v) = rx.try_recv() {
            result = v;
            break;
        }
        while gtk::glib::MainContext::default().iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    dialog.destroy();
    result
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
