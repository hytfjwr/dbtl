//! PR-impact analysis over a baseline↔current [`DagDiff`] — the reviewer-shaped
//! "what does this change break" pack, as pure functions of `&Dag` / `&DagDiff`.
//!
//! This module is the SINGLE source of that analysis for every surface: the
//! `D` modal snapshot ([`DiffView`] in `dbtl-tui` is the same data plus a scroll
//! offset), the `D`→`e` Markdown export, the `dbtl impact` stdout report
//! (Markdown or JSON), and the `dbtl check` CI gate — so the screen, the file,
//! the comment, and the exit code can never disagree. Deterministic by
//! construction: `BTreeSet`/`BTreeMap` keyed sets, sorted `Vec` listings.

use std::collections::{BTreeSet, HashSet};

use serde::Serialize;

use crate::layer::{first_dir, layer_rank};
use crate::{coverage_gap, is_exposure, Dag, DagDiff, ExposureInfo, PackageScope};

/// A changed node whose transitive downstream (buildable) closure is at least
/// this large is flagged as a "hub" risk in the PR impact pack — changing it
/// ripples widely. A pragmatic default: large enough to skip the long tail of
/// leaf models, small enough to surface real fan-out.
pub const PR_HUB_MIN_DOWNSTREAM: usize = 5;

/// The reviewer-shaped "PR Impact Pack" over the `--diff` changed set: the
/// aggregate blast radius of the changed-node set, the affected marts /
/// exposures, a suggested CI selector, and the risk flags. `Serialize` so
/// `dbtl impact --format json` can emit it verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrImpact {
    /// Total affected models: the union of the changed set's downstream model
    /// closures, with the changed nodes themselves excluded.
    pub affected_models: usize,
    /// The marts (downstream models in the `marts` layer) within that blast
    /// radius, by name, sorted.
    pub affected_marts: Vec<String>,
    /// Downstream exposures of the changed set as display strings
    /// (`"name (kind, owner: …)"`), sorted, deduped — the "who cares" line.
    pub affected_exposures: Vec<String>,
    /// The distinct `meta` values (default key `owner`) of the affected
    /// downstream MODELS — "who to page" for the blast radius, sorted.
    pub affected_owners: Vec<String>,
    /// The suggested `dbt build --select a+ b+ …` command over the changed
    /// buildable set, or `None` when nothing buildable changed.
    pub ci_command: Option<String>,
    /// Risk: changed nodes that carry no tests (the [`coverage_gap`]
    /// predicate), by name, sorted.
    pub untested_changes: Vec<String>,
    /// Risk: changed "hub" nodes whose transitive downstream (buildable)
    /// closure is at or above [`PR_HUB_MIN_DOWNSTREAM`], as `(name, count)`
    /// sorted desc by count then name.
    pub changed_hubs: Vec<(String, usize)>,
    /// Risk: layer-violation edges present now but NOT in the baseline, as
    /// `(parent, child)` names, sorted.
    pub new_layer_violations: Vec<(String, String)>,
}

/// The name-resolved diff listings + the [`PrImpact`] analysis — the data
/// payload every readout shares. The TUI's `DiffView` modal payload is this
/// same data plus a scroll offset; `dbtl impact --format json` emits it
/// verbatim. `added`/`modified` are re-sorted by NAME (not `unique_id`)
/// because the listings name nodes, not uids.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DiffReport {
    /// Where the baseline came from (the `--diff` argument), for the title.
    pub baseline: String,
    /// The package scope both graphs were loaded under (`--include-packages`
    /// keeps installed-package nodes in the graphs; the risk metrics inside
    /// `pr` still count the root project only under either scope). Serialized
    /// `"root"` / `"all"` — the first thing the report header prints, so a
    /// pasted pack can never be misread as project-wide.
    pub scope: PackageScope,
    /// Added nodes as `(name, resource_type)`, sorted by name.
    pub added: Vec<(String, String)>,
    /// Removed (baseline-only) nodes as `(name, resource_type)`, sorted.
    pub removed: Vec<(String, String)>,
    /// Modified nodes as `(name, joined reasons)`, sorted by name.
    pub modified: Vec<(String, String)>,
    /// Dependency edges present only in the current Dag, `(parent, child)` names.
    pub edges_added: Vec<(String, String)>,
    /// Dependency edges present only in the baseline, same shape.
    pub edges_removed: Vec<(String, String)>,
    /// The reviewer-shaped PR impact analysis over the changed set.
    pub pr: PrImpact,
}

