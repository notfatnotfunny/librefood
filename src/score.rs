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
    pub weighted_delta: f64,
}

#[derive(Debug, Clone)]
pub struct ScoreSummary {
    pub score: u8,
    pub score_display: String,
    pub label: String,
    pub status: FactorStatus,
    pub profile_id: String,
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
        "Profile “{}” (`{}`) v{} · {} pts · base contributions below",
        result.profile_name, result.profile_id, result.profile_version, result.score as u8
    ));

    for c in &result.contributions {
        let rule_bit = c
            .rule_label
            .clone()
            .unwrap_or_else(|| c.note.clone());
        let raw_bit = match c.raw_value {
            Some(v) => format!("{v:.2}"),
            None => "—".into(),
        };
        let adj_bit = match c.rule_adjust {
            Some(a) => format!("{a:+.1}"),
            None => "—".into(),
        };
        let detail = if c.attribute == "boycott_listed" {
            // Boycott source already carries why + alternatives; show on its own lines.
            format!("{rule_bit}\n{}", c.source)
        } else {
            format!("{rule_bit} · src {}", c.source)
        };
        let row = FactorRow {
            title: c.title.clone(),
            detail,
            value_label: c.value_label.clone(),
            status: FactorStatus::from(c.tone),
            weighted_delta: c.weighted_delta,
        };
        explanation_lines.push(format!(
            "{title} [{id}/{attr}]: value={value} (raw={raw}) · adjust={adj} · Δ {delta:+.1} (w={w:.2}) · src={source} · {note}",
            title = c.title,
            id = c.criterion_id,
            attr = c.attribute,
            value = c.value_label,
            raw = raw_bit,
            adj = adj_bit,
            delta = c.weighted_delta,
            w = c.weight,
            source = c.source,
            note = c.note
        ));
        match c.polarity {
            FactorPolarity::Positive => positives.push(row),
            FactorPolarity::Negative | FactorPolarity::Neutral => negatives.push(row),
        }
    }

    for cap in &result.applied_caps {
        let bounds = match (cap.max_score, cap.min_score) {
            (Some(max), Some(min)) => format!("clamp [{min:.0}, {max:.0}]"),
            (Some(max), None) => format!("max {max:.0}"),
            (None, Some(min)) => format!("min {min:.0}"),
            (None, None) => "no bound".into(),
        };
        explanation_lines.push(format!(
            "Cap applied: {} (attr {} = {:.2}) · {}",
            cap.label, cap.attribute, cap.raw_value, bounds
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
        profile_id: result.profile_id.clone(),
        profile_name: result.profile_name.clone(),
        profile_version: result.profile_version,
        incomplete: result.incomplete,
        negatives,
        positives,
        explanation_lines,
        missing: result.missing_attributes.clone(),
    }
}
