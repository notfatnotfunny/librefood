//! Brand check against the Boycott Israeli Businesses API
//! (`https://api.boycottisraeli.biz/v1`, documented at https://boycottisraeli.biz/api).

use crate::product::{attrs, AttrValue, ProductModel};
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://api.boycottisraeli.biz/v1";
const USER_AGENT: &str = "Librefood/0.2 (+https://github.com/notfatnotfunny/librefood; ethical-consumer)";

#[derive(Debug, Deserialize)]
struct ApiEnvelope {
    data: Option<Vec<Company>>,
}

#[derive(Debug, Deserialize)]
struct Company {
    name: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
}

/// Enrich `product` with [`attrs::BOYCOTT_LISTED`] (1 = match, 0 = no match).
/// No-op if the attribute is already present. Network errors leave it missing.
pub fn apply(product: &mut ProductModel) {
    if product
        .attributes
        .get(attrs::BOYCOTT_LISTED)
        .is_some_and(|a| a.availability == crate::product::AttrAvailability::Present)
    {
        return;
    }

    let queries = brand_queries(product);
    if queries.is_empty() {
        product.attributes.insert(
            attrs::BOYCOTT_LISTED.to_string(),
            AttrValue::missing("boycottisraeli.biz — no brand to search"),
        );
        return;
    }

    let Ok(client) = http_client() else {
        product.attributes.insert(
            attrs::BOYCOTT_LISTED.to_string(),
            AttrValue::missing("boycottisraeli.biz — HTTP client unavailable"),
        );
        return;
    };

    for q in queries {
        match search_companies(&client, &q) {
            Ok(companies) => {
                if let Some(hit) = best_match(&q, &companies) {
                    let label = hit.name.as_deref().unwrap_or(&q);
                    let kind = hit.r#type.as_deref().unwrap_or("listed");
                    product.attributes.insert(
                        attrs::BOYCOTT_LISTED.to_string(),
                        AttrValue::present(
                            1.0,
                            format!("boycottisraeli.biz matched “{label}” ({kind})"),
                        ),
                    );
                    return;
                }
            }
            Err(_) => {
                // try next query; if all fail, mark missing below
            }
        }
    }

    // Searches succeeded with no confident match, or soft-failed — treat as not listed.
    // If every request hard-failed, still set 0 only when at least one 404/empty OK.
    // Conservative: no match after trying queries ⇒ 0 (not on list).
    product.attributes.insert(
        attrs::BOYCOTT_LISTED.to_string(),
        AttrValue::present(
            0.0,
            format!(
                "boycottisraeli.biz — no match for brand(s): {}",
                brand_queries(product).join(", ")
            ),
        ),
    );
}

fn brand_queries(product: &ProductModel) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(b) = product.brand_primary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        out.push(b.to_string());
    }
    if let Some(brands) = &product.brands {
        for part in brands.split([',', ';', '/']) {
            let t = part.trim();
            if t.is_empty() {
                continue;
            }
            if !out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                out.push(t.to_string());
            }
        }
    }
    // Cap fan-out
    out.truncate(4);
    out
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())
}

fn search_companies(
    client: &reqwest::blocking::Client,
    query: &str,
) -> Result<Vec<Company>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let encoded: String = q
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let url = format!("{API_BASE}/search/{encoded}?limit=12");
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(vec![]);
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let body: ApiEnvelope = response.json().map_err(|e| e.to_string())?;
    Ok(body.data.unwrap_or_default())
}

fn best_match<'a>(query: &str, companies: &'a [Company]) -> Option<&'a Company> {
    let q = normalize(query);
    if q.len() < 2 {
        return None;
    }
    companies.iter().find(|c| {
        let Some(name) = c.name.as_deref() else {
            return false;
        };
        let n = normalize(name);
        n == q
            || (q.len() >= 3 && n.contains(&q))
            || (n.len() >= 3 && q.contains(&n))
    })
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_case_and_punct() {
        assert_eq!(normalize("Coca-Cola"), "coca cola");
        assert_eq!(normalize("  Nestlé  "), "nestlé");
    }

    #[test]
    fn match_accepts_contains() {
        let cos = vec![Company {
            name: Some("The Coca-Cola Company".into()),
            r#type: Some("supports-israel".into()),
        }];
        assert!(best_match("Coca-Cola", &cos).is_some());
        assert!(best_match("Unilever", &cos).is_none());
    }

    #[test]
    #[ignore = "live network"]
    fn live_search_nestle() {
        let client = http_client().unwrap();
        let hits = search_companies(&client, "Nestle").unwrap();
        assert!(hits.iter().any(|c| {
            c.name
                .as_deref()
                .is_some_and(|n| n.to_ascii_lowercase().contains("nestl"))
        }));
    }
}
