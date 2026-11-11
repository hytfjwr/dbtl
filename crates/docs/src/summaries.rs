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
    let (tested, testable) = coverage_counts(dag);
    // Column-level coverage: the SAME `column_coverage_counts` the
    // stats dashboard's "Column coverage" gauge quotes — declared columns
    // carrying ≥1 test over the root-project testable set.
    let (col_tested, col_declared) = column_coverage_counts(dag);
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
                Some(rel) => s.push_str(&format!(
                    "- [{}]({})\n",
                    node.name,
                    page_link("README.md", rel)
                )),
                None => s.push_str(&format!("- {}\n", node.name)),
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
                .map(|n| n.name.as_str())
                .unwrap_or(&f.node_uid);
            let baselined = if f.baselined { " (baselined)" } else { "" };
            match files.get(&f.node_uid) {
                Some(rel) => s.push_str(&format!(
                    "- [{}]({}) — {}{}\n",
                    name,
                    page_link("README.md", rel),
                    f.message,
                    baselined
                )),
                None => s.push_str(&format!("- {name} — {}{}\n", f.message, baselined)),
            }
        }
    }
}
