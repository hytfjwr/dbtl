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
//! listings, and [`modified_reasons`] emits its findings in one fixed order.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::cll::{ColumnEdgeKind, ColumnLineage, ColumnRef, ColumnResolver, SqlParseResolver};
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
/// the [`modified_reasons`] fire. Edges are the Dags' pruned 1-hop dependency
/// edges, compared as sets and reported by display NAME (a removed edge's
/// endpoints may not exist in the current Dag at all).
///
/// The column-level analysis resolves each side with a fresh, catalog-less
/// [`SqlParseResolver`] (dialect read from each Dag's own adapter metadata) —
/// callers holding richer resolvers (a loaded `catalog.json`, an explicit
/// `--sql-dialect`, a shared memoized instance) should call
/// [`compute_diff_with`] instead so the diff and their other surfaces resolve
/// identically.
pub fn compute_diff(base: &Dag, current: &Dag) -> DagDiff {
    let base_res = SqlParseResolver::for_dag(None, base);
    let cur_res = SqlParseResolver::for_dag(None, current);
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

// ---------------------------------------------------------------------------
// Column-level diff (the CLL refinement of `modified`)
// ---------------------------------------------------------------------------

/// The column-level half of the diff: for each node `modified` flags, compare
/// the two sides' output-column sets and per-column provenance, classify each
/// difference, and trace the dangerous ones through column lineage to the
/// downstream columns/models/exposures they reach.
///
/// Every input comes through the two resolvers, so the column source priority
/// is exactly the app's own chain — `columns()` and `column_type()` resolve
/// catalog → declared → CLL-inferred; `column_exprs()` supplies the
/// normalized per-column SELECT expressions (`None` = "no expression view",
/// never "changed").
///
/// Classification:
/// - name absent current-side → `removed`; name new current-side → `added`;
/// - a removed/added pair sharing ≥1 direct upstream column → `renamed`
///   (the one heuristic — see below);
/// - shared/paired names with differing `data_type` → `type changed`;
/// - shared/paired names with differing normalized expression → `expression
///   changed`.
///
/// Severity: a removed/renamed column that still has baseline-referenced
/// consumers (they exist current-side and remain linked to this node, or
/// resolve to nothing because the name they read is gone) is `breaking`;
/// `type`/`expression` changes are `partial`; everything else — including
/// a removed column nothing referenced and a fully-migrated rename — is
/// `non-breaking`.
///
/// RENAME heuristic: `r -> a` pairs when `upstream_baseline(r)` ∩
/// `upstream_current(a)` is non-empty (same parent uid + column name — the
/// `o.status as order_status` pattern). Each side pairs at most once;
/// candidates match greedily in (shared-source count desc, old name, new
/// name) order, so an ambiguous fan (`r` and `a` both fan-in the same
/// sources) resolves deterministically rather than by map order. A rename
/// that also changes type or expression gets additional rows on the new
/// name.
///
/// The "still referenced" test for breaking changes is deliberately strict:
/// a baseline downstream column counts as dangling only when it exists
/// current-side AND its current upstream closure still reaches this node —
/// or resolves to NOTHING, the `select m.gone` shape a dropped column
/// leaves behind (a consumer rewritten to draw from elsewhere is migrated,
/// not broken). For renames, columns already re-pointed at the new name are
/// excluded.
fn column_changes(
    base: &Dag,
    current: &Dag,
    modified: &BTreeMap<String, Vec<String>>,
    base_res: &dyn ColumnResolver,
    cur_res: &dyn ColumnResolver,
) -> (BTreeMap<String, Vec<ColumnChange>>, BTreeSet<String>) {
    if modified.is_empty() {
        return (BTreeMap::new(), BTreeSet::new());
    }
    let base_lin = ColumnLineage::new(base, base_res);
    let cur_lin = ColumnLineage::new(current, cur_res);
    let primary = current.primary_package();
    let in_scope = |n: &NodeInfo| primary.as_deref().is_none_or(|p| in_package(n, p));

    // uid → every column the CURRENT side produces: the resolver's column
    // list PLUS the `to` side of its in-edges (a wildcard-carried column that
    // was never declared still materializes downstream). Memoized per uid.
    let mut cur_outs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut exists_cur = |node: &str, col: &str| -> bool {
        cur_outs
            .entry(node.to_string())
            .or_insert_with(|| {
                let mut s: BTreeSet<String> = cur_res.columns(current, node).into_iter().collect();
                s.extend(cur_lin.in_edges(node).iter().map(|e| e.to.column.clone()));
                s
            })
            .contains(col)
    };

    let mut out: BTreeMap<String, Vec<ColumnChange>> = BTreeMap::new();
    let mut analyzed: BTreeSet<String> = BTreeSet::new();
    for uid in modified.keys() {
        // The same exclusion `modified_reasons` applies to the column
        // surface: source mode fills seed columns from the CSV header — a
        // documented superset, not a change.
        let kind = current
            .get(uid)
            .map(|n| n.resource_type.as_str())
            .unwrap_or("");
        if kind == "seed" {
            continue;
        }
        let before: BTreeSet<String> = base_res.columns(base, uid).into_iter().collect();
        let after: BTreeSet<String> = cur_res.columns(current, uid).into_iter().collect();
        if before.is_empty() && after.is_empty() {
            continue;
        }
        // Columns were nameable on at least one side — the node is
        // "analyzed" whether or not any row below fires.
        analyzed.insert(uid.clone());
        let removed: Vec<String> = before.difference(&after).cloned().collect();
        let added: Vec<String> = after.difference(&before).cloned().collect();
        let node_name = current
            .get(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.clone());

        // Direct upstream provenance per output column — the rename input.
        let base_srcs = |col: &str| -> BTreeSet<(String, String)> {
            base_lin
                .upstream(&ColumnRef::new(uid.clone(), col))
                .iter()
                .map(|e| (e.from.node.clone(), e.from.column.clone()))
                .collect()
        };
        let cur_srcs = |col: &str| -> BTreeSet<(String, String)> {
            cur_lin
                .upstream(&ColumnRef::new(uid.clone(), col))
                .iter()
                .map(|e| (e.from.node.clone(), e.from.column.clone()))
                .collect()
        };
        let pairs = rename_pairs(&removed, &added, &base_srcs, &cur_srcs);
        let renamed_old: BTreeSet<&String> = pairs.iter().map(|p| &p.0).collect();
        let renamed_new: BTreeSet<&String> = pairs.iter().map(|p| &p.1).collect();

        let base_exprs = base_res.column_exprs(base, uid);
        let cur_exprs = cur_res.column_exprs(current, uid);

        // The dangling-consumer set for a baseline column: its baseline
        // downstream closure, kept to columns that exist current-side AND
        // still link to `uid` upstream — minus `exclude` (the rename's
        // already-migrated consumers). "Upstream" means VALUE edges only
        // (Passthrough/Rename/Transform): a `FilterOrJoin` fan shapes the
        // row set but produces no value — a column whose only upstream is
        // FOJ (`o.*` shrunk past the name, or `select m.gone` against a
        // known column list) IS the dangling shape the gate exists to
        // catch; a consumer rewired to another node's column (non-empty
        // value upstream that never reaches `uid`) is genuinely migrated.
        let value_up = |cr: &ColumnRef| -> BTreeSet<ColumnRef> {
            let mut seen = BTreeSet::new();
            let mut stack = vec![cr.clone()];
            while let Some(c) = stack.pop() {
                for e in cur_lin.upstream(&c) {
                    if e.kind == ColumnEdgeKind::FilterOrJoin {
                        continue;
                    }
                    if seen.insert(e.from.clone()) {
                        stack.push(e.from);
                    }
                }
            }
            seen
        };
        let mut dangling = |col: &str, exclude: &BTreeSet<ColumnRef>| {
            let closure = base_lin.closure_down(&ColumnRef::new(uid.clone(), col));
            affected_lists(&closure, current, uid, &in_scope, |cr| {
                if !exists_cur(&cr.node, &cr.column) || exclude.contains(cr) {
                    return false;
                }
                let up = value_up(cr);
                up.is_empty() || up.iter().any(|u| u.node == *uid)
            })
        };
        // The current-side spread of a column — for `type`/`expression`/
        // `added` rows.
        let spread = |col: &str| {
            let closure = cur_lin.closure_down(&ColumnRef::new(uid.clone(), col));
            affected_lists(&closure, current, uid, &in_scope, |_| true)
        };
        let push = |column: String,
                    renamed_from: Option<String>,
                    kind: ColumnChangeKind,
                    class: ColumnChangeClass,
                    type_from: Option<String>,
                    type_to: Option<String>,
                    impact: ImpactLists,
                    rows: &mut Vec<ColumnChange>| {
            rows.push(ColumnChange {
                node: uid.clone(),
                node_name: node_name.clone(),
                column,
                renamed_from,
                kind,
                class,
                type_from,
                type_to,
                affected_columns: impact.cols,
                affected_models: impact.models,
                affected_marts: impact.marts,
                affected_exposures: impact.exposures,
            });
        };
        let mut rows: Vec<ColumnChange> = Vec::new();

        for old in &removed {
            if renamed_old.contains(old) {
                continue;
            }
            let impact = dangling(old, &BTreeSet::new());
            let class = if impact.cols.is_empty() {
                ColumnChangeClass::NonBreaking
            } else {
                ColumnChangeClass::Breaking
            };
            push(
                old.clone(),
                None,
                ColumnChangeKind::Removed,
                class,
                None,
                None,
                impact,
                &mut rows,
            );
        }
        for (old, new) in &pairs {
            // Columns the new name already feeds current-side have migrated
            // to it — they are consumers of the rename, not casualties.
            let migrated: BTreeSet<ColumnRef> =
                cur_lin.closure_down(&ColumnRef::new(uid.clone(), new));
            let impact = dangling(old, &migrated);
            // A fully-migrated rename is informational, like `added`: the
            // old name is gone but nothing in the graph still reads it
            // (the gate's Breaking ⟺ dangling-nonempty invariant relies on
            // this classification mirroring `removed`'s).
            let class = if impact.cols.is_empty() {
                ColumnChangeClass::NonBreaking
            } else {
                ColumnChangeClass::Breaking
            };
            push(
                new.clone(),
                Some(old.clone()),
                ColumnChangeKind::Renamed,
                class,
                None,
                None,
                impact,
                &mut rows,
            );
        }

        // Type + expression comparison across shared names (identity pairs)
        // and detected renames (old name -> new name).
        let mut compare: Vec<(String, String)> = before
            .intersection(&after)
            .map(|c| (c.clone(), c.clone()))
            .collect();
        compare.extend(pairs.iter().cloned());
        for (bcol, ccol) in compare {
            if let (Some(b), Some(c)) = (
                base_res.column_type(base, uid, &bcol),
                cur_res.column_type(current, uid, &ccol),
            ) {
                if !type_eq(&b, &c) {
                    let impact = spread(&ccol);
                    push(
                        ccol.clone(),
                        None,
                        ColumnChangeKind::TypeChanged,
                        ColumnChangeClass::Partial,
                        Some(b),
                        Some(c),
                        impact,
                        &mut rows,
                    );
                }
            }
            if let (Some(bm), Some(cm)) = (&base_exprs, &cur_exprs) {
                if let (Some(b), Some(c)) = (bm.get(&bcol), cm.get(&ccol)) {
                    if b != c {
                        let impact = spread(&ccol);
                        push(
                            ccol.clone(),
                            None,
                            ColumnChangeKind::ExprChanged,
                            ColumnChangeClass::Partial,
                            None,
                            None,
                            impact,
                            &mut rows,
                        );
                    }
                }
            }
        }

        for new in &added {
            if renamed_new.contains(new) {
                continue;
            }
            let impact = spread(new);
            push(
                new.clone(),
                None,
                ColumnChangeKind::Added,
                ColumnChangeClass::NonBreaking,
                None,
                None,
                impact,
                &mut rows,
            );
        }

        if !rows.is_empty() {
            rows.sort_by(|a, b| (a.column.as_str(), a.kind).cmp(&(b.column.as_str(), b.kind)));
            out.insert(uid.clone(), rows);
        }
    }
    (out, analyzed)
}

/// The four display lists a [`ColumnChange`] carries — folded out of a
/// column-closure by [`affected_lists`].
struct ImpactLists {
    /// `"model.col"` strings of the kept closure members, sorted.
    cols: Vec<String>,
    /// `model` nodes holding kept columns, by name, sorted.
    models: Vec<String>,
    /// The `models` subset in the `marts` layer, by name, sorted.
    marts: Vec<String>,
    /// Exposures downstream of the affected models + the changed node, as
    /// display strings, sorted.
    exposures: Vec<String>,
}

/// Fold a `closure` of reached [`ColumnRef`]s into the display lists a
/// [`ColumnChange`] carries, resolved against the CURRENT dag:
/// `keep`-passing columns as `"model.col"`, the `model` nodes holding them
/// (with the `marts` subset), and the exposures downstream of those models
/// plus the changed node `home` itself (a dashboard depending on the edited
/// model directly is just as exposed to the column's move).
fn affected_lists(
    closure: &BTreeSet<ColumnRef>,
    current: &Dag,
    home: &str,
    in_scope: &dyn Fn(&NodeInfo) -> bool,
    mut keep: impl FnMut(&ColumnRef) -> bool,
) -> ImpactLists {
    let mut cols: BTreeSet<String> = BTreeSet::new();
    let mut model_uids: BTreeSet<String> = BTreeSet::new();
    for cr in closure {
        let Some(n) = current.get(&cr.node) else {
            continue;
        };
        if !keep(cr) {
            continue;
        }
        cols.insert(format!("{}.{}", n.name, cr.column));
        if n.resource_type == "model" && in_scope(n) {
            model_uids.insert(cr.node.clone());
        }
    }
    let mut roots: BTreeSet<String> = model_uids.clone();
    roots.insert(home.to_string());
    let mut exposure_uids: BTreeSet<String> = BTreeSet::new();
    for from in &roots {
        for u in current.downstream(from) {
            if current
                .get(&u)
                .is_some_and(|n| is_exposure(n) && in_scope(n))
            {
                exposure_uids.insert(u);
            }
        }
    }
    let name_of = |uid: &String| {
        current
            .get(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.clone())
    };
    let mut models: Vec<String> = model_uids.iter().map(&name_of).collect();
    models.sort();
    models.dedup();
    let mut marts: Vec<String> = model_uids
        .iter()
        .filter(|uid| {
            current
                .get(uid)
                .and_then(|n| first_dir(n))
                .is_some_and(|d| d == "marts")
        })
        .map(&name_of)
        .collect();
    marts.sort();
    marts.dedup();
    let mut exposures: Vec<String> = exposure_uids
        .iter()
        .map(|u| format!("{}{}", name_of(u), exposure_note(current.exposure(u))))
        .collect();
    exposures.sort();
    ImpactLists {
        cols: cols.into_iter().collect(),
        models,
        marts,
        exposures,
    }
}

/// Pair removed output names with added ones as `old -> new` renames: a
/// candidate needs ≥1 shared direct upstream column (`upstream_baseline(old)`
/// ∩ `upstream_current(new)` over `(parent uid, column)`), scored by the
/// shared count, matched greedily in (score desc, old, new) order with each
/// side used at most once — deterministic under any fan shape.
fn rename_pairs(
    removed: &[String],
    added: &[String],
    base_srcs: &dyn Fn(&str) -> BTreeSet<(String, String)>,
    cur_srcs: &dyn Fn(&str) -> BTreeSet<(String, String)>,
) -> Vec<(String, String)> {
    let mut cand: Vec<(usize, String, String)> = Vec::new();
    for old in removed {
        let so = base_srcs(old);
        if so.is_empty() {
            continue;
        }
        for new in added {
            let sn = cur_srcs(new);
            if sn.is_empty() {
                continue;
            }
            let score = so.intersection(&sn).count();
            if score > 0 {
                cand.push((score, old.clone(), new.clone()));
            }
        }
    }
    cand.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut used_old: BTreeSet<String> = BTreeSet::new();
    let mut used_new: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for (_, old, new) in cand {
        if used_old.insert(old.clone()) && used_new.insert(new.clone()) {
            out.push((old, new));
        }
    }
    out.sort();
    out
}

/// Warehouse-type comparison: case- and whitespace-blind (`VARCHAR(10)` and
/// `varchar( 10 )` are the same type change-wise).
fn type_eq(a: &str, b: &str) -> bool {
    a.split_whitespace().collect::<String>().to_lowercase()
        == b.split_whitespace().collect::<String>().to_lowercase()
}

/// The change reasons for a node present in BOTH Dags, in one fixed order
/// (materialization, upstream deps, columns, tests, SQL) so the listing is
/// deterministic by construction. Empty = unchanged.
///
/// Compared surfaces are the ones a dbt change actually moves:
/// - `materialized` (the detail's, same source as the lineage box tag),
/// - the direct parent set (a `ref()`/`source()` edit),
/// - the column list as a sorted `(name, data_type)` multiset (order-blind:
///   reordering schema.yml column docs is a tidy, not a model change),
/// - the tests as a sorted `(kind, column)` multiset,
/// - the raw SQL — MODELS only, and only when BOTH sides carry code, trimmed.
///
/// On top of those per-node surfaces, [`compute_diff`] appends `macro:<name>`
/// reasons for nodes that transitively call a changed manifest macro (see
/// [`macro_change_reasons`]) — the one change raw SQL can never show.
///
/// A `--diff` pair routinely spans LOAD MODES (a manifest baseline vs a
/// source-parsed current, or vice versa), so the surfaces the manifest↔source
/// consistency gate (`tests/consistency.rs`) deliberately excludes are
/// excluded here too — otherwise every cross-mode diff would report phantom
/// changes:
/// - seed `columns` (source mode reads the CSV header, dbt records only the
///   documented ones) → the column comparison skips seeds;
/// - snapshot `raw_code` (source mode stores the whole file incl. the
///   `{% snapshot %}` wrapper, dbt stores only the block body) → the SQL
///   comparison runs for `model` nodes only (and a one-sided absence — e.g. a
///   manifest that omits `raw_code` — is a loader artifact, not a change).
fn modified_reasons(base: &Dag, current: &Dag, uid: &str) -> Vec<String> {
    let mut reasons: Vec<String> = Vec::new();
    let kind = current
        .get(uid)
        .map(|n| n.resource_type.as_str())
        .unwrap_or("");
    let empty = NodeDetail::default();
    let bd = base.detail(uid).unwrap_or(&empty);
    let cd = current.detail(uid).unwrap_or(&empty);

    if bd.materialized != cd.materialized {
        let show = |m: &Option<String>| m.clone().unwrap_or_else(|| "(none)".to_string());
        reasons.push(format!(
            "materialized: {} -> {}",
            show(&bd.materialized),
            show(&cd.materialized)
        ));
    }

    let parents = |dag: &Dag| -> BTreeSet<String> {
        dag.subgraph_view(uid, true, false, Some(1))
            .edges
            .into_iter()
            .map(|e| e.parent)
            .collect()
    };
    if parents(base) != parents(current) {
        reasons.push("upstream deps changed".to_string());
    }

    let cols = |d: &NodeDetail| -> Vec<(String, Option<String>)> {
        let mut v: Vec<(String, Option<String>)> = d
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.data_type.clone()))
            .collect();
        v.sort();
        v
    };
    if kind != "seed" && cols(bd) != cols(cd) {
        reasons.push("columns changed".to_string());
    }

    let tests = |dag: &Dag| -> Vec<(String, Option<String>)> {
        let mut v: Vec<(String, Option<String>)> = dag
            .tests(uid)
            .iter()
            .map(|t| (t.kind.clone(), t.column_name.clone()))
            .collect();
        v.sort();
        v
    };
    let (bt, ct) = (tests(base), tests(current));
    if bt != ct {
        // Equal counts can still differ (a test moved column): say "changed",
        // never a no-op-looking "2 -> 2".
        if bt.len() == ct.len() {
            reasons.push("tests changed".to_string());
        } else {
            reasons.push(format!("tests: {} -> {}", bt.len(), ct.len()));
        }
    }

    if kind == "model" {
        if let (Some(b), Some(c)) = (base.raw_code(uid), current.raw_code(uid)) {
            if b.trim() != c.trim() {
                reasons.push("SQL changed".to_string());
            }
        }
    }

    reasons
}

