//! Transparent Librefood product scoring heuristics (Yuka-*like layout*, not Yuka's algorithm).
//!
//! Scores are derived from Open Food Facts nutriments / additive counts and optional Nutri-Score.
//! Thresholds follow common EU-style “low / medium / high” nutrition cue ranges per 100 g.

use crate::off::ProductFacts;

/// Traffic-light status for a factor row or overall score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorStatus {
    Good,
    Mediocre,
    Poor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorKind {
    Negative,
    Positive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorId {
    Energy,
    Additives,
    SaturatedFat,
    Sugars,
    Salt,
    Fiber,
    Proteins,
}

#[derive(Debug, Clone)]
pub struct FactorRow {
    pub id: FactorId,
    pub kind: FactorKind,
    pub title: &'static str,
    pub detail: String,
    pub value_label: String,
    pub status: FactorStatus,
}

#[derive(Debug, Clone)]
pub struct ScoreSummary {
    /// Librefood heuristic score, 0–100.
    pub score: u8,
    /// Short English label (Excellent / Good / Mediocre / Poor).
    pub label: &'static str,
    pub status: FactorStatus,
    pub negatives: Vec<FactorRow>,
    pub positives: Vec<FactorRow>,
}

pub fn score_label(score: u8) -> &'static str {
    match score {
        75..=255 => "Excellent",
        55..=74 => "Good",
        30..=54 => "Mediocre",
        _ => "Poor",
    }
}

pub fn status_for_score(score: u8) -> FactorStatus {
    match score {
        75..=255 => FactorStatus::Good,
        55..=74 => FactorStatus::Good,
        30..=54 => FactorStatus::Mediocre,
        _ => FactorStatus::Poor,
    }
}

/// Map Nutri-Score letter to a 0–100 anchor (public OFF grade, not a proprietary score).
pub fn nutriscore_anchor(grade: char) -> Option<u8> {
    match grade.to_ascii_lowercase() {
        'a' => Some(85),
        'b' => Some(70),
        'c' => Some(50),
        'd' => Some(30),
        'e' => Some(12),
        _ => None,
    }
}

fn fmt_kcal(v: f64) -> String {
    format!("{:.0} kcal", v.round())
}

fn fmt_grams(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{:.0}g", v.round())
    } else {
        format!("{:.1}g", v)
    }
}

fn energy_row(kcal: f64) -> FactorRow {
    let (status, detail) = if kcal <= 150.0 {
        (FactorStatus::Good, "Low calorie density")
    } else if kcal <= 400.0 {
        (FactorStatus::Mediocre, "A bit too caloric")
    } else {
        (FactorStatus::Poor, "Too caloric")
    };
    FactorRow {
        id: FactorId::Energy,
        kind: FactorKind::Negative,
        title: "Energy",
        detail: detail.into(),
        value_label: fmt_kcal(kcal),
        status,
    }
}

fn additives_row(count: u32) -> FactorRow {
    let (status, detail) = if count == 0 {
        (FactorStatus::Good, "No additives listed")
    } else if count <= 2 {
        (FactorStatus::Mediocre, "Some additives")
    } else {
        (FactorStatus::Poor, "Contains additives to avoid")
    };
    FactorRow {
        id: FactorId::Additives,
        kind: FactorKind::Negative,
        title: "Additives",
        detail: detail.into(),
        value_label: count.to_string(),
        status,
    }
}

fn saturated_fat_row(g: f64) -> FactorRow {
    let (status, detail) = if g <= 1.5 {
        (FactorStatus::Good, "Low saturated fat")
    } else if g <= 5.0 {
        (FactorStatus::Mediocre, "A bit too fatty")
    } else {
        (FactorStatus::Poor, "High saturated fat")
    };
    FactorRow {
        id: FactorId::SaturatedFat,
        kind: FactorKind::Negative,
        title: "Saturated fats",
        detail: detail.into(),
        value_label: fmt_grams(g),
        status,
    }
}

