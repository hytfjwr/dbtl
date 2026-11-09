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

use crate::{Dag, NodeDetail};

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
}

impl DagDiff {
    /// Whether the two Dags are identical under the diff's definition.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.modified.is_empty()
            && self.edges_added.is_empty()
            && self.edges_removed.is_empty()
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
pub fn compute_diff(base: &Dag, current: &Dag) -> DagDiff {
    let mut added: BTreeSet<String> = BTreeSet::new();
    let mut modified: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for uid in current.nodes().keys() {
        match base.get(uid) {
            None => {
                added.insert(uid.clone());
            }
            Some(_) => {
                let reasons = modified_reasons(base, current, uid);
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

    DagDiff {
        added,
        removed,
        modified,
        edges_added,
        edges_removed,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RawConfig, RawManifest, RawNode, RawTestMetadata};
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
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            parent_map,
            child_map,
        }
    }

    fn base_dag() -> Dag {
        Dag::build(&manifest(model("b", Some("table"), "select 2"), None))
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
                nodes: HashMap::from([
                    ("seed.p.fiscal".to_string(), seed_node),
                    ("snapshot.p.snap".to_string(), snap_node),
                ]),
                sources: HashMap::new(),
                exposures: HashMap::new(),
                parent_map: HashMap::new(),
                child_map: HashMap::new(),
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
}
