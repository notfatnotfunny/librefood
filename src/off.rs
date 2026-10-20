//! Open Food Facts product lookup (public API, no key required).

use serde::Deserialize;
use thiserror::Error;

const OFF_PRODUCT_URL: &str = "https://world.openfoodfacts.org/api/v2/product";
const USER_AGENT: &str = "Librefood/0.1.0 (https://github.com/librefood; desktop; Rust)";

#[derive(Debug, Clone)]
pub struct ProductInfo {
    pub barcode: String,
    pub name: String,
    pub nutriscore_grade: Option<char>,
    pub brands: Option<String>,
    pub quantity: Option<String>,
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
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn best_product_name(product: &OffProduct) -> String {
    let candidates = [
        product.product_name_fr.clone(),
        product.product_name.clone(),
        product.product_name_en.clone(),
        product.generic_name.clone(),
    ];
    let raw = candidates.into_iter().find_map(non_empty);
    let brands = non_empty(product.brands.clone());

    match (brands.as_deref(), raw.as_deref()) {
        (Some(b), Some(n)) => {
            let primary = b.split(',').next().unwrap_or(b).trim();
            if !primary.is_empty() && !n.to_ascii_lowercase().contains(&primary.to_ascii_lowercase())
            {
                // Crowd-sourced names are sometimes wrong; lead with brand.
                format!("{primary} — {n}")
            } else {
                n.to_string()
            }
        }
        (None, Some(n)) => n.to_string(),
        (Some(b), None) => b.to_string(),
        (None, None) => "Unknown product".to_string(),
    }
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
    let name = best_product_name(&product);

    let grade = product
        .nutriscore_grade
        .or(product.nutrition_grades)
        .and_then(|g| {
            let t = g.trim().to_ascii_lowercase();
            let c = t.chars().next()?;
            if matches!(c, 'a' | 'b' | 'c' | 'd' | 'e') {
                Some(c)
            } else {
                None
            }
        });

    Ok(ProductInfo {
        barcode: code,
        name,
        nutriscore_grade: grade,
        brands: non_empty(product.brands),
        quantity: non_empty(product.quantity),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_spaces() {
        assert_eq!(normalize_barcode(" 301 762 042 2003 "), "3017620422003");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn lookup_nutella() {
        let info = lookup_product("3017620422003").expect("lookup");
        assert!(!info.name.is_empty());
        println!("{} {:?}", info.name, info.nutriscore_grade);
    }

    #[test]
    fn lookup_missing() {
        let err = lookup_product("0000000000000").unwrap_err();
        assert!(matches!(err, LookupError::NotFound));
    }
}