/// Build the shared [`DiffReport`] payload: names resolved from the CURRENT
/// `dag`, every listing sorted. `base` is the baseline `Dag` the `diff` was
/// computed from (needed for the new-layer-violations difference); `baseline`
/// is its display label (the `--diff` argument). `scope` is the package scope
/// both graphs were loaded under — carried onto the report verbatim.
/// `owner_key` is the `meta` key the affected-owners set reads (`"owner"` by
/// default).
pub fn diff_report(
    dag: &Dag,
    base: Option<&Dag>,
    diff: &DagDiff,
    baseline: &str,
    scope: PackageScope,
    owner_key: &str,
) -> DiffReport {
    let pair = |uid: &str| {
        dag.get(uid)
            .map(|n| (n.name.clone(), n.resource_type.clone()))
            .unwrap_or_else(|| (uid.to_string(), String::new()))
    };
    let mut added: Vec<(String, String)> = diff.added.iter().map(|uid| pair(uid)).collect();
    added.sort();
    let mut modified: Vec<(String, String)> = diff
        .modified
        .iter()
        .map(|(uid, reasons)| (pair(uid).0, reasons.join("; ")))
        .collect();
    modified.sort();
    DiffReport {
        baseline: baseline.to_string(),
        scope,
        added,
        removed: diff.removed.clone(),
        modified,
        edges_added: diff.edges_added.clone(),
        edges_removed: diff.edges_removed.clone(),
        pr: pr_impact(dag, base, diff, owner_key),
    }
}

