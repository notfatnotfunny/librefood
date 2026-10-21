//! Brand check against the Boycott Israeli Businesses API
//! (`https://api.boycottisraeli.biz/v1`, documented at https://boycottisraeli.biz/api).

use crate::product::{attrs, AttrValue, ProductModel};
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://api.boycottisraeli.biz/v1";
const USER_AGENT: &str =
    "Librefood/0.2 (+https://github.com/notfatnotfunny/librefood; ethical-consumer)";

/// Common OFF spellings / short names → canonical boycott API query.
/// Keys must be [`normalize`]d (lowercase, punctuation → spaces).
const BRAND_ALIASES: &[(&str, &str)] = &[
    ("coca cola", "Coca-Cola"),
    ("cocacola", "Coca-Cola"),
    ("coke", "Coca-Cola"),
    ("pepsi", "Pepsi"),
    ("pepsico", "PepsiCo"),
    ("nestle", "Nestlé"),
    ("nestlé", "Nestlé"),
    ("mcdonalds", "McDonald's"),
    ("mcdonald s", "McDonald's"),
    ("starbucks", "Starbucks"),
    ("danone", "Danone"),
    ("kraft", "Kraft"),
    ("heinz", "Heinz"),
    ("kelloggs", "Kellogg's"),
    ("kellogg s", "Kellogg's"),
    ("mondelez", "Mondelez"),
    ("unilever", "Unilever"),
    ("loreal", "L'Oréal"),
    ("l oreal", "L'Oréal"),
];

/// Flavor / size words that OFF sometimes stuffs into the brands field.
const BRAND_NOISE: &[&str] = &[
    "zero",
    "light",
    "diet",
    "maxi",
    "mini",
    "original",
    "classic",
    "sugar",
    "free",
    "sans",
    "sucres",
    "sucre",
    "sugarfree",
    "zerosugar",
];

/// Legal / corporate suffixes that are useless as search keys (and cause 404s).
const LEGAL_STOP: &[&str] = &[
    "sa",
    "nv",
    "sa/nv",
    "s.a",
    "s.a.",
    "n.v",
    "n.v.",
    "inc",
    "inc.",
    "ltd",
    "ltd.",
    "gmbh",
    "bv",
    "b.v",
    "b.v.",
    "llc",
    "corp",
    "corp.",
    "corporation",
    "company",
    "co",
    "co.",
    "services",
    "service",
    "plc",
    "ag",
    "spa",
    "s.p.a",
    "s.p.a.",
    "oy",
    "ab",
    "kg",
    "se",
    "group",
    "holding",
    "holdings",
    "international",
    "intl",
    "europe",
    "usa",
    "uk",
];

#[derive(Debug, Deserialize)]
struct ApiEnvelope {
    data: Option<Vec<Company>>,
}

#[derive(Debug, Deserialize)]
struct Company {
    name: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    alternatives: Vec<Alternative>,
}

#[derive(Debug, Deserialize)]
struct Alternative {
    name: Option<String>,
    #[serde(default)]
    website: Option<String>,
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

    let mut saw_ok_response = false;
    for q in &queries {
        match search_companies(&client, q) {
            Ok(companies) => {
                saw_ok_response = true;
                // Match API hits against the query *and* original brand strings
                // (legal entity search can 404 while "Coca-Cola" hits).
                if let Some(hit) = best_match_among(q, product, &companies) {
                    product.attributes.insert(
                        attrs::BOYCOTT_LISTED.to_string(),
                        AttrValue::present(1.0, match_source(hit, q)),
                    );
                    return;
                }
            }
            Err(_) => {}
        }
    }

    let status = if saw_ok_response {
        format!(
            "boycottisraeli.biz — no match for brand(s): {}",
            queries.join(", ")
        )
    } else {
        format!(
            "boycottisraeli.biz — search failed for brand(s): {}",
            queries.join(", ")
        )
    };

    // Successful empty searches ⇒ not on list (0). Total failure ⇒ missing.
    if saw_ok_response {
        product.attributes.insert(
            attrs::BOYCOTT_LISTED.to_string(),
            AttrValue::present(0.0, status),
        );
    } else {
        product.attributes.insert(
            attrs::BOYCOTT_LISTED.to_string(),
            AttrValue::missing(status),
        );
    }
}

