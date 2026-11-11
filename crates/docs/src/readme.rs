use super::*;

/// The index `README.md`: project heading + provenance, a resource-type count
/// breakdown, a full linked node table (in a deterministic logical order), and a
/// whole-project Mermaid diagram.
///
/// `options.nodes` is the generated set (`--select`/`--exclude` resolved by
/// the caller; `None` = the whole `dag`): the Nodes table and the lineage
/// diagrams cover only it, while the Summary counts stay project-wide.
/// `select_label`/`exclude_label` echo the selector strings on the README's
/// `Selection:`/`Excluding:` lines.
pub(super) fn readme_markdown(
    ctx: &PageInputs,
    project_name: &str,
    source_label: &str,
    scope: PackageScope,
    lint: Option<&[Finding]>,
    options: &DocsOptions,
) -> String {
    let dag = ctx.dag;
    let files = ctx.files;
    // The generated set — the Nodes table and lineage describe this graph:
    // the whole Dag, or `nodes` filtered down to the selection. Everything
    // upstream (the Summary, lint/coverage stats) keeps the FULL Dag so a
    // selected run still reports project-wide numbers.
    let selected = options
        .nodes
        .map(|sel| dag.filtered(|n| sel.contains(&n.unique_id)));
    let listed = selected.as_ref().unwrap_or(dag);

    let mut s = String::new();
    s.push_str(&format!("# {project_name}\n\n"));
    // The project's `__overview__` docs block — placed directly under the
    // title, the spot dbt's own docs site renders it. The Dag already carries
    // it trimmed; absent → the provenance line keeps its pre-overview bytes.
    if let Some(overview) = dag.overview() {
        s.push_str(overview);
        s.push_str("\n\n");
    }
    s.push_str(&format!("Generated from: `{source_label}`\n"));
    if let Some(label) = options.select_label {
        s.push_str(&format!("Selection: `{label}`\n"));
    }
    if let Some(label) = options.exclude_label {
        s.push_str(&format!("Excluding: `{label}`\n"));
    }
    // Which side artifacts fed this doc set — declared only when one was
    // explicitly enabled (an artifact-free run emits nothing here at all).
    // The names only, never the paths: the file set must read identically
    // from any checkout.
    let mut includes: Vec<&str> = Vec::new();
    if ctx.catalog.is_some() {
        includes.push("warehouse catalog");
    }
    if ctx.run_results.is_some() {
        includes.push("last run results");
    }
    if !includes.is_empty() {
        s.push_str(&format!("Includes: {}\n", includes.join(", ")));
    }
    s.push('\n');

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

    // The index pages — cross-cutting views over the project living next to
    // this README. They're ALWAYS generated (an empty index states its
    // emptiness), so these links always resolve.
    s.push_str("\n## Indexes\n\n");
    s.push_str("- [Owners](owners.md) — models grouped by the owner meta value\n");
    s.push_str("- [Tags](tags.md) — tagged nodes grouped by tag\n");
    s.push_str("- [Groups](groups.md) — models grouped by group, with access\n");

    // Node table in logical order (see `readme_order`), each linked to its
    // page — the GENERATED set only (`listed`): every row's link resolves.
    s.push_str("\n## Nodes\n\n");
    s.push_str("| Name | Type | Materialization | Tests | Doc |\n");
    s.push_str("| --- | --- | --- | --- | --- |\n");
    for node in readme_order(listed) {
        let uid = node.unique_id.as_str();
        let link = files
            .get(uid)
            .map(|rel| {
                format!(
                    "[{}]({})",
                    md_label(&node.name),
                    page_link("README.md", rel)
                )
            })
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
    push_readme_lineage(&mut s, listed, files);

    s
}

/// Append the README's whole-project lineage. One diagram of the entire
/// graph when it fits — [`FULL_GRAPH_NODE_LIMIT`] nodes is the size cap, and
/// even a smaller graph falls back to the per-layer split when its figure
/// would exceed the Mermaid budget (the same character/edge limits the node
/// pages honour). At or above the limit the single diagram would be an
/// unreadable hairball (and slow / liable to be rejected by Mermaid
/// renderers), so the graph splits into one diagram per logical layer
/// ([`LAYER_ORDER`], unknown layers and non-models grouped last) — each
/// holding only that group's nodes and the edges WITHIN it. A group whose
/// own figure still exceeds the budget is replaced by the omission line and
/// a plain node table.
///
/// The threshold is a fixed constant (documented on [`FULL_GRAPH_NODE_LIMIT`]),
/// so the same project always renders the same way: determinism is preserved
/// either branch. Cross-layer edges are necessarily omitted from the split view
/// (they would reintroduce the hairball); a note says so and points readers at
/// the per-node pages for the full picture. The note names WHICH trigger —
/// the node count or the budget — fired.
/// `dag` is the GENERATED set's graph (the whole project, or the
/// `--select`/`--exclude` selection filtered down by the caller) — the
/// whole-graph figure and the layer splits cover exactly what this run
/// emitted pages for.
pub(super) fn push_readme_lineage(s: &mut String, dag: &Dag, files: &BTreeMap<String, String>) {
    // The README sits at the output root — `page_link` yields the target's
    // stored relative path verbatim, and every link goes through the one
    // resolver like every other page's links do.
    let href = |rel: &str| page_link("README.md", rel);
    if dag.is_empty() {
        s.push_str("No nodes.\n");
        return;
    }
    let whole = dag.full_subgraph();
    // The split trigger — the message names it so a committed README says
    // WHY the graph isn't one picture. The whole-graph figure is rendered
    // only below the node cap: at or above it the split is already decided,
    // so building (then discarding) a multi-MB figure is wasted work.
    let reason = if dag.len() >= FULL_GRAPH_NODE_LIMIT {
        format!(
            "This project has {} nodes (>= {} threshold), so the full graph is \
             split by layer below — one diagram per group, intra-group edges only. \
             For cross-layer lineage of a specific node, open its page (each carries \
             its own neighbourhood diagram).\n",
            dag.len(),
            FULL_GRAPH_NODE_LIMIT
        )
    } else {
        let whole_figure = mermaid_figure(&whole, files, &href);
        if within_mermaid_budget(whole_figure.chars().count(), whole.edges.len()) {
            s.push_str(&whole_figure);
            push_legend(s, dag, &whole, files, &href);
            return;
        }
        format!(
            "The whole-project diagram is {} characters / {} edges — over the \
             rendering limits ({} characters / {} edges) — so the full graph is \
             split by layer below, one diagram per group, intra-group edges only. \
             For cross-layer lineage of a specific node, open its page (each carries \
             its own neighbourhood diagram).\n",
            whole_figure.chars().count(),
            whole.edges.len(),
            MERMAID_MAX_CHARS,
            MERMAID_MAX_EDGES
        )
    };
    // Split by layer group. The order is the model layer order first, then
    // the remaining resource-type groups, mirroring `readme_order`.
    s.push_str(&reason);
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
            continue;
        }
        let figure = mermaid_figure(&sg, files, &href);
        if within_mermaid_budget(figure.chars().count(), sg.edges.len()) {
            push_figure_and_legend(s, dag, &sg, files, figure, &href);
        } else {
            // The group alone still overflows — same wording as the
            // node-page omission, then a plain table so the members are
            // still listed and linked.
            s.push_str(&omitted_diagram_line(sg.nodes.len(), sg.edges.len()));
            s.push_str("\n| Name | Type | Materialization |\n");
            s.push_str("| --- | --- | --- |\n");
            for n in &sg.nodes {
                let cell = files
                    .get(&n.unique_id)
                    .map(|rel| format!("[{}]({})", md_label(&n.name), page_link("README.md", rel)))
                    .unwrap_or_else(|| md_cell(&n.name));
                s.push_str(&format!(
                    "| {} | {} | {} |\n",
                    cell,
                    md_cell(&n.resource_type),
                    md_cell(n.materialized.as_deref().unwrap_or("-")),
                ));
            }
        }
    }
}

