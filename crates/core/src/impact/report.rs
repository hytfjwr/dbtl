//! The shared report payload: the [`PrImpact`] analysis struct, the
//! name-resolved [`DiffReport`] listings, the [`ImpactOpts`] both builders
//! take, and [`diff_report`] — the assembly every readout renders (the `D`
//! modal, the `dbtl impact` stdout report, the `dbtl check` impact block).
//! `Serialize` so `dbtl impact --format json` emits it verbatim.

use std::collections::BTreeSet;

use serde::Serialize;

use super::pr::pr_impact;
use crate::{list_name, ColumnChange, Dag, DagDiff, PackageScope};

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
    /// The column-level refinement of [`PrImpact::affected_models`]: the
    /// distinct current-Dag `model`s a breaking/partial COLUMN change
    /// actually reaches through column lineage — the narrower "who breaks"
    /// count vs the model-granular blast radius.
    pub column_affected_models: usize,
    /// The column-narrowed `dbt build --select` suggestion: the changed
    /// buildable nodes PLUS exactly the models the changed columns reach
    /// (plain names — the column closure is already the downstream set, so
    /// no `+` fan-out). `None` when nothing buildable changed.
    pub ci_command_columns: Option<String>,
    /// Risk: changed nodes that carry no tests (the
    /// [`coverage_gap`](crate::coverage_gap) predicate), by name, sorted.
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
    /// The column-level findings of [`DagDiff::columns`], flattened and sorted
    /// by `(node name, column, kind)` — the shared "Column changes" listing
    /// every surface renders.
    pub column_changes: Vec<ColumnChange>,
    /// The reviewer-shaped PR impact analysis over the changed set.
    pub pr: PrImpact,
}

/// The non-graph inputs [`pr_impact`]/[`diff_report`] share: the `meta` key
/// the affected-owners set reads and the downstream-degree threshold a
/// "changed hub" needs. One options struct so `dbtl check`'s gate and the
/// `dbtl impact`/TUI pack can never disagree on either.
#[derive(Debug, Clone)]
pub struct ImpactOpts<'a> {
    /// The `meta` key read for [`PrImpact::affected_owners`] (`"owner"` by
    /// default; resolved by the caller — `.dbtl.yml` > `config.yml` >
    /// default — so the surfaces can't disagree).
    pub owner_key: &'a str,
    /// The downstream-count threshold [`PrImpact::changed_hubs`] lists at
    /// (`.dbtl.yml` `check.gates.changed_hubs.min_downstream`;
    /// [`PR_HUB_MIN_DOWNSTREAM`] by default).
    pub hub_min_downstream: usize,
    /// A `--select` uid set the report is scoped to (`None` = the whole
    /// diff — the default every existing surface keeps). The changed set
    /// (added ∪ modified) intersects it, so the blast radius, hubs,
    /// untested-changes, exposure and column readouts describe only the
    /// selected changes; `added`/`modified`/`column_changes` listings
    /// intersect it too (`removed` and the edge listings never do — they
    /// name baseline-side names, not current-dag uids).
    pub selection: Option<&'a BTreeSet<String>>,
}

impl Default for ImpactOpts<'_> {
    fn default() -> Self {
        ImpactOpts {
            owner_key: "owner",
            hub_min_downstream: PR_HUB_MIN_DOWNSTREAM,
            selection: None,
        }
    }
}

/// Build the shared [`DiffReport`] payload: names resolved from the CURRENT
/// `dag`, every listing sorted. `base` is the baseline `Dag` the `diff` was
/// computed from (needed for the new-layer-violations difference); `baseline`
/// is its display label (the `--diff` argument). `scope` is the package scope
/// both graphs were loaded under — carried onto the report verbatim. `opts`
/// carries the owner key and hub threshold (see [`ImpactOpts`]).
pub fn diff_report(
    dag: &Dag,
    base: Option<&Dag>,
    diff: &DagDiff,
    baseline: &str,
    scope: PackageScope,
    opts: &ImpactOpts,
) -> DiffReport {
    let pair = |uid: &str| {
        dag.get(uid)
            .map(|n| (list_name(&n.unique_id, &n.name), n.resource_type.clone()))
            .unwrap_or_else(|| (uid.to_string(), String::new()))
    };
    // The current-dag listings intersect the selection; `removed` and the
    // edge diffs don't (they name baseline-side names, not selectable uids).
    let in_sel = |uid: &str| opts.selection.is_none_or(|s| s.contains(uid));
    let mut added: Vec<(String, String)> = diff
        .added
        .iter()
        .filter(|uid| in_sel(uid))
        .map(|uid| pair(uid))
        .collect();
    added.sort();
    let mut modified: Vec<(String, String)> = diff
        .modified
        .iter()
        .filter(|(uid, _)| in_sel(uid))
        .map(|(uid, reasons)| (pair(uid).0, reasons.join("; ")))
        .collect();
    modified.sort();
    let mut column_changes: Vec<ColumnChange> = diff
        .columns
        .iter()
        .filter(|(uid, _)| in_sel(uid))
        .flat_map(|(_, changes)| changes)
        .cloned()
        .collect();
    column_changes.sort_by(|a, b| {
        (a.node_name.as_str(), a.column.as_str(), a.kind).cmp(&(
            b.node_name.as_str(),
            b.column.as_str(),
            b.kind,
        ))
    });
    DiffReport {
        baseline: baseline.to_string(),
        scope,
        added,
        removed: diff.removed.clone(),
        modified,
        edges_added: diff.edges_added.clone(),
        edges_removed: diff.edges_removed.clone(),
        column_changes,
        pr: pr_impact(dag, base, diff, opts),
    }
}
