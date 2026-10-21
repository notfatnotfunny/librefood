//! Deterministic scoring engine — identical inputs ⇒ identical results.

use super::schema::{
    Criterion, FactorPolarity, MissingDataPolicy, ScoreCap, ScoringProfile, Tone,
};

#[cfg(test)]
use super::schema::{ThresholdRule, When};
use crate::product::{AttrAvailability, ProductModel};

/// One explained step in the calculation.
#[derive(Debug, Clone)]
pub struct Contribution {
    pub criterion_id: String,
    pub title: String,
    pub attribute: String,
    pub polarity: FactorPolarity,
    pub raw_value: Option<f64>,
    pub value_label: String,
    pub source: String,
    pub availability: AttrAvailability,
    pub weight: f64,
    pub rule_label: Option<String>,
    pub rule_adjust: Option<f64>,
    pub weighted_delta: f64,
    pub tone: Tone,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct AppliedCap {
    pub label: String,
    pub attribute: String,
    pub raw_value: f64,
    pub max_score: Option<f64>,
    pub min_score: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ScoreResult {
    pub profile_id: String,
    pub profile_name: String,
    #[allow(dead_code)] // kept for reproducibility; not shown in UI
    pub profile_version: u32,
    pub score: f64,
    pub score_display: String,
    pub label: String,
    pub tone: Tone,
    pub incomplete: bool,
    pub contributions: Vec<Contribution>,
    pub applied_caps: Vec<AppliedCap>,
    pub missing_attributes: Vec<String>,
}

/// Score a product with a profile. Pure / deterministic.
pub fn evaluate(product: &ProductModel, profile: &ScoringProfile) -> ScoreResult {
    let criteria = resolve_criteria(product, profile);
    let mut score = profile.base_score;
    let mut contributions = Vec::new();
    let mut missing_attributes = Vec::new();
    let mut incomplete = false;

    for criterion in criteria {
        let (contrib, flag_incomplete) = evaluate_criterion(product, profile, criterion);
        if flag_incomplete {
            incomplete = true;
        }
        if contrib.availability == AttrAvailability::Missing
            || contrib.availability == AttrAvailability::Unverified
        {
            if !missing_attributes.contains(&criterion.attribute) {
                missing_attributes.push(criterion.attribute.clone());
            }
        }
        score += contrib.weighted_delta;
        contributions.push(contrib);
    }

    let mut applied_caps = Vec::new();
    for cap in &profile.caps {
        if let Some(applied) = apply_cap(product, cap, &mut score) {
            applied_caps.push(applied);
        }
    }

    score = profile.scale.clamp(score);
    // Stable half-up rounding to nearest integer for display consistency.
    let rounded = round_half_up(score);
    let (label, tone) = profile.label_for_score(rounded);
    let max = profile.scale.max();
    let score_display = if (max - 100.0).abs() < 1e-9 && profile.scale.min().abs() < 1e-9 {
        format!("{:.0}/100", rounded)
    } else {
        format!("{:.0}", rounded)
    };

    ScoreResult {
        profile_id: profile.id.clone(),
        profile_name: profile.name.clone(),
        profile_version: profile.version,
        score: rounded,
        score_display,
        label,
        tone,
        incomplete,
        contributions,
        applied_caps,
        missing_attributes,
    }
}

fn resolve_criteria<'a>(product: &ProductModel, profile: &'a ScoringProfile) -> &'a [Criterion] {
    for over in &profile.category_overrides {
        if over
            .category_match
            .iter()
            .any(|m| product.is_in_category(m))
        {
            return &over.criteria;
        }
    }
    &profile.criteria
}