/// One layer/resource group for the split README lineage: a display `label` and
/// the member `unique_id`s (sorted). See [`layer_groups`].
pub(super) struct LayerGroup {
    pub(super) label: String,
    pub(super) uids: Vec<String>,
}

/// The shared model→layer bucketing [`layer_groups`] and the per-layer
/// coverage table both use — a model's layer is its first path directory
/// ([`first_dir`]), ranked by [`LAYER_ORDER`] (unknown dirs rank last →
/// `other models`); non-models are skipped. Buckets iterate in rank order;
/// member uids are unsorted (each caller sorts as it needs).
fn bucket_models<'a>(
    nodes: impl Iterator<Item = &'a NodeInfo>,
) -> BTreeMap<usize, (String, Vec<String>)> {
    let mut layers: BTreeMap<usize, (String, Vec<String>)> = BTreeMap::new();
    for node in nodes {
        if node.resource_type != "model" {
            continue;
        }
        let layer = first_dir(node).unwrap_or("");
        let rank = layer_rank(layer);
        let label = LAYER_ORDER.get(rank).copied().unwrap_or("other models");
        layers
            .entry(rank)
            .or_insert_with(|| (label.to_string(), Vec::new()))
            .1
            .push(node.unique_id.clone());
    }
    layers
}

/// The ROOT-PROJECT models, bucketed by [`bucket_models`] — the per-layer
/// coverage table's partition: the same classification the split lineage's
/// model buckets get, narrowed to the root project like every other coverage
/// metric (installed-package models are context, never project health
/// numbers). Empty buckets drop out; member uids sorted.
pub(super) fn model_layer_groups(dag: &Dag) -> Vec<LayerGroup> {
    let primary = dag.primary_package();
    bucket_models(
        dag.nodes()
            .values()
            .filter(|n| primary.as_deref().is_none_or(|p| in_package(n, p))),
    )
    .into_values()
    .map(|(label, mut uids)| {
        uids.sort();
        LayerGroup { label, uids }
    })
    .collect()
}

/// Partition the dag's nodes into ordered groups for the split lineage:
/// model layers in [`LAYER_ORDER`] first (each with its members), then one
/// group per remaining resource type in [`RESOURCE_ORDER`], then a catch-all
/// `other` group — exactly the buckets [`readme_order`] would lay out, so the
/// split diagrams appear in the same order as the node table. Every bucket's
/// uids are sorted, and empty buckets are dropped, so the result is
/// deterministic and gap-free (every node lands in exactly one group).
pub(super) fn layer_groups(dag: &Dag) -> Vec<LayerGroup> {
    let model_layers = bucket_models(dag.nodes().values());
    let mut by_rt: BTreeMap<usize, (String, Vec<String>)> = BTreeMap::new();
    let mut other: Vec<String> = Vec::new();
    for node in dag.nodes().values().filter(|n| n.resource_type != "model") {
        let uid = node.unique_id.clone();
        if let Some(rank) = RESOURCE_ORDER.iter().position(|k| *k == node.resource_type) {
            by_rt
                .entry(rank)
                // The page-directory name doubles as the group label —
                // one pluralization rule for both surfaces.
                .or_insert_with(|| (docs_dir(&node.resource_type).to_string(), Vec::new()))
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
