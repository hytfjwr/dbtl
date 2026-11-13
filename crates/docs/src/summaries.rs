use super::*;

/// Append the README's test-coverage + orphan summary, reusing the TUI's
/// predicates so the numbers can never drift from the `t` lens / `cov` status
/// segment / stats dashboard:
/// - coverage % = the share of TESTABLE resources (model/snapshot/seed) that
///   carry at least one test — the ONE source [`coverage_counts`], the same
///   base `App::coverage_summary` / `dbtl check` quote;
/// - orphan models = models with `direct_down == 0` (no downstream consumer),
///   the same predicate the stats dashboard's `zero_downstream` count uses.
///
/// Both are counted over the ROOT PROJECT ONLY ([`Dag::is_root_project`]):
/// under `--include-packages` installed-package nodes are documented as
/// dependency context but are never part of the project's health numbers.
/// Both are order-independent / sorted, so the section is deterministic.
pub(super) fn push_stats_summary(s: &mut String, dag: &Dag, files: &BTreeMap<String, String>) {
    let (tested, testable) = coverage_counts(dag, None);
    // Column-level coverage: the SAME `column_coverage_counts` the
    // stats dashboard's "Column coverage" gauge quotes — declared columns
    // carrying ≥1 test over the root-project testable set.
    let (col_tested, col_declared) = column_coverage_counts(dag, None);
    // Resolve the owning package ONCE for the orphan sweep — `is_root_project`
    // re-resolves per call.
    let primary = dag.primary_package();
    let mut orphans: Vec<&NodeInfo> = dag
        .nodes()
        .values()
        .filter(|node| {
            primary.as_deref().is_none_or(|p| in_package(node, p))
                && node.resource_type == "model"
                && node.direct_down == 0
        })
        .collect();
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
    // Declared columns carrying at least one test — the integer-percent
    // `checked_div` convention matches the resource coverage above.
    if let Some(pct) = (col_tested * 100).checked_div(col_declared) {
        s.push_str(&format!(
            "- columns tested: {col_tested} / {col_declared} declared columns ({pct}%)\n"
        ));
    }

    // The per-layer table — root-project models bucketed by the SAME
    // taxonomy the split lineage uses (`model_layer_groups` IS the model
    // half of `layer_groups`), each row counted through the one
    // `coverage_counts` base over that bucket's uid set.
    push_layer_coverage(s, dag);

    // Documentation coverage sits directly after Test coverage — the SAME
    // shared predicates the coverage gate quotes (`doc_coverage_counts` /
    // `column_doc_coverage_counts` over `model_documented` /
    // `column_documented`), never a second implementation.
    push_doc_coverage(s, dag);

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
            let name = list_name(&node.unique_id, &node.name);
            match files.get(&node.unique_id) {
                Some(rel) => s.push_str(&format!(
                    "- [{}]({})\n",
                    md_label(&name),
                    page_link("README.md", rel)
                )),
                None => s.push_str(&format!("- {name}\n")),
            }
        }
    }
}

