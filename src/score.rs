//! UI presentation helpers from engine [`ScoreResult`] (Negatives / Positives rows).

use crate::engine::{eval::ScoreResult, FactorPolarity, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorStatus {
    Good,
    Mediocre,
    Poor,
}

impl From<Tone> for FactorStatus {
    fn from(t: Tone) -> Self {
        match t {
            Tone::Good => Self::Good,
            Tone::Mediocre => Self::Mediocre,
            Tone::Poor => Self::Poor,
            Tone::Neutral => Self::Mediocre,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorRow {
    pub title: String,
    pub detail: String,
    pub value_label: String,
    pub status: FactorStatus,
    pub polarity: FactorPolarity,
    pub weighted_delta: f64,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ScoreSummary {
    pub score: u8,
    pub score_display: String,
    pub label: String,
    pub status: FactorStatus,
    pub profile_name: String,
    pub profile_version: u32,
    pub incomplete: bool,
    pub negatives: Vec<FactorRow>,
    pub positives: Vec<FactorRow>,
    pub explanation_lines: Vec<String>,
    pub missing: Vec<String>,
}

pub fn summary_from_result(result: &ScoreResult) -> ScoreSummary {
    let mut negatives = Vec::new();
    let mut positives = Vec::new();
    let mut explanation_lines = Vec::new();

    explanation_lines.push(format!(
        "Profile “{}” v{} · base contributions below",
        result.profile_name, result.profile_version
    ));

    for c in &result.contributions {
        let detail = c
            .rule_label
            .clone()
            .unwrap_or_else(|| c.note.clone());
        let row = FactorRow {
            title: c.title.clone(),
            detail,
            value_label: c.value_label.clone(),
            status: FactorStatus::from(c.tone),
            polarity: c.polarity,
            weighted_delta: c.weighted_delta,
            source: c.source.clone(),
        };
        explanation_lines.push(format!(
            "{title}: value={value} · Δ {delta:+.1} (w={w:.2}) · {note}",
            title = c.title,
            value = c.value_label,
            delta = c.weighted_delta,
            w = c.weight,
            note = c.note
        ));
        match c.polarity {
            FactorPolarity::Positive => positives.push(row),
            FactorPolarity::Negative | FactorPolarity::Neutral => negatives.push(row),
        }
    }

    for cap in &result.applied_caps {
        explanation_lines.push(format!(
            "Cap applied: {} (attr {} = {})",
            cap.label, cap.attribute, cap.raw_value
        ));
    }
    if result.incomplete {
        explanation_lines.push("Result marked incomplete due to missing data policy.".into());
    }

    ScoreSummary {
        score: result.score.clamp(0.0, 255.0) as u8,
        score_display: result.score_display.clone(),
        label: result.label.clone(),
        status: FactorStatus::from(result.tone),
        profile_name: result.profile_name.clone(),
        profile_version: result.profile_version,
        incomplete: result.incomplete,
        negatives,
        positives,
        explanation_lines,
        missing: result.missing_attributes.clone(),
    }
}
