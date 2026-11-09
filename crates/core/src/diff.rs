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
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
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
}
