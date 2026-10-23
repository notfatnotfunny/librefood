//! Local History and Favorites persistence under `~/.config/librefood/`.

use crate::product::ProductModel;
use crate::score::ScoreSummary;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HISTORY_CAP: usize = 100;

/// Shared snapshot of a looked-up product for history / favorites lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub barcode: String,
    pub name: String,
    pub brand: Option<String>,
    pub score_display: Option<String>,
    pub score_label: Option<String>,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    /// Unix timestamp (seconds).
    pub timestamp: u64,
    /// Full product model for offline open (absent on older history.json rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<ProductModel>,
}

impl LibraryEntry {
    pub fn from_scored(product: &ProductModel, summary: &ScoreSummary) -> Self {
        Self {
            barcode: product.barcode.clone(),
            name: product.name.clone(),
            brand: product
                .brand_primary
                .clone()
                .or_else(|| product.brands.clone()),
            score_display: Some(summary.score_display.clone()),
            score_label: Some(summary.label.clone()),
            profile_id: Some(summary.profile_id.clone()),
            profile_name: Some(summary.profile_name.clone()),
            timestamp: now_secs(),
            product: Some(product.clone()),
        }
    }

    pub fn brand_label(&self) -> String {
        self.brand.clone().unwrap_or_default()
    }

    pub fn date_label(&self) -> String {
        format_timestamp(self.timestamp)
    }

    /// Traffic-light status inferred from stored score label.
    pub fn score_status(&self) -> crate::score::FactorStatus {
        use crate::score::FactorStatus;
        match self.score_label.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
            s if s.contains("excellent") || s.contains("good") => FactorStatus::Good,
            s if s.contains("mediocre") || s.contains("fair") || s.contains("ok") => {
                FactorStatus::Mediocre
            }
            s if s.contains("poor") || s.contains("bad") => FactorStatus::Poor,
            _ => FactorStatus::Mediocre,
        }
    }

    /// Cached product usable for the product page without a network round-trip.
    pub fn cached_product(&self) -> Option<&ProductModel> {
        self.product
            .as_ref()
            .filter(|p| !p.barcode.is_empty() && !p.name.is_empty())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_timestamp(ts: u64) -> String {
    // Simple UTC-ish display without chrono dep: YYYY-MM-DD HH:MM
    let days = ts / 86400;
    let rem = ts % 86400;
    let hours = rem / 3600;
    let mins = (rem % 3600) / 60;
    // Civil date from days since epoch (proleptic Gregorian)
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02} {hours:02}:{mins:02}")
}

/// Howard Hinnant’s civil_from_days (days since 1970-01-01 → y/m/d).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn config_dir() -> Result<PathBuf, String> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("librefood"));
    }
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home).join(".config/librefood"))
}

fn load_entries(path: &Path) -> Vec<LibraryEntry> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_entries(path: &Path, entries: &[LibraryEntry]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    entries: Vec<LibraryEntry>,
}

impl HistoryStore {
    pub fn open_default() -> Result<Self, String> {
        let path = config_dir()?.join("history.json");
        Ok(Self::open(path))
    }

    pub fn open(path: PathBuf) -> Self {
        let entries = load_entries(&path);
        Self { path, entries }
    }

    pub fn entries(&self) -> &[LibraryEntry] {
        &self.entries
    }

    pub fn find(&self, barcode: &str) -> Option<&LibraryEntry> {
        self.entries.iter().find(|e| e.barcode == barcode)
    }

    pub fn record(&mut self, entry: LibraryEntry) -> Result<(), String> {
        self.entries.retain(|e| e.barcode != entry.barcode);
        self.entries.insert(0, entry);
        if self.entries.len() > HISTORY_CAP {
            self.entries.truncate(HISTORY_CAP);
        }
        save_entries(&self.path, &self.entries)
    }

    pub fn remove(&mut self, barcode: &str) -> Result<(), String> {
        self.entries.retain(|e| e.barcode != barcode);
        save_entries(&self.path, &self.entries)
    }

    pub fn clear(&mut self) -> Result<(), String> {
        self.entries.clear();
        save_entries(&self.path, &self.entries)
    }
}

#[derive(Debug)]
pub struct FavoritesStore {
    path: PathBuf,
    entries: Vec<LibraryEntry>,
}

impl FavoritesStore {
    pub fn open_default() -> Result<Self, String> {
        let path = config_dir()?.join("favorites.json");
        Ok(Self::open(path))
    }

    pub fn open(path: PathBuf) -> Self {
        let entries = load_entries(&path);
        Self { path, entries }
    }

    pub fn entries(&self) -> &[LibraryEntry] {
        &self.entries
    }

    pub fn find(&self, barcode: &str) -> Option<&LibraryEntry> {
        self.entries.iter().find(|e| e.barcode == barcode)
    }

