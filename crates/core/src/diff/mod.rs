//! Manifest diff: compare a BASELINE [`Dag`] (another manifest.json or another
//! checkout's project source, loaded once at startup via `--diff`) against the
//! CURRENT one, and classify every node as added / removed / modified.
//!
//! Pure data + a pure function: [`compute_diff`] reads two `Dag`s and produces
//! a [`DagDiff`] — no IO, no UI types — so it is unit-testable from synthetic
//! manifests like `dag.rs` itself. The App owns the baseline and recomputes the
//! diff on every reload (the baseline never changes after startup); the Diff
//! lens, the `D` modal, and the status `diff` chip all read the SAME `DagDiff`,
//! so the three readouts can never disagree.
//!
//! Identity is the dbt `unique_id` (stable across runs: `model.<project>.<name>`),
//! so a renamed model deliberately reads as removed + added — that is what a
//! rename IS to every downstream `ref()`.
//!
//! Determinism: `BTreeSet`/`BTreeMap` for the keyed sets, sorted `Vec`s for the
//! listings, and `modified_reasons` emits its findings in one fixed order.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::cll::{
    ColumnEdgeKind, ColumnLineage, ColumnRef, ColumnResolver, SqlDialect, SqlParseResolver,
};
use crate::impact::exposure_note;
use crate::layer::first_dir;
use crate::{in_package, is_exposure, Dag, NodeDetail, NodeInfo};

/// How a CURRENT-dag node relates to the baseline (the per-node readout the
/// Diff lens tints by). Removed nodes have no current-dag home, so they appear
/// only in [`DagDiff::removed`] (the modal / counts), never as a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    /// Present now, absent from the baseline.
    Added,
    /// Present in both, but its definition changed (see the recorded reasons).
    Modified,
    /// Present in both and unchanged.
    Unchanged,
}

/// How dangerous a column-level change is for the columns and exposures
/// downstream of it — the class the `dbtl check --diff --no-breaking` gate
/// reads. Ordered Breaking < Partial < NonBreaking so a sorted listing leads
/// with the worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnChangeClass {
    /// A baseline-referenced column no longer exists under its old name —
    /// downstream consumers of it now dangle (`removed` / `renamed`).
    Breaking,
    /// The column survives but its shape moved — `data_type` or the SELECT
    /// expression changed; consumers still compile but may compute
    /// differently.
    Partial,
    /// Nothing downstream depended on the change (added columns,
    /// unreferenced removals).
    NonBreaking,
}

impl ColumnChangeClass {
    /// The short display label (`breaking` / `partial-breaking` /
    /// `non-breaking`).
    pub fn label(self) -> &'static str {
        match self {
            ColumnChangeClass::Breaking => "breaking",
            ColumnChangeClass::Partial => "partial-breaking",
            ColumnChangeClass::NonBreaking => "non-breaking",
        }
    }
}

/// What changed about one column of a node present in BOTH dags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnChangeKind {
    /// A baseline column no current column carries the name of.
    Removed,
    /// A baseline column re-emerged under a new name fed by the same
    /// upstream column(s) — detected by the shared-provenance pairing in
    /// [`compute_diff_with`].
    Renamed,
    /// Same column, different `data_type` (catalog first, then declared).
    TypeChanged,
    /// Same column, different normalized SELECT expression (AST-compared —
    /// formatting and comments can never produce this).
    ExprChanged,
    /// A new output column (that no removed baseline column renames into).
    Added,
}

impl ColumnChangeKind {
    /// The short display label (`removed` / `renamed` / `type changed` /
    /// `expression changed` / `added`).
    pub fn label(self) -> &'static str {
        match self {
            ColumnChangeKind::Removed => "removed",
            ColumnChangeKind::Renamed => "renamed",
            ColumnChangeKind::TypeChanged => "type changed",
            ColumnChangeKind::ExprChanged => "expression changed",
            ColumnChangeKind::Added => "added",
        }
    }
}

