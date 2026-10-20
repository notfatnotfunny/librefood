//! Open Food Facts product lookup (public API, no key required).

use crate::score::{self, ScoreSummary};
use serde::Deserialize;
use thiserror::Error;

const OFF_PRODUCT_URL: &str = "https://world.openfoodfacts.org/api/v2/product";
const USER_AGENT: &str = "Librefood/0.1.0 (https://github.com/notfatnotfunny/librefood; desktop; Rust)";

/// Raw nutrition / additive facts used by Librefood heuristics.
#[derive(Debug, Clone, Default)]
pub struct ProductFacts {
    pub nutriscore_grade: Option<char>,
    pub energy_kcal_100g: Option<f64>,
    pub sugars_100g: Option<f64>,
    pub saturated_fat_100g: Option<f64>,
    pub salt_100g: Option<f64>,
    pub fiber_100g: Option<f64>,
    pub proteins_100g: Option<f64>,
    pub additives_n: Option<u32>,
    pub additives_tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProductInfo {
    pub barcode: String,
    /// Product title (not merged with brand).
    pub name: String,
    pub nutriscore_grade: Option<char>,
    pub brands: Option<String>,
    pub quantity: Option<String>,
    /// Primary brand for header display.
    pub brand_primary: Option<String>,
    #[allow(dead_code)]
    pub image_url: Option<String>,
    /// Downloaded front image bytes (JPEG/PNG/WebP), if fetch succeeded.
    pub image_bytes: Option<Vec<u8>>,
    pub facts: ProductFacts,
    pub score: ScoreSummary,
}

#[derive(Debug, Error)]
pub enum LookupError {
    #[error("Network error: {0}")]
    Network(String),
    #[error("Product not found")]
    NotFound,
    #[error("Invalid response from Open Food Facts")]
    BadResponse,
    #[error("Empty barcode")]
    EmptyBarcode,
}

#[derive(Debug, Deserialize)]
struct OffResponse {
    status: Option<i32>,
    product: Option<OffProduct>,
}

#[derive(Debug, Deserialize)]
struct OffProduct {
    product_name: Option<String>,
    product_name_en: Option<String>,
    product_name_fr: Option<String>,
    generic_name: Option<String>,
    brands: Option<String>,
    quantity: Option<String>,
    nutriscore_grade: Option<String>,
    nutrition_grades: Option<String>,
    image_front_small_url: Option<String>,
    image_front_url: Option<String>,
    image_small_url: Option<String>,
    image_url: Option<String>,
    additives_n: Option<u32>,
    additives_tags: Option<Vec<String>>,
    nutriments: Option<OffNutriments>,
}

#[derive(Debug, Deserialize, Default)]
struct OffNutriments {
    #[serde(rename = "energy-kcal_100g")]
    energy_kcal_100g: Option<f64>,
    #[serde(rename = "energy-kcal")]
    energy_kcal: Option<f64>,
    #[serde(rename = "sugars_100g")]
    sugars_100g: Option<f64>,
    #[serde(rename = "saturated-fat_100g")]
    saturated_fat_100g: Option<f64>,
    #[serde(rename = "salt_100g")]
    salt_100g: Option<f64>,
    #[serde(rename = "fiber_100g")]
    fiber_100g: Option<f64>,
    #[serde(rename = "proteins_100g")]
    proteins_100g: Option<f64>,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn primary_brand(brands: Option<&str>) -> Option<String> {
    brands
        .and_then(|b| b.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn product_title(product: &OffProduct) -> String {
    let candidates = [
        product.product_name.clone(),
        product.product_name_en.clone(),
        product.product_name_fr.clone(),
        product.generic_name.clone(),
    ];
    candidates
        .into_iter()
        .find_map(non_empty)
        .unwrap_or_else(|| {
            primary_brand(product.brands.as_deref()).unwrap_or_else(|| "Unknown product".into())
        })
}

fn parse_grade(product: &OffProduct) -> Option<char> {
    product
        .nutriscore_grade
        .clone()
        .or_else(|| product.nutrition_grades.clone())
        .and_then(|g| {
            let t = g.trim().to_ascii_lowercase();
            let c = t.chars().next()?;
            if matches!(c, 'a' | 'b' | 'c' | 'd' | 'e') {
                Some(c)
            } else {
                None
            }
        })
}

fn pick_image_url(product: &OffProduct) -> Option<String> {
    [
        product.image_front_small_url.clone(),
        product.image_small_url.clone(),
        product.image_front_url.clone(),
        product.image_url.clone(),
    ]
    .into_iter()
    .find_map(non_empty)
}

fn facts_from_product(product: &OffProduct, grade: Option<char>) -> ProductFacts {
    let n = product.nutriments.as_ref();
    let tags = product.additives_tags.clone().unwrap_or_default();
    let additives_n = product
        .additives_n
        .or_else(|| (!tags.is_empty()).then_some(tags.len() as u32));

    ProductFacts {
        nutriscore_grade: grade,
        energy_kcal_100g: n.and_then(|x| x.energy_kcal_100g.or(x.energy_kcal)),
        sugars_100g: n.and_then(|x| x.sugars_100g),
        saturated_fat_100g: n.and_then(|x| x.saturated_fat_100g),
        salt_100g: n.and_then(|x| x.salt_100g),
        fiber_100g: n.and_then(|x| x.fiber_100g),
        proteins_100g: n.and_then(|x| x.proteins_100g),
        additives_n,
        additives_tags: tags,
    }
}

fn fetch_image_bytes(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    let resp = client.get(url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().ok()?;
    if bytes.is_empty() || bytes.len() > 2_000_000 {
        return None;
    }
    Some(bytes.to_vec())
}

/// Normalize typed/pasted barcodes: strip spaces and keep digits (and optional leading zeros).
pub fn normalize_barcode(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

pub fn lookup_product(barcode: &str) -> Result<ProductInfo, LookupError> {
    let code = normalize_barcode(barcode);
    if code.is_empty() {
        return Err(LookupError::EmptyBarcode);
    }

    let url = format!("{OFF_PRODUCT_URL}/{code}.json");

    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| LookupError::Network(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .map_err(|e| LookupError::Network(e.to_string()))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(LookupError::NotFound);
    }

    if !response.status().is_success() {
        return Err(LookupError::Network(format!(
            "HTTP {}",
            response.status()
        )));
    }

    let body: OffResponse = response
        .json()
        .map_err(|_| LookupError::BadResponse)?;

    // OFF v2: status == 0 means not found; status == 1 means found.
    if body.status == Some(0) {
        return Err(LookupError::NotFound);
    }

    let product = body.product.ok_or(LookupError::NotFound)?;
    let name = product_title(&product);
    let brands = non_empty(product.brands.clone());
    let brand_primary = primary_brand(brands.as_deref());
    let grade = parse_grade(&product);
    let image_url = pick_image_url(&product);
    let image_bytes = image_url
        .as_deref()
        .and_then(|u| fetch_image_bytes(&client, u));
    let facts = facts_from_product(&product, grade);
    let score = score::evaluate(&facts);

    Ok(ProductInfo {
        barcode: code,
        name,
        nutriscore_grade: grade,
        brands,
        quantity: non_empty(product.quantity),
        brand_primary,
        image_url,
        image_bytes,
        facts,
        score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_spaces() {
        assert_eq!(normalize_barcode(" 301 762 042 2003 "), "3017620422003");
    }

    #[test]
    fn primary_brand_takes_first() {
        assert_eq!(
            primary_brand(Some("Nutella, Ferrero")).as_deref(),
            Some("Nutella")
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn lookup_nutella() {
        let info = lookup_product("3017620422003").expect("lookup");
        assert!(!info.name.is_empty());
        assert!(info.facts.sugars_100g.is_some());
        assert!(info.score.score <= 40);
        println!(
            "{} score={}/100 {:?} grade={:?}",
            info.name, info.score.score, info.score.label, info.nutriscore_grade
        );
    }

    #[test]
    fn lookup_missing() {
        let err = lookup_product("0000000000000").unwrap_err();
        assert!(matches!(err, LookupError::NotFound));
    }
}