fn evaluate_criterion(
    product: &ProductModel,
    profile: &ScoringProfile,
    criterion: &Criterion,
) -> (Contribution, bool) {
    let policy = criterion
        .missing_policy
        .unwrap_or(profile.missing_data);
    let attr = product.get_attr(&criterion.attribute);

    let (raw_value, availability, source) = match attr {
        Some(a) => (a.value, a.availability, a.source.clone()),
        None => (
            None,
            AttrAvailability::Missing,
            format!("attribute `{}` not provided by data source", criterion.attribute),
        ),
    };

    let value_label = match raw_value {
        Some(v) => format_value(&criterion.attribute, v),
        None => "—".into(),
    };

    if raw_value.is_none() || availability != AttrAvailability::Present {
        let mut incomplete = false;
        let (delta, note, tone) = match policy {
            MissingDataPolicy::Ignore | MissingDataPolicy::Unknown => {
                (0.0, "Missing data — ignored".into(), Tone::Neutral)
            }
            MissingDataPolicy::Penalize => {
                let p = criterion
                    .missing_penalty
                    .unwrap_or(profile.default_missing_penalty);
                let d = p * criterion.weight;
                (
                    d,
                    format!("Missing data — penalty {p} × weight {:.2}", criterion.weight),
                    Tone::Poor,
                )
            }
            MissingDataPolicy::Incomplete => {
                incomplete = true;
                (
                    0.0,
                    "Missing data — result marked incomplete".into(),
                    Tone::Mediocre,
                )
            }
        };
        return (
            Contribution {
                criterion_id: criterion.id.clone(),
                title: criterion.title.clone(),
                attribute: criterion.attribute.clone(),
                polarity: criterion.polarity,
                raw_value,
                value_label,
                source,
                availability,
                weight: criterion.weight,
                rule_label: None,
                rule_adjust: None,
                weighted_delta: delta,
                tone,
                note,
            },
            incomplete,
        );
    }

    let v = raw_value.unwrap();
    let matched = criterion.rules.iter().find(|r| r.when.matches(v));
    let (rule_label, rule_adjust, tone, note) = match matched {
        Some(rule) => (
            Some(rule.label.clone()),
            Some(rule.adjust),
            rule.tone,
            format!(
                "Rule “{}” matched (adjust {:+} × weight {:.2})",
                rule.label, rule.adjust, criterion.weight
            ),
        ),
        None => (
            None,
            Some(0.0),
            Tone::Neutral,
            "No matching rule — zero adjustment".into(),
        ),
    };
    let adjust = rule_adjust.unwrap_or(0.0);
    let weighted = adjust * criterion.weight;

    (
        Contribution {
            criterion_id: criterion.id.clone(),
            title: criterion.title.clone(),
            attribute: criterion.attribute.clone(),
            polarity: criterion.polarity,
            raw_value: Some(v),
            value_label,
            source,
            availability,
            weight: criterion.weight,
            rule_label,
            rule_adjust,
            weighted_delta: weighted,
            tone,
            note,
        },
        false,
    )
}

fn apply_cap(product: &ProductModel, cap: &ScoreCap, score: &mut f64) -> Option<AppliedCap> {
    let v = product.attr_number(&cap.attribute)?;
    if !cap.when.matches(v) {
        return None;
    }
    if let Some(max) = cap.max_score {
        if *score > max {
            *score = max;
        }
    }
    if let Some(min) = cap.min_score {
        if *score < min {
            *score = min;
        }
    }
    Some(AppliedCap {
        label: cap.label.clone(),
        attribute: cap.attribute.clone(),
        raw_value: v,
        max_score: cap.max_score,
        min_score: cap.min_score,
    })
}

fn format_value(attribute: &str, v: f64) -> String {
    if attribute == "additives_n"
        || attribute == "nova_group"
        || attribute == "allergen_count"
        || attribute == "nutriscore_ordinal"
        || attribute == "organic"
        || attribute == "boycott_listed"
        || attribute == "wikirate_score"
    {
        if attribute == "wikirate_score" {
            return format!("{:.0}/100", v.round());
        }
        if (v - v.round()).abs() < 1e-9 {
            return format!("{}", v.round() as i64);
        }
    }
    if attribute.contains("energy") {
        return format!("{:.0} kcal", v.round());
    }
    if (v - v.round()).abs() < 0.05 {
        format!("{:.0}g", v.round())
    } else {
        format!("{:.1}g", v)
    }
}

fn round_half_up(v: f64) -> f64 {
    if v >= 0.0 {
        (v + 0.5).floor()
    } else {
        (v - 0.5).ceil()
    }
}

