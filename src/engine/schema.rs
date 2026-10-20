//! Declarative scoring-profile schema (JSON-serializable, no executable code).

use serde::{Deserialize, Serialize};

/// How missing attribute values are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissingDataPolicy {
    /// Skip the criterion (no adjustment).
    #[default]
    Ignore,
    /// Apply a fixed penalty (see `missing_penalty` on the criterion or profile default).
    Penalize,
    /// Record as unknown; no score change.
    Unknown,
    /// Mark the whole result incomplete; still compute a partial score.
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Good,
    #[default]
    Neutral,
    Mediocre,
    Poor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactorPolarity {
    #[default]
    Negative,
    Positive,
    Neutral,
}

/// Output scale for the profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScoreScale {
    Numeric { min: f64, max: f64 },
}

impl Default for ScoreScale {
    fn default() -> Self {
        Self::Numeric {
            min: 0.0,
            max: 100.0,
        }
    }
}

impl ScoreScale {
    pub fn clamp(&self, v: f64) -> f64 {
        match self {
            Self::Numeric { min, max } => v.clamp(*min, *max),
        }
    }

    pub fn max(&self) -> f64 {
        match self {
            Self::Numeric { max, .. } => *max,
        }
    }

    pub fn min(&self) -> f64 {
        match self {
            Self::Numeric { min, .. } => *min,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreLabelBand {
    /// Inclusive lower bound on the final numeric score.
    pub min: f64,
    pub label: String,
    #[serde(default)]
    pub tone: Tone,
}

/// Predicate on a numeric attribute value (first match wins within a criterion).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum When {
    Always,
    Lte { value: f64 },
    Lt { value: f64 },
    Gte { value: f64 },
    Gt { value: f64 },
    Eq { value: f64 },
    Between { min: f64, max: f64 },
}

impl When {
    pub fn matches(&self, v: f64) -> bool {
        match self {
            Self::Always => true,
            Self::Lte { value } => v <= *value,
            Self::Lt { value } => v < *value,
            Self::Gte { value } => v >= *value,
            Self::Gt { value } => v > *value,
            Self::Eq { value } => (v - *value).abs() < 1e-9,
            Self::Between { min, max } => v >= *min && v <= *max,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdRule {
    pub when: When,
    /// Score adjustment before multiplying by criterion weight.
    pub adjust: f64,
    pub label: String,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    pub id: String,
    pub title: String,
    /// Attribute key on [`crate::product::ProductModel`].
    pub attribute: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub polarity: FactorPolarity,
    /// Ordered rules; first matching `when` applies.
    pub rules: Vec<ThresholdRule>,
    #[serde(default)]
    pub missing_policy: Option<MissingDataPolicy>,
    /// Used when policy is Penalize and value is missing.
    #[serde(default)]
    pub missing_penalty: Option<f64>,
}

fn default_weight() -> f64 {
    1.0
}

/// Cap or floor the final score when a condition on an attribute holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreCap {
    pub attribute: String,
    pub when: When,
    #[serde(default)]
    pub max_score: Option<f64>,
    #[serde(default)]
    pub min_score: Option<f64>,
    pub label: String,
}

/// Optional override: when product matches any category tag substring, use these criteria instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryOverride {
    /// Substrings matched against OFF `categories_tags` (case-insensitive).
    pub category_match: Vec<String>,
    pub criteria: Vec<Criterion>,
}

/// A versioned, declarative scoring profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Monotonic profile version for reproducibility.
    pub version: u32,
    #[serde(default)]
    pub scale: ScoreScale,
    #[serde(default = "default_base")]
    pub base_score: f64,
    #[serde(default)]
    pub missing_data: MissingDataPolicy,
    #[serde(default)]
    pub default_missing_penalty: f64,
    #[serde(default)]
    pub labels: Vec<ScoreLabelBand>,
    pub criteria: Vec<Criterion>,
    #[serde(default)]
    pub caps: Vec<ScoreCap>,
    #[serde(default)]
    pub category_overrides: Vec<CategoryOverride>,
    /// Built-in example profiles are marked read-only templates (can still duplicate).
    #[serde(default)]
    pub builtin: bool,
}

fn default_base() -> f64 {
    50.0
}

impl ScoringProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("profile id is empty".into());
        }
        if self.name.trim().is_empty() {
            return Err("profile name is empty".into());
        }
        if self.version == 0 {
            return Err("profile version must be >= 1".into());
        }
        let (lo, hi) = (self.scale.min(), self.scale.max());
        if !(lo < hi) {
            return Err("scale min must be < max".into());
        }
        if self.base_score < lo || self.base_score > hi {
            return Err(format!("base_score {0} outside scale [{lo}, {hi}]", self.base_score));
        }
        for c in &self.criteria {
            if c.id.trim().is_empty() {
                return Err("criterion id is empty".into());
            }
            if c.attribute.trim().is_empty() {
                return Err(format!("criterion {} has empty attribute", c.id));
            }
            if c.weight.is_nan() || c.weight < 0.0 {
                return Err(format!("criterion {} has invalid weight", c.id));
            }
            if c.rules.is_empty() {
                return Err(format!("criterion {} has no rules", c.id));
            }
        }
        Ok(())
    }

    pub fn label_for_score(&self, score: f64) -> (String, Tone) {
        let mut bands = self.labels.clone();
        bands.sort_by(|a, b| {
            b.min.partial_cmp(&a.min)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for band in &bands {
            if score >= band.min {
                return (band.label.clone(), band.tone);
            }
        }
        (
            format!("{:.0}", score),
            Tone::Neutral,
        )
    }
}