    pub fn contains(&self, barcode: &str) -> bool {
        self.entries.iter().any(|e| e.barcode == barcode)
    }

    pub fn add(&mut self, entry: LibraryEntry) -> Result<(), String> {
        self.entries.retain(|e| e.barcode != entry.barcode);
        self.entries.insert(0, entry);
        save_entries(&self.path, &self.entries)
    }

    pub fn remove(&mut self, barcode: &str) -> Result<(), String> {
        self.entries.retain(|e| e.barcode != barcode);
        save_entries(&self.path, &self.entries)
    }

    pub fn toggle(&mut self, entry: LibraryEntry) -> Result<bool, String> {
        if self.contains(&entry.barcode) {
            self.remove(&entry.barcode)?;
            Ok(false)
        } else {
            self.add(entry)?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::{attrs, AttrValue};
    use std::collections::BTreeMap;

    fn sample_product(barcode: &str, name: &str) -> ProductModel {
        let mut attributes = BTreeMap::new();
        attributes.insert(
            attrs::SUGARS_100G.to_string(),
            AttrValue::present(10.0, "test"),
        );
        ProductModel {
            barcode: barcode.into(),
            name: name.into(),
            brands: Some("Brand".into()),
            brand_primary: Some("Brand".into()),
            quantity: None,
            image_url: None,
            image_bytes: Some(b"img".to_vec()),
            nutriscore_grade: Some('d'),
            nova_group: Some(4),
            additives_tags: vec![],
            allergen_tags: vec![],
            categories_tags: vec![],
            labels_tags: vec![],
            ecoscore_grade: None,
            attributes,
        }
    }

    #[test]
    fn history_dedupes_and_caps() {
        let dir = std::env::temp_dir().join(format!("lf-hist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        let mut store = HistoryStore::open(path);
        for i in 0..105 {
            store
                .record(LibraryEntry {
                    barcode: format!("{i:013}"),
                    name: format!("P{i}"),
                    brand: None,
                    score_display: Some("10/100".into()),
                    score_label: Some("Poor".into()),
                    profile_id: Some("nutrition-quality".into()),
                    profile_name: Some("General".into()),
                    timestamp: i,
                    product: None,
                })
                .unwrap();
        }
        assert_eq!(store.entries().len(), HISTORY_CAP);
        // Re-record an older one → moves to front
        let old = store.entries().last().unwrap().barcode.clone();
        store
            .record(LibraryEntry {
                barcode: old.clone(),
                name: "Updated".into(),
                brand: Some("Brand".into()),
                score_display: Some("50/100".into()),
                score_label: Some("Mediocre".into()),
                profile_id: None,
                profile_name: None,
                timestamp: 9999,
                product: Some(sample_product(&old, "Updated")),
            })
            .unwrap();
        assert_eq!(store.entries()[0].barcode, old);
        assert_eq!(store.entries()[0].name, "Updated");
        assert!(store.entries()[0].cached_product().is_some());
        assert_eq!(store.entries().iter().filter(|e| e.barcode == old).count(), 1);
        store.remove(&old).unwrap();
        assert!(!store.entries().iter().any(|e| e.barcode == old));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn favorites_toggle_persists_product() {
        let dir = std::env::temp_dir().join(format!("lf-fav-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("favorites.json");
        let mut store = FavoritesStore::open(path.clone());
        let product = sample_product("3017620422003", "Nutella");
        let entry = LibraryEntry {
            barcode: product.barcode.clone(),
            name: product.name.clone(),
            brand: product.brand_primary.clone(),
            score_display: Some("19/100".into()),
            score_label: Some("Poor".into()),
            profile_id: Some("nutrition-quality".into()),
            profile_name: Some("General nutritional quality".into()),
            timestamp: 1,
            product: Some(product.clone()),
        };
        assert!(store.toggle(entry.clone()).unwrap());
        assert!(store.contains("3017620422003"));
        // Reload from disk — product must round-trip.
        let reloaded = FavoritesStore::open(path);
        let cached = reloaded.find("3017620422003").unwrap().cached_product().unwrap();
        assert_eq!(cached.name, "Nutella");
        assert_eq!(cached.image_bytes.as_deref(), Some(b"img".as_slice()));
        assert!(!store.toggle(entry).unwrap());
        assert!(!store.contains("3017620422003"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_json_without_product_loads() {
        let dir = std::env::temp_dir().join(format!("lf-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        fs::write(
            &path,
            r#"[{"barcode":"1","name":"Old","brand":null,"score_display":null,"score_label":null,"profile_id":null,"profile_name":null,"timestamp":1}]"#,
        )
        .unwrap();
        let store = HistoryStore::open(path);
        assert_eq!(store.entries().len(), 1);
        assert!(store.entries()[0].product.is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
