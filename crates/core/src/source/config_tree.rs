use super::*;

/// A resolved directory-hierarchical config from a `dbt_project.yml` resource
/// tree (`models:` / `seeds:` / `snapshots:` / `sources:`).
#[derive(Debug, Clone, Default)]
pub(super) struct TreeConfig {
    pub(super) materialized: Option<String>,
    /// The deepest `+enabled` declaration's proven value.
    pub(super) enabled: Option<bool>,
    /// The deepest `+enabled` declaration couldn't be evaluated (dbt
    /// would hard-error; the resource stays enabled and the node is
    /// marked `enabled`-unverifiable by the caller).
    pub(super) enabled_unproven: bool,
    /// The `+meta` dict merged down the tree: deeper levels override per
    /// TOP-LEVEL key (a nested dict is replaced wholesale, not deep-merged —
    /// verified against real dbt).
    pub(super) meta: serde_json::Map<String, serde_json::Value>,
    /// `+unique_key` — the raw declared value (string or string list; deeper
    /// wins wholesale, like every non-`meta` config key).
    pub(super) unique_key: Option<serde_json::Value>,
    /// `+freshness` on the `sources:` tree — the lowest-precedence freshness
    /// (schema.yml source- and table-level values sit above it; `+loader`
    /// and `+loaded_at_field` are deliberately NOT read — real dbt ignores
    /// them in dbt_project.yml, verified on 1.12.6).
    pub(super) freshness: Option<RawFreshness>,
    /// `+access` / `+group` — the project-tree spellings of the model
    /// governance configs (dbt records `access` on models only; `group`
    /// applies to any resource the tree covers).
    pub(super) access: Option<String>,
    pub(super) group: Option<String>,
    /// `+contract` — the tree's contract declaration resolved to the wire
    /// type (`{enforced: bool}` object, or a bare bool tolerated).
    pub(super) contract: Option<RawContract>,
    /// `+incremental_strategy` — `append` / `merge` / `delete+insert` /
    /// `insert_overwrite` / `microbatch`.
    pub(super) incremental_strategy: Option<String>,
    /// `+tags` — ACCUMULATES down the tree rather than overriding (a
    /// nested `+tags` extends the outer list — verified on 1.12.6), so
    /// the list is ordered shallower-first.
    pub(super) tags: Vec<String>,
}

impl TreeConfig {
    /// The `+enabled` verdict as an [`Enabled`] — [`Enabled::Unproven`]
    /// when the deepest declaration couldn't be evaluated, `None` when
    /// the tree declares no `+enabled` at all.
    pub(super) fn enabled_verdict(&self) -> Option<Enabled> {
        if self.enabled_unproven {
            Some(Enabled::Unproven)
        } else {
            self.enabled
                .map(|b| if b { Enabled::On } else { Enabled::Off })
        }
    }
}

/// The config-tree lookup path for a node: the project name, then each
/// directory of its resource-root-relative `path`, then the file stem (so a
/// file-level `models: p: marts: my_model: +materialized:` resolves too).
pub(super) fn tree_segments(proj: &str, rel: &str) -> Vec<String> {
    let mut segs = vec![proj.to_string()];
    let mut parts: Vec<&str> = rel.split('/').collect();
    let file = parts.pop().unwrap_or("");
    segs.extend(parts.iter().map(|s| s.to_string()));
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    if !stem.is_empty() {
        segs.push(stem.to_string());
    }
    segs
}

/// Walk a resource tree along `segments`, collecting `+materialized` /
/// `+enabled` / `+meta` at every level — a deeper (more specific) value
/// overrides a shallower one, dbt's most-specific-path rule. Descent stops at
/// the first missing segment (configs collected above it still apply). Only
/// `+`-prefixed keys are configs; bare keys are path segments. `+meta` maps
/// merge by top-level key (a deeper key replaces the whole value) through the
/// same [`meta_map`] conversion schema.yml uses — non-string keys stringify
/// the same way and a non-mapping `+meta` warns once per tree level.
/// `tree_path` names the walked path (`models`, `models.sample.staging`, …)
/// so the warning says which level misdeclared it. `vars` resolves
/// `+enabled` through [`Vars::tree_enabled`]: a native bool or a
/// `{{ var(...) }}` the project-file render scope sees — anything dbt
/// would reject (a plain `'false'` string, an unresolvable var) warns
/// once per level and keeps the resource enabled. `None` = the tree
/// doesn't apply `+enabled` at all (the `sources:` walk — the key is
/// not implemented there), so it stays a silent bool-only read rather
/// than warning on an ignored key.
pub(super) fn resolve_tree_config(
    tree: &serde_json::Value,
    segments: &[String],
    tree_path: String,
    warnings: &mut Vec<String>,
    vars: Option<&Vars>,
) -> TreeConfig {
    resolve_tree_config_read(tree, segments, tree_path, warnings, vars, true)
}

/// The `sources:` variant of [`resolve_tree_config`] — dbt ignores a
/// `+tags` rung on the `sources:` tree entirely, so neither its value
/// nor its non-scalar warning is read there.
pub(super) fn resolve_source_tree_config(
    tree: &serde_json::Value,
    segments: &[String],
    tree_path: String,
    warnings: &mut Vec<String>,
    vars: Option<&Vars>,
) -> TreeConfig {
    resolve_tree_config_read(tree, segments, tree_path, warnings, vars, false)
}