/// Helper for tests: build a single always-matching rule.
#[cfg(test)]
pub fn always_rule(adjust: f64, label: &str) -> ThresholdRule {
    ThresholdRule {
        when: When::Always,
        adjust,
        label: label.into(),
        tone: Tone::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::schema::*;
    use crate::product::{attrs, AttrValue};
    use std::collections::BTreeMap;

    fn product_with(attrs_map: Vec<(&str, f64)>) -> ProductModel {
        let mut attributes = BTreeMap::new();
        for (k, v) in attrs_map {
            attributes.insert(k.to_string(), AttrValue::present(v, "test"));
        }
        ProductModel {
            barcode: "1".into(),
            name: "Test".into(),
            brands: None,
            brand_primary: None,
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
            attributes,
        }
    }

    fn base_profile(criteria: Vec<Criterion>) -> ScoringProfile {
        ScoringProfile {
            id: "t".into(),
            name: "Test".into(),
            description: String::new(),
            version: 1,
            scale: ScoreScale::default(),
            base_score: 50.0,
            missing_data: MissingDataPolicy::Ignore,
            default_missing_penalty: -5.0,
            labels: vec![
                ScoreLabelBand {
                    min: 75.0,
                    label: "Excellent".into(),
                    tone: Tone::Good,
                },
                ScoreLabelBand {
                    min: 55.0,
                    label: "Good".into(),
                    tone: Tone::Good,
                },
                ScoreLabelBand {
                    min: 30.0,
                    label: "Mediocre".into(),
                    tone: Tone::Mediocre,
                },
                ScoreLabelBand {
                    min: 0.0,
                    label: "Poor".into(),
                    tone: Tone::Poor,
                },
            ],
            criteria,
            caps: vec![],
            category_overrides: vec![],
            builtin: false,
        }
    }

    #[test]
    fn weighted_threshold_and_cap() {
        let profile = ScoringProfile {
            caps: vec![ScoreCap {
                attribute: attrs::ADDITIVES_N.into(),
                when: When::Gte { value: 5.0 },
                max_score: Some(40.0),
                min_score: None,
                label: "Many additives".into(),
            }],
            ..base_profile(vec![Criterion {
                id: "sugars".into(),
                title: "Sugars".into(),
                attribute: attrs::SUGARS_100G.into(),
                weight: 2.0,
                polarity: FactorPolarity::Negative,
                rules: vec![
                    ThresholdRule {
                        when: When::Lte { value: 5.0 },
                        adjust: 5.0,
                        label: "Low".into(),
                        tone: Tone::Good,
                    },
                    ThresholdRule {
                        when: When::Gt { value: 5.0 },
                        adjust: -10.0,
                        label: "High".into(),
                        tone: Tone::Poor,
                    },
                ],
                missing_policy: None,
                missing_penalty: None,
            }])
        };

        let p = product_with(vec![
            (attrs::SUGARS_100G, 20.0),
            (attrs::ADDITIVES_N, 6.0),
        ]);
        let r = evaluate(&p, &profile);
        // 50 + (-10)*2 = 30, then capped to 40? 30 < 40 so stays 30
        assert_eq!(r.score, 30.0);
        assert_eq!(r.label, "Mediocre");
        assert_eq!(r.contributions[0].weighted_delta, -20.0);

        let p2 = product_with(vec![
            (attrs::SUGARS_100G, 1.0),
            (attrs::ADDITIVES_N, 6.0),
        ]);
        let r2 = evaluate(&p2, &profile);
        // 50 + 5*2 = 60, capped to 40
        assert_eq!(r2.score, 40.0);
        assert_eq!(r2.applied_caps.len(), 1);
    }

    #[test]
    fn missing_penalize_and_incomplete() {
        let mut profile = base_profile(vec![Criterion {
            id: "fiber".into(),
            title: "Fiber".into(),
            attribute: attrs::FIBER_100G.into(),
            weight: 1.0,
            polarity: FactorPolarity::Positive,
            rules: vec![always_rule(8.0, "ok")],
            missing_policy: Some(MissingDataPolicy::Penalize),
            missing_penalty: Some(-7.0),
        }]);
        let p = product_with(vec![]);
        let r = evaluate(&p, &profile);
        assert_eq!(r.score, 43.0); // 50-7

        profile.criteria[0].missing_policy = Some(MissingDataPolicy::Incomplete);
        let r2 = evaluate(&p, &profile);
        assert!(r2.incomplete);
        assert_eq!(r2.score, 50.0);
    }

    #[test]
    fn deterministic_repeat() {
        let profile = base_profile(vec![Criterion {
            id: "energy".into(),
            title: "Energy".into(),
            attribute: attrs::ENERGY_KCAL_100G.into(),
            weight: 1.0,
            polarity: FactorPolarity::Negative,
            rules: vec![
                ThresholdRule {
                    when: When::Lte { value: 150.0 },
                    adjust: 4.0,
                    label: "Low".into(),
                    tone: Tone::Good,
                },
                ThresholdRule {
                    when: When::Gt { value: 150.0 },
                    adjust: -8.0,
                    label: "High".into(),
                    tone: Tone::Poor,
                },
            ],
            missing_policy: None,
            missing_penalty: None,
        }]);
        let p = product_with(vec![(attrs::ENERGY_KCAL_100G, 393.0)]);
        let a = evaluate(&p, &profile);
        let b = evaluate(&p, &profile);
        assert_eq!(a.score, b.score);
        assert_eq!(a.score_display, b.score_display);
        assert_eq!(a.contributions.len(), b.contributions.len());
    }

    #[test]
    fn category_override_switches_criteria() {
        let profile = ScoringProfile {
            category_overrides: vec![CategoryOverride {
                category_match: vec!["beverages".into()],
                criteria: vec![Criterion {
                    id: "sugars_bev".into(),
                    title: "Sugars (beverage)".into(),
                    attribute: attrs::SUGARS_100G.into(),
                    weight: 1.0,
                    polarity: FactorPolarity::Negative,
                    rules: vec![always_rule(-30.0, "drink sugar")],
                    missing_policy: None,
                    missing_penalty: None,
                }],
            }],
            ..base_profile(vec![Criterion {
                id: "sugars".into(),
                title: "Sugars".into(),
                attribute: attrs::SUGARS_100G.into(),
                weight: 1.0,
                polarity: FactorPolarity::Negative,
                rules: vec![always_rule(-5.0, "solid sugar")],
                missing_policy: None,
                missing_penalty: None,
            }])
        };
        let mut p = product_with(vec![(attrs::SUGARS_100G, 10.0)]);
        p.categories_tags = vec!["en:beverages".into()];
        let r = evaluate(&p, &profile);
        assert_eq!(r.score, 20.0); // 50-30
        assert_eq!(r.contributions[0].criterion_id, "sugars_bev");
    }
}
