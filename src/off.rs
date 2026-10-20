//! Open Food Facts adapter — fetches source data into [`ProductModel`] (no scoring).

use crate::product::{self, attrs, AttrValue, ProductModel};
use serde::Deserialize;
use std::collections::BTreeMap;
use thiserror::Error;

const OFF_PRODUCT_URL: &str = "https://world.openfoodfacts.org/api/v2/product";
const OFF_SEARCH_URL: &str = "https://world.openfoodfacts.org/cgi/search.pl";
const USER_AGENT: &str =
    "Librefood/0.2.0 (https://github.com/notfatnotfunny/librefood; desktop; Rust)";

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
    #[error("Empty search query")]
    EmptyQuery,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub barcode: String,
    pub name: String,
    pub brands: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OffResponse {
    status: Option<i32>,
    product: Option<OffProduct>,
}

#[derive(Debug, Deserialize)]
struct OffSearchResponse {
    products: Option<Vec<OffSearchProduct>>,
}

#[derive(Debug, Deserialize)]
struct OffSearchProduct {
    code: Option<String>,
    product_name: Option<String>,
    brands: Option<String>,
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
    nova_group: Option<u8>,
    ecoscore_score: Option<f64>,
    ecoscore_grade: Option<String>,
    image_front_small_url: Option<String>,
    image_front_url: Option<String>,
    image_small_url: Option<String>,
    image_url: Option<String>,
    additives_n: Option<u32>,
    additives_tags: Option<Vec<String>>,
    allergens_tags: Option<Vec<String>>,
    categories_tags: Option<Vec<String>>,
    labels_tags: Option<Vec<String>>,
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

fn put_opt(
    map: &mut BTreeMap<String, AttrValue>,
    key: &str,
    value: Option<f64>,
    source: &str,
) {
    match value {
        Some(v) => {
            map.insert(key.to_string(), AttrValue::present(v, source));
        }
        None => {
            map.insert(key.to_string(), AttrValue::missing(source));
        }
    }
}

fn is_organic(labels: &[String]) -> bool {
    labels.iter().any(|l| {
        let s = l.to_ascii_lowercase();
        s.contains("organic") || s.contains("bio")
    })
}

fn to_model(code: String, product: OffProduct, image_bytes: Option<Vec<u8>>) -> ProductModel {
    let grade = parse_grade(&product);
    let brands = non_empty(product.brands.clone());
    let brand_primary = primary_brand(brands.as_deref());
    let image_url = pick_image_url(&product);
    let additives_tags = product.additives_tags.clone().unwrap_or_default();
    let allergen_tags = product.allergens_tags.clone().unwrap_or_default();
    let categories_tags = product.categories_tags.clone().unwrap_or_default();
    let labels_tags = product.labels_tags.clone().unwrap_or_default();
    let n = product.nutriments.as_ref();

    let additives_n = product
        .additives_n
        .or_else(|| (!additives_tags.is_empty()).then_some(additives_tags.len() as u32));

    let mut attributes = BTreeMap::new();
    put_opt(
        &mut attributes,
        attrs::ENERGY_KCAL_100G,
        n.and_then(|x| x.energy_kcal_100g.or(x.energy_kcal)),
        "Open Food Facts nutriments.energy-kcal_100g",
    );
    put_opt(
        &mut attributes,
        attrs::SUGARS_100G,
        n.and_then(|x| x.sugars_100g),
        "Open Food Facts nutriments.sugars_100g",
    );
    put_opt(
        &mut attributes,
        attrs::SATURATED_FAT_100G,
        n.and_then(|x| x.saturated_fat_100g),
        "Open Food Facts nutriments.saturated-fat_100g",
    );
    put_opt(
        &mut attributes,
        attrs::SALT_100G,
        n.and_then(|x| x.salt_100g),
        "Open Food Facts nutriments.salt_100g",
    );
    put_opt(
        &mut attributes,
        attrs::FIBER_100G,
        n.and_then(|x| x.fiber_100g),
        "Open Food Facts nutriments.fiber_100g",
    );
    put_opt(
        &mut attributes,
        attrs::PROTEINS_100G,
        n.and_then(|x| x.proteins_100g),
        "Open Food Facts nutriments.proteins_100g",
    );
    put_opt(
        &mut attributes,
        attrs::ADDITIVES_N,
        additives_n.map(|n| n as f64),
        "Open Food Facts additives_n / additives_tags",
    );
    put_opt(
        &mut attributes,
        attrs::NOVA_GROUP,
        product.nova_group.map(|n| n as f64),
        "Open Food Facts nova_group",
    );
    put_opt(
        &mut attributes,
        attrs::NUTRISCORE_ORDINAL,
        grade.and_then(product::nutriscore_ordinal),
        "Open Food Facts nutriscore_grade (A=5…E=1)",
    );
    put_opt(
        &mut attributes,
        attrs::ORGANIC,
        Some(if is_organic(&labels_tags) { 1.0 } else { 0.0 }),
        "Open Food Facts labels_tags (organic/bio heuristic)",
    );
    put_opt(
        &mut attributes,
        attrs::ECOSCORE,
        product.ecoscore_score,
        "Open Food Facts ecoscore_score",
    );
    put_opt(
        &mut attributes,
        attrs::ALLERGEN_COUNT,
        Some(allergen_tags.len() as f64),
        "Open Food Facts allergens_tags length",
    );

    ProductModel {
        barcode: code,
        name: product_title(&product),
        brands,
        brand_primary,
        quantity: non_empty(product.quantity),
        image_url,
        image_bytes,
        nutriscore_grade: grade,
        nova_group: product.nova_group,
        additives_tags,
        allergen_tags,
        categories_tags,
        labels_tags,
        ecoscore_grade: non_empty(product.ecoscore_grade),
        attributes,
    }
}

fn http_client() -> Result<reqwest::blocking::Client, LookupError> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| LookupError::Network(e.to_string()))
}

