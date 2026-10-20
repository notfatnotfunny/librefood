//! Scoring profile persistence, builtins, import/export.

use crate::engine::ScoringProfile;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const BUILTINS: &[(&str, &str)] = &[
    (
        "nutrition-quality",
        include_str!("../data/profiles/nutrition-quality.json"),
    ),
    (
        "minimally-processed",
        include_str!("../data/profiles/minimally-processed.json"),
    ),
    (
        "additive-conscious",
        include_str!("../data/profiles/additive-conscious.json"),
    ),
    (
        "environmental-impact",
        include_str!("../data/profiles/environmental-impact.json"),
    ),
    (
        "ethical-consumer",
        include_str!("../data/profiles/ethical-consumer.json"),
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoreMeta {
    active_id: Option<String>,
}

#[derive(Debug)]
pub struct ProfileStore {
    root: PathBuf,
    profiles: Vec<ScoringProfile>,
    active_id: String,
}

impl ProfileStore {
    pub fn open_default() -> Result<Self, String> {
        let root = config_dir()?.join("profiles");
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        Self::open(root)
    }

    pub fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        // Drop legacy blank built-in / leftover blank.json (no longer shipped).
        let blank_path = root.join("blank.json");
        if blank_path.exists() {
            let _ = fs::remove_file(&blank_path);
        }
        let mut profiles = Vec::new();
        for (_, json) in BUILTINS {
            let mut p: ScoringProfile =
                serde_json::from_str(json).map_err(|e| format!("builtin parse: {e}"))?;
            p.builtin = true;
            p.validate()?;
            profiles.push(p);
        }
        // User custom profiles only — never overwrite built-in ids from disk.
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if path.file_name().and_then(|n| n.to_str()) == Some("_meta.json") {
                    continue;
                }
                let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let mut p: ScoringProfile = serde_json::from_str(&text)
                    .map_err(|e| format!("parse {}: {e}", path.display()))?;
                if is_builtin_id(&p.id) || p.id == "blank" {
                    // Ignore attempts to shadow built-ins; remove stale blank.
                    if p.id == "blank" {
                        let _ = fs::remove_file(&path);
                    }
                    continue;
                }
                p.builtin = false;
                p.validate()?;
                if !profiles.iter().any(|x| x.id == p.id) {
                    profiles.push(p);
                }
            }
        }

        let meta_path = root.join("_meta.json");
        let meta: StoreMeta = if meta_path.exists() {
            serde_json::from_str(&fs::read_to_string(&meta_path).map_err(|e| e.to_string())?)
                .unwrap_or_default()
        } else {
            StoreMeta::default()
        };

        let active_id = meta
            .active_id
            .filter(|id| profiles.iter().any(|p| &p.id == id))
            .unwrap_or_else(|| "nutrition-quality".into());

        Ok(Self {
            root,
            profiles,
            active_id,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> &[ScoringProfile] {
        &self.profiles
    }

    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    pub fn active(&self) -> &ScoringProfile {
        self.get(&self.active_id)
            .expect("active profile must exist")
    }

    pub fn get(&self, id: &str) -> Option<&ScoringProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn set_active(&mut self, id: &str) -> Result<(), String> {
        if !self.profiles.iter().any(|p| p.id == id) {
            return Err(format!("unknown profile `{id}`"));
        }
        self.active_id = id.to_string();
        self.persist_meta()
    }

    pub fn save(&mut self, mut profile: ScoringProfile) -> Result<(), String> {
        if is_builtin_id(&profile.id) {
            return Err(
                "cannot overwrite a built-in profile — Duplicate it, then edit the copy".into(),
            );
        }
        profile.builtin = false;
        profile.validate()?;
        let path = self.root.join(format!("{}.json", sanitize_id(&profile.id)));
        let json = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())?;
        if let Some(idx) = self.profiles.iter().position(|p| p.id == profile.id) {
            self.profiles[idx] = profile;
        } else {
            self.profiles.push(profile);
        }
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let Some(p) = self.get(id) else {
            return Err(format!("unknown profile `{id}`"));
        };
        if p.builtin || is_builtin_id(id) {
            return Err("cannot delete built-in profiles".into());
        }
        let path = self.root.join(format!("{}.json", sanitize_id(id)));
        if path.exists() {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        self.profiles.retain(|p| p.id != id);
        if self.active_id == id {
            self.active_id = "nutrition-quality".into();
            self.persist_meta()?;
        }
        Ok(())
    }

    pub fn duplicate(&mut self, id: &str, new_name: &str) -> Result<ScoringProfile, String> {
        let src = self
            .get(id)
            .ok_or_else(|| format!("unknown profile `{id}`"))?
            .clone();
        let new_id = unique_id(&slugify(new_name), &self.profiles);
        let mut copy = src;
        copy.id = new_id.clone();
        copy.name = new_name.to_string();
        copy.version = 1;
        copy.builtin = false;
        copy.description = format!("Duplicated from `{}`. {}", id, copy.description);
        self.save(copy.clone())?;
        Ok(copy)
    }

    /// Create a new blank custom profile (saved under the config profiles dir).
    pub fn create_new(&mut self, name: &str) -> Result<ScoringProfile, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("profile name is empty".into());
        }
        let new_id = unique_id(&slugify(name), &self.profiles);
        let mut profile = blank_profile_template();
        profile.id = new_id;
        profile.name = name.to_string();
        profile.builtin = false;
        profile.version = 1;
        profile.description =
            "Custom profile created in Librefood. Edit weights and rules to suit your goals."
                .into();
        self.save(profile.clone())?;
        self.set_active(&profile.id)?;
        Ok(profile)
    }

    pub fn import_json(&mut self, text: &str) -> Result<ScoringProfile, String> {
        let mut p: ScoringProfile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        p.builtin = false;
        if self.profiles.iter().any(|x| x.id == p.id) {
            p.id = unique_id(&p.id, &self.profiles);
        }
        p.validate()?;
        self.save(p.clone())?;
        Ok(p)
    }

    pub fn export_json(&self, id: &str) -> Result<String, String> {
        let p = self
            .get(id)
            .ok_or_else(|| format!("unknown profile `{id}`"))?;
        serde_json::to_string_pretty(p).map_err(|e| e.to_string())
    }

    fn persist_meta(&self) -> Result<(), String> {
        let meta = StoreMeta {
            active_id: Some(self.active_id.clone()),
        };
        let path = self.root.join("_meta.json");
        fs::write(
            path,
            serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

fn config_dir() -> Result<PathBuf, String> {
    // Prefer XDG via HOME when glib display isn't up (tests).
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("librefood"));
    }
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home).join(".config/librefood"))
}