/// The PR Impact Pack over the changed set of a [`DagDiff`]: blast radius,
/// affected marts/exposures, suggested CI command, and risk flags.
///
/// - `dag`: the CURRENT graph (names, closures, tests, lens predicates).
/// - `base`: the baseline graph the `diff` was computed from, for the
///   "newly introduced" layer-violation difference; `None` → every current
///   violation counts as new.
/// - `diff`: the changed set (added ∪ modified; identity = `unique_id`).
/// - `owner_key`: the `meta` key read for [`PrImpact::affected_owners`]
///   (`"owner"` by default; made explicit so the TUI's `config.yml` and the
///   CLI can't disagree).
///
/// Deterministic: the changed set is sorted; closures come from `HashSet`s
/// but every emitted listing is sorted; the new-violation set is a `BTreeSet`
/// difference.
pub fn pr_impact(dag: &Dag, base: Option<&Dag>, diff: &DagDiff, owner_key: &str) -> PrImpact {
    // The changed BUILDABLE set: added ∪ modified, restricted to nodes that
    // still exist in the current Dag and that `dbt build` can target
    // (model / seed / snapshot). Removed nodes are gone — not buildable, not
    // traversable; added/modified exposures aren't buildable either.
    let mut changed: Vec<String> = diff
        .added
        .iter()
        .chain(diff.modified.keys())
        .filter(|uid| {
            dag.get(uid)
                .is_some_and(|n| matches!(n.resource_type.as_str(), "model" | "seed" | "snapshot"))
        })
        .cloned()
        .collect();
    changed.sort();
    changed.dedup();
    let changed_set: HashSet<&str> = changed.iter().map(String::as_str).collect();

    // The union of every changed node's downstream closure — the aggregate
    // blast radius. Split into affected models (with the marts subset) and
    // affected exposures; the changed nodes themselves are excluded. Under
    // the `All` package scope the closures include installed-package nodes,
    // but the counts below stay scoped to the root project — an installed
    // package's own consumers are dependency context, never YOUR blast
    // radius (the same rule `coverage_counts`/`dbtl check` share).
    let mut down_uids: HashSet<String> = HashSet::new();
    for uid in &changed {
        down_uids.extend(dag.downstream(uid));
    }
    let mut affected_models = 0usize;
    let mut affected_marts: Vec<String> = Vec::new();
    let mut affected_owners: BTreeSet<String> = BTreeSet::new();
    let mut exposure_uids: Vec<String> = Vec::new();
    for uid in &down_uids {
        if changed_set.contains(uid.as_str()) {
            continue;
        }
        let Some(n) = dag.get(uid) else { continue };
        if !dag.is_root_project(n) {
            continue;
        }
        if is_exposure(n) {
            exposure_uids.push(uid.clone());
        } else if n.resource_type == "model" {
            affected_models += 1;
            if first_dir(n) == Some("marts") {
                affected_marts.push(n.name.clone());
            }
            // The affected-owner set collects the SAME model population
            // `affected_models` counts — downstream models only, the changed
            // set excluded.
            if let Some(owner) = dag.detail(uid).and_then(|d| d.meta.get(owner_key)) {
                affected_owners.insert(owner.clone());
            }
        }
    }
    affected_marts.sort();
    exposure_uids.sort();
    exposure_uids.dedup();
    let mut affected_exposures: Vec<String> = exposure_uids
        .iter()
        .filter_map(|uid| dag.get(uid).map(|n| (n, dag.exposure(uid))))
        .map(|(n, payload)| format!("{}{}", n.name, exposure_note(payload)))
        .collect();
    affected_exposures.sort();

    // Suggested CI command: each changed node with a `+` (downstream)
    // selector — `dbt build --select a+ b+ …`, the manual equivalent of
    // `state:modified+`. `None` when nothing buildable changed.
    let ci_command = if changed.is_empty() {
        None
    } else {
        let mut names: Vec<String> = changed
            .iter()
            .filter_map(|uid| dag.get(uid).map(|n| n.name.clone()))
            .collect();
        names.sort();
        names.dedup();
        let selectors: Vec<String> = names.iter().map(|n| format!("{n}+")).collect();
        Some(format!("dbt build --select {}", selectors.join(" ")))
    };

    // Risk: changed nodes that carry no tests (the coverage_gap predicate) —
    // scoped to the root project: an installed package's own test gaps are
    // its maintainers' problem, not a risk flag in YOUR diff.
    let mut untested_changes: Vec<String> = changed
        .iter()
        .filter_map(|uid| dag.get(uid))
        .filter(|n| dag.is_root_project(n) && coverage_gap(n))
        .map(|n| n.name.clone())
        .collect();
    untested_changes.sort();

    // Risk: changed hubs (wide downstream fan-out). The buildable-downstream
    // count (exposures excluded) is the blast radius — kept graph-wide, since
    // installed-package consumers DO get hit; only the flagged membership is
    // root-scoped (a changed package node is context, never your hub risk).
    let mut changed_hubs: Vec<(String, usize)> = changed
        .iter()
        .filter_map(|uid| {
            let down = dag
                .downstream(uid)
                .iter()
                .filter(|id| dag.get(id).is_some_and(|n| !is_exposure(n)))
                .count();
            (down >= PR_HUB_MIN_DOWNSTREAM)
                .then(|| {
                    dag.get(uid)
                        .filter(|n| dag.is_root_project(n))
                        .map(|n| (n.name.clone(), down))
                })
                .flatten()
        })
        .collect();
    changed_hubs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // Risk: layer-violation edges present now but NOT in the baseline. Both
    // sides are uid pairs (identity = unique_id), so the difference is a true
    // "newly introduced" set; resolve names from the current Dag for display.
    let current_v: BTreeSet<(String, String)> = layer_violation_edges(dag).into_iter().collect();
    let base_v: BTreeSet<(String, String)> = base
        .map(|b| layer_violation_edges(b).into_iter().collect())
        .unwrap_or_default();
    let mut new_layer_violations: Vec<(String, String)> = current_v
        .difference(&base_v)
        .map(|(p, c)| {
            let pn = dag.get(p).map_or_else(|| p.clone(), |n| n.name.clone());
            let cn = dag.get(c).map_or_else(|| c.clone(), |n| n.name.clone());
            (pn, cn)
        })
        .collect();
    new_layer_violations.sort();

    PrImpact {
        affected_models,
        affected_marts,
        affected_exposures,
        affected_owners: affected_owners.into_iter().collect(),
        ci_command,
        untested_changes,
        changed_hubs,
        new_layer_violations,
    }
}