/// Normalize typed/pasted barcodes: strip spaces and keep digits.
pub fn normalize_barcode(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

pub fn lookup_product(barcode: &str) -> Result<ProductModel, LookupError> {
    let code = normalize_barcode(barcode);
    if code.is_empty() {
        return Err(LookupError::EmptyBarcode);
    }

    let url = format!("{OFF_PRODUCT_URL}/{code}.json");
    let client = http_client()?;

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

    let body: OffResponse = response.json().map_err(|_| LookupError::BadResponse)?;
    if body.status == Some(0) {
        return Err(LookupError::NotFound);
    }
    let product = body.product.ok_or(LookupError::NotFound)?;
    let image_url = pick_image_url(&product);
    let image_bytes = image_url
        .as_deref()
        .and_then(|u| fetch_image_bytes(&client, u));
    Ok(to_model(code, product, image_bytes))
}

/// Simple OFF text search; returns top hits (barcode + name).
pub fn search_products(query: &str, page_size: u32) -> Result<Vec<SearchHit>, LookupError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(LookupError::EmptyQuery);
    }
    let client = http_client()?;
    let response = client
        .get(OFF_SEARCH_URL)
        .query(&[
            ("search_terms", q),
            ("search_simple", "1"),
            ("action", "process"),
            ("json", "1"),
            ("page_size", &page_size.to_string()),
        ])
        .send()
        .map_err(|e| LookupError::Network(e.to_string()))?;
    if !response.status().is_success() {
        return Err(LookupError::Network(format!(
            "HTTP {}",
            response.status()
        )));
    }
    let body: OffSearchResponse = response.json().map_err(|_| LookupError::BadResponse)?;
    let mut hits = Vec::new();
    for p in body.products.unwrap_or_default() {
        let Some(barcode) = non_empty(p.code) else {
            continue;
        };
        let name = non_empty(p.product_name).unwrap_or_else(|| "Unknown".into());
        hits.push(SearchHit {
            barcode,
            name,
            brands: non_empty(p.brands),
        });
    }
    Ok(hits)
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
        assert!(info.attr_number(attrs::SUGARS_100G).is_some());
        println!("{} sugars={:?}", info.name, info.attr_number(attrs::SUGARS_100G));
    }

    #[test]
    fn lookup_missing() {
        let err = lookup_product("0000000000000").unwrap_err();
        assert!(matches!(err, LookupError::NotFound));
    }
}