/// One classified column change inside a modified node: what moved, how
/// dangerous it is, and what the move reaches downstream through column
/// lineage. `Serialize` — the struct is part of the shared `dbtl impact
/// --format json` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnChange {
    /// The node's `unique_id` (the node exists in both dags — it's modified).
    pub node: String,
    /// The node's display name (from the current Dag).
    pub node_name: String,
    /// The CURRENT-side column name — for `Removed` the baseline name, since
    /// no current column carries it.
    pub column: String,
    /// For `Renamed`: the baseline name that now maps to `column`.
    pub renamed_from: Option<String>,
    /// What changed.
    pub kind: ColumnChangeKind,
    /// How dangerous it is downstream.
    pub class: ColumnChangeClass,
    /// For `TypeChanged`: the baseline and current types.
    pub type_from: Option<String>,
    /// For `TypeChanged`: the current type.
    pub type_to: Option<String>,
    /// The columns the change reaches, as `"model.column"` strings, sorted.
    /// For breaking kinds: the baseline-referenced consumers still present
    /// in the current Dag — the references that now dangle. For
    /// partial/added: the column's current-side downstream spread.
    pub affected_columns: Vec<String>,
    /// Current-dag `model` nodes holding affected columns, by name, sorted —
    /// the column-precise "affected models" (the narrower CI selector's
    /// downstream half).
    pub affected_models: Vec<String>,
    /// The [`affected_models`](ColumnChange::affected_models) subset sitting
    /// in the `marts` layer, by name, sorted.
    pub affected_marts: Vec<String>,
    /// Exposures downstream of this node and its affected models, as display
    /// strings (`"name (kind, owner: …)"`), sorted — who to warn per change.
    pub affected_exposures: Vec<String>,
}

/// The computed baseline↔current difference. Built once per (baseline, Dag)
/// pair by [`compute_diff`]; every field is deterministically ordered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DagDiff {
    /// `unique_id`s present in the current Dag but not the baseline.
    pub added: BTreeSet<String>,
    /// Nodes present only in the baseline, as `(name, resource_type)` sorted —
    /// they have no current `unique_id` to key UI state by, so the display pair
    /// is captured here at diff time.
    pub removed: Vec<(String, String)>,
    /// `unique_id` → the (fixed-order) list of human-readable change reasons,
    /// for nodes present in both Dags whose definition differs.
    pub modified: BTreeMap<String, Vec<String>>,
    /// Dependency edges present only in the current Dag, as
    /// `(parent name, child name)` sorted.
    pub edges_added: Vec<(String, String)>,
    /// Dependency edges present only in the baseline, same shape.
    pub edges_removed: Vec<(String, String)>,
    /// The column-level refinement of `modified`: `unique_id` → the node's
    /// classified column changes, each row sorted `(column, kind)`. A node
    /// whose diff is column-invisible (docs-only, formatting-only, WHERE-only
    /// SQL edits) has no entry.
    pub columns: BTreeMap<String, Vec<ColumnChange>>,
    /// The `modified` uids the column analysis could see at all — the
    /// resolver named output columns on at least one side. Membership marks
    /// "analyzed, no column findings" (vs absent = "unanalyzable"), so the
    /// narrowed CI selector can tell a proven column-clean node (safe to
    /// drop its `+` fan-out) from a node the analysis never saw (kept wide).
    pub columns_analyzed: BTreeSet<String>,
}

impl DagDiff {
    /// Whether the two Dags are identical under the diff's definition.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.modified.is_empty()
            && self.edges_added.is_empty()
            && self.edges_removed.is_empty()
            && self.columns.is_empty()
    }

    /// The column changes that are `Breaking`-classified AND still
    /// downstream-referenced — the set `dbtl check --diff --no-breaking`
    /// fails on. A renamed column whose consumers all migrated and a removed
    /// column nobody references are excluded (empty `affected_columns`).
    pub fn breaking_referenced(&self) -> Vec<&ColumnChange> {
        self.columns
            .values()
            .flatten()
            .filter(|c| c.class == ColumnChangeClass::Breaking && !c.affected_columns.is_empty())
            .collect()
    }

    /// The diff status of a CURRENT-dag node (the Diff lens's per-node metric).
    pub fn status(&self, unique_id: &str) -> DiffStatus {
        if self.added.contains(unique_id) {
            DiffStatus::Added
        } else if self.modified.contains_key(unique_id) {
            DiffStatus::Modified
        } else {
            DiffStatus::Unchanged
        }
    }

    /// `(added, modified, removed)` node counts — the status-chip numbers.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.added.len(), self.modified.len(), self.removed.len())
    }
}

