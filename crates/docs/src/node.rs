use super::*;

/// The Markdown page for a single node `uid`. `lint` is the node's shared
/// [`run_rules`](dbtl_core::run_rules) findings (in rule order): `Some`
/// renders the `Lint violations` section (labeled `None.` when empty), `None`
/// skips it entirely — lint never ran.
pub(super) fn node_markdown(
    dag: &Dag,
    uid: &str,
    files: &BTreeMap<String, String>,
    lint: Option<&[&Finding]>,
) -> String {
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

    // Meta — the `meta` dict as a key-value table, sorted by key
    // (`NodeDetail.meta` is a `BTreeMap`, so iteration IS the sort). Nested
    // values render their compact-JSON form. Always present (labeled, never
    // dropped — same convention as the other sections).
    s.push_str("\n## Meta\n\n");
    let meta = detail.map(|d| &d.meta);
    match meta {
        Some(m) if !m.is_empty() => {
            s.push_str("| Key | Value |\n");
            s.push_str("| --- | --- |\n");
            for (k, v) in m {
                s.push_str(&format!("| {} | {} |\n", md_cell(k), md_cell(v)));
            }
        }
        _ => s.push_str("No meta.\n"),
    }

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

    // Tests — the `Target` column resolves a `relationships` test's `to:`
    // (linked to the target's own page when it has one), and `Rows` carries
    // the unit-test shape (`in:`/`given:`/`expect:` counts) the manifest
    // records (Issue 11).
    s.push_str("\n## Tests\n\n");
    let tests = dag.tests(uid);
    if tests.is_empty() {
        s.push_str("No tests.\n");
    } else {
        s.push_str("| Test | Column | Target | Rows |\n");
        s.push_str("| --- | --- | --- | --- |\n");
        for t in tests {
            // The relationships `to:` target: the resolved display name when
            // the node survived pruning, else the raw uid; linked to its
            // page like a dependency entry. `.col` appends the `field:`.
            let target = match t.ref_uid.as_deref() {
                Some(ref_uid) => {
                    let name = t.ref_name.as_deref().unwrap_or(ref_uid);
                    let label = match t.ref_column.as_deref() {
                        Some(rc) => format!("{name}.{rc}"),
                        None => name.to_string(),
                    };
                    match files.get(ref_uid) {
                        Some(rel) => format!("[{}]({})", md_cell(&label), link_between_nodes(rel)),
                        None => md_cell(&label),
                    }
                }
                None => String::new(),
            };
            // The unit-test shape: `in:`/input-table count plus the inline
            // row counts the manifest carries (fixture:/CSV sides stay blank).
            let rows = t.unit.as_ref().map_or(String::new(), |u| {
                let mut r = format!("in:{}", u.inputs);
                if let Some(g) = u.given_rows {
                    r.push_str(&format!(" given:{g}"));
                }
                if let Some(e) = u.expect_rows {
                    r.push_str(&format!(" expect:{e}"));
                }
                r
            });
            // The Test column: schema tests show the KIND (their recorded
            // name is a hashed `not_null_order_items_…` slug — noise), while
            // a unit test's name IS its identity (`kind` alone reads `unit`).
            let test_label = if t.unit.is_some() {
                t.name.as_str()
            } else {
                t.kind.as_str()
            };
            s.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                md_cell(test_label),
                md_cell(t.column_name.as_deref().unwrap_or("")),
                target,
                md_cell(&rows),
            ));
        }
    }

    // Lint violations — the shared `run_rules` findings for THIS node, in rule
    // order. Rendered only when lint ran (`Some`): same "labeled, never
    // dropped" convention as the other sections.
    if let Some(fs) = lint {
        s.push_str("\n## Lint violations\n\n");
        if fs.is_empty() {
            s.push_str("None.\n");
        } else {
            for f in fs {
                s.push_str(&format!("- `{}`: {}\n", f.rule.name(), f.message));
            }
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
