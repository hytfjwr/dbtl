//! The static Markdown documentation generator behind `dbtl docs --out <DIR>`.
//!
//! A pure transform: [`generate_docs`] turns a loaded [`Dag`] into a list of
//! `(relative path, Markdown contents)` pairs — one node page per node plus an
//! index `README.md`. The CLI layer ([`crate::main`]) does the IO (create the
//! dir, write each file); this module never touches the filesystem or the
//! terminal, so it is unit-testable headlessly and reproducible byte-for-byte.
//!
//! Determinism is a hard requirement (the docs are meant to be committed and
//! `git diff`-checked in CI): every `HashSet`-sourced closure
//! ([`Dag::upstream`] / [`Dag::downstream`]) is sorted before use, node
//! iteration is over a sorted `unique_id` order, and per-node filenames are
//! assigned from a shared collision-avoidance map (the same approach as the
//! Mermaid node-id map) so two runs over the same `Dag` produce identical bytes.

use std::collections::{BTreeMap, HashSet};

use crate::app::subgraph_mermaid_linked;
use crate::model_list::{first_dir, layer_rank};
use crate::{coverage_gap, Dag, NodeInfo, LAYER_ORDER};

/// The subdirectory under `<out>` that holds every node page. The index
/// `README.md` sits at the root and links into here.
const NODES_DIR: &str = "models";

/// Generate the full documentation set for `dag` as `(relative path, contents)`
/// pairs: one `models/<sanitized-uid>.md` per node and a single `README.md`
/// index. `project_name` titles the index; `source_label` records where the
/// data came from (a manifest path or a project dir) so a reader knows the
/// provenance. Pure and deterministic — see the module docs.
pub fn generate_docs(dag: &Dag, project_name: &str, source_label: &str) -> Vec<(String, String)> {
    // Assign every node a stable relative file path up front, so both the node
    // pages AND the README links resolve through the SAME map — a link can
    // never point at a file that was named differently (no broken links).
    let files = node_files(dag);

    let mut out: Vec<(String, String)> = Vec::with_capacity(files.len() + 1);
    // Node pages in sorted unique_id order (deterministic write order).
    for (uid, rel_path) in &files {
        out.push((rel_path.clone(), node_markdown(dag, uid, &files)));
    }
    out.push((
        "README.md".to_string(),
        readme_markdown(dag, project_name, source_label, &files),
    ));
    out
}

/// Assign each node a relative file path (`models/<sanitized-uid>.md`),
/// resolving collisions deterministically.
///
/// Sanitizing `unique_id` is lossy (`model.p.x_y` and `model.p_x.y` both
/// flatten to `model_p_x_y`), so two distinct uids could claim the same file
/// and silently overwrite each other. Mirroring [`crate::app::mermaid_ids`],
/// uids are visited in sorted order (determinism) and a clashing stem gets the
/// first free `_2`, `_3`, … numeric suffix. The returned map is a `BTreeMap`,
/// so iterating it is itself in sorted-uid order.
fn node_files(dag: &Dag) -> BTreeMap<String, String> {
    let mut uids: Vec<&str> = dag.nodes().keys().map(String::as_str).collect();
    uids.sort_unstable();
    let mut used: HashSet<String> = HashSet::new();
    let mut files = BTreeMap::new();
    for uid in uids {
        let base = sanitize_stem(uid);
        let mut stem = base.clone();
        let mut n = 1usize;
        while !used.insert(stem.clone()) {
            n += 1;
            stem = format!("{base}_{n}");
        }
        files.insert(uid.to_string(), format!("{NODES_DIR}/{stem}.md"));
    }
    files
}

/// Sanitize a `unique_id` into a filesystem-safe file stem: every non
/// `[A-Za-z0-9._-]` char becomes `_`. The `.` separators in a uid are KEPT
/// (they read naturally as `model.project.name.md` and are filesystem-safe),
/// while `/`, spaces, quotes, and other hostile chars collapse to `_`. An empty
/// result falls back to `_` so the stem is never blank.
fn sanitize_stem(uid: &str) -> String {
    let stem: String = uid
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "_".to_string()
    } else {
        stem
    }
}

/// A relative link FROM a node page (in `models/`) TO another node's page. Both
/// live in the same directory, so the link is just the other file's basename.
fn link_between_nodes(target_rel_path: &str) -> &str {
    target_rel_path
        .rsplit('/')
        .next()
        .unwrap_or(target_rel_path)
}

