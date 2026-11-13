//! The PR-impact aggregation: [`pr_impact`] computes the blast radius,
//! affected marts/exposures/owners, suggested CI selectors, and risk flags
//! over the changed set. The two shared primitives it composes live here
//! too: [`layer_violation_edges`] (the single violation predicate the lens,
//! the stats worklist, and `dbtl check` all read) and [`exposure_note`] (the
//! one exposure-line formatting rule).

use std::collections::{BTreeSet, HashSet};

use super::report::{ImpactOpts, PrImpact};
use crate::dag::testable;
use crate::layer::{first_dir, layer_rank, VIOLATION_LAYERS};
use crate::{
    coverage_gap, in_package, is_exposure, list_name, ColumnChangeClass, Dag, DagDiff,
    ExposureInfo, NodeInfo,
};

/// The PR Impact Pack over the changed set of a [`DagDiff`]: blast radius,
/// affected marts/exposures, suggested CI command, and risk flags.
///
/// - `dag`: the CURRENT graph (names, closures, tests, lens predicates).
/// - `base`: the baseline graph the `diff` was computed from, for the
///   "newly introduced" layer-violation difference; `None` → every current
///   violation counts as new.
/// - `diff`: the changed set (added ∪ modified; identity = `unique_id`).
/// - `opts`: the [`ImpactOpts`] — the `meta` key read for
///   [`PrImpact::affected_owners`] and the hub threshold
///   [`PrImpact::changed_hubs`] applies.
///
/// Deterministic: the changed set is sorted; closures come from `HashSet`s
/// but every emitted listing is sorted; the new-violation set is a `BTreeSet`
/// difference.
pub fn pr_impact(dag: &Dag, base: Option<&Dag>, diff: &DagDiff, opts: &ImpactOpts) -> PrImpact {
    let owner_key = opts.owner_key;
    // The changed BUILDABLE set: added ∪ modified, restricted to nodes that
    // still exist in the current Dag and that `dbt build` can target
    // (model / seed / snapshot). Removed nodes are gone — not buildable, not
    // traversable; added/modified exposures aren't buildable either.
    let mut changed: Vec<String> = diff
        .added
        .iter()
        .chain(diff.modified.keys())
        .filter(|uid| {
            opts.selection.is_none_or(|s| s.contains(uid.as_str()))
                && dag.get(uid).is_some_and(testable)
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
    //
    // The owning package is resolved ONCE up front — every filter below shares
    // it (`is_root_project` re-resolves per call, so a sweep would pay an
    // O(nodes) scan per node on unrecorded manifests).
    let primary = dag.primary_package();
    let in_scope = |n: &NodeInfo| primary.as_deref().is_none_or(|p| in_package(n, p));
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
        if !in_scope(n) {
            continue;
        }
        if is_exposure(n) {
            exposure_uids.push(uid.clone());
        } else if n.resource_type == "model" {
            affected_models += 1;
            if first_dir(n) == Some("marts") {
                affected_marts.push(list_name(&n.unique_id, &n.name));
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
            .filter_map(|uid| dag.get(uid).map(|n| list_name(&n.unique_id, &n.name)))
            .collect();
        names.sort();
        names.dedup();
        let selectors: Vec<String> = names.iter().map(|n| format!("{n}+")).collect();
        Some(format!("dbt build --select {}", selectors.join(" ")))
    };

    // The column-level narrowing: the distinct current-Dag models a
    // breaking/partial COLUMN change actually reaches (vs the node-granular
    // blast radius above). These are already the column-closure set, so the
    // narrower selector names them plainly — no `+` fan-out — alongside the
    // changed buildable nodes themselves.
    let mut col_models: BTreeSet<String> = BTreeSet::new();
    for (uid, changes) in &diff.columns {
        // Column findings belong to a MODIFIED node — out-of-selection
        // nodes contribute none, the same rule the listings apply.
        if !opts.selection.is_none_or(|s| s.contains(uid)) {
            continue;
        }
        for ch in changes {
            if ch.class == ColumnChangeClass::NonBreaking {
                continue;
            }
            col_models.extend(ch.affected_models.iter().cloned());
        }
    }
    let column_affected_models = col_models.len();
    let ci_command_columns = if changed.is_empty() {
        None
    } else {
        // A changed node narrows to just its name only when the column
        // analysis SAW it (`columns_analyzed`) AND every reason it changed
        // is accounted for by column-level rows. The opaque reasons —
        // `SQL changed`, `macro:*`, `upstream deps changed` — can alter a
        // model's whole downstream reach without producing a single column
        // row: a WHERE/JOIN/GROUP BY edit, a set operation, a macro body
        // swap, a new parent. When such a reason produced no rows the
        // change escaped the column surface, so the `+` fan-out stays —
        // `stg_orders+` after a `where u.disabled_flag = false -> true`
        // edit keeps `fct_orders` building instead of going stale.
        let column_accounted = |uid: &str| -> bool {
            let reasons = diff.modified.get(uid).map(Vec::as_slice).unwrap_or(&[]);
            let opaque = reasons.iter().any(|r| {
                r == "SQL changed" || r.starts_with("macro:") || r == "upstream deps changed"
            });
            let rows = diff.columns.get(uid).is_some_and(|v| !v.is_empty());
            diff.columns_analyzed.contains(uid) && (!opaque || rows)
        };
        let mut selectors: BTreeSet<String> = changed
            .iter()
            .filter_map(|uid| {
                dag.get(uid).map(|n| {
                    if column_accounted(uid) {
                        list_name(&n.unique_id, &n.name)
                    } else {
                        format!("{}+", list_name(&n.unique_id, &n.name))
                    }
                })
            })
            .collect();
        selectors.extend(col_models.iter().cloned());
        Some(format!(
            "dbt build --select {}",
            selectors.iter().cloned().collect::<Vec<_>>().join(" ")
        ))
    };

    // Risk: changed nodes that carry no tests (the coverage_gap predicate) —
    // scoped to the root project: an installed package's own test gaps are
    // its maintainers' problem, not a risk flag in YOUR diff.
    let mut untested_changes: Vec<String> = changed
        .iter()
        .filter_map(|uid| dag.get(uid))
        .filter(|n| in_scope(n) && coverage_gap(n))
        .map(|n| list_name(&n.unique_id, &n.name))
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
            (down >= opts.hub_min_downstream)
                .then(|| {
                    dag.get(uid)
                        .filter(|n| in_scope(n))
                        .map(|n| (list_name(&n.unique_id, &n.name), down))
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
        // A violation belongs to the CHILD (the referencing side — the same
        // node the `layer_violation` lint flags). Selecting marts must not
        // re-report a staging-side violation whose child is out of scope.
        .filter(|(_, c)| opts.selection.is_none_or(|s| s.contains(c)))
        .map(|(p, c)| {
            let parent_name = dag
                .get(p)
                .map_or_else(|| p.clone(), |n| list_name(&n.unique_id, &n.name));
            let child_name = dag
                .get(c)
                .map_or_else(|| c.clone(), |n| list_name(&n.unique_id, &n.name));
            (parent_name, child_name)
        })
        .collect();
    new_layer_violations.sort();

    PrImpact {
        affected_models,
        affected_marts,
        affected_exposures,
        affected_owners: affected_owners.into_iter().collect(),
        ci_command,
        column_affected_models,
        ci_command_columns,
        untested_changes,
        changed_hubs,
        new_layer_violations,
    }
}

/// All kept 1-hop `(parent, child)` edges where BOTH endpoints are `model`
/// nodes in a [`VIOLATION_LAYERS`] directory (staging / intermediate /
/// marts — `utilities` and unmapped dirs carry no data-flow direction, so
/// their edges never count) and the parent's layer ranks LATER than the
/// child's (e.g. a `staging` model fed by a `marts` model — upstream data
/// flows the wrong way down the layer taxonomy). Sorted via `Dag::edges()`.
/// The single violation predicate: the `LayerViolation` lens, the stats
/// worklist, the PR impact pack's "new violations" risk, and `dbtl check`
/// all read it.
///
/// Layer-violation edges are scoped to the root project the same way the
/// other metrics are — but on BOTH endpoints, not just the child: a package
/// node's layer dir is read off the PACKAGE's own `path` taxonomy, which is
/// not the root project's layer order. A package→root edge ranking
/// "backwards" would flag every package consumer as a violation — pure
/// noise, so an edge with either endpoint installed is dependency context,
/// never your project's health.
pub fn layer_violation_edges(dag: &Dag) -> Vec<(String, String)> {
    // Resolve the owning package ONCE — the per-edge scope test shares it
    // (`is_root_project` would re-resolve per edge).
    let primary = dag.primary_package();
    let in_scope = |n: &NodeInfo| primary.as_deref().is_none_or(|p| in_package(n, p));
    dag.edges()
        .into_iter()
        .filter(|(parent, child)| {
            let (Some(p), Some(c)) = (dag.get(parent), dag.get(child)) else {
                return false;
            };
            if p.resource_type != "model" || c.resource_type != "model" {
                return false;
            }
            if !in_scope(p) || !in_scope(c) {
                return false;
            }
            let (Some(pd), Some(cd)) = (first_dir(p), first_dir(c)) else {
                return false;
            };
            VIOLATION_LAYERS.contains(&pd)
                && VIOLATION_LAYERS.contains(&cd)
                && layer_rank(pd) > layer_rank(cd)
        })
        .collect()
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
