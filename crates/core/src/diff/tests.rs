use super::*;
use crate::manifest::{RawConfig, RawDependsOn, RawMacro, RawManifest, RawNode, RawTestMetadata};
use std::collections::HashMap;

fn model(name: &str, materialized: Option<&str>, sql: &str) -> RawNode {
    RawNode {
        name: name.into(),
        resource_type: "model".into(),
        path: Some(format!("marts/{name}.sql")),
        raw_code: Some(sql.to_string()),
        config: RawConfig {
            materialized: materialized.map(String::from),
            ..Default::default()
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
    let mut child_map = HashMap::from([("model.p.a".to_string(), vec!["model.p.b".to_string()])]);
    let mut parent_map = HashMap::from([("model.p.b".to_string(), vec!["model.p.a".to_string()])]);
    if let Some((uid, node)) = extra {
        nodes.insert(uid.to_string(), node);
        parent_map.insert(uid.to_string(), vec!["model.p.b".to_string()]);
        child_map.insert("model.p.b".to_string(), vec![uid.to_string()]);
    }
    RawManifest {
        unresolved_adjacent: Default::default(),
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
        docs: Default::default(),
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
            unresolved_adjacent: Default::default(),
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
            docs: Default::default(),
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
        unresolved_adjacent: Default::default(),
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
        docs: Default::default(),
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

fn row<'a>(diff: &'a DagDiff, uid: &str, col: &str, kind: ColumnChangeKind) -> &'a ColumnChange {
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
    assert_eq!(
        r.affected_columns,
        vec!["down.id".to_string(), "down.status".to_string()],
        "the still-written `m.status` ref means down's query can't \
         compile — every column it emits dies, not just the one that \
         named the dead column"
    );
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
fn column_removed_referenced_in_join_predicate_is_breaking() {
    // The under-flag shape: down references `m.status` ONLY in the JOIN
    // condition — a FilterOrJoin dependency that never carried the
    // column's value. `mid.status` disappears; down's SQL is unchanged,
    // so the join still names a column that doesn't exist — the query
    // can't run and `down.id` (which never read `status` as a value)
    // is broken with it.
    let base = Dag::build(&col_manifest(
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
                col_model(
                    "other",
                    "staging",
                    "select 1",
                    &[("id", "int"), ("s", "text")],
                ),
            ),
            (
                "model.t.mid",
                col_model(
                    "mid",
                    "staging",
                    "select u.id, u.status from \"up\" u",
                    &[("id", "int"), ("status", "text")],
                ),
            ),
            (
                "model.t.down",
                col_model(
                    "down",
                    "marts",
                    "select o.id from \"other\" o join \"mid\" m on m.status = o.s",
                    &[("id", "int")],
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
    // Current: mid drops `status`; down's join predicate is untouched —
    // `m.status` still written, now resolving to nothing.
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
                col_model(
                    "other",
                    "staging",
                    "select 1",
                    &[("id", "int"), ("s", "text")],
                ),
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
                    "select o.id from \"other\" o join \"mid\" m on m.status = o.s",
                    &[("id", "int")],
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
        ColumnChangeClass::Breaking,
        "a FOJ-only reference still dangling must gate: {r:?}"
    );
    assert_eq!(r.affected_columns, vec!["down.id".to_string()]);
}

#[test]
fn column_removed_referenced_in_where_clause_is_breaking() {
    // Same shape through a WHERE predicate (`stg_orders … where
    // u.disabled_flag = false` then the flag goes away): the filter
    // still names the dead column, the model can't compile, and its
    // lone output dies with it.
    let base = col_dag(
        "select u.id, u.status from \"up\" u",
        &[("id", "int"), ("status", "text")],
        "select m.id from \"mid\" m where m.status = 'x'",
        &[("id", "int")],
        false,
    );
    let current = col_dag(
        "select u.id from \"up\" u",
        &[("id", "int")],
        "select m.id from \"mid\" m where m.status = 'x'",
        &[("id", "int")],
        false,
    );
    let diff = compute_diff(&base, &current);
    let r = row(&diff, "model.t.mid", "status", ColumnChangeKind::Removed);
    assert_eq!(
        r.class,
        ColumnChangeClass::Breaking,
        "the WHERE still names the dead column: {r:?}"
    );
    assert_eq!(r.affected_columns, vec!["down.id".to_string()]);
}

#[test]
fn column_removed_where_clause_deleted_is_migrated() {
    // The over-flag shape: mid drops `status` AND down drops the WHERE
    // that referenced it in the same change. The baseline FOJ edge is
    // gone current-side — but so is the reference itself; down.id
    // keeps a live value source (`m.id`) and reads as migrated, not
    // dangling.
    let base = col_dag(
        "select u.id, u.status from \"up\" u",
        &[("id", "int"), ("status", "text")],
        "select m.id from \"mid\" m where m.status = 'x'",
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
    assert_eq!(
        r.class,
        ColumnChangeClass::NonBreaking,
        "down deleted the reference — migrated, not broken: {r:?}"
    );
    assert!(r.affected_columns.is_empty());
    assert!(diff.breaking_referenced().is_empty());
}

#[test]
fn column_removed_consumer_repointed_at_sibling_is_migrated() {
    // Over-flag, sibling-column variant: down repointed `status` from
    // `m.status` to `m.id` in the same change that dropped it. The old
    // column is gone but the consumer's wiring is live — migrated.
    let base = col_dag(
        "select u.id, u.status from \"up\" u",
        &[("id", "int"), ("status", "text")],
        "select m.id, m.status from \"mid\" m",
        &[("id", "int"), ("status", "text")],
        false,
    );
    let current = col_dag(
        "select u.id from \"up\" u",
        &[("id", "int")],
        "select m.id, m.id as status from \"mid\" m",
        &[("id", "int"), ("status", "text")],
        false,
    );
    let diff = compute_diff(&base, &current);
    let r = row(&diff, "model.t.mid", "status", ColumnChangeKind::Removed);
    assert_eq!(
        r.class,
        ColumnChangeClass::NonBreaking,
        "down.status now draws from m.id — a live sibling: {r:?}"
    );
    assert!(r.affected_columns.is_empty());
}

#[test]
fn column_removed_consumer_output_renamed_still_dangles() {
    // The consumer-side rename shape: down renamed its OUTPUT `a` ->
    // `b` but the expression still reads `m.status`. The old output
    // name is gone current-side, but the REFERENCE is still written —
    // the query can't compile, so `down.b` is the dangling casualty.
    let base = col_dag(
        "select u.id, u.status from \"up\" u",
        &[("id", "int"), ("status", "text")],
        "select m.status as a from \"mid\" m",
        &[("a", "text")],
        false,
    );
    let current = col_dag(
        "select u.id from \"up\" u",
        &[("id", "int")],
        "select m.status as b from \"mid\" m",
        &[("b", "text")],
        false,
    );
    let diff = compute_diff(&base, &current);
    let r = row(&diff, "model.t.mid", "status", ColumnChangeKind::Removed);
    assert_eq!(
        r.class,
        ColumnChangeClass::Breaking,
        "renaming the output doesn't migrate the reference: {r:?}"
    );
    assert_eq!(r.affected_columns, vec!["down.b".to_string()]);
}

#[test]
fn column_rename_with_type_change_emits_both_rows() {
    // A rename that also retypes: `status` -> `order_status` with the
    // declared type moving int -> text. The rename consumes the
    // removed+added names; the type move still earns its own
    // TypeChanged row through the (old -> new) compare pair.
    let base = col_dag(
        "select u.id, u.status from \"up\" u",
        &[("id", "int"), ("status", "int")],
        "select m.id, m.status from \"mid\" m",
        &[("id", "int"), ("status", "int")],
        false,
    );
    let current = col_dag(
        "select u.id, u.status as order_status from \"up\" u",
        &[("id", "int"), ("order_status", "text")],
        "select m.id, m.order_status from \"mid\" m",
        &[("id", "int"), ("order_status", "text")],
        false,
    );
    let diff = compute_diff(&base, &current);
    let rows = &diff.columns["model.t.mid"];
    let renamed = rows
        .iter()
        .find(|r| r.column == "order_status" && r.kind == ColumnChangeKind::Renamed)
        .expect("the rename row exists");
    assert_eq!(renamed.renamed_from.as_deref(), Some("status"));
    let typed = rows
        .iter()
        .find(|r| r.column == "order_status" && r.kind == ColumnChangeKind::TypeChanged)
        .expect("the composite type-change row exists");
    assert_eq!(typed.type_from.as_deref(), Some("int"));
    assert_eq!(typed.type_to.as_deref(), Some("text"));
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
    assert_eq!(
        r.affected_columns,
        vec!["down.id".to_string(), "down.status".to_string()],
        "down's query still names `m.status` → unresolvable → the whole \
         model's outputs are casualties"
    );
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