fn resolve_tree_config_read(
    tree: &serde_json::Value,
    segments: &[String],
    tree_path: String,
    warnings: &mut Vec<String>,
    vars: Option<&Vars>,
    read_tags: bool,
) -> TreeConfig {
    let mut out = TreeConfig::default();
    if !tree.is_object() {
        return out; // missing level (indexing Null yields Null) — nothing deeper
    }
    if let Some(m) = tree["+materialized"].as_str() {
        out.materialized = Some(m.to_string());
    }
    // This level's `+enabled` verdict — `Some(Some(b))` a proven bool,
    // `Some(None)` declared-but-unprovable (warned once per level), `None`
    // not declared. The DEEPEST declaration wins, so `mine` only counts
    // when no deeper level declared (merged below).
    let declared = !tree["+enabled"].is_null();
    let mine: Option<Option<bool>> = match vars.map(|v| v.tree_enabled(&tree["+enabled"])) {
        Some(Some(e)) => Some(Some(e)),
        Some(None) if declared => {
            warnings.push(format!(
                "dbt_project.yml '+enabled' at '{tree_path}' could not be evaluated as a boolean ({}); the resource stays enabled",
                serde_json::to_string(&tree["+enabled"]).unwrap_or_else(|_| "?".to_string())
            ));
            Some(None)
        }
        Some(None) => None,
        None => tree["+enabled"].as_bool().map(Some),
    };
    out.meta.extend(meta_map(
        &tree["+meta"],
        warnings,
        &format!("dbt_project.yml '+meta' at '{tree_path}'"),
    ));
    // `+unique_key` keeps its declared shape (string or list) — the same
    // normalization point `Dag::build` uses for every `unique_key` source.
    if !tree["+unique_key"].is_null() {
        out.unique_key = Some(tree["+unique_key"].clone());
    }
    // `+freshness` shares schema.yml's shape ({warn_after:…, error_after:…})
    // — parse it into the wire type so the merge is a plain precedence
    // chain. A malformed block warns like a malformed `+meta` instead of
    // silently becoming "no freshness".
    if !tree["+freshness"].is_null() {
        match serde_json::from_value::<RawFreshness>(tree["+freshness"].clone()) {
            Ok(f) => out.freshness = Some(f),
            Err(e) => warnings.push(format!(
                "dbt_project.yml '+freshness' at '{tree_path}' is not a freshness block ({e}); ignoring"
            )),
        }
    }
    if let Some(s) = tree["+access"].as_str() {
        out.access = Some(s.to_string());
    }
    if let Some(s) = tree["+group"].as_str() {
        out.group = Some(s.to_string());
    }
    if let Some(c) = contract_value(&tree["+contract"]) {
        out.contract = Some(c);
    }
    if let Some(s) = tree["+incremental_strategy"].as_str() {
        out.incremental_strategy = Some(s.to_string());
    }
    // `+tags` — a bare scalar wraps to a one-element list, elements
    // stringify (`7` → `"7"`, verified on 1.12.6); a non-scalar member
    // (a dict, a nested list) warns once per level like `+meta`. The
    // `sources:` walk skips the key wholesale — dbt ignores it there.
    let tags_v = &tree["+tags"];
    if read_tags && !tags_v.is_null() {
        let bad = match tags_v {
            serde_json::Value::Array(items) => items.iter().any(|i| scalar_to_string(i).is_none()),
            v => scalar_to_string(v).is_none(),
        };
        if bad {
            warnings.push(format!(
                "dbt_project.yml '+tags' at '{tree_path}' has a non-string member; ignoring it"
            ));
        }
        out.tags = tag_values_of(tags_v);
    }
    let deeper = segments.split_first().map(|(seg, rest)| {
        resolve_tree_config_read(
            &tree[seg.as_str()],
            rest,
            format!("{tree_path}.{seg}"),
            warnings,
            vars,
            read_tags,
        )
    });
    // The deepest DECLARED `+enabled` wins — proven or unproven. An
    // unproven deeper value beats a proven shallower one (dbt would
    // hard-error on the deeper level, never reach the shallower's
    // verdict); only when the deeper walk declared nothing does `mine`
    // apply.
    let deeper_verdict = deeper.as_ref().and_then(|d| {
        if d.enabled_unproven {
            Some(None)
        } else {
            d.enabled.map(Some)
        }
    });
    match deeper_verdict.or(mine) {
        Some(Some(b)) => out.enabled = Some(b),
        Some(None) => out.enabled_unproven = true,
        None => {}
    }
    if let Some(deeper) = deeper {
        out.materialized = deeper.materialized.or(out.materialized);
        out.meta.extend(deeper.meta); // deeper keys replace same-named ones
        out.unique_key = deeper.unique_key.or(out.unique_key);
        out.freshness = deeper.freshness.or(out.freshness);
        out.access = deeper.access.or(out.access);
        out.group = deeper.group.or(out.group);
        out.contract = deeper.contract.or(out.contract);
        out.incremental_strategy = deeper.incremental_strategy.or(out.incremental_strategy);
        out.tags.extend(deeper.tags); // shallower-first accumulation
    }
    out
}
