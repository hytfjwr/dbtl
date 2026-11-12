use super::*;

/// A resolved directory-hierarchical config from a `dbt_project.yml` resource
/// tree (`models:` / `seeds:` / `snapshots:` / `sources:`).
#[derive(Debug, Clone, Default)]
pub(super) struct TreeConfig {
    pub(super) materialized: Option<String>,
    pub(super) enabled: Option<bool>,
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
/// so the warning says which level misdeclared it.
pub(super) fn resolve_tree_config(
    tree: &serde_json::Value,
    segments: &[String],
    tree_path: String,
    warnings: &mut Vec<String>,
) -> TreeConfig {
    let mut out = TreeConfig::default();
    if !tree.is_object() {
        return out; // missing level (indexing Null yields Null) — nothing deeper
    }
    if let Some(m) = tree["+materialized"].as_str() {
        out.materialized = Some(m.to_string());
    }
    if let Some(e) = tree["+enabled"].as_bool() {
        out.enabled = Some(e);
    }
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
    if let Some((seg, rest)) = segments.split_first() {
        let deeper = resolve_tree_config(
            &tree[seg.as_str()],
            rest,
            format!("{tree_path}.{seg}"),
            warnings,
        );
        out.materialized = deeper.materialized.or(out.materialized);
        out.enabled = deeper.enabled.or(out.enabled);
        out.meta.extend(deeper.meta); // deeper keys replace same-named ones
        out.unique_key = deeper.unique_key.or(out.unique_key);
        out.freshness = deeper.freshness.or(out.freshness);
        out.access = deeper.access.or(out.access);
        out.group = deeper.group.or(out.group);
        out.contract = deeper.contract.or(out.contract);
        out.incremental_strategy = deeper.incremental_strategy.or(out.incremental_strategy);
    }
    out
}