/// Compare the `base`line Dag against the `current` one. Nodes are keyed by
/// `unique_id`; a node present in both is [`DiffStatus::Modified`] when any of
/// the `modified_reasons` fire. Edges are the Dags' pruned 1-hop dependency
/// edges, compared as sets and reported by display NAME (a removed edge's
/// endpoints may not exist in the current Dag at all).
///
/// The column-level analysis resolves each side with a fresh, catalog-less
/// [`SqlParseResolver`] — the current side on its own adapter dialect, the
/// baseline pinned to the same dialect when the current Dag names one
/// ([`SqlDialect::for_diff_baseline`]; one dialect per diff or normalized
/// expressions can't compare). Callers holding richer resolvers (a loaded
/// `catalog.json`, an explicit `--sql-dialect`, a shared memoized instance)
/// should call [`compute_diff_with`] instead so the diff and their other
/// surfaces resolve identically.
pub fn compute_diff(base: &Dag, current: &Dag) -> DagDiff {
    let cur_res = SqlParseResolver::for_dag(None, current);
    let base_res = SqlParseResolver::new(None, SqlDialect::for_diff_baseline(current, base));
    compute_diff_with(base, current, &base_res, &cur_res)
}

/// [`compute_diff`] with caller-supplied column resolvers — one per side,
/// since the baseline's catalog/dialect may differ from the current one's.
/// The resolvers power the column analysis (column lists, provenance,
/// types, normalized expressions) so the result matches whatever strategy
/// the caller's other surfaces use.
pub fn compute_diff_with(
    base: &Dag,
    current: &Dag,
    base_res: &dyn ColumnResolver,
    cur_res: &dyn ColumnResolver,
) -> DagDiff {
    // Shared-macro impact is computed once for the whole diff (a graph-global
    // closure, not a per-node surface): node uid -> names of CHANGED macros it
    // transitively calls.
    let macro_reasons = macro_change_reasons(base, current);

    let mut added: BTreeSet<String> = BTreeSet::new();
    let mut modified: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for uid in current.nodes().keys() {
        match base.get(uid) {
            None => {
                added.insert(uid.clone());
            }
            Some(_) => {
                let mut reasons = modified_reasons(base, current, uid);
                if let Some(names) = macro_reasons.get(uid) {
                    reasons.extend(names.iter().map(|n| format!("macro:{n}")));
                }
                if !reasons.is_empty() {
                    modified.insert(uid.clone(), reasons);
                }
            }
        }
    }

    let mut removed: Vec<(String, String)> = base
        .nodes()
        .values()
        .filter(|n| current.get(&n.unique_id).is_none())
        .map(|n| (n.name.clone(), n.resource_type.clone()))
        .collect();
    removed.sort();

    // Edge sets over unique_ids; the listings resolve display names from
    // whichever Dag knows the endpoint (removed edges may name removed nodes).
    let base_edges: BTreeSet<(String, String)> = base.edges().into_iter().collect();
    let current_edges: BTreeSet<(String, String)> = current.edges().into_iter().collect();
    let name_in = |dag: &Dag, uid: &str| {
        dag.get(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.to_string())
    };
    let mut edges_added: Vec<(String, String)> = current_edges
        .difference(&base_edges)
        .map(|(p, c)| (name_in(current, p), name_in(current, c)))
        .collect();
    edges_added.sort();
    let mut edges_removed: Vec<(String, String)> = base_edges
        .difference(&current_edges)
        .map(|(p, c)| (name_in(base, p), name_in(base, c)))
        .collect();
    edges_removed.sort();

    let (columns, columns_analyzed) = column_changes(base, current, &modified, base_res, cur_res);

    DagDiff {
        added,
        removed,
        modified,
        edges_added,
        edges_removed,
        columns,
        columns_analyzed,
    }
}

mod columns;
#[cfg(test)]
mod tests;

use columns::*;
