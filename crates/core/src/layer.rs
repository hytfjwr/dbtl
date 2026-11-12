//! The logical-layer taxonomy shared by the model list, the lineage layer
//! lenses, and the static docs generator.
//!
//! A model's layer is the first directory of its `path` (`staging/x.sql` →
//! `staging`); ordering is the fixed [`LAYER_ORDER`], never alphabetical. The
//! rule lives in `dbtl-core` (not the TUI's `model_list`) so every consumer —
//! list grouping, `LayerViolation`, layered Mermaid docs — classifies a node by
//! the SAME taxonomy with no duplicated logic.

use crate::NodeInfo;

/// dbt layer order. Load-bearing and fixed: this is the logical layer order,
/// NOT alphabetical (alphabetical would be intermediate/marts/staging/utilities,
/// which is wrong). Groups are emitted in exactly this order; any unexpected
/// fifth top-level directory is appended after these, so we never panic.
pub const LAYER_ORDER: [&str; 4] = ["staging", "intermediate", "marts", "utilities"];

/// The layers a layer-VIOLATION judges — the data-flow pipeline. `utilities`
/// ranks inside [`LAYER_ORDER`] but carries no data-flow direction (a
/// `utils/` helper read by staging is normal, not a backward jump), and a
/// dir naming none of these three has no direction at all — edges touching
/// either never count as violations.
pub const VIOLATION_LAYERS: [&str; 3] = ["staging", "intermediate", "marts"];

/// First directory of a `path` (the part before the first `/`).
///
/// Returns `None` when the node has no `path` (sources/seeds/snapshots), so
/// only models — which always carry a `path` — contribute a layer. `pub` so the
/// lineage layer + layer-violation lenses classify a node by the SAME rule the
/// list grouping uses (no duplicated taxonomy).
pub fn first_dir(node: &NodeInfo) -> Option<&str> {
    node.path
        .as_deref()
        .map(|p| p.split('/').next().unwrap_or(p))
}

/// Rank a layer for ordering: known layers get their fixed index ([`LAYER_ORDER`]
/// position); any unknown layer is sorted after all known ones (and never panics).
/// `pub` so the layer-violation lens can compare two models' ranks (a backward
/// edge is `rank(parent) > rank(child)`) using the SAME order the list groups by.
pub fn layer_rank(layer: &str) -> usize {
    LAYER_ORDER
        .iter()
        .position(|&l| l == layer)
        .unwrap_or(LAYER_ORDER.len())
}