/// Brand search strings derived from OFF brands (aliases, legal-suffix strip).
/// Shared with WikiRate enrichment.
pub fn brand_queries(product: &ProductModel) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    if let Some(b) = product
        .brand_primary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        raw.push(b.to_string());
    }
    if let Some(brands) = &product.brands {
        // Split on comma/semicolon only — do NOT split on `/` (breaks "SA/NV").
        for part in brands.split([',', ';']) {
            let t = part.trim();
            if t.is_empty() {
                continue;
            }
            push_unique(&mut raw, t);
        }
    }

    let mut out: Vec<String> = Vec::new();
    // Aliases first — API 404s on "Cocacola" / "Coke" but hits on "Coca-Cola".
    for r in &raw {
        let cleaned = strip_brand_noise(r);
        for candidate in [cleaned.as_str(), r.as_str()] {
            if let Some(alias) = alias_for(candidate) {
                push_unique(&mut out, alias);
            }
        }
    }
    for r in &raw {
        let cleaned = strip_brand_noise(r);
        // Prefer cleaned ("Coke") over noisy OFF brands ("Coke ZERO").
        let candidate = if cleaned.is_empty() {
            r.as_str()
        } else {
            cleaned.as_str()
        };
        if let Some(core) = brand_core(candidate) {
            push_unique(&mut out, &core);
        }
        if let Some(stripped) = strip_legal_suffixes(candidate) {
            push_unique(&mut out, &stripped);
            if let Some(core) = brand_core(&stripped) {
                push_unique(&mut out, &core);
            }
        }
        // Also try legal-strip on the raw string (entity names rarely have noise).
        if cleaned != *r {
            if let Some(stripped) = strip_legal_suffixes(r) {
                push_unique(&mut out, &stripped);
            }
        }
        if !is_legal_stop(candidate) && candidate.chars().count() >= 4 {
            push_unique(&mut out, candidate);
        }
    }

    out.truncate(8);
    out
}

fn alias_for(s: &str) -> Option<&'static str> {
    let n = normalize(s);
    if n.is_empty() {
        return None;
    }
    let compact = n.replace(' ', "");
    for &(key, canon) in BRAND_ALIASES {
        let kn = normalize(key);
        let kc = kn.replace(' ', "");
        if n == kn || compact == kc {
            return Some(canon);
        }
    }
    None
}

fn strip_brand_noise(s: &str) -> String {
    let parts: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| !BRAND_NOISE.contains(&normalize(p).as_str()))
        .collect();
    if parts.is_empty() {
        s.trim().to_string()
    } else {
        parts.join(" ")
    }
}

fn push_unique(out: &mut Vec<String>, s: &str) {
    let t = s.trim();
    if t.is_empty() || is_legal_stop(t) {
        return;
    }
    if out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
        return;
    }
    out.push(t.to_string());
}

fn is_legal_stop(s: &str) -> bool {
    let n = normalize(s);
    LEGAL_STOP.contains(&n.as_str()) || n == "sa nv"
}

fn strip_legal_suffixes(s: &str) -> Option<String> {
    let mut parts: Vec<String> = s
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    let mut changed = false;
    while parts
        .last()
        .is_some_and(|p| is_legal_stop(p) || normalize(p) == "sa nv")
    {
        // "SA/NV" may be one token
        parts.pop();
        changed = true;
    }
    // Drop trailing "Services" / "Group" style words
    while parts.last().is_some_and(|p| is_legal_stop(p)) {
        parts.pop();
        changed = true;
    }
    if !changed || parts.is_empty() {
        return None;
    }
    let joined = parts.join(" ");
    if joined.chars().count() < 3 {
        return None;
    }
    Some(joined)
}

fn brand_core(s: &str) -> Option<String> {
    let tokens: Vec<&str> = s
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|t| !t.is_empty() && !is_legal_stop(t))
        .collect();
    let first = *tokens.first()?;
    let first = first.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
    if first.chars().count() >= 4 {
        return Some(first.to_string());
    }
    if tokens.len() >= 2 {
        let a = tokens[0];
        let b = tokens[1];
        if !is_legal_stop(b) {
            return Some(format!("{a} {b}"));
        }
    }
    None
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