/// The shared-macro impact map: for every node present in the CURRENT Dag, the
/// sorted names of CHANGED macros the node transitively calls — the fix for
/// "an edit inside a shared macro is invisible in raw SQL".
///
/// A macro is *changed* when its `macro_sql` differs across the pair or it
/// exists on only one side (added or removed). A node is *impacted* when the
/// changed macro is reachable from the node's `depends_on.macros` through the
/// CURRENT macro→macro call graph (`depends_on.macros` on macros), so a change
/// two calls deep still lands on the model. The walk is `seen`-guarded, so a
/// macro cycle terminates instead of looping.
///
/// Empty when either Dag lacks macro data — synthesized source-mode manifests
/// carry no macros, so a manifest↔source diff reports no phantom `macro:`
/// reasons (the same cross-mode exclusion the other surfaces get).
fn macro_change_reasons(base: &Dag, current: &Dag) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if base.macros().is_empty() && current.macros().is_empty() {
        return out;
    }

    let mut changed: BTreeSet<String> = BTreeSet::new();
    for uid in base.macros().keys().chain(current.macros().keys()) {
        let changed_here = match (base.macros().get(uid), current.macros().get(uid)) {
            (Some(b), Some(c)) => b.sql != c.sql,
            // Present on one side only: added or removed — either way the
            // compiled surface moved for anything still calling it.
            _ => true,
        };
        if changed_here {
            changed.insert(uid.clone());
        }
    }
    if changed.is_empty() {
        return out;
    }

    // A changed macro's display name: prefer the current side's record, fall
    // back to the baseline's (a removed macro still names the reason), then to
    // the uid's last segment.
    let name_of = |uid: &str| -> String {
        current
            .macros()
            .get(uid)
            .or_else(|| base.macros().get(uid))
            .map(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| uid.rsplit('.').next().unwrap_or(uid).to_string())
    };

    for uid in current.nodes().keys() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = current.node_macros(uid).to_vec();
        let mut hits: BTreeSet<String> = BTreeSet::new();
        while let Some(m) = queue.pop() {
            if !seen.insert(m.clone()) {
                continue;
            }
            if changed.contains(&m) {
                hits.insert(name_of(&m));
            }
            if let Some(info) = current.macros().get(&m) {
                queue.extend(info.depends_on.iter().cloned());
            }
        }
        if !hits.is_empty() {
            out.insert(uid.clone(), hits);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        RawConfig, RawDependsOn, RawMacro, RawManifest, RawNode, RawTestMetadata,
    };
    use std::collections::HashMap;

    fn model(name: &str, materialized: Option<&str>, sql: &str) -> RawNode {
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("marts/{name}.sql")),
            raw_code: Some(sql.to_string()),
            config: RawConfig {
                materialized: materialized.map(String::from),
            },
            ..Default::default()
        }
    }

    /// A tiny manifest: a -> b, with `b`'s shape parameterized so each test can
    /// perturb exactly one compared surface.
    fn manifest(b: RawNode, extra: Option<(&str, RawNode)>) -> RawManifest {
        let mut nodes = HashMap::from([
            (
                "model.p.a".to_string(),
                model("a", Some("view"), "select 1"),
            ),
            ("model.p.b".to_string(), b),
        ]);
        let mut child_map =
            HashMap::from([("model.p.a".to_string(), vec!["model.p.b".to_string()])]);
        let mut parent_map =
            HashMap::from([("model.p.b".to_string(), vec!["model.p.a".to_string()])]);
        if let Some((uid, node)) = extra {
            nodes.insert(uid.to_string(), node);
            parent_map.insert(uid.to_string(), vec!["model.p.b".to_string()]);
            child_map.insert("model.p.b".to_string(), vec![uid.to_string()]);
        }
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
            parent_map,
            child_map,
        }
    }

    fn base_dag() -> Dag {
        Dag::build(&manifest(model("b", Some("table"), "select 2"), None))
    }

    /// A manifest macro record: `name`/`macro_sql` plus optional macro→macro
    /// deps (the `depends_on.macros` call edges).
    fn mac(name: &str, sql: &str, deps: &[&str]) -> RawMacro {
        RawMacro {
            name: name.into(),
            macro_sql: Some(sql.into()),
            depends_on: RawDependsOn {
                nodes: vec![],
                macros: deps.iter().map(|d| d.to_string()).collect(),
            },
        }
    }

    /// The `a -> b` fixture manifest with a `macros` map attached; `b`'s
    /// `depends_on.macros` is set to `b_calls` (the direct call list).
    fn manifest_with_macros(macros: Vec<(&str, RawMacro)>, b_calls: &[&str]) -> RawManifest {
        let mut m = manifest(model("b", Some("table"), "select 2"), None);
        m.nodes.get_mut("model.p.b").unwrap().depends_on.macros =
            b_calls.iter().map(|s| s.to_string()).collect();
        m.macros = macros
            .into_iter()
            .map(|(uid, mac)| (uid.to_string(), mac))
            .collect();
        m
    }

    #[test]
    fn identical_dags_diff_empty() {
        let diff = compute_diff(&base_dag(), &base_dag());
        assert!(diff.is_empty(), "{diff:?}");
        assert_eq!(diff.status("model.p.a"), DiffStatus::Unchanged);
        assert_eq!(diff.counts(), (0, 0, 0));
    }

    #[test]
    fn added_and_removed_nodes_and_edges_are_classified() {
        let base = base_dag();
        let current = Dag::build(&manifest(
            model("b", Some("table"), "select 2"),
            Some(("model.p.c", model("c", Some("view"), "select 3"))),
        ));
        let diff = compute_diff(&base, &current);
        assert_eq!(diff.status("model.p.c"), DiffStatus::Added);
        assert_eq!(diff.status("model.p.a"), DiffStatus::Unchanged);
        assert_eq!(diff.counts(), (1, 0, 0));
        assert_eq!(diff.edges_added, vec![("b".to_string(), "c".to_string())]);
        assert!(diff.edges_removed.is_empty());

        // The reverse direction reads as removed (node AND edge).
        let diff = compute_diff(&current, &base);
        assert_eq!(diff.counts(), (0, 0, 1));
        assert_eq!(diff.removed, vec![("c".to_string(), "model".to_string())]);
        assert_eq!(diff.edges_removed, vec![("b".to_string(), "c".to_string())]);
    }

    #[test]
    fn modified_reasons_fire_per_surface_in_fixed_order() {
        let base = base_dag();
        // Change materialization AND SQL on b — two reasons, fixed order.
        let current = Dag::build(&manifest(
            model("b", Some("incremental"), "select 22"),
            None,
        ));
        let diff = compute_diff(&base, &current);
        assert_eq!(diff.status("model.p.b"), DiffStatus::Modified);
        assert_eq!(
            diff.modified["model.p.b"],
            vec![
                "materialized: table -> incremental".to_string(),
                "SQL changed".to_string(),
            ]
        );
        assert_eq!(diff.counts(), (0, 1, 0));
    }

    #[test]
    fn upstream_dep_change_is_a_reason() {
        let base = base_dag();
        // Drop the a -> b edge: b keeps its shape but loses its parent.
        let mut m = manifest(model("b", Some("table"), "select 2"), None);
        m.parent_map.clear();
        m.child_map.clear();
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["upstream deps changed".to_string()]
        );
        assert_eq!(diff.edges_removed, vec![("a".to_string(), "b".to_string())]);
    }

    #[test]
    fn test_count_change_is_a_reason() {
        let base = base_dag();
        let mut m = manifest(model("b", Some("table"), "select 2"), None);
        m.nodes.insert(
            "test.p.t".to_string(),
            RawNode {
                name: "not_null_b_id".into(),
                resource_type: "test".into(),
                attached_node: Some("model.p.b".into()),
                column_name: Some("id".into()),
                test_metadata: Some(RawTestMetadata {
                    name: Some("not_null".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["tests: 0 -> 1".to_string()]
        );
    }

    #[test]
    fn one_sided_raw_code_is_not_a_change() {
        // A manifest that omits raw_code (vs a baseline that has it) is a loader
        // artifact: it must NOT read as "SQL changed".
        let base = base_dag();
        let mut b = model("b", Some("table"), "");
        b.raw_code = None;
        let current = Dag::build(&manifest(b, None));
        let diff = compute_diff(&base, &current);
        assert_eq!(diff.status("model.p.b"), DiffStatus::Unchanged, "{diff:?}");
        // And trailing whitespace never reads as an edit.
        let current = Dag::build(&manifest(model("b", Some("table"), "select 2\n"), None));
        assert!(compute_diff(&base, &current).is_empty());
    }

    #[test]
    fn cross_mode_surfaces_are_excluded_per_node_kind() {
        // Seed columns and snapshot raw_code are the two surfaces the
        // manifest<->source consistency gate excludes; the diff must not read
        // them as changes either (a --diff pair routinely spans load modes).
        let seed = |cols: &[&str]| {
            let mut n = RawNode {
                name: "fiscal".into(),
                resource_type: "seed".into(),
                ..Default::default()
            };
            for c in cols {
                n.columns
                    .insert(c.to_string(), serde_json::json!({ "name": c }));
            }
            n
        };
        let snap = |sql: &str| RawNode {
            name: "snap".into(),
            resource_type: "snapshot".into(),
            raw_code: Some(sql.to_string()),
            ..Default::default()
        };
        let dag_with = |seed_node: RawNode, snap_node: RawNode| {
            Dag::build(&RawManifest {
                metadata: Default::default(),
                nodes: HashMap::from([
                    ("seed.p.fiscal".to_string(), seed_node),
                    ("snapshot.p.snap".to_string(), snap_node),
                ]),
                sources: HashMap::new(),
                exposures: HashMap::new(),
                semantic_models: HashMap::new(),
                metrics: HashMap::new(),
                saved_queries: HashMap::new(),
                unit_tests: HashMap::new(),
                functions: HashMap::new(),
                parent_map: HashMap::new(),
                child_map: HashMap::new(),
                macros: HashMap::new(),
            })
        };
        // Base: documented seed columns only + the dbt-style snapshot body.
        let base = dag_with(seed(&["id"]), snap("select 1"));
        // Current: CSV-header superset + the whole-file snapshot wrapper.
        let current = dag_with(
            seed(&["id", "year", "label"]),
            snap("{% snapshot snap %}\nselect 1\n{% endsnapshot %}"),
        );
        let diff = compute_diff(&base, &current);
        assert!(
            diff.is_empty(),
            "seed columns / snapshot SQL are loader artifacts, not changes: {diff:?}"
        );
    }

    #[test]
    fn column_reorder_is_not_a_change_and_equal_count_test_change_is_named() {
        // Reordering schema.yml column docs is a tidy, not a model change.
        let with_cols = |cols: &[&str]| {
            let mut b = model("b", Some("table"), "select 2");
            for c in cols {
                b.columns
                    .insert(c.to_string(), serde_json::json!({ "name": c }));
            }
            Dag::build(&manifest(b, None))
        };
        let diff = compute_diff(&with_cols(&["id", "amount"]), &with_cols(&["amount", "id"]));
        assert!(diff.is_empty(), "column order is ignored: {diff:?}");

        // A test that moved column (same count) reads as "tests changed",
        // never a no-op-looking "1 -> 1".
        let with_test = |col: &str| {
            let mut m = manifest(model("b", Some("table"), "select 2"), None);
            m.nodes.insert(
                "test.p.t".to_string(),
                RawNode {
                    name: format!("not_null_b_{col}"),
                    resource_type: "test".into(),
                    attached_node: Some("model.p.b".into()),
                    column_name: Some(col.into()),
                    test_metadata: Some(RawTestMetadata {
                        name: Some("not_null".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
            Dag::build(&m)
        };
        let diff = compute_diff(&with_test("id"), &with_test("amount"));
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["tests changed".to_string()]
        );
    }

    #[test]
    fn diff_is_deterministic() {
        let base = base_dag();
        let current = Dag::build(&manifest(
            model("b", Some("incremental"), "select 9"),
            Some(("model.p.c", model("c", None, "select 3"))),
        ));
        assert_eq!(compute_diff(&base, &current), compute_diff(&base, &current));
    }

    #[test]
    fn macro_change_marks_callers_while_unaffected_nodes_stay_clean() {
        // The README's old caveat: a shared macro's edit never shows in a
        // model's raw SQL, so `SQL changed` can't catch it — `macro:` can.
        let base = Dag::build(&manifest_with_macros(
            vec![("macro.p.cents", mac("cents", "v1", &[]))],
            &["macro.p.cents"],
        ));
        let current = Dag::build(&manifest_with_macros(
            vec![("macro.p.cents", mac("cents", "v2", &[]))],
            &["macro.p.cents"],
        ));
        let diff = compute_diff(&base, &current);
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["macro:cents".to_string()],
            "b calls the changed macro even though its raw SQL is identical"
        );
        assert_eq!(
            diff.status("model.p.a"),
            DiffStatus::Unchanged,
            "a calls no macro"
        );
    }

    #[test]
    fn macro_change_propagates_through_macro_to_macro_deps() {
        // b -> cents -> surrogate: the deep change still lands on b.
        let base = Dag::build(&manifest_with_macros(
            vec![
                ("macro.p.cents", mac("cents", "v1", &["macro.p.surrogate"])),
                ("macro.p.surrogate", mac("surrogate", "s1", &[])),
            ],
            &["macro.p.cents"],
        ));
        let current = Dag::build(&manifest_with_macros(
            vec![
                ("macro.p.cents", mac("cents", "v1", &["macro.p.surrogate"])),
                ("macro.p.surrogate", mac("surrogate", "s2", &[])),
            ],
            &["macro.p.cents"],
        ));
        let diff = compute_diff(&base, &current);
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["macro:surrogate".to_string()],
            "transitive macro dep surfaces the changed macro's name"
        );
    }

    #[test]
    fn unchanged_macros_and_macro_cycles_are_handled() {
        // Same bodies on both sides: no macro reasons, even though b calls one.
        let mk = |sql: &str| {
            Dag::build(&manifest_with_macros(
                vec![
                    ("macro.p.a", mac("a", sql, &["macro.p.b"])),
                    ("macro.p.b", mac("b", sql, &["macro.p.a"])), // a<->b cycle
                ],
                &["macro.p.a"],
            ))
        };
        let diff = compute_diff(&mk("v1"), &mk("v1"));
        assert!(diff.is_empty(), "unchanged macros: {diff:?}");

        // A cycle still terminates: changed once, reasoned once.
        let changed = Dag::build(&manifest_with_macros(
            vec![
                ("macro.p.a", mac("a", "v2", &["macro.p.b"])),
                ("macro.p.b", mac("b", "v1", &["macro.p.a"])),
            ],
            &["macro.p.a"],
        ));
        let diff = compute_diff(&mk("v1"), &changed);
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["macro:a".to_string()],
            "a changed once inside the a<->b cycle"
        );
    }

    #[test]
    fn macro_reason_appends_after_the_sql_reason_in_fixed_order() {
        // A node that changed BOTH its SQL and a called macro lists the macro
        // reason last — the listing order is fixed, not hit-order.
        let base = Dag::build(&manifest_with_macros(
            vec![("macro.p.cents", mac("cents", "v1", &[]))],
            &["macro.p.cents"],
        ));
        let mut m = manifest_with_macros(
            vec![("macro.p.cents", mac("cents", "v2", &[]))],
            &["macro.p.cents"],
        );
        m.nodes.get_mut("model.p.b").unwrap().raw_code = Some("select 99".into());
        let current = Dag::build(&m);
        let diff = compute_diff(&base, &current);
        assert_eq!(
            diff.modified["model.p.b"],
            vec!["SQL changed".to_string(), "macro:cents".to_string()]
        );
    }

    #[test]
    fn source_mode_side_disables_macro_detection() {
        // A synthesized source-mode manifest carries no macros: the SAME node
        // set minus macro data must not report phantom macro: reasons (the
        // cross-mode exclusion, like seed columns).
        let base = Dag::build(&manifest_with_macros(
            vec![("macro.p.cents", mac("cents", "v1", &[]))],
            &["macro.p.cents"],
        ));
        let source_like = base_dag(); // no macros map at all
        let diff = compute_diff(&base, &source_like);
        assert!(
            !diff
                .modified
                .values()
                .flatten()
                .any(|r| r.starts_with("macro:")),
            "no macro reasons when one side has no macro data: {diff:?}"
        );
    }

    // ---- column-level diff (CLL breaking-change analysis) -----------------

    use crate::manifest::{RawExposure, RawExposureOwner};

    /// A `model` node carrying declared `(name, data_type)` columns (the
    /// declared tier of the resolver's column priority) and `raw_code` the
    /// CLL resolver parses for provenance — `compiled_code` empty, so the
    /// resolver's Jinja-lite renderer feeds it the raw text.
    fn col_model(name: &str, layer: &str, sql: &str, cols: &[(&str, &str)]) -> RawNode {
        let columns: serde_json::Map<String, serde_json::Value> = cols
            .iter()
            .map(|(n, t)| {
                (
                    n.to_string(),
                    serde_json::json!({"name": n, "data_type": t}),
                )
            })
            .collect();
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("{layer}/{name}.sql")),
            raw_code: Some(sql.to_string()),
            columns,
            ..Default::default()
        }
    }

    /// `uid -> node` models, `parent -> child` edges, `uid -> exposure`
    /// consumers — the column-diff shape is always `up -> mid -> down`.
    fn col_manifest(
        nodes: Vec<(&str, RawNode)>,
        edges: &[(&str, &str)],
        exposures: Vec<(&str, RawExposure)>,
    ) -> RawManifest {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for (p, c) in edges {
            parent_map
                .entry(c.to_string())
                .or_default()
                .push(p.to_string());
            child_map
                .entry(p.to_string())
                .or_default()
                .push(c.to_string());
        }
        RawManifest {
            metadata: Default::default(),
            nodes: nodes.into_iter().map(|(u, n)| (u.to_string(), n)).collect(),
            sources: HashMap::new(),
            exposures: exposures
                .into_iter()
                .map(|(u, e)| (u.to_string(), e))
                .collect(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map,
            child_map,
        }
    }

    /// `up -> mid -> down`, with `mid`'s SQL + declared columns variable and
    /// `down` fixed per test. An `orders_dash` exposure sits on `down` so the
    /// affected-exposures list is exercised.
    fn col_dag(
        mid_sql: &str,
        mid_cols: &[(&str, &str)],
        down_sql: &str,
        down_cols: &[(&str, &str)],
        dash_on_down: bool,
    ) -> Dag {
        let exposures = if dash_on_down {
            vec![(
                "exposure.t.dash",
                RawExposure {
                    name: "orders_dash".into(),
                    exposure_type: Some("dashboard".into()),
                    owner: RawExposureOwner {
                        name: Some("Fin".into()),
                        email: Some("fin@x.io".into()),
                    },
                    depends_on: RawDependsOn {
                        nodes: vec!["model.t.down".to_string()],
                        macros: vec![],
                    },
                    ..Default::default()
                },
            )]
        } else {
            Vec::new()
        };
        Dag::build(&col_manifest(
            vec![
                (
                    "model.t.up",
                    col_model(
                        "up",
                        "staging",
                        "select 1",
                        &[("id", "int"), ("status", "text"), ("amount", "int")],
                    ),
                ),
                (
                    "model.t.mid",
                    col_model("mid", "staging", mid_sql, mid_cols),
                ),
                (
                    "model.t.down",
                    col_model("down", "marts", down_sql, down_cols),
                ),
            ],
            &[
                ("model.t.up", "model.t.mid"),
                ("model.t.mid", "model.t.down"),
            ],
            exposures,
        ))
    }

    fn row<'a>(
        diff: &'a DagDiff,
        uid: &str,
        col: &str,
        kind: ColumnChangeKind,
    ) -> &'a ColumnChange {
        diff.columns
            .get(uid)
            .unwrap_or_else(|| panic!("{uid} carries column changes: {:?}", diff.columns))
            .iter()
            .find(|r| r.column == col && r.kind == kind)
            .unwrap_or_else(|| panic!("a {kind:?} row on {uid}.{col}: {:?}", diff.columns.get(uid)))
    }

    #[test]
    fn column_removed_with_dangling_consumer_is_breaking() {
        // mid drops `status`; down still reads `m.status` — the reference
        // resolves to nothing current-side, which IS the dangling shape.
        let base = col_dag(
            "select u.id, u.status from \"up\" u",
            &[("id", "int"), ("status", "text")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            true,
        );
        let current = col_dag(
            "select u.id from \"up\" u",
            &[("id", "int")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            true,
        );
        let diff = compute_diff(&base, &current);
        assert!(
            diff.modified.contains_key("model.t.mid"),
            "the declared-column + SQL move marks mid modified: {diff:?}"
        );
        let r = row(&diff, "model.t.mid", "status", ColumnChangeKind::Removed);
        assert_eq!(r.class, ColumnChangeClass::Breaking);
        assert_eq!(r.affected_columns, vec!["down.status".to_string()]);
        assert_eq!(r.affected_models, vec!["down".to_string()]);
        assert_eq!(r.affected_marts, vec!["down".to_string()]);
        assert_eq!(
            r.affected_exposures,
            vec!["orders_dash (dashboard, owner: Fin <fin@x.io>)".to_string()],
            "the exposure on the affected consumer is named"
        );
        assert_eq!(diff.breaking_referenced().len(), 1, "{diff:?}");
        assert!(diff.columns_analyzed.contains("model.t.mid"));
    }

    #[test]
    fn column_removed_unreferenced_is_non_breaking() {
        // mid drops `status` but nothing ever consumed it — the removal is
        // informational, not breaking, and the gate's set stays empty.
        let base = col_dag(
            "select u.id, u.status from \"up\" u",
            &[("id", "int"), ("status", "text")],
            "select m.id from \"mid\" m",
            &[("id", "int")],
            false,
        );
        let current = col_dag(
            "select u.id from \"up\" u",
            &[("id", "int")],
            "select m.id from \"mid\" m",
            &[("id", "int")],
            false,
        );
        let diff = compute_diff(&base, &current);
        let r = row(&diff, "model.t.mid", "status", ColumnChangeKind::Removed);
        assert_eq!(r.class, ColumnChangeClass::NonBreaking);
        assert!(r.affected_columns.is_empty());
        assert!(diff.breaking_referenced().is_empty());
    }

    #[test]
    fn column_removed_migrated_consumer_is_not_dangling() {
        // mid drops `status`; down rewired its `status` to a DIFFERENT
        // node's column (`o.status`) — a real migration: it exists
        // current-side, resolves upstream, and the closure never reaches
        // mid → excluded from the dangling set, so the removal reads
        // non-breaking.
        let base = col_dag(
            "select u.id, u.status from \"up\" u",
            &[("id", "int"), ("status", "text")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            false,
        );
        // current: `other` (a sibling stage) still emits `status`, and down
        // now draws it from there — `select o.status`. The node-level
        // mid -> down edge stays; the COLUMN provenance moved off mid.
        let current = Dag::build(&col_manifest(
            vec![
                (
                    "model.t.up",
                    col_model(
                        "up",
                        "staging",
                        "select 1",
                        &[("id", "int"), ("status", "text")],
                    ),
                ),
                (
                    "model.t.other",
                    col_model("other", "staging", "select 1", &[("status", "text")]),
                ),
                (
                    "model.t.mid",
                    col_model(
                        "mid",
                        "staging",
                        "select u.id from \"up\" u",
                        &[("id", "int")],
                    ),
                ),
                (
                    "model.t.down",
                    col_model(
                        "down",
                        "marts",
                        "select o.status from \"other\" o",
                        &[("status", "text")],
                    ),
                ),
            ],
            &[
                ("model.t.up", "model.t.mid"),
                ("model.t.mid", "model.t.down"),
                ("model.t.other", "model.t.down"),
            ],
            vec![],
        ));
        let diff = compute_diff(&base, &current);
        let r = row(&diff, "model.t.mid", "status", ColumnChangeKind::Removed);
        assert_eq!(
            r.class,
            ColumnChangeClass::NonBreaking,
            "down.status now draws from other.status — migrated: {r:?}"
        );
        assert!(diff.breaking_referenced().is_empty());
    }

    #[test]
    fn column_rename_pairs_by_shared_upstream_and_flags_unmigrated() {
        // mid: `status` -> `order_status` (same u.status source); down still
        // reads `m.status` → the old name dangles → breaking, and the pair
        // is recorded instead of a removed+added split.
        let base = col_dag(
            "select u.id, u.status from \"up\" u",
            &[("id", "int"), ("status", "text")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            true,
        );
        let current = col_dag(
            "select u.id, u.status as order_status from \"up\" u",
            &[("id", "int"), ("order_status", "text")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            true,
        );
        let diff = compute_diff(&base, &current);
        let r = row(
            &diff,
            "model.t.mid",
            "order_status",
            ColumnChangeKind::Renamed,
        );
        assert_eq!(r.renamed_from.as_deref(), Some("status"));
        assert_eq!(r.class, ColumnChangeClass::Breaking);
        assert_eq!(r.affected_columns, vec!["down.status".to_string()]);
        let rows = &diff.columns["model.t.mid"];
        assert!(
            !rows
                .iter()
                .any(|r| r.column == "status" && r.kind == ColumnChangeKind::Removed),
            "the rename consumes the removed name: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|r| r.column == "order_status" && r.kind == ColumnChangeKind::Added),
            "the rename consumes the added name: {rows:?}"
        );
        assert_eq!(diff.breaking_referenced().len(), 1);
    }

    #[test]
    fn column_rename_fully_migrated_is_non_breaking() {
        // Same rename, but down rewrote to `m.order_status as status`: the
        // old name is gone yet nothing still reads it — informational.
        let base = col_dag(
            "select u.id, u.status from \"up\" u",
            &[("id", "int"), ("status", "text")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            false,
        );
        let current = col_dag(
            "select u.id, u.status as order_status from \"up\" u",
            &[("id", "int"), ("order_status", "text")],
            "select m.id, m.order_status as status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            false,
        );
        let diff = compute_diff(&base, &current);
        let r = row(
            &diff,
            "model.t.mid",
            "order_status",
            ColumnChangeKind::Renamed,
        );
        assert_eq!(r.class, ColumnChangeClass::NonBreaking, "{r:?}");
        assert!(r.affected_columns.is_empty());
        assert!(diff.breaking_referenced().is_empty());
    }

    #[test]
    fn column_type_change_is_partial_and_spreads_current_side() {
        // mid's declared `amount` type moves int -> bigint; down consumes
        // m.amount — a partial move flagged with both types, spreading
        // through the CURRENT lineage (down.amount still resolves).
        let base = col_dag(
            "select u.id, u.amount from \"up\" u",
            &[("id", "int"), ("amount", "int")],
            "select m.id, m.amount from \"mid\" m",
            &[("id", "int"), ("amount", "int")],
            true,
        );
        let current = col_dag(
            "select u.id, u.amount from \"up\" u",
            &[("id", "int"), ("amount", "bigint")],
            "select m.id, m.amount from \"mid\" m",
            &[("id", "int"), ("amount", "int")],
            true,
        );
        let diff = compute_diff(&base, &current);
        let r = row(
            &diff,
            "model.t.mid",
            "amount",
            ColumnChangeKind::TypeChanged,
        );
        assert_eq!(r.class, ColumnChangeClass::Partial);
        assert_eq!(r.type_from.as_deref(), Some("int"));
        assert_eq!(r.type_to.as_deref(), Some("bigint"));
        assert_eq!(r.affected_columns, vec!["down.amount".to_string()]);
        assert!(diff.breaking_referenced().is_empty(), "partial never gates");
    }

    #[test]
    fn column_expr_change_is_partial_but_formatting_is_not() {
        // upper -> lower is a real expression change on status_uc; the SAME
        // expression reflowed + commented is not.
        let mk = |mid_sql: &str| {
            col_dag(
                mid_sql,
                &[("id", "int"), ("status_uc", "text")],
                "select m.id, m.status_uc from \"mid\" m",
                &[("id", "int"), ("status_uc", "text")],
                false,
            )
        };
        let base = mk("select u.id, upper(u.status) as status_uc from \"up\" u");
        let current = mk("select u.id, lower(u.status) as status_uc from \"up\" u");
        let diff = compute_diff(&base, &current);
        let r = row(
            &diff,
            "model.t.mid",
            "status_uc",
            ColumnChangeKind::ExprChanged,
        );
        assert_eq!(r.class, ColumnChangeClass::Partial);
        assert_eq!(r.affected_columns, vec!["down.status_uc".to_string()]);

        // Formatting/comments/case: the normalized AST expr strings match.
        let reformatted =
            mk("select  u.id,\n    UPPER( u.status )  AS  status_uc -- louder\nfrom \"up\" u");
        let diff = compute_diff(&base, &reformatted);
        assert!(
            diff.modified.contains_key("model.t.mid"),
            "raw SQL moved (formatting) → modified, {diff:?}"
        );
        assert!(
            !diff.columns.contains_key("model.t.mid"),
            "normalized exprs equal → no column rows: {:?}",
            diff.columns
        );
        assert!(
            diff.columns_analyzed.contains("model.t.mid"),
            "…but the node WAS analyzed — proven column-clean"
        );
    }

    #[test]
    fn column_added_is_non_breaking() {
        // A new output column reaches nothing yet — informational only.
        let base = col_dag(
            "select u.id from \"up\" u",
            &[("id", "int")],
            "select m.id from \"mid\" m",
            &[("id", "int")],
            false,
        );
        let current = col_dag(
            "select u.id, u.status as region from \"up\" u",
            &[("id", "int"), ("region", "text")],
            "select m.id from \"mid\" m",
            &[("id", "int")],
            false,
        );
        let diff = compute_diff(&base, &current);
        let r = row(&diff, "model.t.mid", "region", ColumnChangeKind::Added);
        assert_eq!(r.class, ColumnChangeClass::NonBreaking);
        assert!(r.affected_columns.is_empty());
    }

    #[test]
    fn column_diff_is_deterministic() {
        let base = col_dag(
            "select u.id, u.status from \"up\" u",
            &[("id", "int"), ("status", "text")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            true,
        );
        let current = col_dag(
            "select u.id from \"up\" u",
            &[("id", "int")],
            "select m.id, m.status from \"mid\" m",
            &[("id", "int"), ("status", "text")],
            true,
        );
        assert_eq!(compute_diff(&base, &current), compute_diff(&base, &current));
    }
}
