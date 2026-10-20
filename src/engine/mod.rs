//! Configurable, deterministic food scoring engine (UI- and OFF-independent).

pub mod eval;
pub mod schema;

pub use eval::{evaluate, ScoreResult};
pub use schema::{
    Criterion, FactorPolarity, MissingDataPolicy, ScoreLabelBand, ScoreScale, ScoringProfile,
    ThresholdRule, Tone, When,
};

/// Documentation of formula / execution order (also in docs/scoring-engine.md).
/// Shown in the Manage profiles dialog.
pub const ENGINE_NOTES: &str = r#"
Librefood scoring engine
------------------------
1. Start at profile.base_score.
2. If a category_override matches product categories_tags, use its criteria; else profile.criteria.
3. For each criterion (order preserved): resolve attribute; apply missing_data policy or first matching rule;
   add (rule.adjust * criterion.weight) to the running score.
4. Apply caps in order (max_score / min_score when attribute condition matches).
5. Clamp to scale; round half-up to integer for display; pick label from bands (highest min ≤ score).
"#;