/// The Markdown page for a single node `uid`.
fn node_markdown(dag: &Dag, uid: &str, files: &BTreeMap<String, String>) -> String {
    let node = match dag.get(uid) {
        Some(n) => n,
        // Should not happen (uid came from the node map), but never panic.
        None => return format!("# {uid}\n\nNode not found.\n"),
    };
    let detail = dag.detail(uid);
    let mut s = String::new();

    // Heading + metadata.
    s.push_str(&format!("# {}\n\n", node.name));
    s.push_str(&format!("- resource type: `{}`\n", node.resource_type));
    s.push_str(&format!(
        "- materialization: {}\n",
        opt_code(
            node.materialized
                .as_deref()
                .or_else(|| detail.and_then(|d| d.materialized.as_deref()))
        )
    ));
    if let Some(d) = detail {
        s.push_str(&format!(
            "- database: {}\n",
            opt_code(d.database.as_deref())
        ));
        s.push_str(&format!("- schema: {}\n", opt_code(d.schema.as_deref())));
        s.push_str(&format!(
            "- tags: {}\n",
            if d.tags.is_empty() {
                "none".to_string()
            } else {
                d.tags
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }
    s.push_str(&format!("- unique_id: `{uid}`\n"));
    s.push_str(&format!(
        "- source file: {}\n",
        opt_code(detail.and_then(|d| d.original_file_path.as_deref()))
    ));

    // Description.
    s.push_str("\n## Description\n\n");
    let desc = detail.and_then(|d| d.description.as_deref());
    s.push_str(desc.unwrap_or("No description."));
    s.push('\n');

    // Columns.
    s.push_str("\n## Columns\n\n");
    let columns = detail.map(|d| d.columns.as_slice()).unwrap_or(&[]);
    if columns.is_empty() {
        s.push_str("No columns.\n");
    } else {
        s.push_str("| Name | Type | Description |\n");
        s.push_str("| --- | --- | --- |\n");
        for col in columns {
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                md_cell(&col.name),
                md_cell(col.data_type.as_deref().unwrap_or("")),
                md_cell(col.description.as_deref().unwrap_or("")),
            ));
        }
    }

    // Tests.
    s.push_str("\n## Tests\n\n");
    let tests = dag.tests(uid);
    if tests.is_empty() {
        s.push_str("No tests.\n");
    } else {
        s.push_str("| Test | Column |\n");
        s.push_str("| --- | --- |\n");
        for t in tests {
            s.push_str(&format!(
                "| {} | {} |\n",
                md_cell(&t.kind),
                md_cell(t.column_name.as_deref().unwrap_or("")),
            ));
        }
    }

    // Dependencies: direct parents/children (named, linked) + transitive counts.
    // The direct-neighbour uid lists come from depth-1 directional subgraphs
    // (the only public way to read 1-hop adjacency); the subgraph already sorts
    // its nodes, so the lists are deterministic.
    let parents = direct_neighbours(dag, uid, true);
    let children = direct_neighbours(dag, uid, false);
    let up_total = dag.upstream(uid).len();
    let down_total = dag.downstream(uid).len();

    s.push_str("\n## Upstream dependencies\n\n");
    s.push_str(&format!(
        "- direct parents: {}\n- transitive ancestors: {}\n\n",
        node.direct_up, up_total
    ));
    push_dep_list(&mut s, dag, &parents, files);

    s.push_str("\n## Downstream dependencies\n\n");
    s.push_str(&format!(
        "- direct children: {}\n- transitive descendants: {}\n\n",
        node.direct_down, down_total
    ));
    push_dep_list(&mut s, dag, &children, files);

    // Per-node Mermaid lineage (reuses the shared core, so the id assignment and
    // label escaping match the interactive `m` yank). Built as the FULL upstream
    // + downstream closure (unbounded depth) — the SAME node set the `m` yank
    // produces at its default view (upstream+downstream, no depth limit), so the
    // two surfaces render the same diagram for a node and never diverge. (A
    // central node in a large project therefore gets a large figure; the README's
    // whole-graph diagram is the one that degrades to a per-layer split.)
    s.push_str("\n## Lineage\n\n");
    s.push_str(
        "Full lineage (all upstream + downstream dependencies). \
         Matches the interactive `m` yank for this node.\n\n",
    );
    let sg = dag.subgraph_view(uid, true, true, None);
    if sg.nodes.is_empty() {
        s.push_str("No lineage.\n");
    } else {
        // Links resolve FROM this page (in `models/`) TO sibling pages, so the
        // href is the target's basename — the same relative form the dependency
        // lists use.
        push_linked_diagram(&mut s, dag, &sg, files, |rel| {
            link_between_nodes(rel).to_string()
        });
    }

    s
}

/// Append a linked Mermaid diagram for `sg` followed by a plain-Markdown legend
/// (node name → page link). Two redundant navigation paths on purpose: the
/// Mermaid `click` lines work where Mermaid renders them, and the legend works
/// everywhere Markdown renders — so a reader can always reach a node's page even
/// if `click` is stripped (GitHub historically restricts `click` for security).
///
/// `href_of` maps a target node's stored relative file path (from `files`) to
/// the href to emit, so callers in different directories (`models/` vs the repo
/// root) get a correct relative link. Deterministic: the diagram order is fixed
/// by [`subgraph_mermaid_linked`] and the legend walks `sg.nodes` (already sorted
/// by `unique_id`).
fn push_linked_diagram(
    s: &mut String,
    dag: &Dag,
    sg: &crate::Subgraph,
    files: &BTreeMap<String, String>,
    href_of: impl Fn(&str) -> String,
) {
    s.push_str(&subgraph_mermaid_linked(sg, |uid| {
        files.get(uid).map(|rel| href_of(rel))
    }));
    // Legend: every node in the diagram, linked to its page. Survives anywhere
    // the Mermaid `click` does not.
    s.push_str("\nNodes in this diagram:\n\n");
    for n in &sg.nodes {
        let name = dag
            .get(&n.unique_id)
            .map(|i| i.name.as_str())
            .unwrap_or(&n.name);
        match files.get(&n.unique_id) {
            Some(rel) => s.push_str(&format!("- [{}]({})\n", name, href_of(rel))),
            None => s.push_str(&format!("- {name}\n")),
        }
    }
}

/// Append a "direct dependency" bullet list (each entry linked to its node
/// page), or a "None." line when empty. The `names` are already sorted.
fn push_dep_list(s: &mut String, dag: &Dag, deps: &[String], files: &BTreeMap<String, String>) {
    if deps.is_empty() {
        s.push_str("None.\n");
        return;
    }
    for uid in deps {
        let name = dag.get(uid).map(|n| n.name.as_str()).unwrap_or(uid);
        match files.get(uid) {
            Some(rel) => s.push_str(&format!("- [{}]({})\n", name, link_between_nodes(rel))),
            None => s.push_str(&format!("- {name}\n")),
        }
    }
}

/// The index `README.md`: project heading + provenance, a resource-type count
/// breakdown, a full linked node table (in a deterministic logical order), and a
/// whole-project Mermaid diagram.
fn readme_markdown(
    dag: &Dag,
    project_name: &str,
    source_label: &str,
    files: &BTreeMap<String, String>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {project_name}\n\n"));
    s.push_str(&format!("Generated from: `{source_label}`\n\n"));

    // Summary: total + per-resource-type breakdown (the kinds present, in a
    // fixed display order so the line is deterministic).
    s.push_str("## Summary\n\n");
    s.push_str(&format!("- total nodes: {}\n", dag.len()));
    for kind in RESOURCE_ORDER {
        let n = dag.count_by_resource_type(kind);
        if n > 0 {
            s.push_str(&format!("- {kind}: {n}\n"));
        }
    }

    // Test-coverage + orphan summary, computed from the SAME predicates the TUI
    // uses (`coverage_gap` / the `coverage_summary` base / `direct_down == 0`)
    // so the numbers here match the `t` lens, the `cov` status segment, and the
    // stats dashboard exactly — no second implementation to drift.
    push_stats_summary(&mut s, dag, files);

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
fn push_readme_lineage(s: &mut String, dag: &Dag, files: &BTreeMap<String, String>) {
    let href = |rel: &str| rel.to_string();
    if dag.is_empty() {
        s.push_str("No nodes.\n");
        return;
    }
    if dag.len() < FULL_GRAPH_NODE_LIMIT {
        let sg = full_subgraph(dag);
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
        let mut edges: Vec<crate::Edge> = dag
            .edges()
            .into_iter()
            .filter(|(p, c)| member_uids.contains(p.as_str()) && member_uids.contains(c.as_str()))
            .map(|(parent, child)| crate::Edge { parent, child })
            .collect();
        edges.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.child.cmp(&b.child)));
        let sg = crate::Subgraph {
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
struct LayerGroup {
    label: String,
    uids: Vec<String>,
}

/// Partition the dag's nodes into ordered groups for the split lineage:
/// model layers in [`LAYER_ORDER`] first (each with its members), then one
/// group per remaining resource type in [`RESOURCE_ORDER`], then a catch-all
/// `other` group — exactly the buckets [`readme_order`] would lay out, so the
/// split diagrams appear in the same order as the node table. Every bucket's
/// uids are sorted, and empty buckets are dropped, so the result is
/// deterministic and gap-free (every node lands in exactly one group).
fn layer_groups(dag: &Dag) -> Vec<LayerGroup> {
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

/// Append the README's test-coverage + orphan summary, reusing the TUI's
/// predicates so the numbers can never drift from the `t` lens / `cov` status
/// segment / stats dashboard:
/// - coverage % = the share of TESTABLE resources (model/snapshot/seed) that
///   carry at least one test — the inverse of [`coverage_gap`], over the SAME
///   base [`crate::app::App::coverage_summary`] sums;
/// - orphan models = models with `direct_down == 0` (no downstream consumer),
///   the same predicate the stats dashboard's `zero_downstream` count uses.
///
/// Both are order-independent / sorted, so the section is deterministic.
fn push_stats_summary(s: &mut String, dag: &Dag, files: &BTreeMap<String, String>) {
    let mut tested = 0usize;
    let mut testable = 0usize;
    let mut orphans: Vec<&NodeInfo> = Vec::new();
    for node in dag.nodes().values() {
        // `coverage_gap` IS the testable-and-untested predicate; a testable node
        // is one whose resource_type the gap predicate considers.
        if matches!(node.resource_type.as_str(), "model" | "snapshot" | "seed") {
            testable += 1;
            if !coverage_gap(node) {
                tested += 1;
            }
        }
        if node.resource_type == "model" && node.direct_down == 0 {
            orphans.push(node);
        }
    }
    s.push_str("\n## Test coverage\n\n");
    // Integer percent, truncated — matches the status `cov` segment's
    // `tested * 100 / total` rather than introducing rounding here. `checked_div`
    // yields `None` for an empty testable set (no division by zero).
    match (tested * 100).checked_div(testable) {
        None => s.push_str("- testable resources (model/snapshot/seed): 0\n"),
        Some(pct) => {
            s.push_str(&format!(
                "- tested: {tested} / {testable} testable resources (model/snapshot/seed)\n"
            ));
            s.push_str(&format!("- coverage: {pct}%\n"));
            s.push_str(&format!("- untested: {}\n", testable - tested));
        }
    }

    s.push_str("\n## Orphan models\n\n");
    s.push_str("Models with no downstream consumer (`direct_down == 0`):\n\n");
    if orphans.is_empty() {
        s.push_str("None.\n");
    } else {
        orphans.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.unique_id.cmp(&b.unique_id))
        });
        for node in orphans {
            match files.get(&node.unique_id) {
                Some(rel) => s.push_str(&format!("- [{}]({})\n", node.name, rel)),
                None => s.push_str(&format!("- {}\n", node.name)),
            }
        }
    }
}

/// The node-count threshold at/above which the README full lineage is split by
/// layer instead of drawn as one diagram. Chosen so the common small/medium
/// project (the 93-node fixture, typical jaffle-shop-scale demos) still gets a
/// single whole-graph picture, while genuinely large projects degrade to
/// per-layer diagrams rather than an unreadable — and renderer-straining —
/// hairball. A fixed constant, so a given project always renders identically
/// (determinism); raise it if your renderer comfortably handles bigger graphs.
const FULL_GRAPH_NODE_LIMIT: usize = 150;

/// Resource types in their README display order. Anything not listed is grouped
/// under "other" by [`readme_order`] (sorted by uid), so an unexpected kind is
/// never dropped.
const RESOURCE_ORDER: &[&str] = &["model", "snapshot", "seed", "source", "exposure"];

/// All nodes in a deterministic logical order for the README table:
/// - models first, in dbt layer order ([`layer_rank`]) then by name/unique_id —
///   mirroring the left pane's grouping;
/// - then the remaining resource types in [`RESOURCE_ORDER`], each by
///   name/unique_id;
/// - then any unrecognized kind, by unique_id.
fn readme_order(dag: &Dag) -> Vec<&NodeInfo> {
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
fn resource_rank(resource_type: &str) -> usize {
    RESOURCE_ORDER
        .iter()
        .position(|k| *k == resource_type)
        .unwrap_or(RESOURCE_ORDER.len())
}

/// The layer rank for a model (so models sort by dbt layer order before name);
/// non-models all share rank 0, leaving their name/unique_id tie-break to order
/// them — the resource-type rank already separates the groups.
fn model_layer_rank(node: &NodeInfo) -> usize {
    if node.resource_type != "model" {
        return 0;
    }
    let layer = crate::model_list::first_dir(node).unwrap_or("");
    layer_rank(layer)
}

/// A synthetic whole-project [`crate::Subgraph`]: every node (sorted by uid) and
/// every edge (sorted), with no node marked selected (an empty `selected` uid
/// matches nothing, so [`subgraph_mermaid_linked`] adds no ` *`). Reuses [`Dag::edges`]
/// and the node map directly, so the whole-graph diagram goes through the same
/// shared Mermaid core as every per-node one.
fn full_subgraph(dag: &Dag) -> crate::Subgraph {
    let mut nodes: Vec<NodeInfo> = dag.nodes().values().cloned().collect();
    nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));
    let edges = dag
        .edges()
        .into_iter()
        .map(|(parent, child)| crate::Edge { parent, child })
        .collect();
    crate::Subgraph {
        selected: String::new(),
        nodes,
        edges,
    }
}