/// Append the README's static-lint summary — the shared
/// [`run_rules`](dbtl_core::run_rules) findings `dbtl check --lint` gates on
/// and the TUI's Lint lens/worklist quote, so every surface reports the same
/// numbers. Rendered only when lint ran (`Some`); a clean run is still stated
/// ("No violations."), an absent run adds nothing.
///
/// Layout: a rule → severity + count table in registry order (violating
/// rules only), then a per-rule list of the violating nodes, each linked to
/// its page — deterministic by construction (rules in eval order, findings
/// in sorted `unique_id` order). A finding the baseline file freezes carries
/// a `(baselined)` suffix, and the totals line counts them.
pub(super) fn push_lint_summary(
    s: &mut String,
    dag: &Dag,
    files: &BTreeMap<String, String>,
    lint: Option<&[Finding]>,
) {
    let Some(findings) = lint else { return };
    s.push_str("\n## Lint\n\n");
    s.push_str(
        "Static structure checks (`dbtl check --lint` gates on the same findings; \
         rules/thresholds come from `.dbtl.yml`):\n\n",
    );
    if findings.is_empty() {
        s.push_str("No violations.\n");
        return;
    }
    s.push_str("| Rule | Severity | Findings |\n");
    s.push_str("| --- | --- | --- |\n");
    // Registry order, not ALL order — the rule registry is the ordering
    // contract for every lint surface (the two agree by construction).
    for info in dbtl_core::lint::REGISTRY {
        let fs: Vec<&Finding> = findings.iter().filter(|f| f.rule == info.rule).collect();
        if !fs.is_empty() {
            s.push_str(&format!(
                "| `{}` | {} | {} |\n",
                info.rule.name(),
                fs[0].severity.name(),
                fs.len()
            ));
        }
    }
    let baselined = findings.iter().filter(|f| f.baselined).count();
    let baselined_note = if baselined > 0 {
        format!(" ({baselined} baselined)")
    } else {
        String::new()
    };
    s.push_str(&format!(
        "\n{} finding(s) across {} rule(s){}.\n",
        findings.len(),
        dbtl_core::lint::REGISTRY
            .iter()
            .filter(|i| findings.iter().any(|f| f.rule == i.rule))
            .count(),
        baselined_note
    ));
    for info in dbtl_core::lint::REGISTRY {
        let fs: Vec<&Finding> = findings.iter().filter(|f| f.rule == info.rule).collect();
        if fs.is_empty() {
            continue;
        }
        s.push_str(&format!("\n### `{}`\n\n", info.rule.name()));
        for f in fs {
            let name = dag
                .get(&f.node_uid)
                .map(|n| list_name(&f.node_uid, &n.name))
                .unwrap_or_else(|| f.node_uid.clone());
            let baselined = if f.baselined { " (baselined)" } else { "" };
            match files.get(&f.node_uid) {
                Some(rel) => s.push_str(&format!(
                    "- [{}]({}) — {}{}\n",
                    md_label(&name),
                    page_link("README.md", rel),
                    f.message,
                    baselined
                )),
                None => s.push_str(&format!("- {name} — {}{}\n", f.message, baselined)),
            }
        }
    }
}

/// The per-layer test-coverage table inside `## Test coverage`: one row per
/// populated layer bucket ([`model_layer_groups`] — the same classification
/// the split lineage uses, so the two never disagree about which bucket a
/// model belongs to), counted by the shared [`coverage_counts`] over that
/// bucket's uid set, and a `Total (models)` row over the whole model set.
/// Seeds and snapshots are counted in the overall coverage numbers above but
/// are not split by layer — a trailing line says so.
fn push_layer_coverage(s: &mut String, dag: &Dag) {
    s.push_str("\n| Layer | Models | Tested | Coverage |\n");
    s.push_str("| --- | --- | --- | --- |\n");
    let (mut models, mut tested_models) = (0usize, 0usize);
    for group in model_layer_groups(dag) {
        let uids: BTreeSet<String> = group.uids.into_iter().collect();
        let (tested, total) = coverage_counts(dag, Some(&uids));
        models += total;
        tested_models += tested;
        // Rows always carry ≥1 model — `checked_div` stays a guard, never a
        // real branch (the `Total` row is the one that can be 0).
        let pct = (tested * 100)
            .checked_div(total)
            .map(|p| format!("{p}%"))
            .unwrap_or_else(|| "-".to_string());
        s.push_str(&format!(
            "| {} | {total} | {tested} | {pct} |\n",
            group.label
        ));
    }
    let pct = (tested_models * 100)
        .checked_div(models)
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "-".to_string());
    s.push_str(&format!(
        "| Total (models) | {models} | {tested_models} | {pct} |\n"
    ));
    s.push_str(
        "\nSeeds and snapshots are included in the totals above but are not split by layer.\n",
    );
}

/// `## Documentation coverage` — the share of root-project models carrying
/// a non-empty description, plus declared columns carrying one — counted by
/// the shared [`doc_coverage_counts`] / [`column_doc_coverage_counts`]
/// predicates so the section counts exactly what the docs coverage gate
/// would. Same truncated-integer `checked_div` percent convention as Test
/// coverage.
fn push_doc_coverage(s: &mut String, dag: &Dag) {
    let (documented, total) = doc_coverage_counts(dag, None);
    let (col_documented, col_declared) = column_doc_coverage_counts(dag, None);
    s.push_str("\n## Documentation coverage\n\n");
    match (documented * 100).checked_div(total) {
        None => s.push_str("- documentable models: 0\n"),
        Some(pct) => s.push_str(&format!(
            "- models documented: {documented} / {total} models ({pct}%)\n"
        )),
    }
    if let Some(pct) = (col_documented * 100).checked_div(col_declared) {
        s.push_str(&format!(
            "- columns documented: {col_documented} / {col_declared} declared columns ({pct}%)\n"
        ));
    }
}