fn sugars_row(g: f64) -> FactorRow {
    let (status, detail) = if g <= 5.0 {
        (FactorStatus::Good, "Low sugar")
    } else if g <= 22.5 {
        (FactorStatus::Mediocre, "A bit too sugary")
    } else {
        (FactorStatus::Poor, "High sugar")
    };
    FactorRow {
        id: FactorId::Sugars,
        kind: FactorKind::Negative,
        title: "Sugars",
        detail: detail.into(),
        value_label: fmt_grams(g),
        status,
    }
}

fn salt_row(g: f64) -> FactorRow {
    let (status, detail) = if g <= 0.3 {
        (FactorStatus::Good, "Low salt")
    } else if g <= 1.5 {
        (FactorStatus::Mediocre, "A bit salty")
    } else {
        (FactorStatus::Poor, "High salt")
    };
    FactorRow {
        id: FactorId::Salt,
        kind: FactorKind::Negative,
        title: "Salt",
        detail: detail.into(),
        value_label: fmt_grams(g),
        status,
    }
}

fn fiber_row(g: f64) -> FactorRow {
    let (status, detail) = if g >= 6.0 {
        (FactorStatus::Good, "Excellent amount of fiber")
    } else if g >= 3.0 {
        (FactorStatus::Good, "Good amount of fiber")
    } else if g >= 1.5 {
        (FactorStatus::Mediocre, "Some fiber")
    } else {
        (FactorStatus::Poor, "Low fiber")
    };
    FactorRow {
        id: FactorId::Fiber,
        kind: FactorKind::Positive,
        title: "Fiber",
        detail: detail.into(),
        value_label: fmt_grams(g),
        status,
    }
}

fn proteins_row(g: f64) -> FactorRow {
    let (status, detail) = if g >= 10.0 {
        (FactorStatus::Good, "Excellent amount of protein")
    } else if g >= 5.0 {
        (FactorStatus::Good, "Some protein")
    } else if g >= 2.5 {
        (FactorStatus::Mediocre, "Low protein")
    } else {
        (FactorStatus::Poor, "Very little protein")
    };
    FactorRow {
        id: FactorId::Proteins,
        kind: FactorKind::Positive,
        title: "Proteins",
        detail: detail.into(),
        value_label: fmt_grams(g),
        status,
    }
}

fn status_delta(kind: FactorKind, status: FactorStatus) -> f64 {
    match (kind, status) {
        (FactorKind::Negative, FactorStatus::Poor) => -12.0,
        (FactorKind::Negative, FactorStatus::Mediocre) => -6.0,
        (FactorKind::Negative, FactorStatus::Good) => 2.0,
        (FactorKind::Positive, FactorStatus::Good) => 8.0,
        (FactorKind::Positive, FactorStatus::Mediocre) => 2.0,
        (FactorKind::Positive, FactorStatus::Poor) => -4.0,
    }
}

