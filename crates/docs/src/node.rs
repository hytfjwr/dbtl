use super::*;

/// The Markdown page for a single node `uid`. `lint` is the node's shared
/// [`run_rules`](dbtl_core::run_rules) findings (in rule order): `Some`
/// renders the `Lint violations` section (labeled `None.` when empty), `None`
/// skips it entirely — lint never ran. `depth` is the lineage-diagram depth
/// (`DocsOptions::lineage_depth`).
///
/// The section set and order depend on the resource type (pages carry only
/// the sections meaningful for their kind — a direction the page doesn't
/// list is also left out of its diagram):
/// - `model`/`snapshot`: heading bullets, Description, Columns, Tests, SQL,
///   Lint violations, Upstream, Downstream, Lineage (both ways), Meta;
/// - `source`/`seed`: bullets, Description, Columns, Tests, Lint violations,
///   Downstream, Lineage (downstream), Meta;
/// - `exposure`: bullets, Description, Depends on, Lineage (upstream);
/// - `semantic_model`/`metric`/`saved_query`/other: bullets, Description,
///   Upstream, Lineage (upstream).
pub(super) fn node_markdown(
    dag: &Dag,
    uid: &str,
    files: &BTreeMap<String, String>,
    lint: Option<&[&Finding]>,
    depth: LineageDepth,
) -> String {
    let node = match dag.get(uid) {
        Some(n) => n,
        // Should not happen (uid came from the node map), but never panic.
        None => return format!("# {uid}\n\nNode not found.\n"),
    };
    let detail = dag.detail(uid);
    let page_rel = files.get(uid).map(String::as_str).unwrap_or("");
    let rt = node.resource_type.as_str();
    let mut s = String::new();

    // Heading + the resource-type's metadata bullets.
    s.push_str(&format!("# {}\n\n", node.name));
    push_heading_bullets(&mut s, dag, uid, node, detail);

    // Description.
    s.push_str("\n## Description\n\n");
    let desc = detail.and_then(|d| d.description.as_deref());
    s.push_str(desc.unwrap_or("No description."));
    s.push('\n');

    match rt {
        "model" | "snapshot" => {
            push_columns(&mut s, detail);
            push_tests(&mut s, dag, uid, files, page_rel);
            push_sql(&mut s, dag, uid, node, detail);
            push_lint(&mut s, lint);
            push_upstream(&mut s, dag, uid, node, files, page_rel);
            push_downstream(&mut s, dag, uid, node, files, page_rel);
            push_node_lineage(&mut s, dag, uid, files, page_rel, Sides::Both, depth);
            push_meta(&mut s, detail);
        }
        "source" | "seed" => {
            push_columns(&mut s, detail);
            push_tests(&mut s, dag, uid, files, page_rel);
            push_lint(&mut s, lint);
            push_downstream(&mut s, dag, uid, node, files, page_rel);
            push_node_lineage(&mut s, dag, uid, files, page_rel, Sides::Downstream, depth);
            push_meta(&mut s, detail);
        }
        "exposure" => {
            // The consumed set is just the direct parents — the lineage
            // diagram (upstream only) carries the transitive picture.
            s.push_str("\n## Depends on\n\n");
            let parents = direct_neighbours(dag, uid, true);
            push_dep_list(&mut s, dag, &parents, files, page_rel);
            push_node_lineage(&mut s, dag, uid, files, page_rel, Sides::Upstream, depth);
        }
        _ => {
            push_upstream(&mut s, dag, uid, node, files, page_rel);
            push_node_lineage(&mut s, dag, uid, files, page_rel, Sides::Upstream, depth);
        }
    }

    s
}

