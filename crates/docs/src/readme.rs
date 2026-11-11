use super::*;

/// The index `README.md`: project heading + provenance, a resource-type count
/// breakdown, a full linked node table (in a deterministic logical order), and a
/// whole-project Mermaid diagram.
pub(super) fn readme_markdown(
    dag: &Dag,
    project_name: &str,
    source_label: &str,
    files: &BTreeMap<String, String>,
    scope: PackageScope,
    lint: Option<&[Finding]>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {project_name}\n\n"));
    s.push_str(&format!("Generated from: `{source_label}`\n\n"));

    // Summary: total + per-resource-type breakdown. Kinds in RESOURCE_ORDER
    // come first in their fixed display order, then every OTHER kind present
    // in the graph (semantic_model/metric/saved_query, or a type this version
    // does not classify) sorted by name — the breakdown enumerates ALL types
    // in the manifest, so the counts always sum to `total nodes`. The scope
    // bullet leads: it declares which graph the numbers describe — under
    // `--include-packages` the node table + lineage cover every installed
    // package while the coverage/orphan stats below still count the root
    // project only, and the bullet says exactly that.
    s.push_str("## Summary\n\n");
    match scope {
        PackageScope::All => {
            // Count the installed-package nodes kept as context — the owning
            // package resolved ONCE for the sweep (`is_root_project`
            // re-resolves per call).
            let primary = dag.primary_package();
            let pkgs = dag
                .nodes()
                .values()
                .filter(|n| primary.as_deref().is_some_and(|p| !in_package(n, p)))
                .count();
            if pkgs == 0 {
                // `--include-packages` on a package-free manifest: the bare
                // label reads cleaner than "(0 installed-package nodes)".
                s.push_str(&format!("- scope: {}\n", scope.describe()));
            } else {
                s.push_str(&format!(
                    "- scope: all packages ({pkgs} installed-package nodes; metrics count the root project only)\n"
                ));
            }
        }
        PackageScope::Root => s.push_str(&format!("- scope: {}\n", scope.describe())),
    }
    s.push_str(&format!("- total nodes: {}\n", dag.len()));
    for kind in RESOURCE_ORDER {
        let n = dag.count_by_resource_type(kind);
        if n > 0 {
            s.push_str(&format!("- {kind}: {n}\n"));
        }
    }
    let mut unlisted: BTreeMap<&str, usize> = BTreeMap::new();
    for node in dag.nodes().values() {
        if !RESOURCE_ORDER.contains(&node.resource_type.as_str()) {
            *unlisted.entry(node.resource_type.as_str()).or_default() += 1;
        }
    }
    for (kind, n) in unlisted {
        s.push_str(&format!("- {kind}: {n}\n"));
    }

    // Test-coverage + orphan summary, computed from the SAME predicates the TUI
    // uses (`coverage_gap` / the `coverage_summary` base / `direct_down == 0`)
    // so the numbers here match the `t` lens, the `cov` status segment, and the
    // stats dashboard exactly — no second implementation to drift.
    push_stats_summary(&mut s, dag, files);

    // The lint summary — the shared `run_rules` findings `dbtl check --lint`
    // gates on (rendered only when lint ran).
    push_lint_summary(&mut s, dag, files, lint);

    // Node table in logical order (see `readme_order`), each linked to its page.
    s.push_str("\n## Nodes\n\n");
    s.push_str("| Name | Type | Materialization | Tests | Doc |\n");
    s.push_str("| --- | --- | --- | --- | --- |\n");
    for node in readme_order(dag) {
        let uid = node.unique_id.as_str();
        let link = files
            .get(uid)
            .map(|rel| format!("[{}]({})", md_cell(&node.name), rel))
            .unwrap_or_else(|| md_cell(&node.name));
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            md_cell(&node.name),
            md_cell(&node.resource_type),
            md_cell(node.materialized.as_deref().unwrap_or("-")),
            node.test_count,
            link,
        ));
    }

    // Whole-project Mermaid: every node + every edge, through the same shared
    // core (so ids/labels match the per-node diagrams). Built as a synthetic
    // full subgraph with no marked selection. Links resolve FROM the README (at
    // the repo root) so the href is the page's full relative path (`models/…`).
    s.push_str("\n## Lineage\n\n");
    push_readme_lineage(&mut s, dag, files);

    s
}