fn is_builtin_id(id: &str) -> bool {
    BUILTINS.iter().any(|(bid, _)| *bid == id)
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn slugify(name: &str) -> String {
    let s: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "profile".into()
    } else {
        s
    }
}

fn unique_id(base: &str, existing: &[ScoringProfile]) -> String {
    let base = sanitize_id(base);
    if !existing.iter().any(|p| p.id == base) {
        return base;
    }
    for i in 2..1000 {
        let candidate = format!("{base}-{i}");
        if !existing.iter().any(|p| p.id == candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", chrono_like_suffix())
}

fn chrono_like_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn blank_profile_template() -> ScoringProfile {
    use crate::engine::{
        Criterion, FactorPolarity, MissingDataPolicy, ScoreLabelBand, ScoreScale, ThresholdRule,
        Tone, When,
    };
    use crate::product::attrs;
    ScoringProfile {
        id: "new".into(),
        name: "New profile".into(),
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
        criteria: vec![Criterion {
            id: "sugars".into(),
            title: "Sugars".into(),
            attribute: attrs::SUGARS_100G.into(),
            weight: 1.0,
            polarity: FactorPolarity::Negative,
            rules: vec![
                ThresholdRule {
                    when: When::Lte { value: 5.0 },
                    adjust: 5.0,
                    label: "Low sugar".into(),
                    tone: Tone::Good,
                },
                ThresholdRule {
                    when: When::Gt { value: 5.0 },
                    adjust: -5.0,
                    label: "Higher sugar".into(),
                    tone: Tone::Mediocre,
                },
            ],
            missing_policy: None,
            missing_penalty: None,
        }],
        caps: vec![],
        category_overrides: vec![],
        builtin: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_builtins_and_switch() {
        let dir = std::env::temp_dir().join(format!(
            "librefood-profile-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut store = ProfileStore::open(dir.clone()).unwrap();
        assert_eq!(store.list().len(), 5);
        assert!(store.list().iter().any(|p| p.id == "ethical-consumer"));
        assert!(!store.list().iter().any(|p| p.id == "blank"));
        store.set_active("additive-conscious").unwrap();
        assert_eq!(store.active_id(), "additive-conscious");
        let dup = store.duplicate("nutrition-quality", "My Custom").unwrap();
        assert!(!dup.builtin);
        assert_eq!(dup.name, "My Custom");
        assert!(store
            .save({
                let mut p = store.get("nutrition-quality").unwrap().clone();
                p.name = "hacked".into();
                p
            })
            .is_err());
        assert!(store.delete("nutrition-quality").is_err());
        let exported = store.export_json(&dup.id).unwrap();
        assert!(exported.contains("My Custom"));
        let imported = store
            .import_json(&exported.replace(&dup.id, "imported-x"))
            .unwrap();
        assert_eq!(imported.id, "imported-x");

        let created = store.create_new("Kitchen Rules").unwrap();
        assert!(!created.builtin);
        assert_eq!(created.name, "Kitchen Rules");
        assert_eq!(store.active_id(), created.id);
        assert!(dir.join(format!("{}.json", created.id)).exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