/// The heading's metadata bullets — the fields each resource type declares.
/// Missing values render `none` (the `opt_code` convention).
fn push_heading_bullets(
    s: &mut String,
    dag: &Dag,
    uid: &str,
    node: &NodeInfo,
    detail: Option<&dbtl_core::NodeDetail>,
) {
    s.push_str(&format!("- resource type: `{}`\n", node.resource_type));
    match node.resource_type.as_str() {
        "model" | "snapshot" => {
            s.push_str(&format!(
                "- materialization: {}\n",
                opt_code(
                    node.materialized
                        .as_deref()
                        .or_else(|| detail.and_then(|d| d.materialized.as_deref()))
                )
            ));
            let access = detail.and_then(|d| d.access.as_deref());
            s.push_str(&format!("- access: {}\n", opt_code(access)));
            let contract = if detail.is_some_and(|d| d.contract_enforced) {
                "`enforced`"
            } else {
                "`not enforced`"
            };
            s.push_str(&format!("- contract: {contract}\n"));
            s.push_str(&format!(
                "- group: {}\n",
                opt_code(detail.and_then(|d| d.group.as_deref()))
            ));
            // `version`/`latest` are only meaningful on versioned models —
            // they still render (`none` when unversioned) so every page of
            // one kind carries the same bullet set.
            s.push_str(&format!(
                "- version: {}\n",
                opt_code(detail.and_then(|d| d.version.as_deref()))
            ));
            s.push_str(&format!(
                "- latest: {}\n",
                opt_code(detail.and_then(|d| d.latest_version.as_deref()))
            ));
            push_db_schema_tags(s, detail);
        }
        "source" => {
            s.push_str(&format!(
                "- database: {}\n",
                opt_code(detail.and_then(|d| d.database.as_deref()))
            ));
            s.push_str(&format!(
                "- schema: {}\n",
                opt_code(detail.and_then(|d| d.schema.as_deref()))
            ));
            // The warehouse table name: the `identifier:` override when set,
            // else the table name — the same `relation_name`-or-`name` rule
            // CLL matches `FROM` clauses against.
            let identifier = detail
                .and_then(|d| d.relation_name.as_deref())
                .unwrap_or(&node.name);
            s.push_str(&format!("- identifier: `{identifier}`\n"));
            let relation = [
                detail.and_then(|d| d.database.as_deref()),
                detail.and_then(|d| d.schema.as_deref()),
                Some(identifier),
            ]
            .into_iter()
            .flatten()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(".");
            s.push_str(&format!("- relation: {}\n", opt_code(Some(&relation))));
            push_tags(s, detail);
        }
        "seed" => {
            s.push_str(&format!(
                "- materialization: {}\n",
                opt_code(
                    node.materialized
                        .as_deref()
                        .or_else(|| detail.and_then(|d| d.materialized.as_deref()))
                )
            ));
            push_db_schema_tags(s, detail);
        }
        "exposure" => {
            let ex = dag.exposure(uid);
            s.push_str(&format!(
                "- type: {}\n",
                opt_code(ex.and_then(|e| e.exposure_type.as_deref()))
            ));
            // `name <email>` — the exposure_note style: each half shows on
            // its own when the other is absent.
            let owner =
                ex.and_then(
                    |e| match (e.owner_name.as_deref(), e.owner_email.as_deref()) {
                        (Some(n), Some(m)) => Some(format!("{n} <{m}>")),
                        (Some(n), None) => Some(n.to_string()),
                        (None, Some(m)) => Some(format!("<{m}>")),
                        (None, None) => None,
                    },
                );
            s.push_str(&format!("- owner: {}\n", opt_code(owner.as_deref())));
            s.push_str(&format!(
                "- url: {}\n",
                opt_code(ex.and_then(|e| e.url.as_deref()))
            ));
        }
        _ => push_tags(s, detail),
    }
    s.push_str(&format!("- unique_id: `{uid}`\n"));
    s.push_str(&format!(
        "- source file: {}\n",
        opt_code(detail.and_then(|d| d.original_file_path.as_deref()))
    ));
}