/// Append the README's whole-project lineage. Below [`FULL_GRAPH_NODE_LIMIT`]
/// nodes it is one diagram of the entire graph; at or above it the single
/// diagram would be an unreadable hairball (and slow / liable to be rejected by
/// Mermaid renderers), so the graph is split into one diagram per logical layer
/// ([`LAYER_ORDER`], unknown layers and non-models grouped last) — each diagram
/// holding only that group's nodes and the edges WITHIN it.
///
/// The threshold is a fixed constant (documented on [`FULL_GRAPH_NODE_LIMIT`]),
/// so the same project always renders the same way: determinism is preserved
/// either branch. Cross-layer edges are necessarily omitted from the split view
/// (they would reintroduce the hairball); a note says so and points readers at
/// the per-node pages for the full picture.
pub(super) fn push_readme_lineage(s: &mut String, dag: &Dag, files: &BTreeMap<String, String>) {
    let href = |rel: &str| rel.to_string();
    if dag.is_empty() {
        s.push_str("No nodes.\n");
        return;
    }
    if dag.len() < FULL_GRAPH_NODE_LIMIT {
        let sg = dag.full_subgraph();
        push_linked_diagram(s, dag, &sg, files, href);
        return;
    }
    // Large graph: split by layer group. The order is the model layer order
    // first, then the remaining resource-type groups, mirroring `readme_order`.
    s.push_str(&format!(
        "This project has {} nodes (>= {} threshold), so the full graph is \
         split by layer below — one diagram per group, intra-group edges only. \
         For cross-layer lineage of a specific node, open its page (each carries \
         its own neighbourhood diagram).\n",
        dag.len(),
        FULL_GRAPH_NODE_LIMIT
    ));
    for group in layer_groups(dag) {
        let member_uids: HashSet<&str> = group.uids.iter().map(String::as_str).collect();
        let nodes: Vec<NodeInfo> = group
            .uids
            .iter()
            .filter_map(|u| dag.get(u).cloned())
            .collect();
        let mut edges: Vec<Edge> = dag
            .edges()
            .into_iter()
            .filter(|(p, c)| member_uids.contains(p.as_str()) && member_uids.contains(c.as_str()))
            .map(|(parent, child)| Edge { parent, child })
            .collect();
        edges.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.child.cmp(&b.child)));
        let sg = Subgraph {
            selected: String::new(),
            nodes,
            edges,
        };
        s.push_str(&format!("\n### {} ({})\n\n", group.label, group.uids.len()));
        if sg.nodes.is_empty() {
            s.push_str("No nodes.\n");
        } else {
            push_linked_diagram(s, dag, &sg, files, href);
        }
    }
}

/// One layer/resource group for the split README lineage: a display `label` and
/// the member `unique_id`s (sorted). See [`layer_groups`].
pub(super) struct LayerGroup {
    pub(super) label: String,
    pub(super) uids: Vec<String>,
}

/// Partition the dag's nodes into ordered groups for the split lineage:
/// model layers in [`LAYER_ORDER`] first (each with its members), then one
/// group per remaining resource type in [`RESOURCE_ORDER`], then a catch-all
/// `other` group — exactly the buckets [`readme_order`] would lay out, so the
/// split diagrams appear in the same order as the node table. Every bucket's
/// uids are sorted, and empty buckets are dropped, so the result is
/// deterministic and gap-free (every node lands in exactly one group).
pub(super) fn layer_groups(dag: &Dag) -> Vec<LayerGroup> {
    let mut model_layers: BTreeMap<usize, (String, Vec<String>)> = BTreeMap::new();
    let mut by_rt: BTreeMap<usize, (String, Vec<String>)> = BTreeMap::new();
    let mut other: Vec<String> = Vec::new();
    for node in dag.nodes().values() {
        let uid = node.unique_id.clone();
        if node.resource_type == "model" {
            let layer = first_dir(node).unwrap_or("");
            let rank = layer_rank(layer);
            let label = LAYER_ORDER.get(rank).copied().unwrap_or("other models");
            model_layers
                .entry(rank)
                .or_insert_with(|| (label.to_string(), Vec::new()))
                .1
                .push(uid);
        } else if let Some(rank) = RESOURCE_ORDER.iter().position(|k| *k == node.resource_type) {
            by_rt
                .entry(rank)
                .or_insert_with(|| (format!("{}s", node.resource_type), Vec::new()))
                .1
                .push(uid);
        } else {
            other.push(uid);
        }
    }
    let mut groups = Vec::new();
    for (_, (label, mut uids)) in model_layers {
        uids.sort();
        groups.push(LayerGroup { label, uids });
    }
    for (_, (label, mut uids)) in by_rt {
        uids.sort();
        groups.push(LayerGroup { label, uids });
    }
    if !other.is_empty() {
        other.sort();
        groups.push(LayerGroup {
            label: "other".to_string(),
            uids: other,
        });
    }
    groups
}