/// The direct (1-hop) neighbour uids of `uid`, on the `upstream` side (parents)
/// when `upstream` is true, else the downstream side (children). Read off a
/// depth-1 directional [`Dag::subgraph_view`] — the public surface for 1-hop
/// adjacency — and returned sorted (the subgraph nodes are already sorted by
/// uid, and the node itself is filtered out).
fn direct_neighbours(dag: &Dag, uid: &str, upstream: bool) -> Vec<String> {
    let sg = dag.subgraph_view(uid, upstream, !upstream, Some(1));
    sg.nodes
        .into_iter()
        .map(|n| n.unique_id)
        .filter(|id| id != uid)
        .collect()
}

/// Format an optional value as inline code, or the literal `none` when absent.
fn opt_code(value: Option<&str>) -> String {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => format!("`{v}`"),
        None => "none".to_string(),
    }
}

/// Escape text for a single Markdown table cell: a `|` would start a new column
/// and a newline would break the row, so both are neutralized. The text is kept
/// otherwise verbatim (names/descriptions may carry `"`/`(`/`)` — those are
/// harmless inside a cell).
fn md_cell(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_dag;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

    fn fixture_dag() -> Dag {
        load_dag(FIXTURE).expect("fixture manifest must load")
    }

    /// Find the contents of the generated file at `rel_path`.
    fn file<'a>(out: &'a [(String, String)], rel_path: &str) -> &'a str {
        out.iter()
            .find(|(p, _)| p == rel_path)
            .map(|(_, c)| c.as_str())
            .unwrap_or_else(|| panic!("expected a generated file at {rel_path}"))
    }

    #[test]
    fn generates_one_page_per_node_plus_readme() {
        let dag = fixture_dag();
        let out = generate_docs(&dag, "jaffle_finance", "tests/fixtures/manifest.json");
        // 93 nodes (45 model + 38 source + 7 seed + 1 snapshot + 2 exposure)
        // plus the README index.
        assert_eq!(out.len(), dag.len() + 1, "one page per node + README");
        let readme_count = out.iter().filter(|(p, _)| p == "README.md").count();
        assert_eq!(readme_count, 1, "exactly one README");
        // Every node page lives under the nodes dir.
        for (path, _) in &out {
            assert!(
                path == "README.md" || path.starts_with(&format!("{NODES_DIR}/")),
                "unexpected path {path}"
            );
        }
    }

    #[test]
    fn node_paths_are_unique_no_collisions() {
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        let mut paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        let count = paths.len();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), count, "no two files share a path");
    }

    #[test]
    fn deterministic_byte_identical_across_runs() {
        let dag = fixture_dag();
        let a = generate_docs(&dag, "jaffle_finance", "tests/fixtures/manifest.json");
        let b = generate_docs(&dag, "jaffle_finance", "tests/fixtures/manifest.json");
        assert_eq!(a, b, "two generations must be byte-identical");
    }

    #[test]
    fn readme_links_resolve_to_real_generated_files() {
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        let readme = file(&out, "README.md");
        // Pull every `models/...md` target out of the README markdown links and
        // confirm each names a file we actually generated (no broken links).
        let mut checked = 0usize;
        for chunk in readme.split("](") {
            if let Some(end) = chunk.find(')') {
                let target = &chunk[..end];
                if target.starts_with(&format!("{NODES_DIR}/")) && target.ends_with(".md") {
                    assert!(
                        generated.contains(target),
                        "README links to missing file {target}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= dag.len(), "every node is linked from the README");
    }

    #[test]
    fn readme_has_summary_counts_and_mermaid() {
        let dag = fixture_dag();
        let out = generate_docs(&dag, "jaffle_finance", "manifest.json");
        let readme = file(&out, "README.md");
        assert!(readme.contains("# jaffle_finance"), "project heading");
        assert!(
            readme.contains("Generated from: `manifest.json`"),
            "provenance"
        );
        assert!(readme.contains("- total nodes: 93"), "total count");
        assert!(readme.contains("- model: 45"), "model count");
        assert!(readme.contains("- source: 38"), "source count");
        assert!(readme.contains("- seed: 7"), "seed count");
        assert!(readme.contains("- snapshot: 1"), "snapshot count");
        assert!(
            readme.contains("```mermaid\ngraph LR\n"),
            "project mermaid fence"
        );
    }

    #[test]
    fn node_page_has_all_sections_and_a_mermaid_fence() {
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        // Pick a model that the fixture freezes: fct_subscription_process.
        let uid = dag
            .nodes()
            .values()
            .find(|n| n.name == "fct_subscription_process")
            .map(|n| n.unique_id.clone())
            .expect("fixture has fct_subscription_process");
        let files = node_files(&dag);
        let page = file(&out, &files[&uid]);
        assert!(page.starts_with("# fct_subscription_process"), "heading");
        for section in [
            "## Description",
            "## Columns",
            "## Tests",
            "## Upstream dependencies",
            "## Downstream dependencies",
            "## Lineage",
        ] {
            assert!(page.contains(section), "missing section {section}");
        }
        assert!(page.contains("- unique_id: `"), "unique_id metadata");
        assert!(
            page.contains("```mermaid\ngraph LR\n"),
            "per-node mermaid fence"
        );
        // It has upstream dependencies, so the closure-count lines are present.
        assert!(
            page.contains("- transitive ancestors:"),
            "transitive up count"
        );
        assert!(
            page.contains("- transitive descendants:"),
            "transitive down count"
        );
    }

    #[test]
    fn seed_with_accepted_values_test_lists_it() {
        // The fixture's fiscal_years seed carries an accepted_values test.
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        let uid = dag
            .nodes()
            .values()
            .find(|n| n.name == "fiscal_years" && n.resource_type == "seed")
            .map(|n| n.unique_id.clone())
            .expect("fixture has the fiscal_years seed");
        let files = node_files(&dag);
        let page = file(&out, &files[&uid]);
        assert!(page.contains("## Tests"), "tests section present");
        assert!(
            page.contains("accepted_values"),
            "lists the accepted_values test"
        );
    }

    #[test]
    fn empty_sections_are_labeled_not_dropped() {
        // A source with no tests still shows the Tests heading + "No tests."
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        let uid = dag
            .nodes()
            .values()
            .find(|n| n.resource_type == "source" && dag.tests(&n.unique_id).is_empty())
            .map(|n| n.unique_id.clone())
            .expect("fixture has an untested source");
        let files = node_files(&dag);
        let page = file(&out, &files[&uid]);
        assert!(
            page.contains("## Tests\n\nNo tests."),
            "labeled empty tests"
        );
    }

    #[test]
    fn hostile_names_and_descriptions_do_not_break_markdown_or_mermaid() {
        // A node whose name/description carry a `"`, a `|`, and a newline must
        // not break the Mermaid label, the Markdown table cell, or the fence.
        use crate::{RawDependsOn, RawManifest, RawNode};
        use std::collections::HashMap;
        let mut columns = serde_json::Map::new();
        columns.insert(
            "id".to_string(),
            serde_json::json!({
                "name": "id",
                "data_type": "integer",
                "description": "a | piped \"quoted\" column"
            }),
        );
        let nodes = HashMap::from([
            (
                "model.p.evil".to_string(),
                RawNode {
                    name: "ev\"il | model".into(),
                    resource_type: "model".into(),
                    path: Some("staging/evil.sql".into()),
                    description: Some("line one\nline \"two\" | pipe".into()),
                    columns,
                    depends_on: RawDependsOn {
                        nodes: vec!["model.p.up".to_string()],
                    },
                    ..Default::default()
                },
            ),
            (
                "model.p.up".to_string(),
                RawNode {
                    name: "up".into(),
                    resource_type: "model".into(),
                    path: Some("staging/up.sql".into()),
                    ..Default::default()
                },
            ),
        ]);
        let parent_map =
            HashMap::from([("model.p.evil".to_string(), vec!["model.p.up".to_string()])]);
        let child_map =
            HashMap::from([("model.p.up".to_string(), vec!["model.p.evil".to_string()])]);
        let manifest = RawManifest {
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            parent_map,
            child_map,
        };
        let dag = Dag::build(&manifest);
        let out = generate_docs(&dag, "p", "src");
        let files = node_files(&dag);
        let page = file(&out, &files["model.p.evil"]);
        // The Mermaid label escapes the quote (#quot;) and drops control chars,
        // so the raw `"` never reaches a Mermaid `["..."]` string nor the fence.
        assert!(page.contains("#quot;"), "mermaid label escaped the quote");
        // The hostile column description carries a `|`; the table cell escapes
        // it to `\|` so the row keeps its column count.
        assert!(
            page.contains("a \\| piped"),
            "table cell escaped the pipe in the column description"
        );
        // The whole-project README must also survive the hostile name.
        let readme = file(&out, "README.md");
        assert!(
            readme.contains("ev\"il \\| model"),
            "README cell escaped the pipe"
        );
        // Determinism still holds with the hostile data.
        let again = generate_docs(&dag, "p", "src");
        assert_eq!(out, again, "deterministic even with hostile data");
    }

    // ---- Sprint 2 additions ----

    /// Pull every `models/…md` target out of a `click <id> "<href>"` line.
    fn click_targets(markdown: &str) -> Vec<String> {
        markdown
            .lines()
            .filter_map(|l| {
                let l = l.trim_start();
                let rest = l.strip_prefix("click ")?;
                // `click <id> "<href>"` — the href is the only quoted span.
                let start = rest.find('"')? + 1;
                let end = rest[start..].find('"')? + start;
                Some(rest[start..end].to_string())
            })
            .collect()
    }

    #[test]
    fn diagram_click_links_target_real_generated_files() {
        // Every `click` link in EVERY generated page (node pages + README) must
        // name a file we actually generated — no broken navigation.
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        let mut total_clicks = 0usize;
        for (page, contents) in &out {
            // README hrefs are root-relative (`models/…`); node-page hrefs are
            // bare basenames (`….md`), resolved against the page's own dir.
            let in_models = page.starts_with(&format!("{NODES_DIR}/"));
            for target in click_targets(contents) {
                total_clicks += 1;
                let resolved = if target.contains('/') {
                    target.clone()
                } else if in_models {
                    format!("{NODES_DIR}/{target}")
                } else {
                    target.clone()
                };
                assert!(
                    generated.contains(resolved.as_str()),
                    "{page}: click links to missing file {target} (resolved {resolved})"
                );
            }
        }
        assert!(total_clicks > 0, "at least some click links were emitted");
    }

    #[test]
    fn every_diagram_has_a_legend_pointing_at_real_files() {
        // The legend ("Nodes in this diagram:" + bullet links) is the
        // click-independent fallback; its links must resolve too.
        let dag = fixture_dag();
        let out = generate_docs(&dag, "p", "src");
        let generated: HashSet<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        for (page, contents) in &out {
            // A page with lineage carries a legend.
            if !contents.contains("```mermaid") {
                continue;
            }
            assert!(
                contents.contains("Nodes in this diagram:"),
                "{page}: a diagram is missing its legend"
            );
            let in_models = page.starts_with(&format!("{NODES_DIR}/"));
            // Legend links are Markdown `](href)`; resolve like click targets.
            for chunk in contents.split("](") {
                if let Some(end) = chunk.find(')') {
                    let target = &chunk[..end];
                    if !target.ends_with(".md") {
                        continue;
                    }
                    let resolved = if target.contains('/') {
                        target.to_string()
                    } else if in_models {
                        format!("{NODES_DIR}/{target}")
                    } else {
                        target.to_string()
                    };
                    assert!(
                        generated.contains(resolved.as_str()),
                        "{page}: legend/link target {target} missing (resolved {resolved})"
                    );
                }
            }
        }
    }

    #[test]
    fn readme_coverage_summary_matches_coverage_gap_predicate() {
        // The README's coverage numbers must be exactly the `coverage_gap` base
        // (the same predicate the TUI's `t` lens / `cov` segment use): testable
        // = model|snapshot|seed, tested = NOT coverage_gap.
        let dag = fixture_dag();
        let mut testable = 0usize;
        let mut tested = 0usize;
        for n in dag.nodes().values() {
            if matches!(n.resource_type.as_str(), "model" | "snapshot" | "seed") {
                testable += 1;
                if !crate::coverage_gap(n) {
                    tested += 1;
                }
            }
        }
        let pct = tested * 100 / testable;
        let out = generate_docs(&dag, "p", "src");
        let readme = file(&out, "README.md");
        assert!(
            readme.contains(&format!(
                "- tested: {tested} / {testable} testable resources"
            )),
            "coverage tested/total line, expected {tested}/{testable}"
        );
        assert!(
            readme.contains(&format!("- coverage: {pct}%")),
            "coverage percent line, expected {pct}%"
        );
        assert!(
            readme.contains(&format!("- untested: {}", testable - tested)),
            "untested count line"
        );
    }

    #[test]
    fn readme_orphan_list_matches_direct_down_zero_models() {
        // The orphan list is exactly the models with `direct_down == 0`, sorted.
        let dag = fixture_dag();
        let mut expected: Vec<&str> = dag
            .nodes()
            .values()
            .filter(|n| n.resource_type == "model" && n.direct_down == 0)
            .map(|n| n.name.as_str())
            .collect();
        expected.sort_unstable();
        let out = generate_docs(&dag, "p", "src");
        let readme = file(&out, "README.md");
        assert!(readme.contains("## Orphan models"), "orphan section");
        // The section after "## Orphan models" lists every expected name; pull
        // the bullet names that appear under it.
        let section = readme
            .split("## Orphan models")
            .nth(1)
            .expect("orphan section present");
        for name in &expected {
            assert!(
                section.contains(&format!("[{name}](")) || section.contains(&format!("- {name}\n")),
                "orphan model {name} missing from the README list"
            );
        }
        assert!(!expected.is_empty(), "fixture has orphan models to assert");
    }

    #[test]
    fn small_project_renders_one_whole_graph_diagram() {
        // Below the threshold (the 93-node fixture), the README lineage is a
        // SINGLE diagram (no per-layer `###` split headings).
        let dag = fixture_dag();
        assert!(dag.len() < FULL_GRAPH_NODE_LIMIT, "fixture is small");
        let out = generate_docs(&dag, "p", "src");
        let readme = file(&out, "README.md");
        let lineage = readme
            .split("## Lineage")
            .nth(1)
            .expect("README has a Lineage section");
        let fences = lineage.matches("```mermaid").count();
        assert_eq!(fences, 1, "small project = exactly one whole-graph diagram");
        assert!(
            !lineage.contains("split by layer"),
            "no split note below the threshold"
        );
    }

    /// A synthetic dag of `count` chained models spread across the four logical
    /// layers, used to cross the [`FULL_GRAPH_NODE_LIMIT`] threshold in tests.
    fn large_layered_dag(count: usize) -> Dag {
        use crate::{RawDependsOn, RawManifest, RawNode};
        use std::collections::HashMap;
        let layers = ["staging", "intermediate", "marts", "utilities"];
        let mut nodes = HashMap::new();
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for i in 0..count {
            let layer = layers[i % layers.len()];
            let uid = format!("model.big.m{i:04}");
            // Chain each node to the previous one in the SAME layer so every
            // group carries some intra-group edges.
            let depends: Vec<String> = if i >= layers.len() {
                vec![format!("model.big.m{:04}", i - layers.len())]
            } else {
                Vec::new()
            };
            for p in &depends {
                parent_map.entry(uid.clone()).or_default().push(p.clone());
                child_map.entry(p.clone()).or_default().push(uid.clone());
            }
            nodes.insert(
                uid.clone(),
                RawNode {
                    name: format!("m{i:04}"),
                    resource_type: "model".into(),
                    path: Some(format!("{layer}/m{i:04}.sql")),
                    depends_on: RawDependsOn { nodes: depends },
                    ..Default::default()
                },
            );
        }
        Dag::build(&RawManifest {
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            parent_map,
            child_map,
        })
    }

    #[test]
    fn large_project_splits_full_diagram_by_layer() {
        // At/above the threshold the README lineage splits into one diagram per
        // layer group, with an explanatory note — the single hairball is avoided.
        let dag = large_layered_dag(FULL_GRAPH_NODE_LIMIT + 8);
        assert!(dag.len() >= FULL_GRAPH_NODE_LIMIT);
        let out = generate_docs(&dag, "big", "src");
        let readme = file(&out, "README.md");
        let lineage = readme
            .split("## Lineage")
            .nth(1)
            .expect("README has a Lineage section");
        assert!(
            lineage.contains("split by layer"),
            "large project carries the split note"
        );
        // One diagram per layer group (staging/intermediate/marts/utilities).
        let fences = lineage.matches("```mermaid").count();
        assert_eq!(fences, 4, "one diagram per layer group");
        for layer in LAYER_ORDER {
            assert!(
                lineage.contains(&format!("### {layer} (")),
                "layer heading for {layer}"
            );
        }
        // Still deterministic for the large/split case.
        let again = generate_docs(&dag, "big", "src");
        assert_eq!(out, again, "split lineage is deterministic");
    }

    #[test]
    fn split_diagrams_cover_every_node_exactly_once() {
        // Every node lands in exactly one layer group — no node dropped, none
        // duplicated across groups.
        let dag = large_layered_dag(FULL_GRAPH_NODE_LIMIT + 8);
        let groups = layer_groups(&dag);
        let mut all: Vec<String> = groups.iter().flat_map(|g| g.uids.clone()).collect();
        let total = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), total, "no node appears in two groups");
        assert_eq!(total, dag.len(), "every node lands in exactly one group");
    }

    #[test]
    fn full_doc_set_is_byte_identical_with_all_sprint2_additions() {
        // The end-to-end determinism guarantee covering the new surface: click
        // links, legends, the stats summary, and (via the large dag) the split.
        let small = fixture_dag();
        let a = generate_docs(&small, "jaffle_finance", "manifest.json");
        let b = generate_docs(&small, "jaffle_finance", "manifest.json");
        assert_eq!(a, b, "small project: byte-identical across runs");
        let big = large_layered_dag(FULL_GRAPH_NODE_LIMIT + 8);
        let c = generate_docs(&big, "big", "src");
        let d = generate_docs(&big, "big", "src");
        assert_eq!(c, d, "large project: byte-identical across runs");
    }

    #[test]
    fn project_name_comes_from_unique_id_segment() {
        // The Dag-derived project name (the `unique_id` project segment) is what
        // the docs index should title with — robust to weird source labels.
        let dag = fixture_dag();
        assert_eq!(
            dag.project_name(),
            Some("jaffle_finance"),
            "project name read from unique_id"
        );
    }

    /// Extract the single fenced `\`\`\`mermaid … \`\`\`` block from a page,
    /// including both fences and the trailing newline — the exact byte shape
    /// [`subgraph_mermaid_linked`] / [`crate::app::App::lineage_mermaid`] emit.
    fn fenced_mermaid_block(page: &str) -> String {
        let open = "```mermaid\n";
        let start = page.find(open).expect("page has a mermaid fence");
        let after = start + open.len();
        let rel_end = page[after..].find("\n```\n").expect("closing fence");
        let end = after + rel_end + "\n```\n".len();
        page[start..end].to_string()
    }

    /// Drop the docs-only `click <id> "<href>"` navigation lines from a fenced
    /// Mermaid block, leaving the bare diagram body (what the `m` yank emits).
    fn strip_click_lines(block: &str) -> String {
        block
            .lines()
            .filter(|l| !l.trim_start().starts_with("click "))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    #[test]
    fn per_node_lineage_figure_equals_the_m_key_yank() {
        // The per-node docs Lineage figure must render the SAME diagram the
        // interactive `m` yank produces for that node at the default view
        // (upstream + downstream, unbounded depth) — both go through the one
        // shared `subgraph_mermaid_linked` core over the SAME full-closure
        // subgraph. The docs variant only adds `click` navigation lines inside
        // the fence; stripping those must leave a body byte-identical to the
        // yank. This locks the two surfaces together so they can never drift.
        use crate::App;
        use std::path::PathBuf;

        let dag = fixture_dag();
        let files = node_files(&dag);
        let out = generate_docs(&dag, "p", "src");

        // A spread of shapes: a deep mart, a table model, and a leaf source.
        for name in ["fct_subscription_process", "pos_txn"] {
            let uid = dag
                .nodes()
                .values()
                .find(|n| n.name == name)
                .map(|n| n.unique_id.clone())
                .unwrap_or_else(|| panic!("fixture has {name}"));

            let mut app = App::new(dag.clone(), PathBuf::from(FIXTURE));
            assert!(app.select_by_name(name), "select {name}");
            let yank = app.lineage_mermaid().expect("m yank for a selected node");

            let page = file(&out, &files[&uid]);
            let body = strip_click_lines(&fenced_mermaid_block(page));
            assert_eq!(
                body, yank,
                "docs Lineage figure for {name} must equal the `m` yank (minus click lines)"
            );
        }
    }
}
