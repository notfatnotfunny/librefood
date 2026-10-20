//! Normalized product model — source OFF data, separate from derived scores.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Canonical attribute keys used by scoring profiles.
pub mod attrs {
    pub const ENERGY_KCAL_100G: &str = "energy_kcal_100g";
    pub const SUGARS_100G: &str = "sugars_100g";
    pub const SATURATED_FAT_100G: &str = "saturated_fat_100g";
    pub const SALT_100G: &str = "salt_100g";
    pub const FIBER_100G: &str = "fiber_100g";
    pub const PROTEINS_100G: &str = "proteins_100g";
    pub const ADDITIVES_N: &str = "additives_n";
    pub const NOVA_GROUP: &str = "nova_group";
    pub const NUTRISCORE_ORDINAL: &str = "nutriscore_ordinal"; // A=5 … E=1
    pub const ORGANIC: &str = "organic"; // 1.0 / 0.0
    pub const ECOSCORE: &str = "ecoscore_score"; // 0–100 when present
    pub const ALLERGEN_COUNT: &str = "allergen_count";
    /// 1.0 if brand matched on boycottisraeli.biz, 0.0 if searched with no match.
    pub const BOYCOTT_LISTED: &str = "boycott_listed";
}

/// Availability of a single attribute from the data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrAvailability {
    Present,
    Missing,
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttrValue {
    pub value: Option<f64>,
    pub availability: AttrAvailability,
    /// Human-readable source note (e.g. "Open Food Facts nutriments.sugars_100g").
    pub source: String,
}

impl AttrValue {
    pub fn present(value: f64, source: impl Into<String>) -> Self {
        Self {
            value: Some(value),
            availability: AttrAvailability::Present,
            source: source.into(),
        }
    }

    pub fn missing(source: impl Into<String>) -> Self {
        Self {
            value: None,
            availability: AttrAvailability::Missing,
            source: source.into(),
        }
    }
}

/// Product identity + raw OFF fields + normalized numeric attributes for the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductModel {
    pub barcode: String,
    pub name: String,
    pub brands: Option<String>,
    pub brand_primary: Option<String>,
    pub quantity: Option<String>,
    pub image_url: Option<String>,
    #[serde(skip)]
    pub image_bytes: Option<Vec<u8>>,
    /// Nutri-Score letter when published by OFF (source data, not a Librefood score).
    pub nutriscore_grade: Option<char>,
    pub nova_group: Option<u8>,
    pub additives_tags: Vec<String>,
    pub allergen_tags: Vec<String>,
    pub categories_tags: Vec<String>,
    pub labels_tags: Vec<String>,
    pub ecoscore_grade: Option<String>,
    pub attributes: BTreeMap<String, AttrValue>,
}

impl ProductModel {
    pub fn get_attr(&self, key: &str) -> Option<&AttrValue> {
        self.attributes.get(key)
    }

    pub fn attr_number(&self, key: &str) -> Option<f64> {
        self.attributes.get(key).and_then(|a| a.value)
    }

    pub fn is_in_category(&self, needle: &str) -> bool {
        let n = needle.to_ascii_lowercase();
        self.categories_tags
            .iter()
            .any(|c| c.to_ascii_lowercase().contains(&n))
    }
}

/// Map Nutri-Score A–E to ordinal 5–1 (higher = better nutrition grade from OFF).
pub fn nutriscore_ordinal(grade: char) -> Option<f64> {
    match grade.to_ascii_lowercase() {
        'a' => Some(5.0),
        'b' => Some(4.0),
        'c' => Some(3.0),
        'd' => Some(2.0),
        'e' => Some(1.0),
        _ => None,
    }
}
