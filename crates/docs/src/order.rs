use super::*;

/// The node-count threshold at/above which the README full lineage is split by
/// layer instead of drawn as one diagram. Chosen so the common small/medium
/// project (the 93-node fixture, typical jaffle-shop-scale demos) still gets a
/// single whole-graph picture, while genuinely large projects degrade to
/// per-layer diagrams rather than an unreadable — and renderer-straining —
/// hairball. A fixed constant, so a given project always renders identically
/// (determinism); raise it if your renderer comfortably handles bigger graphs.
pub(super) const FULL_GRAPH_NODE_LIMIT: usize = 150;

/// Resource types in their README display order. Anything not listed is grouped
/// under "other" by [`readme_order`] (sorted by uid), so an unexpected kind is
/// never dropped.
pub(super) const RESOURCE_ORDER: &[&str] = &["model", "snapshot", "seed", "source", "exposure"];

/// All nodes in a deterministic logical order for the README table:
/// - models first, in dbt layer order ([`layer_rank`]) then by name/unique_id —
///   mirroring the left pane's grouping;
/// - then the remaining resource types in [`RESOURCE_ORDER`], each by
///   name/unique_id;
/// - then any unrecognized kind, by unique_id.
pub(super) fn readme_order(dag: &Dag) -> Vec<&NodeInfo> {
    let mut nodes: Vec<&NodeInfo> = dag.nodes().values().collect();
    nodes.sort_by(|a, b| {
        resource_rank(&a.resource_type)
            .cmp(&resource_rank(&b.resource_type))
            .then_with(|| model_layer_rank(a).cmp(&model_layer_rank(b)))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.unique_id.cmp(&b.unique_id))
    });
    nodes
}

/// Display rank of a resource type for the README ordering (its index in
/// [`RESOURCE_ORDER`], unknown kinds last).
pub(super) fn resource_rank(resource_type: &str) -> usize {
    RESOURCE_ORDER
        .iter()
        .position(|k| *k == resource_type)
        .unwrap_or(RESOURCE_ORDER.len())
}

/// The layer rank for a model (so models sort by dbt layer order before name);
/// non-models all share rank 0, leaving their name/unique_id tie-break to order
/// them — the resource-type rank already separates the groups.
pub(super) fn model_layer_rank(node: &NodeInfo) -> usize {
    if node.resource_type != "model" {
        return 0;
    }
    let layer = first_dir(node).unwrap_or("");
    layer_rank(layer)
}

/// The direct (1-hop) neighbour uids of `uid`, on the `upstream` side (parents)
/// when `upstream` is true, else the downstream side (children). Read off a
/// depth-1 directional [`Dag::subgraph_view`] — the public surface for 1-hop
/// adjacency — and returned sorted (the subgraph nodes are already sorted by
/// uid, and the node itself is filtered out).
pub(super) fn direct_neighbours(dag: &Dag, uid: &str, upstream: bool) -> Vec<String> {
    let sg = dag.subgraph_view(uid, upstream, !upstream, Some(1));
    sg.nodes
        .into_iter()
        .map(|n| n.unique_id)
        .filter(|id| id != uid)
        .collect()
}

/// Format an optional value as inline code, or the literal `none` when absent.
pub(super) fn opt_code(value: Option<&str>) -> String {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => format!("`{v}`"),
        None => "none".to_string(),
    }
}

/// Escape text for a single Markdown table cell: a `|` would start a new column
/// and a newline would break the row, so both are neutralized. The text is kept
/// otherwise verbatim (names/descriptions may carry `"`/`(`/`)` — those are
/// harmless inside a cell).
pub(super) fn md_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}
