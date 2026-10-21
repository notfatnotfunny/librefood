//! Brand enrichment from the WikiRate open API (`https://wikirate.org`).
//!
//! Looks up the product brand as a WikiRate company and reads the latest
//! World Benchmarking Alliance Food & Agriculture Benchmark answer, normalized
//! to 0–100 for scoring profiles.

use crate::boycott;
use crate::product::{attrs, AttrValue, ProductModel};
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://wikirate.org";
const USER_AGENT: &str =
    "Librefood/0.2 (+https://github.com/notfatnotfunny/librefood; ethical-consumer)";

/// Primary food-relevant numeric benchmark (typically ~0–10 on WikiRate).
const METRIC_WBA_FAB: &str = "World_Benchmarking_Alliance+Food_and_Agriculture_Benchmark";
/// Fallback nutrition-policy score (often 0–100).
const METRIC_ATNI: &str = "Access_to_Nutrition_Index+Access_to_Nutrition_Overall_Score";

/// Extra WikiRate card names that search alone may miss.
const COMPANY_HINTS: &[(&str, &str)] = &[
    ("coca cola", "The Coca Cola Company"),
    ("cocacola", "The Coca Cola Company"),
    ("coke", "The Coca Cola Company"),
    ("pepsi", "PepsiCo Inc."),
    ("pepsico", "PepsiCo Inc."),
    ("nestle", "Nestle"),
    ("nestlé", "Nestle"),
    ("unilever", "Unilever"),
    ("danone", "Danone Group"),
    ("mondelez", "Mondelez International"),
    ("kellogg", "Kellogg Co."),
    ("kelloggs", "Kellogg Co."),
    ("barilla", "Barilla S.p.A."),
];

#[derive(Debug, Deserialize)]
struct ListEnvelope {
    #[serde(default)]
    items: Vec<CompanyHit>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompanyHit {
    id: Option<u64>,
    name: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnswerEnvelope {
    #[serde(default)]
    items: Vec<AnswerHit>,
    /// Single-answer cards may embed value at the top level.
    value: Option<serde_json::Value>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnswerHit {
    name: Option<String>,
    value: Option<serde_json::Value>,
}

/// Enrich `product` with [`attrs::WIKIRATE_SCORE`] (0–100).
/// No-op if already present. Network / not-found → missing attribute.
pub fn apply(product: &mut ProductModel) {
    if product
        .attributes
        .get(attrs::WIKIRATE_SCORE)
        .is_some_and(|a| a.availability == crate::product::AttrAvailability::Present)
    {
        return;
    }

    let queries = search_queries(product);
    if queries.is_empty() {
        product.attributes.insert(
            attrs::WIKIRATE_SCORE.to_string(),
            AttrValue::missing("wikirate.org — no brand to search"),
        );
        return;
    }

    let Ok(client) = http_client() else {
        product.attributes.insert(
            attrs::WIKIRATE_SCORE.to_string(),
            AttrValue::missing("wikirate.org — HTTP client unavailable"),
        );
        return;
    };

    let mut last_note = String::from("wikirate.org — no company match");
    for q in &queries {
        match find_company(&client, q) {
            Ok(Some(company)) => match fetch_score(&client, &company) {
                Ok(Some(score)) => {
                    product.attributes.insert(
                        attrs::WIKIRATE_SCORE.to_string(),
                        AttrValue::present(score.value_100, score.source),
                    );
                    return;
                }
                Ok(None) => {
                    last_note = format!(
                        "wikirate.org — “{}” found, no usable benchmark score",
                        company.name.as_deref().unwrap_or(q)
                    );
                }
                Err(e) => {
                    last_note = format!("wikirate.org — score fetch failed ({e})");
                }
            },
            Ok(None) => {
                last_note = format!("wikirate.org — no company for “{q}”");
            }
            Err(e) => {
                last_note = format!("wikirate.org — search failed ({e})");
            }
        }
    }

    product.attributes.insert(
        attrs::WIKIRATE_SCORE.to_string(),
        AttrValue::missing(last_note),
    );
}

struct ScoreHit {
    value_100: f64,
    source: String,
}

fn search_queries(product: &ProductModel) -> Vec<String> {
    let mut out = boycott::brand_queries(product);
    // WikiRate-specific full company names first.
    let mut hinted = Vec::new();
    for q in &out {
        if let Some(hint) = company_hint(q) {
            if !hinted.iter().any(|x: &String| x.eq_ignore_ascii_case(hint)) {
                hinted.push(hint.to_string());
            }
        }
    }
    hinted.append(&mut out);
    hinted.truncate(8);
    hinted
}

fn company_hint(s: &str) -> Option<&'static str> {
    let n = normalize(s);
    let compact = n.replace(' ', "");
    for &(key, canon) in COMPANY_HINTS {
        let kn = normalize(key);
        let kc = kn.replace(' ', "");
        if n == kn || compact == kc || n.starts_with(&(kn.clone() + " ")) {
            return Some(canon);
        }
    }
    if let Some(tok) = n.split_whitespace().next() {
        for &(key, canon) in COMPANY_HINTS {
            let kn = normalize(key);
            if tok == kn || tok == kn.replace(' ', "") {
                return Some(canon);
            }
        }
    }
    None
}

fn find_company(
    client: &reqwest::blocking::Client,
    query: &str,
) -> Result<Option<CompanyHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(None);
    }

