use super::*;

/// A resolved directory-hierarchical config from a `dbt_project.yml` resource
/// tree (`models:` / `seeds:` / `snapshots:`).
#[derive(Debug, Clone, Default)]
pub(super) struct TreeConfig {
    pub(super) materialized: Option<String>,
    pub(super) enabled: Option<bool>,
    /// The `+meta` dict merged down the tree: deeper levels override per
    /// TOP-LEVEL key (a nested dict is replaced wholesale, not deep-merged —
    /// verified against real dbt).
    pub(super) meta: serde_json::Map<String, serde_json::Value>,
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
    }
    out
}