/// All kept 1-hop `(parent, child)` edges where BOTH endpoints are `model`
/// nodes and the parent's layer ranks LATER than the child's
/// (e.g. a `staging` model fed by a `marts` model — upstream data flows the
/// wrong way down the layer taxonomy). Sorted via `Dag::edges()`. The single
/// violation predicate: the `LayerViolation` lens, the stats worklist, the PR
/// impact pack's "new violations" risk, and `dbtl check` all read it.
/// Layer-violation edges are scoped to the root project the same way the
/// other metrics are: the violating CHILD must be yours. An edge between two
/// installed-package nodes is dependency context, not your project's health.
pub fn layer_violation_edges(dag: &Dag) -> Vec<(String, String)> {
    dag.edges()
        .into_iter()
        .filter(|(parent, child)| {
            let (Some(p), Some(c)) = (dag.get(parent), dag.get(child)) else {
                return false;
            };
            if p.resource_type != "model" || c.resource_type != "model" {
                return false;
            }
            if !dag.is_root_project(c) {
                return false;
            }
            let pr = layer_rank(first_dir(p).unwrap_or(""));
            let cr = layer_rank(first_dir(c).unwrap_or(""));
            pr > cr
        })
        .collect()
}

/// The `(tested, total)` coverage pair over buildable nodes
/// (model / snapshot / seed): `total` counts them all, `tested` counts those
/// with at least one test — the ONE coverage source shared by the status `cov`
/// segment, the `t` lens, the stats dashboard, `dbtl-docs`' summary, and
/// `dbtl check --coverage-min`.
///
/// Always counted over the ROOT PROJECT ONLY ([`Dag::is_root_project`]): under
/// the `All` package scope installed-package nodes exist in the graph but are
/// never part of your project's test coverage. Under the default `Root` scope
/// (or a single-package manifest) the predicate passes every node, so this is
/// the exact count it always was.
pub fn coverage_counts(dag: &Dag) -> (usize, usize) {
    let mut total = 0;
    let mut tested = 0;
    for n in dag.nodes().values() {
        if !dag.is_root_project(n) {
            continue;
        }
        if matches!(n.resource_type.as_str(), "model" | "snapshot" | "seed") {
            total += 1;
            if n.test_count > 0 {
                tested += 1;
            }
        }
    }
    (tested, total)
}

/// The parenthesised kind/owner suffix for one exposure line:
/// `" (dashboard, owner: Finance <fin@example.com>)"`. Each part is optional;
/// the kind falls back to the bare `exposure` so the line always names what it
/// is. Owner shows `name <email>`, either half standing alone when the other
/// is missing. The ONE exposure-line rule: the impact report, the PR impact
/// pack's affected-exposure lines, and `dbtl impact` all format through it.
pub fn exposure_note(payload: Option<&ExposureInfo>) -> String {
    let kind = payload
        .and_then(|p| p.exposure_type.as_deref())
        .unwrap_or("exposure");
    let owner = payload.map_or_else(String::new, |p| {
        match (p.owner_name.as_deref(), p.owner_email.as_deref()) {
            (Some(n), Some(e)) => format!(", owner: {n} <{e}>"),
            (Some(n), None) => format!(", owner: {n}"),
            (None, Some(e)) => format!(", owner: <{e}>"),
            (None, None) => String::new(),
        }
    });
    format!(" ({kind}{owner})")
}