    // 1) Direct card mark (works for “The Coca Cola Company”, “Unilever”, …).
    if let Some(hit) = try_company_card(client, q)? {
        return Ok(Some(hit));
    }

    // 2) Filtered company search.
    let mut url = format!(
        "{API_BASE}/Companies.json?filter%5Bname%5D={}&limit=12",
        urlencode(q)
    );
    if let Ok(key) = std::env::var("WIKIRATE_API_KEY") {
        if !key.is_empty() {
            url.push_str("&api_key=");
            url.push_str(&urlencode(&key));
        }
    }
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body: ListEnvelope = response.json().map_err(|e| e.to_string())?;
    Ok(pick_company(q, &body.items).cloned())
}

fn try_company_card(
    client: &reqwest::blocking::Client,
    name: &str,
) -> Result<Option<CompanyHit>, String> {
    let mark = card_mark(name);
    if mark.is_empty() {
        return Ok(None);
    }
    let mut url = format!("{API_BASE}/{mark}.json");
    if let Ok(key) = std::env::var("WIKIRATE_API_KEY") {
        if !key.is_empty() {
            url.push_str("?api_key=");
            url.push_str(&urlencode(&key));
        }
    }
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 404 {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body: CompanyHit = response.json().map_err(|e| e.to_string())?;
    if body.name.is_none() && body.id.is_none() {
        return Ok(None);
    }
    Ok(Some(body))
}

fn pick_company<'a>(query: &str, items: &'a [CompanyHit]) -> Option<&'a CompanyHit> {
    if items.is_empty() {
        return None;
    }
    let nq = normalize(query);
    items.iter().find(|c| {
        c.name
            .as_deref()
            .is_some_and(|n| normalize(n) == nq)
    })
    .or_else(|| {
        items.iter().find(|c| {
            c.name.as_deref().is_some_and(|n| {
                let nn = normalize(n);
                nn.starts_with(&nq) || nq.starts_with(&nn)
            })
        })
    })
    .or_else(|| {
        items.iter().find(|c| {
            c.name
                .as_deref()
                .is_some_and(|n| normalize(n).contains(&nq) && nq.chars().count() >= 4)
        })
    })
    .or_else(|| items.first())
}

fn fetch_score(
    client: &reqwest::blocking::Client,
    company: &CompanyHit,
) -> Result<Option<ScoreHit>, String> {
    let mark = company_mark(company);
    if mark.is_empty() {
        return Ok(None);
    }

    if let Some(s) = metric_score(client, METRIC_WBA_FAB, &mark, true)? {
        return Ok(Some(s));
    }
    if let Some(s) = metric_score(client, METRIC_ATNI, &mark, false)? {
        return Ok(Some(s));
    }
    Ok(None)
}