/// The `database`/`schema`/`tags` bullet triplet the buildable kinds share.
fn push_db_schema_tags(s: &mut String, detail: Option<&dbtl_core::NodeDetail>) {
    s.push_str(&format!(
        "- database: {}\n",
        opt_code(detail.and_then(|d| d.database.as_deref()))
    ));
    s.push_str(&format!(
        "- schema: {}\n",
        opt_code(detail.and_then(|d| d.schema.as_deref()))
    ));
    push_tags(s, detail);
}

/// The `tags` bullet: the list inline-code-joined, `none` when empty.
fn push_tags(s: &mut String, detail: Option<&dbtl_core::NodeDetail>) {
    let tags = detail.map(|d| d.tags.as_slice()).unwrap_or(&[]);
    s.push_str(&format!(
        "- tags: {}\n",
        if tags.is_empty() {
            "none".to_string()
        } else {
            tags.iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
}

/// The `Columns` section: the declared column table or `No columns.`.
fn push_columns(s: &mut String, detail: Option<&dbtl_core::NodeDetail>) {
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
}

/// The `Tests` section — the `Target` column resolves a `relationships`
/// test's `to:` (linked to the target's own page when it has one), and
/// `Rows` carries the unit-test shape (`in:`/`given:`/`expect:` counts) the
/// manifest records.
fn push_tests(
    s: &mut String,
    dag: &Dag,
    uid: &str,
    files: &BTreeMap<String, String>,
    page_rel: &str,
) {
    s.push_str("\n## Tests\n\n");
    let tests = dag.tests(uid);
    if tests.is_empty() {
        s.push_str("No tests.\n");
        return;
    }
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
                    Some(rel) => format!("[{}]({})", md_label(&label), page_link(page_rel, rel)),
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

/// The `SQL` section (models and snapshots only): the model's SQL collapsed
/// inside a `<details>` block. The text is the manifest-carried
/// `compiled_code` when present (the artifact's own expansion — never a
/// `target/compiled/` file: the doc set must not change with the local
/// checkout's build state), else the raw SQL; snapshots always show raw.
/// Neither → `No SQL.` (the section stays). `.py` models get the `python`
/// fence tag. The fence is sized one backtick past the longest backtick run
/// inside so embedded inline code can never close it early.
fn push_sql(
    s: &mut String,
    dag: &Dag,
    uid: &str,
    node: &NodeInfo,
    detail: Option<&dbtl_core::NodeDetail>,
) {
    s.push_str("\n## SQL\n\n");
    let compiled = (node.resource_type == "model")
        .then(|| {
            dag.compiled(uid)
                .filter(|c| c.origin == CompiledOrigin::Manifest)
                .map(|c| c.code.as_str())
        })
        .flatten();
    let code = compiled
        .or_else(|| dag.raw_code(uid))
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let Some(code) = code else {
        s.push_str("No SQL.\n");
        return;
    };
    let lang = if detail
        .and_then(|d| d.original_file_path.as_deref())
        .is_some_and(|p| p.ends_with(".py"))
    {
        "python"
    } else {
        "sql"
    };
    let fence = code_fence(code);
    s.push_str("<details><summary>SQL</summary>\n\n");
    s.push_str(&format!("{fence}{lang}\n{code}\n{fence}\n\n</details>\n"));
}

/// The `Lint violations` section — the shared `run_rules` findings for THIS
/// node, in rule order, each labeled with its severity (`(baselined)` marks
/// a finding the baseline file freezes). Rendered only when lint ran
/// (`Some`): same "labeled, never dropped" convention as the other sections.
fn push_lint(s: &mut String, lint: Option<&[&Finding]>) {
    let Some(fs) = lint else { return };
    s.push_str("\n## Lint violations\n\n");
    if fs.is_empty() {
        s.push_str("None.\n");
        return;
    }
    for f in fs {
        let baselined = if f.baselined { " (baselined)" } else { "" };
        s.push_str(&format!(
            "- `{}` ({}): {}{}\n",
            f.rule.name(),
            f.severity.name(),
            f.message,
            baselined
        ));
    }
}

/// The `Upstream dependencies` section: direct parents (named, linked) plus
/// the transitive-ancestor count.
fn push_upstream(
    s: &mut String,
    dag: &Dag,
    uid: &str,
    node: &NodeInfo,
    files: &BTreeMap<String, String>,
    page_rel: &str,
) {
    let parents = direct_neighbours(dag, uid, true);
    let up_total = dag.upstream(uid).len();
    s.push_str("\n## Upstream dependencies\n\n");
    s.push_str(&format!(
        "- direct parents: {}\n- transitive ancestors: {}\n\n",
        node.direct_up, up_total
    ));
    push_dep_list(s, dag, &parents, files, page_rel);
}

/// The `Downstream dependencies` section: direct children plus the
/// transitive-descendant count.
fn push_downstream(
    s: &mut String,
    dag: &Dag,
    uid: &str,
    node: &NodeInfo,
    files: &BTreeMap<String, String>,
    page_rel: &str,
) {
    let children = direct_neighbours(dag, uid, false);
    let down_total = dag.downstream(uid).len();
    s.push_str("\n## Downstream dependencies\n\n");
    s.push_str(&format!(
        "- direct children: {}\n- transitive descendants: {}\n\n",
        node.direct_down, down_total
    ));
    push_dep_list(s, dag, &children, files, page_rel);
}

/// The `Meta` section — the `meta` dict as a key-value table, sorted by key
/// (`NodeDetail.meta` is a `BTreeMap`, so iteration IS the sort). Nested
/// values render their compact-JSON form. Rendered for the kinds whose
/// section set carries it (labeled, never dropped — same convention as the
/// other sections).
fn push_meta(s: &mut String, detail: Option<&dbtl_core::NodeDetail>) {
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
}

/// Which directions a node's lineage diagram covers: `Both` for the
/// buildable kinds (the page lists both dependency sides), `Upstream` for
/// consumers (exposures / semantic resources — the page lists only what it
/// depends on, so the diagram draws only that side too), `Downstream` for
/// the upstream-only kinds (sources / seeds — the page lists what consumes
/// them).
#[derive(Debug, Clone, Copy)]
enum Sides {
    Both,
    Upstream,
    Downstream,
}

impl Sides {
    fn flags(self) -> (bool, bool) {
        match self {
            Sides::Both => (true, true),
            Sides::Upstream => (true, false),
            Sides::Downstream => (false, true),
        }
    }
}

/// The per-node Mermaid lineage: the node's neighbourhood at the configured
/// `depth`, drawn through the shared core (so the id assignment and label
/// escaping match the interactive `m` yank — at the same view the two
/// surfaces produce the same diagram where a yank exists: models, the only
/// lineage-rootable kind). Links resolve from this page to the sibling
/// pages via [`page_link`].
///
/// When the figure would exceed the Mermaid budget, the depth drops to the
/// largest hop count that still fits. A deeper view is always a superset
/// (more nodes, more edges, more characters), so the budget check is
/// monotonic in depth and the search is a binary search — not a linear
/// descent. Depths beyond the graph's own maximum distance all draw the
/// same figure, so the search starts at `min(requested, max_distance)`.
/// A depth-1 figure that is still too big is replaced by the omission line
/// (no figure, no legend — the direct parent/child lists above still carry
/// the neighbourhood).
fn push_node_lineage(
    s: &mut String,
    dag: &Dag,
    uid: &str,
    files: &BTreeMap<String, String>,
    page_rel: &str,
    sides: Sides,
    depth: LineageDepth,
) {
    s.push_str("\n## Lineage\n\n");
    let (upstream, downstream) = sides.flags();
    let href = |rel: &str| page_link(page_rel, rel);
    let render = |depth: Option<usize>| {
        let sg = dag.subgraph_view(uid, upstream, downstream, depth);
        let figure = mermaid_figure(&sg, files, &href);
        (sg, figure)
    };
    // The `m`-yank parity claim only holds where the TUI can root the
    // lineage at this node — models. Other resource kinds can't be the
    // lineage root, so their caption ends after the depth note instead
    // of claiming a comparison that doesn't exist.
    let is_model = dag.get(uid).is_some_and(|n| n.resource_type == "model");
    let caption = |note: &str| {
        if is_model {
            format!("{note} Matches the interactive `m` yank for this node at the same view.\n\n")
        } else {
            format!("{note}\n\n")
        }
    };

    // Attempt at the requested depth first — the common case is one render.
    let requested = depth.hops();
    let (sg, figure) = render(requested);
    let mut failed = (sg.nodes.len(), sg.edges.len());
    if within_mermaid_budget(figure.chars().count(), sg.edges.len()) {
        let note = match depth {
            LineageDepth::All => {
                let dir = match sides {
                    Sides::Both => "upstream + downstream",
                    Sides::Upstream => "upstream",
                    Sides::Downstream => "downstream",
                };
                format!("Full lineage (all {dir} dependencies).")
            }
            LineageDepth::Hops(n) => format!("Lineage within {n} hops."),
        };
        s.push_str(&caption(&note));
        push_figure_and_legend(s, dag, &sg, files, figure, &href);
        return;
    }

    // Over budget — binary-search the largest depth in [1, top) that fits
    // (`top` = the deepest depth that can differ from the requested figure;
    // anything ≥ max_distance draws identically).
    let max_d = max_lineage_distance(dag, uid, upstream, downstream);
    let top = requested.unwrap_or(max_d).min(max_d);
    let (mut lo, mut hi) = (1usize, top.saturating_sub(1));
    let mut best: Option<(usize, Subgraph, String)> = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let (sg, figure) = render(Some(mid));
        if within_mermaid_budget(figure.chars().count(), sg.edges.len()) {
            best = Some((mid, sg, figure));
            lo = mid + 1;
        } else {
            failed = (sg.nodes.len(), sg.edges.len());
            if mid == 1 {
                break;
            }
            hi = mid - 1;
        }
    }
    let Some((drawn, sg, figure)) = best else {
        s.push_str(&omitted_diagram_line(failed.0, failed.1));
        return;
    };
    let orig = match depth {
        LineageDepth::All => "the full lineage".to_string(),
        LineageDepth::Hops(n) => format!("{n} hops"),
    };
    s.push_str(&caption(&format!(
        "Lineage within {drawn} hops (reduced from {orig} — deeper views exceed GitHub's Mermaid limits)."
    )));
    push_figure_and_legend(s, dag, &sg, files, figure, &href);
}

/// The farthest hop count reachable from `uid` through the given directions —
/// the depth at or above which `subgraph_view`'s node set stops growing
/// (needed to skip redundant retries when an `all` figure is over budget).
/// BFS over the 1-hop adjacency; deterministic (only the distance matters).
fn max_lineage_distance(dag: &Dag, uid: &str, upstream: bool, downstream: bool) -> usize {
    let mut seen: HashSet<&str> = HashSet::from([uid]);
    let mut frontier: Vec<&str> = vec![uid];
    let mut depth = 0usize;
    while !frontier.is_empty() {
        let mut next: Vec<&str> = Vec::new();
        for u in &frontier {
            if upstream {
                for v in dag.direct_parents(u) {
                    if seen.insert(v.as_str()) {
                        next.push(v.as_str());
                    }
                }
            }
            if downstream {
                for v in dag.direct_children(u) {
                    if seen.insert(v.as_str()) {
                        next.push(v.as_str());
                    }
                }
            }
        }
        if !next.is_empty() {
            depth += 1;
        }
        frontier = next;
    }
    depth
}