/// The PR Impact Report as Markdown — the file `D`→`e` writes and the stdout
/// `dbtl impact` prints. A pure formatter over the [`DiffReport`] payload (all
/// names already resolved and sorted) plus the project name for the title, so
/// the export is byte-identical to what the modal renders and re-emitting the
/// same diff is deterministic. Empty sections are omitted so the report stays
/// tight.
pub fn pr_impact_markdown(project: &str, report: &DiffReport) -> String {
    let pr = &report.pr;
    let mut out = format!("# PR Impact Report: {project}\n\n");
    out.push_str(&format!("- baseline: `{}`\n", report.baseline));
    out.push_str(&format!("- scope: {}\n", report.scope.describe()));
    out.push_str(&format!(
        "- changed: +{} added, ~{} modified, -{} removed\n",
        report.added.len(),
        report.modified.len(),
        report.removed.len()
    ));
    out.push_str(&format!(
        "- edges: +{} / -{}\n",
        report.edges_added.len(),
        report.edges_removed.len()
    ));
    out.push_str(&format!("- affected models: {}\n", pr.affected_models));
    if !pr.affected_marts.is_empty() {
        out.push_str(&format!("- affected marts: {}\n", pr.affected_marts.len()));
    }
    if !pr.affected_exposures.is_empty() {
        out.push_str(&format!(
            "- affected exposures: {}\n",
            pr.affected_exposures.len()
        ));
    }
    if !pr.affected_owners.is_empty() {
        out.push_str(&format!(
            "- affected owners: {}\n",
            pr.affected_owners.len()
        ));
    }

    // The changed-node listings, mirroring the `D` modal sections. A list of
    // `(title, rows)`; empty sections are skipped (a clean diff prints none).
    let node_row = |name: &str, kind: &str| {
        if kind.is_empty() {
            format!("- {name}")
        } else {
            format!("- {name} ({kind})")
        }
    };
    let sections: [(&str, Vec<String>); 5] = [
        (
            "Added",
            report.added.iter().map(|(n, k)| node_row(n, k)).collect(),
        ),
        (
            "Modified",
            report
                .modified
                .iter()
                .map(|(n, reasons)| format!("- {n}: {reasons}"))
                .collect(),
        ),
        (
            "Removed",
            report.removed.iter().map(|(n, k)| node_row(n, k)).collect(),
        ),
        (
            "Edges added",
            report
                .edges_added
                .iter()
                .map(|(p, c)| format!("- {p} -> {c}"))
                .collect(),
        ),
        (
            "Edges removed",
            report
                .edges_removed
                .iter()
                .map(|(p, c)| format!("- {p} -> {c}"))
                .collect(),
        ),
    ];
    for (title, rows) in &sections {
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {title} ({})\n", rows.len()));
        for row in rows {
            out.push_str(row);
            out.push('\n');
        }
    }

    out.push_str(&format!(
        "\n## Blast radius ({} affected models)\n",
        pr.affected_models
    ));
    if pr.affected_marts.is_empty() {
        out.push_str("- no affected marts\n");
    } else {
        out.push_str(&format!(
            "\n### Affected marts ({})\n",
            pr.affected_marts.len()
        ));
        for name in &pr.affected_marts {
            out.push_str(&format!("- {name}\n"));
        }
    }

    if !pr.affected_exposures.is_empty() {
        out.push_str(&format!(
            "\n## Affected exposures ({})\n",
            pr.affected_exposures.len()
        ));
        for e in &pr.affected_exposures {
            out.push_str(&format!("- {e}\n"));
        }
    }

    if !pr.affected_owners.is_empty() {
        out.push_str(&format!(
            "\n## Affected owners ({})\n",
            pr.affected_owners.len()
        ));
        for o in &pr.affected_owners {
            out.push_str(&format!("- {o}\n"));
        }
    }

    out.push_str("\n## Suggested CI command\n");
    match &pr.ci_command {
        Some(cmd) => out.push_str(&format!("```sh\n{cmd}\n```\n")),
        None => out.push_str("- no buildable changes\n"),
    }

    // Risk flags — each subsection only when it has entries.
    let has_risk = !pr.untested_changes.is_empty()
        || !pr.changed_hubs.is_empty()
        || !pr.new_layer_violations.is_empty();
    if has_risk {
        out.push_str("\n## Risk flags\n");
        if !pr.untested_changes.is_empty() {
            out.push_str(&format!(
                "\n### Untested changes ({})\n",
                pr.untested_changes.len()
            ));
            for name in &pr.untested_changes {
                out.push_str(&format!("- {name}\n"));
            }
        }
        if !pr.changed_hubs.is_empty() {
            out.push_str(&format!("\n### Changed hubs ({})\n", pr.changed_hubs.len()));
            for (name, count) in &pr.changed_hubs {
                out.push_str(&format!("- {name} ({count} downstream)\n"));
            }
        }
        if !pr.new_layer_violations.is_empty() {
            out.push_str(&format!(
                "\n### New layer violations ({})\n",
                pr.new_layer_violations.len()
            ));
            for (p, c) in &pr.new_layer_violations {
                out.push_str(&format!("- {p} -> {c}\n"));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RawManifest, RawNode};
    use crate::{compute_diff, Dag};
    use std::collections::HashMap;

    /// A two-model manifest `stg_x -> fct_x` (staging feeding a mart — the
    /// legal direction), with `x`'s SQL and tests parameterized.
    fn two_model_manifest() -> RawManifest {
        let mut nodes = HashMap::from([
            (
                "model.p.stg_x".to_string(),
                RawNode {
                    name: "stg_x".into(),
                    resource_type: "model".into(),
                    path: Some("staging/stg_x.sql".into()),
                    raw_code: Some("select 1".into()),
                    ..Default::default()
                },
            ),
            (
                "model.p.fct_x".to_string(),
                RawNode {
                    name: "fct_x".into(),
                    resource_type: "model".into(),
                    path: Some("marts/fct_x.sql".into()),
                    raw_code: Some("select 2".into()),
                    ..Default::default()
                },
            ),
        ]);
        nodes.get_mut("model.p.fct_x").unwrap().depends_on.nodes =
            vec!["model.p.stg_x".to_string()];
        RawManifest {
            metadata: Default::default(),
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),

            parent_map: HashMap::from([(
                "model.p.fct_x".to_string(),
                vec!["model.p.stg_x".to_string()],
            )]),
            child_map: HashMap::from([(
                "model.p.stg_x".to_string(),
                vec!["model.p.fct_x".to_string()],
            )]),
        }
    }

    /// `stg_x` (owner=carol, CHANGED) feeding two downstream models owned by
    /// alice and bob — the affected-owners population.
    fn owner_manifest() -> RawManifest {
        let model = |name: &str, path: &str, owner: Option<&str>| {
            (
                format!("model.p.{name}"),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    path: Some(path.into()),
                    raw_code: Some("select 1".into()),
                    meta: owner
                        .map(|o| {
                            serde_json::Map::from_iter([(
                                "owner".to_string(),
                                serde_json::Value::from(o),
                            )])
                        })
                        .unwrap_or_default(),
                    ..Default::default()
                },
            )
        };
        let mut nodes = HashMap::from([
            model("stg_x", "staging/stg_x.sql", Some("carol")),
            model("fct_x", "marts/fct_x.sql", Some("alice")),
            model("mart_b", "marts/mart_b.sql", Some("bob")),
            model("orphan", "marts/orphan.sql", Some("mallory")),
        ]);
        // stg_x → fct_x → mart_b; orphan is downstream of nothing.
        nodes.get_mut("model.p.fct_x").unwrap().depends_on.nodes =
            vec!["model.p.stg_x".to_string()];
        nodes.get_mut("model.p.mart_b").unwrap().depends_on.nodes =
            vec!["model.p.fct_x".to_string()];
        RawManifest {
            metadata: Default::default(),
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map: HashMap::from([
                (
                    "model.p.fct_x".to_string(),
                    vec!["model.p.stg_x".to_string()],
                ),
                (
                    "model.p.mart_b".to_string(),
                    vec!["model.p.fct_x".to_string()],
                ),
            ]),
            child_map: HashMap::from([
                (
                    "model.p.stg_x".to_string(),
                    vec!["model.p.fct_x".to_string()],
                ),
                (
                    "model.p.fct_x".to_string(),
                    vec!["model.p.mart_b".to_string()],
                ),
            ]),
        }
    }

    #[test]
    fn affected_owners_collects_distinct_downstream_owners() {
        let base = Dag::build(&owner_manifest());
        // Change stg_x's SQL → modified; fct_x + mart_b are its downstream.
        let mut m = owner_manifest();
        m.nodes.get_mut("model.p.stg_x").unwrap().raw_code = Some("select 2".into());
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        let pr = pr_impact(&current, Some(&base), &diff, "owner");
        assert_eq!(
            pr.affected_owners,
            vec!["alice".to_string(), "bob".to_string()],
            "distinct downstream owners, sorted — the changed node's own owner \
             (carol) and the untouched orphan's (mallory) are both excluded"
        );
        // A different `owner_key` reads a different meta key (here: nothing).
        let pr = pr_impact(&current, Some(&base), &diff, "team");
        assert!(pr.affected_owners.is_empty());
        // The markdown section lists them.
        let report = diff_report(&current, Some(&base), &diff, "b", "owner");
        let md = pr_impact_markdown("p", &report);
        assert!(md.contains("## Affected owners (2)\n- alice\n- bob\n"));
    }

    #[test]
    fn pr_impact_reports_blast_radius_ci_and_untested_changes() {
        let base = Dag::build(&two_model_manifest());
        // Current: same graph, but fct_x's SQL changed → modified, buildable,
        // and (no tests on it) an untested change.
        let mut m = two_model_manifest();
        m.nodes.get_mut("model.p.fct_x").unwrap().raw_code = Some("select 22".into());
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        let pr = pr_impact(&current, Some(&base), &diff, "owner");

        assert_eq!(pr.untested_changes, vec!["fct_x".to_string()]);
        assert_eq!(pr.ci_command.as_deref(), Some("dbt build --select fct_x+"));
        assert_eq!(pr.affected_models, 0, "fct_x has no downstream");
        assert!(pr.new_layer_violations.is_empty());
    }

    #[test]
    fn new_layer_violation_is_diffed_against_the_baseline() {
        let base = Dag::build(&two_model_manifest());
        // Flip the edge: fct_x (marts) now feeds stg_x (staging) — a violation.
        let mut m = two_model_manifest();
        m.parent_map = HashMap::from([(
            "model.p.stg_x".to_string(),
            vec!["model.p.fct_x".to_string()],
        )]);
        m.child_map = HashMap::from([(
            "model.p.fct_x".to_string(),
            vec!["model.p.stg_x".to_string()],
        )]);
        m.nodes.get_mut("model.p.stg_x").unwrap().depends_on.nodes =
            vec!["model.p.fct_x".to_string()];
        m.nodes.get_mut("model.p.fct_x").unwrap().depends_on.nodes = vec![];
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        let pr = pr_impact(&current, Some(&base), &diff, "owner");
        assert_eq!(
            pr.new_layer_violations,
            vec![("fct_x".to_string(), "stg_x".to_string())],
            "the flipped edge is a NEW violation vs the legal baseline"
        );
        // And the same violation present in BOTH sides is not "new".
        let pr = pr_impact(
            &current,
            Some(&current),
            &compute_diff(&current, &current),
            "owner",
        );
        assert!(pr.new_layer_violations.is_empty());
    }

    #[test]
    fn coverage_counts_matches_the_buildable_base() {
        let dag = Dag::build(&two_model_manifest());
        assert_eq!(coverage_counts(&dag), (0, 2), "two models, no tests");
    }

    #[test]
    fn diff_report_and_markdown_are_deterministic() {
        let base = Dag::build(&two_model_manifest());
        let mut m = two_model_manifest();
        m.nodes.get_mut("model.p.fct_x").unwrap().raw_code = Some("select 22".into());
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        let r1 = diff_report(&current, Some(&base), &diff, "base", PackageScope::Root, "owner");
        let r2 = diff_report(&current, Some(&base), &diff, "base", PackageScope::Root, "owner");
        assert_eq!(r1, r2);
        let md = pr_impact_markdown("p", &r1);
        assert!(md.contains("# PR Impact Report: p"));
        assert!(
            md.contains("- scope: root project only"),
            "the scope line leads the report, got: {md}"
        );
        assert!(md.contains("Modified"), "changed node listed");
        assert!(md.contains("fct_x"));
        // JSON shape used by `dbtl impact --format json`.
        let json = serde_json::to_string(&r1).expect("serializes");
        assert!(json.contains("\"affected_models\""));
    }
}