fn metric_score(
    client: &reqwest::blocking::Client,
    metric: &str,
    company_mark: &str,
    wba_scale: bool,
) -> Result<Option<ScoreHit>, String> {
    let mut url = format!("{API_BASE}/{metric}+{company_mark}.json");
    if let Ok(key) = std::env::var("WIKIRATE_API_KEY") {
        if !key.is_empty() {
            url.push_str("?api_key=");
            url.push_str(&urlencode(&key));
        }
    }
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 404 {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body: AnswerEnvelope = response.json().map_err(|e| e.to_string())?;

    let mut best: Option<(u32, f64, String)> = None;
    for it in &body.items {
        let Some(raw) = parse_number(it.value.as_ref()) else {
            continue;
        };
        let year = year_from_name(it.name.as_deref()).unwrap_or(0);
        let label = it.name.clone().unwrap_or_default();
        if best.as_ref().is_none_or(|(y, _, _)| year >= *y) {
            best = Some((year, raw, label));
        }
    }
    if best.is_none() {
        if let Some(raw) = parse_number(body.value.as_ref()) {
            let year = year_from_name(body.name.as_deref()).unwrap_or(0);
            best = Some((year, raw, body.name.unwrap_or_default()));
        }
    }
    let Some((year, raw, label)) = best else {
        return Ok(None);
    };

    let value_100 = if wba_scale {
        // WBA Food & Agriculture Benchmark answers are ~0–10.
        (raw * 10.0).clamp(0.0, 100.0)
    } else if raw <= 10.0 {
        (raw * 10.0).clamp(0.0, 100.0)
    } else {
        raw.clamp(0.0, 100.0)
    };

    let metric_label = if wba_scale {
        "WBA Food & Agriculture Benchmark"
    } else {
        "Access to Nutrition Index"
    };
    let year_bit = if year > 0 {
        format!(" ({year})")
    } else {
        String::new()
    };
    let company_name = company_mark.replace('_', " ");
    let _ = label;
    let source = format!(
        "wikirate.org · {metric_label}{year_bit} · raw {raw:.2} → {value_100:.0}/100 · {company_name}\nhttps://wikirate.org/{metric}+{company_mark}"
    );
    Ok(Some(ScoreHit { value_100, source }))
}

fn company_mark(company: &CompanyHit) -> String {
    if let Some(url) = &company.url {
        if let Some(mark) = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(|s| s.trim_end_matches(".json"))
            .filter(|s| !s.is_empty())
        {
            return mark.to_string();
        }
    }
    company
        .name
        .as_deref()
        .map(card_mark)
        .unwrap_or_default()
}

fn card_mark(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| match c {
            ' ' | '/' => '_',
            c => c,
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn year_from_name(name: Option<&str>) -> Option<u32> {
    let name = name?;
    let tail = name.rsplit('+').next()?;
    tail.parse().ok()
}

fn parse_number(v: Option<&serde_json::Value>) -> Option<f64> {
    let v = v?;
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("unknown") || t.eq_ignore_ascii_case("yes") || t.eq_ignore_ascii_case("no")
            {
                return None;
            }
            t.parse().ok()
        }
        _ => None,
    }
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(18))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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
    use crate::product::AttrAvailability;
    use std::collections::BTreeMap;

    fn product(brand: &str) -> ProductModel {
        ProductModel {
            barcode: "1".into(),
            name: brand.into(),
            brands: Some(brand.into()),
            brand_primary: Some(brand.split([',', ';']).next().unwrap().trim().into()),
            quantity: None,
            image_url: None,
            image_bytes: None,
            nutriscore_grade: None,
            nova_group: None,
            additives_tags: vec![],
            allergen_tags: vec![],
            categories_tags: vec![],
            labels_tags: vec![],
            ecoscore_grade: None,
            attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn card_mark_spaces_to_underscores() {
        assert_eq!(card_mark("The Coca Cola Company"), "The_Coca_Cola_Company");
    }

    #[test]
    fn company_hint_maps_coke() {
        assert_eq!(company_hint("Coke ZERO"), Some("The Coca Cola Company"));
        assert_eq!(company_hint("Cocacola"), Some("The Coca Cola Company"));
    }

    #[test]
    fn pick_company_prefers_exact() {
        let items = vec![
            CompanyHit {
                id: Some(1),
                name: Some("Coca-Cola Enterprises".into()),
                url: None,
            },
            CompanyHit {
                id: Some(8912),
                name: Some("The Coca Cola Company".into()),
                url: Some("https://wikirate.org/The_Coca_Cola_Company.json".into()),
            },
        ];
        let hit = pick_company("The Coca Cola Company", &items).unwrap();
        assert_eq!(hit.id, Some(8912));
    }

    #[test]
    #[ignore = "live network"]
    fn live_coca_cola_has_wikirate_score() {
        let mut p = product("Cocacola");
        apply(&mut p);
        let attr = p.attributes.get(attrs::WIKIRATE_SCORE).expect("attr");
        assert_eq!(attr.availability, AttrAvailability::Present);
        let v = attr.value.expect("value");
        assert!(v > 0.0 && v <= 100.0, "score={v} src={}", attr.source);
        assert!(attr.source.contains("wikirate.org"), "{}", attr.source);
    }
}