fn best_match_among<'a>(
    query: &str,
    product: &ProductModel,
    companies: &'a [Company],
) -> Option<&'a Company> {
    let mut needles: Vec<String> = vec![normalize(query)];
    if let Some(b) = &product.brand_primary {
        needles.push(normalize(b));
        if let Some(c) = brand_core(b) {
            needles.push(normalize(&c));
        }
        if let Some(s) = strip_legal_suffixes(b) {
            needles.push(normalize(&s));
        }
    }
    if let Some(brands) = &product.brands {
        for part in brands.split([',', ';']) {
            let t = part.trim();
            if t.is_empty() {
                continue;
            }
            needles.push(normalize(t));
            if let Some(c) = brand_core(t) {
                needles.push(normalize(&c));
            }
        }
    }
    needles.retain(|n| n.chars().count() >= 3);
    needles.sort();
    needles.dedup();

    companies.iter().find(|c| {
        let Some(name) = c.name.as_deref() else {
            return false;
        };
        let n = normalize(name);
        needles.iter().any(|q| names_match(q, &n))
    })
}

fn names_match(query: &str, company: &str) -> bool {
    if query.len() < 3 || company.is_empty() {
        return false;
    }
    if company == query {
        return true;
    }
    // Prefer whole-token / substantial overlap to avoid "nv" ∈ "nvidia".
    if query.len() >= 4 && (company.contains(query) || query.contains(company)) {
        return true;
    }
    // Hyphenless: "cocacola" vs "coca cola"
    let q2 = query.replace(' ', "");
    let c2 = company.replace(' ', "");
    q2.len() >= 4 && c2.len() >= 4 && (c2.contains(&q2) || q2.contains(&c2))
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

/// Build the attribute source note: match label, brief why, cited alternatives.
fn match_source(hit: &Company, query: &str) -> String {
    let label = hit.name.as_deref().unwrap_or(query);
    let kind = humanize_type(hit.r#type.as_deref().unwrap_or("listed"));
    let mut parts = vec![format!("boycottisraeli.biz · “{label}” ({kind})")];
    if let Some(why) = brief_why(hit.description.as_deref()) {
        parts.push(format!("Why: {why}"));
    }
    if let Some(alts) = format_alternatives(&hit.alternatives) {
        parts.push(format!("Alternatives: {alts}"));
    }
    parts.join("\n")
}

fn humanize_type(t: &str) -> String {
    t.replace('-', " ")
}

/// Prefer the API’s “Israel Connection” blurb; else the first paragraph. Keep it short.
fn brief_why(description: Option<&str>) -> Option<String> {
    let raw = description?.trim();
    if raw.is_empty() {
        return None;
    }
    let stripped = strip_md_light(raw);
    let focus = if let Some(idx) = stripped
        .to_ascii_lowercase()
        .find("israel connection:")
    {
        stripped[idx + "israel connection:".len()..].trim()
    } else {
        stripped
            .split("\n\n")
            .next()
            .unwrap_or(stripped.as_str())
            .trim()
    };
    let focus = focus.trim_start_matches(['*', ':', '-', ' ']).trim();
    if focus.is_empty() {
        return None;
    }
    Some(truncate_words(focus, 240))
}

fn format_alternatives(alts: &[Alternative]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for a in alts.iter().take(4) {
        let Some(name) = a.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        match a
            .website
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(url) => parts.push(format!("{name} ({url})")),
            None => parts.push(name.to_string()),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn strip_md_light(s: &str) -> String {
    s.replace("**", "").replace("__", "")
}

fn truncate_words(s: &str, max_chars: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max_chars {
        return s;
    }
    let mut out = String::new();
    for w in s.split_whitespace() {
        let next_len = out.chars().count() + 1 + w.chars().count();
        if !out.is_empty() && next_len > max_chars.saturating_sub(1) {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    if out.is_empty() {
        s.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
    } else {
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::AttrAvailability;
    use std::collections::BTreeMap;

    fn product(brand: &str, name: &str) -> ProductModel {
        ProductModel {
            barcode: "1".into(),
            name: name.into(),
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
    fn normalize_folds_case_and_punct() {
        assert_eq!(normalize("Coca-Cola"), "coca cola");
        assert_eq!(normalize("  Nestlé  "), "nestlé");
    }

    #[test]
    fn brand_queries_prefer_coca_cola_core() {
        let p = product("COCA-COLA SERVICES SA/NV", "Cocacola zero sugar");
        let q = brand_queries(&p);
        assert!(
            q.iter().any(|x| normalize(x) == "coca cola" || x.eq_ignore_ascii_case("COCA-COLA")),
            "queries={q:?}"
        );
        assert!(!q.iter().any(|x| x.eq_ignore_ascii_case("NV")), "queries={q:?}");
        assert!(!q.iter().any(|x| x.eq_ignore_ascii_case("SA")), "queries={q:?}");
        assert!(
            !q.iter().any(|x| x.eq_ignore_ascii_case("Cocacola")),
            "must not use product-name token: {q:?}"
        );
    }

    #[test]
    fn brand_queries_ignore_product_name_tokens() {
        let p = product("Barilla", "Pesto Rosso 200g");
        let q = brand_queries(&p);
        assert_eq!(q, vec!["Barilla".to_string()], "queries={q:?}");
    }

    #[test]
    fn brand_queries_alias_cocacola_and_coke_zero() {
        let a = brand_queries(&product("Cocacola", "Cocacola zero"));
        assert!(
            a.iter().any(|x| x.eq_ignore_ascii_case("Coca-Cola")),
            "Cocacola → Coca-Cola: {a:?}"
        );
        let b = brand_queries(&product("Coke ZERO", "Cocacola mini zero sugar"));
        assert!(
            b.first().is_some_and(|x| x.eq_ignore_ascii_case("Coca-Cola")),
            "Coke ZERO should prefer Coca-Cola first: {b:?}"
        );
        assert!(
            !b.iter().any(|x| normalize(x) == "coke zero"),
            "should not query noisy brand as-is: {b:?}"
        );
    }

    #[test]
    fn match_accepts_coca_cola_company() {
        let cos = vec![Company {
            name: Some("The Coca-Cola Company".into()),
            r#type: Some("supports-israel".into()),
            description: None,
            alternatives: vec![],
        }];
        let p = product("COCA-COLA SERVICES SA/NV", "Cocacola zero sugar");
        assert!(best_match_among("COCA-COLA", &p, &cos).is_some());
        let other = product("Unilever", "soap");
        assert!(best_match_among("Unilever", &other, &cos).is_none());
    }

    #[test]
    fn short_tokens_do_not_match_nvidia() {
        let cos = vec![Company {
            name: Some("NVIDIA".into()),
            r#type: Some("supports-israel".into()),
            description: None,
            alternatives: vec![],
        }];
        let p = product("NV", "x");
        assert!(best_match_among("NV", &p, &cos).is_none());
    }

    #[test]
    fn match_source_includes_why_and_alternatives() {
        let hit = Company {
            name: Some("Coca-Cola".into()),
            r#type: Some("supports-israel".into()),
            description: Some(
                "Coca-Cola operates in Israel.\n\n**Israel Connection:** Israeli bottler operates in the West Bank. Long-standing BDS target.".into(),
            ),
            alternatives: vec![
                Alternative {
                    name: Some("Cawston Press".into()),
                    website: Some("https://www.cawstonpress.com".into()),
                },
                Alternative {
                    name: Some("Fever-Tree".into()),
                    website: Some("https://www.fever-tree.com".into()),
                },
            ],
        };
        let src = match_source(&hit, "Coca-Cola");
        assert!(src.contains("Coca-Cola"), "{src}");
        assert!(src.contains("supports israel"), "{src}");
        assert!(
            src.contains("Why:") && src.contains("West Bank"),
            "{src}"
        );
        assert!(
            src.contains("Alternatives:")
                && src.contains("Cawston Press (https://www.cawstonpress.com)")
                && src.contains("Fever-Tree (https://www.fever-tree.com)"),
            "{src}"
        );
    }

    #[test]
    #[ignore = "live network"]
    fn live_coca_cola_services_entity_matches() {
        let mut p = product("COCA-COLA SERVICES SA/NV", "Cocacola zero sugar");
        apply(&mut p);
        let attr = p.attributes.get(attrs::BOYCOTT_LISTED).expect("attr");
        assert_eq!(attr.availability, AttrAvailability::Present);
        assert_eq!(attr.value, Some(1.0), "src={}", attr.source);
        assert!(
            attr.source.contains("Why:") || attr.source.contains("Alternatives:"),
            "expected why/alternatives in src={}",
            attr.source
        );
    }

    #[test]
    #[ignore = "live network"]
    fn live_cocacola_alias_matches() {
        let mut p = product("Cocacola", "Cocacola zero");
        apply(&mut p);
        let attr = p.attributes.get(attrs::BOYCOTT_LISTED).expect("attr");
        assert_eq!(attr.value, Some(1.0), "src={}", attr.source);
        assert!(attr.source.contains("Why:"), "src={}", attr.source);
    }

    #[test]
    #[ignore = "live network"]
    fn live_coke_zero_alias_matches() {
        let mut p = product("Coke ZERO", "mini");
        apply(&mut p);
        let attr = p.attributes.get(attrs::BOYCOTT_LISTED).expect("attr");
        assert_eq!(attr.value, Some(1.0), "src={}", attr.source);
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