/// Build negative/positive rows and a 0–100 Librefood score from OFF facts.
pub fn evaluate(facts: &ProductFacts) -> ScoreSummary {
    let mut negatives = Vec::new();
    let mut positives = Vec::new();

    if let Some(v) = facts.energy_kcal_100g {
        negatives.push(energy_row(v));
    }
    if let Some(n) = facts.additives_n {
        negatives.push(additives_row(n));
    } else if !facts.additives_tags.is_empty() {
        negatives.push(additives_row(facts.additives_tags.len() as u32));
    }
    if let Some(v) = facts.saturated_fat_100g {
        negatives.push(saturated_fat_row(v));
    }
    if let Some(v) = facts.sugars_100g {
        negatives.push(sugars_row(v));
    }
    if let Some(v) = facts.salt_100g {
        negatives.push(salt_row(v));
    }
    if let Some(v) = facts.fiber_100g {
        positives.push(fiber_row(v));
    }
    if let Some(v) = facts.proteins_100g {
        positives.push(proteins_row(v));
    }

    let mut from_factors = 55.0_f64;
    for row in negatives.iter().chain(positives.iter()) {
        from_factors += status_delta(row.kind, row.status);
    }
    from_factors = from_factors.clamp(0.0, 100.0);

    let score = match facts.nutriscore_grade.and_then(nutriscore_anchor) {
        Some(anchor) => ((from_factors + f64::from(anchor)) / 2.0).round() as u8,
        None => from_factors.round() as u8,
    };

    ScoreSummary {
        score,
        label: score_label(score),
        status: status_for_score(score),
        negatives,
        positives,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::off::ProductFacts;

    fn sample_camille_like() -> ProductFacts {
        ProductFacts {
            nutriscore_grade: Some('d'),
            energy_kcal_100g: Some(393.0),
            sugars_100g: Some(25.2),
            saturated_fat_100g: Some(4.7),
            salt_100g: Some(0.8),
            fiber_100g: Some(6.0),
            proteins_100g: Some(6.2),
            additives_n: Some(6),
            additives_tags: vec![
                "en:e322".into(),
                "en:e500".into(),
                "en:e503".into(),
                "en:e450".into(),
                "en:e471".into(),
                "en:e330".into(),
            ],
        }
    }

    #[test]
    fn camille_like_is_low_with_expected_rows() {
        let s = evaluate(&sample_camille_like());
        assert!(s.score < 45, "expected low score, got {}", s.score);
        assert!(
            matches!(s.label, "Poor" | "Mediocre"),
            "unexpected label {}",
            s.label
        );

        let energy = s.negatives.iter().find(|r| r.id == FactorId::Energy).unwrap();
        assert_eq!(energy.status, FactorStatus::Mediocre);
        assert!(energy.value_label.contains("393"));

        let add = s.negatives.iter().find(|r| r.id == FactorId::Additives).unwrap();
        assert_eq!(add.status, FactorStatus::Poor);
        assert_eq!(add.value_label, "6");

        let sugars = s.negatives.iter().find(|r| r.id == FactorId::Sugars).unwrap();
        assert_eq!(sugars.status, FactorStatus::Poor);

        let fiber = s.positives.iter().find(|r| r.id == FactorId::Fiber).unwrap();
        assert_eq!(fiber.status, FactorStatus::Good);
        assert!(fiber.detail.to_lowercase().contains("excellent"));
    }

    #[test]
    fn nutella_like_high_sugar_and_fat() {
        let facts = ProductFacts {
            nutriscore_grade: Some('e'),
            energy_kcal_100g: Some(539.0),
            sugars_100g: Some(56.3),
            saturated_fat_100g: Some(10.6),
            salt_100g: Some(0.107),
            fiber_100g: Some(0.0),
            proteins_100g: Some(6.3),
            additives_n: Some(0),
            additives_tags: vec![],
        };
        let s = evaluate(&facts);
        assert!(s.score <= 35, "expected poor, got {}", s.score);
        let sugars = s.negatives.iter().find(|r| r.id == FactorId::Sugars).unwrap();
        assert_eq!(sugars.status, FactorStatus::Poor);
        let energy = s.negatives.iter().find(|r| r.id == FactorId::Energy).unwrap();
        assert_eq!(energy.status, FactorStatus::Poor);
        let add = s.negatives.iter().find(|r| r.id == FactorId::Additives).unwrap();
        assert_eq!(add.status, FactorStatus::Good);
    }

    #[test]
    fn labels_and_anchors() {
        assert_eq!(score_label(90), "Excellent");
        assert_eq!(score_label(60), "Good");
        assert_eq!(score_label(40), "Mediocre");
        assert_eq!(score_label(10), "Poor");
        assert_eq!(nutriscore_anchor('A'), Some(85));
        assert_eq!(nutriscore_anchor('e'), Some(12));
    }

    #[test]
    fn empty_facts_still_scores() {
        let s = evaluate(&ProductFacts::default());
        assert_eq!(s.score, 55);
        assert!(s.negatives.is_empty());
        assert!(s.positives.is_empty());
    }
}
