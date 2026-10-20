# Librefood scoring engine

Librefood separates **Open Food Facts source data** from **derived scores**. Scores come only from a declarative **scoring profile** (JSON). Profiles are not executable code — there is no `eval`, scripting, or unsafe dynamic execution.

## Formula and execution order

1. Start at `profile.base_score`.
2. If any `category_overrides[].category_match` substring matches the product’s OFF `categories_tags`, use that override’s `criteria`; otherwise use `profile.criteria`.
3. For each criterion **in order**:
   - Read the named attribute from the normalized product model.
   - If missing: apply the criterion’s `missing_policy`, or else `profile.missing_data`
     (`ignore` / `penalize` / `unknown` / `incomplete`).
   - If present: take the **first** `rules[]` entry whose `when` matches; add  
     `rule.adjust * criterion.weight` to the running score.
4. Apply `caps[]` in order (optional `max_score` / `min_score` when the cap’s attribute condition matches).
5. Clamp to `scale` (`numeric` min/max).
6. Round **half-up** to an integer for display; pick the label band with the highest `min ≤ score`.

Identical product data + identical profile version ⇒ identical score (deterministic).

## Profile schema (summary)

See example files under `data/profiles/`:

| File | Intent |
|------|--------|
| `nutrition-quality.json` | General nutrition signals |
| `minimally-processed.json` | NOVA + additives |
| `additive-conscious.json` | Additive-first |
| `environmental-impact.json` | Eco-Score / organic / NOVA |

Fields include weighted `criteria`, threshold `rules`, `caps`, `category_overrides`, `missing_data`, and `labels` for named categories on a custom numeric scale.

## Persistence

- Built-ins are embedded at compile time.
- User copies / edits live in `~/.config/librefood/profiles/` (or `$XDG_CONFIG_HOME/librefood/profiles/`).
- Active profile id is stored in `_meta.json` in that directory.
- Import / export is plain JSON (Manage profiles dialog, or copy files).

## UI workflow

1. Look up a barcode (or search by name, or scan).
2. Choose a profile in the dropdown — the same product is rescored immediately.
3. **Compare** shows scores for every profile on the current product.
4. **Profiles** page (bottom nav, or Search → Profiles) → list / set active / duplicate / edit customs / import / export; built-ins are read-only (Duplicate to customize). Expand **How scoring works** for engine notes. Search keeps a compact profile dropdown for scoring only.
5. Calculation steps list each criterion’s id/attribute, raw value, rule adjust, weight, source, and Δ contribution.

Custom scores are **not** official Nutri-Score grades and are **not** medical advice. OFF Nutri-Score letters, when present, are shown as source data only.
