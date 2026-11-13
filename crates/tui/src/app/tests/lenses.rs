use super::*;

// ---- Step B: test-coverage lens ('t') + automatic root↔cursor path ----

/// The one fixture node that is a coverage gap (testable, zero tests): the
/// delivery-lanes snapshot. All 45 models and all 7 seeds carry
/// tests, so the snapshot is the sole gap (verified against the fixture).
const GAP_SNAPSHOT: &str = "snapshot.jaffle_finance.delivery_lanes_snapshot";

#[test]
fn coverage_gap_predicate_over_resource_type_and_tests() {
    let a = app();
    // A tested model is NOT a gap; the snapshot with zero tests IS.
    let txn = a.dag.get("model.jaffle_finance.pos_txn").unwrap();
    assert!(
        txn.test_count > 0 && !coverage_gap(txn),
        "tested model: no gap"
    );
    let snap = a.dag.get(GAP_SNAPSHOT).unwrap();
    assert!(
        snap.test_count == 0 && coverage_gap(snap),
        "untested snapshot is a coverage gap"
    );
    // A source is NEVER a gap even with zero tests (excluded by type).
    let src = a
        .dag
        .nodes()
        .values()
        .find(|n| n.resource_type == "source" && n.test_count == 0)
        .expect("fixture has an untested source");
    assert!(!coverage_gap(src), "a source is never a coverage gap");
    // A synthetic untested model IS a gap (predicate is pure over NodeInfo).
    let m = NodeInfo {
        resource_type: "model".into(),
        test_count: 0,
        ..Default::default()
    };
    assert!(coverage_gap(&m), "an untested model is a gap");
}

#[test]
fn coverage_summary_counts_testable_resources() {
    // Fixture: 45 models + 7 seeds + 1 snapshot = 53 testable; only the
    // snapshot is untested → 52 tested.
    let a = app();
    assert_eq!(
        a.coverage_summary(),
        (52, 53),
        "coverage over model/snapshot/seed: 52 of 53 carry tests"
    );
}

#[test]
fn lineage_path_set_empty_when_cursor_home() {
    // Cursor home (== selection): the set is empty so a home cursor
    // highlights nothing.
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert_eq!(a.lineage_cursor_uid().as_deref(), Some(FCT), "cursor home");
    assert!(
        a.lineage_path_set().is_empty(),
        "a home cursor produces an empty path set"
    );
}

#[test]
fn lineage_path_set_connects_root_to_an_upstream_cursor() {
    // Off-root (upstream) cursor: the path contains both endpoints, every
    // member is adjacent to another in the (undirected) subgraph edges, and
    // the result is deterministic across two calls.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft); // cursor → one column upstream
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur, FCT, "cursor left the root");

    let path = a.lineage_path_set();
    assert!(path.contains(FCT), "path includes the root");
    assert!(path.contains(&cur), "path includes the cursor");
    assert!(path.len() >= 2);

    // Connectivity: every path node touches another path node via some edge
    // (treated undirected), so the highlighted set is one connected chain.
    let sg = a.lineage_subgraph();
    for uid in &path {
        let touches = sg.edges.iter().any(|e| {
            (e.parent == *uid && path.contains(&e.child))
                || (e.child == *uid && path.contains(&e.parent))
        });
        assert!(touches, "path node {uid} is connected to another path node");
    }
    // Deterministic.
    assert_eq!(path, a.lineage_path_set(), "two calls are identical");
}

#[test]
fn lineage_path_set_is_undirected_for_a_downstream_cursor() {
    // The cursor can sit DOWNSTREAM of the root (not just upstream): the
    // undirected BFS must still connect them. Root a node and place the
    // cursor on a known downstream model.
    let root = "model.jaffle_finance.int_delivery_lanes__combined";
    let down_model = "model.jaffle_finance.fct_delivery_monthly_snapshot";
    let mut a = app();
    a.select_by_unique_id(root);
    assert!(
        a.dag.downstream(root).contains(down_model),
        "precondition: the model is downstream of the root"
    );
    assert!(
        a.lineage_subgraph().contains(down_model),
        "the downstream model is in the rooted subgraph"
    );
    a.tab_mut().lineage_cursor = Some(down_model.to_string());
    let path = a.lineage_path_set();
    assert!(path.contains(root), "undirected path includes the root");
    assert!(
        path.contains(down_model),
        "undirected path includes the downstream cursor"
    );
    // Connected chain (undirected) and deterministic.
    let sg = a.lineage_subgraph();
    for uid in &path {
        let touches = sg.edges.iter().any(|e| {
            (e.parent == *uid && path.contains(&e.child))
                || (e.child == *uid && path.contains(&e.parent))
        });
        assert!(touches, "downstream path node {uid} stays connected");
    }
    assert_eq!(path, a.lineage_path_set(), "deterministic");
}

#[test]
fn coverage_lens_sets_warn_tint_only_when_active() {
    // Root on the gap snapshot so its own box is in the layout. With the lens
    // Off no attr carries a tint; cycling to Coverage marks the gap snapshot
    // Warn while a tested upstream model and a source stay None.
    let mut a = app();
    a.select_by_unique_id(FCT); // any selectable model; we layout the snapshot subgraph below
    let lay = crate::layout(&a.dag.subgraph(GAP_SNAPSHOT));

    let styles_off = a.lineage_styles(&lay);
    assert!(
        styles_off.values().all(|c| c.lens == LensTint::None),
        "lens off: nothing is tinted"
    );

    // One `t` press from default == Coverage (the old behaviour).
    a.ui_state.cycle_lens();
    assert_eq!(a.ui_state.lens(), LineageLens::Coverage);
    let styles_on = a.lineage_styles(&lay);
    assert_eq!(
        styles_on.get(GAP_SNAPSHOT).unwrap().lens,
        LensTint::Warn,
        "Coverage lens: the gap snapshot is Warn-tinted"
    );
    // A tested upstream model is NOT tinted.
    let tested_up = "model.jaffle_finance.stg_masterdata__companies";
    assert!(
        styles_on.contains_key(tested_up),
        "precondition: tested model is in the snapshot subgraph"
    );
    assert_eq!(
        styles_on.get(tested_up).unwrap().lens,
        LensTint::None,
        "a tested model is not tinted under the coverage lens"
    );
    // A source upstream is never tinted (excluded by coverage_gap).
    let src = lay
        .rects
        .keys()
        .find(|uid| uid.starts_with("source."))
        .expect("the snapshot subgraph has an upstream source");
    assert_eq!(
        styles_on.get(src).unwrap().lens,
        LensTint::None,
        "a source is never tinted under the coverage lens"
    );
}

#[test]
fn degree_heat_lens_buckets_by_transitive_downstream() {
    // Fixture-anchored buckets (computed, not guessed): pos_files__assignment
    // has 16 transitive downstream (HeatHigh), stg_payment__shoppers has 1
    // (HeatLow), and the leaf int_shoppers__combined has 0 (None). The heat
    // metric is the raw Dag closure — deliberately UNSPLIT, so the fixture's 2
    // exposures count as consumers: FCT lands at 4 (HeatMid) and
    // stg_payment__suppliers at 7, crossing into HeatHigh.
    let mut a = app();
    let hub = "model.jaffle_finance.pos_files__assignment";
    let suppliers = "model.jaffle_finance.stg_payment__suppliers";
    let low = "model.jaffle_finance.stg_payment__shoppers";
    let leaf = "model.jaffle_finance.int_shoppers__combined";
    assert_eq!(a.dag.downstream(hub).len(), 16, "fixture hub blast radius");
    assert_eq!(
        a.dag.downstream(suppliers).len(),
        7,
        "suppliers blast radius (5 models + 2 exposures)"
    );
    assert_eq!(
        a.dag.downstream(FCT).len(),
        4,
        "fixture FCT blast radius (2 models + 2 exposures)"
    );
    assert_eq!(a.dag.downstream(low).len(), 1, "fixture low blast radius");
    assert_eq!(a.dag.downstream(leaf).len(), 0, "fixture leaf");
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, hub),
        LensTint::HeatHigh
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, suppliers),
        LensTint::HeatHigh,
        "the exposure consumers push suppliers across the high boundary"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, FCT),
        LensTint::HeatMid,
        "the exposure consumers push FCT into the mid bucket"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, low),
        LensTint::HeatLow
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::DegreeHeat, leaf),
        LensTint::None
    );
}

#[test]
fn layer_lens_tints_models_by_layer_and_leaves_non_models_untinted() {
    // Each layer maps to its own tint; a source keeps its class colour (None).
    let mut a = app();
    let staging = "model.jaffle_finance.stg_payment__shoppers";
    let inter = "model.jaffle_finance.int_shoppers__combined";
    let marts = FCT; // fct_subscription_process is a marts model
    assert_eq!(
        tint_of(&mut a, LineageLens::Layer, staging),
        LensTint::LayerStaging
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::Layer, inter),
        LensTint::LayerIntermediate
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::Layer, marts),
        LensTint::LayerMarts
    );
    // A source upstream of stg_payment__shoppers gets None (its class colour).
    a.select_by_unique_id(staging);
    while a.ui_state.lens() != LineageLens::Layer {
        a.ui_state.cycle_lens();
    }
    let lay = crate::layout(&a.lineage_subgraph());
    let styles = a.lineage_styles(&lay);
    let src = lay
        .rects
        .keys()
        .find(|uid| uid.starts_with("source."))
        .expect("staging subgraph has an upstream source");
    assert_eq!(
        styles.get(src).unwrap().lens,
        LensTint::None,
        "a source is untinted under the layer lens (keeps its class colour)"
    );
}

#[test]
fn layer_violation_edges_empty_on_a_clean_fixture_and_no_violation_tint() {
    // The committed fixture is a clean dbt project: no marts→staging-style
    // backward edge, so the violation set is empty and no node is tinted.
    let mut a = app();
    assert!(
        layer_violation_edges(&a.dag).is_empty(),
        "the clean fixture has no layer-violation edges"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::LayerViolation, FCT),
        LensTint::None,
        "no node is violation-tinted on a clean project"
    );
}

/// A tiny synthetic Dag with a deliberate layer violation: a `marts` model
/// (`mt`) feeds a `staging` model (`st`) — a backward edge — plus a clean
/// `staging → intermediate` edge (`st → it`) that is NOT a violation.
fn violation_dag() -> Dag {
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    let mut add = |id: &str, name: &str, path: &str| {
        nodes.insert(
            id.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(path.into()),
                ..Default::default()
            },
        );
    };
    add("model.p.mt", "mt", "marts/mt.sql");
    add("model.p.st", "st", "staging/st.sql");
    add("model.p.it", "it", "intermediate/it.sql");
    // child_map: mt → st (BACKWARD), st → it (clean). parent_map mirrors it.
    let mut child_map = HashMap::new();
    child_map.insert("model.p.mt".to_string(), vec!["model.p.st".to_string()]);
    child_map.insert("model.p.st".to_string(), vec!["model.p.it".to_string()]);
    let mut parent_map = HashMap::new();
    parent_map.insert("model.p.st".to_string(), vec!["model.p.mt".to_string()]);
    parent_map.insert("model.p.it".to_string(), vec!["model.p.st".to_string()]);
    Dag::build(&RawManifest {
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
        docs: Default::default(),
    })
}

#[test]
fn layer_violation_edges_finds_the_backward_edge_only() {
    // Exactly the marts→staging edge is a violation; the clean staging→inter
    // edge is not. Deterministic (sorted) output.
    let dag = violation_dag();
    assert_eq!(
        layer_violation_edges(&dag),
        vec![("model.p.mt".to_string(), "model.p.st".to_string())],
        "only the marts→staging backward edge is a violation"
    );
}

#[test]
fn violation_lens_tints_both_incident_nodes_and_spares_the_rest() {
    // Root on the marts model so the whole synthetic graph is laid out; under
    // the violation lens the two incident nodes (mt, st) are Violation-tinted,
    // the clean downstream `it` is not.
    let mut a = App::new(
        violation_dag(),
        PathBuf::from("/tmp/x/target/manifest.json"),
    );
    a.select_by_unique_id("model.p.mt");
    a.ui_state.cycle_lens(); // Coverage
    a.ui_state.cycle_lens(); // DegreeHeat
    a.ui_state.cycle_lens(); // Layer
    a.ui_state.cycle_lens(); // LayerViolation
    assert_eq!(a.ui_state.lens(), LineageLens::LayerViolation);
    let lay = crate::layout(&a.lineage_subgraph());
    let styles = a.lineage_styles(&lay);
    assert_eq!(
        styles.get("model.p.mt").unwrap().lens,
        LensTint::Violation,
        "the marts parent is violation-tinted"
    );
    assert_eq!(
        styles.get("model.p.st").unwrap().lens,
        LensTint::Violation,
        "the staging child is violation-tinted"
    );
    assert_eq!(
        styles.get("model.p.it").unwrap().lens,
        LensTint::None,
        "the clean downstream model is not violation-tinted"
    );
}

// ---- the Lint lens ----------------------------------------------------------

#[test]
fn lint_findings_are_computed_once_from_the_shared_rules() {
    // The fixture has 83 findings under the shipped defaults — the 2
    // error-severity gate findings plus the warn-severity set (the
    // structure rules' 25 + 11 `unused_sources` + 6
    // `incremental_without_unique_key` + 38 `sources_without_freshness`
    // + 1 `unused_models`, frozen counts in tests/manifest_fixture.rs) —
    // the App computes them once at construction and every surface
    // (lens, stats worklist) reads that one vector.
    let a = app();
    assert_eq!(a.lint_findings().len(), 83, "the frozen fixture count");
    assert!(
        a.lint_findings()
            .iter()
            .any(|f| f.node_uid == "model.jaffle_finance.pos_cat"),
        "pos_cat is a violating node"
    );
}

#[test]
fn lint_lens_tints_every_finding_node_by_worst_severity() {
    // The lens marks a node by its WORST finding severity: an error-finding
    // node is LintError (danger), a warn-only node is LintWarn (warn), and a
    // lint-clean model keeps its class colour. The fixture's warn findings
    // live on `unused_sources` violations, so a warn-only MODEL needs a
    // config that pins a model rule to warn.
    let mut a = app();
    let error_uid = a
        .lint_findings()
        .iter()
        .find(|f| f.severity == crate::Severity::Error)
        .map(|f| f.node_uid.clone())
        .expect("the fixture has error-severity findings");
    assert_eq!(
        tint_of(&mut a, LineageLens::Lint, &error_uid),
        LensTint::LintError,
        "{error_uid} has error findings → LintError"
    );
    let linted: std::collections::HashSet<&str> = a
        .lint_findings()
        .iter()
        .map(|f| f.node_uid.as_str())
        .collect();
    let clean = a
        .dag
        .nodes()
        .values()
        .find(|n| n.resource_type == "model" && !linted.contains(n.unique_id.as_str()))
        .expect("the fixture has lint-clean models")
        .unique_id
        .clone();
    assert_eq!(
        tint_of(&mut a, LineageLens::Lint, &clean),
        LensTint::None,
        "{clean} has no findings → untinted"
    );
}

/// A staging→staging→marts chain: `staging_depends_on_staging`
/// (warn-default) hits the child `stg_b`, while the error-default gate
/// rules hit every untested/undocumented model — `stg_b` carries BOTH
/// severities, `stg_a`/`mart_c` error ones only.
fn lint_warn_dag() -> Dag {
    pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 1")),
            ("model.p.stg_b", pr_model("stg_b", "staging", "select 1")),
            ("model.p.mart_c", pr_model("mart_c", "marts", "select 1")),
        ],
        &[
            ("model.p.stg_a", "model.p.stg_b"),
            ("model.p.stg_b", "model.p.mart_c"),
        ],
        vec![],
    )
}

#[test]
fn lint_lens_warn_only_nodes_tint_warn_and_error_wins() {
    // `stg_b` carries a warn finding (`staging_depends_on_staging`) AND error
    // findings (the gate rules) — the lens marks it LintError: the worst
    // severity wins.
    let mut a = App::new(lint_warn_dag(), PathBuf::from("/p/target/manifest.json"));
    let sevs: Vec<crate::Severity> = a
        .lint_findings()
        .iter()
        .filter(|f| f.node_uid == "model.p.stg_b")
        .map(|f| f.severity)
        .collect();
    assert!(
        sevs.contains(&crate::Severity::Error) && sevs.contains(&crate::Severity::Warn),
        "stg_b carries both severities, got {sevs:?}"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::Lint, "model.p.stg_b"),
        LensTint::LintError,
        "error wins over warn on the same node"
    );

    // Turn every rule off except `staging_depends_on_staging` pinned to
    // warn: stg_b keeps only its warn finding → LintWarn; the other models
    // lose all findings → untinted (disabled rules never tint).
    let mut cfg = crate::LintConfig::default();
    for r in crate::LintRule::ALL {
        cfg.rules.insert(r, crate::RuleEnabled::Off);
    }
    cfg.rules.insert(
        crate::LintRule::StagingDependsOnStaging,
        crate::RuleEnabled::On(Some(crate::Severity::Warn)),
    );
    a.set_lint_config(cfg);
    assert_eq!(
        tint_of(&mut a, LineageLens::Lint, "model.p.stg_b"),
        LensTint::LintWarn,
        "stg_b's only remaining finding is warn → LintWarn"
    );
    assert_eq!(
        tint_of(&mut a, LineageLens::Lint, "model.p.stg_a"),
        LensTint::None,
        "no findings under this config → untinted"
    );
}

#[test]
fn lint_lens_is_skipped_in_the_cycle_when_no_findings_exist() {
    // Same dead-slot skip as Diff/RunStatus: with every rule disabled there are
    // no findings, so the cycle never lands on the Lint lens.
    let mut a = app();
    let mut cfg = crate::LintConfig::default();
    for r in crate::LintRule::ALL {
        cfg.rules.insert(r, crate::RuleEnabled::Off);
    }
    a.set_lint_config(cfg);
    assert!(a.lint_findings().is_empty(), "all rules off → no findings");
    for _ in 0..12 {
        apply_action(&mut a, Action::CycleLens);
        assert_ne!(
            a.ui_state.lens(),
            LineageLens::Lint,
            "Lint is skipped without findings"
        );
    }
    // And findings come back when a rule is re-enabled (set_lint_config
    // recomputes — the seam `reload` rides too).
    let mut on = crate::LintConfig::default();
    for r in crate::LintRule::ALL {
        on.rules.insert(r, crate::RuleEnabled::Off);
    }
    on.rules.remove(&crate::LintRule::UndocumentedModels);
    a.set_lint_config(on);
    assert!(
        a.lint_findings().is_empty(),
        "undocumented_models is clean on the fixture"
    );
}

#[test]
fn stats_view_carries_the_lint_worklist_from_the_shared_findings() {
    // The stats modal's worklist groups the SAME `run_rules` vector per rule —
    // the numbers can never drift from `dbtl check --lint` or the lens.
    let a = app();
    let sv = a.compute_stats_view();
    assert_eq!(
        sv.lint_findings.len(),
        10,
        "10 rules have findings (frozen)"
    );
    assert_eq!(sv.lint_findings[0].rule, "direct_join_to_source");
    assert_eq!(
        sv.lint_findings[0].names.len(),
        16,
        "the frozen count, sorted names"
    );
    assert!(sv.lint_findings[0].names.contains(&"pos_cat".to_string()));
    // Every group is sorted, non-empty, and carries the severity its
    // findings share (a rule's findings always share its configured
    // severity, so one field per group is lossless).
    for wl in &sv.lint_findings {
        assert!(
            !wl.names.is_empty(),
            "{} groups only list violations",
            wl.rule
        );
        let mut sorted = wl.names.clone();
        sorted.sort();
        assert_eq!(wl.names, sorted, "{} names are sorted", wl.rule);
        assert!(
            a.lint_findings()
                .iter()
                .filter(|f| f.rule.name() == wl.rule)
                .all(|f| f.severity == wl.severity),
            "{}'s worklist severity matches its findings'",
            wl.rule
        );
    }
    // The severity split — the frozen fixture's 2 gate findings are
    // error-severity (missing_primary_key_tests), the warn side 81 (the
    // structure rules' 25 + 11 `unused_sources` + 6
    // `incremental_without_unique_key` + 38 `sources_without_freshness`
    // + 1 `unused_models`, frozen in tests/manifest_fixture.rs), matching
    // `dbtl check --lint`'s counts.
    assert_eq!((sv.lint_errors, sv.lint_warnings), (2, 81));
}

#[test]
fn stats_view_column_coverage_matches_the_shared_counts() {
    // The stats dashboard's column coverage is the SHARED
    // `column_coverage_counts` — the exact numbers the docs README prints
    // (frozen at 212/322 on the jaffle_finance fixture, asserted in
    // tests/manifest_fixture.rs).
    let a = app();
    let sv = a.compute_stats_view();
    assert_eq!(
        (sv.col_tested, sv.col_declared),
        crate::column_coverage_counts(&a.dag, None),
        "the modal quotes the shared helper verbatim"
    );
    assert_eq!((sv.col_tested, sv.col_declared), (212, 322), "frozen");
}

#[test]
fn malformed_dbtl_yml_warns_via_notice_in_both_modes() {
    // The TUI's malformed-config policy: warn + shipped defaults, never block
    // the screen. Manifest mode finds `.dbtl.yml` at the project root above
    // `target/`; source mode finds it in the project dir itself.
    let root = tempfile::tempdir().unwrap();
    let manifest_path = root.path().join("target").join("manifest.json");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    std::fs::copy(FIXTURE, &manifest_path).unwrap();
    std::fs::write(
        root.path().join(".dbtl.yml"),
        "lint: [not, valid]\n", // malformed: lint must be a map
    )
    .unwrap();
    let dag = load_dag(&manifest_path).unwrap();
    let mut a = App::new(dag, manifest_path);
    let note = a.take_notice().expect("manifest mode warns once");
    assert!(
        note.contains(".dbtl.yml"),
        "manifest-mode notice names the bad file, got: {note}"
    );
    assert_eq!(a.take_notice(), None, "the toast is one-shot");

    // Source mode: same warning queued by with_source — and main.rs's
    // source-mode hint must APPEND (append_notice), not overwrite it.
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(proj.path().join(".dbtl.yml"), "lint: [nope]\n").unwrap();
    let dag = load_dag(FIXTURE).unwrap();
    let mut a = App::from_source(dag, proj.path().to_path_buf());
    a.append_notice("Source mode: lineage is inferred");
    let note = a.take_notice().expect("source mode warns once");
    assert!(note.contains(".dbtl.yml"), "lint warning kept: {note}");
    assert!(
        note.contains("Source mode"),
        "the source-mode hint rides along: {note}"
    );
}

#[test]
fn lineage_styles_sets_on_path_for_an_off_root_cursor() {
    // With an off-root cursor, exactly the root↔cursor path nodes carry
    // on_path; nodes off the path do not.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur, FCT);

    let path = a.lineage_path_set();
    // Build the layout the loop draws (display subgraph), then style it.
    let lay = crate::layout(&a.lineage_display_subgraph());
    let styles = a.lineage_styles(&lay);
    for (uid, attr) in &styles {
        assert_eq!(
            attr.on_path,
            path.contains(uid),
            "on_path matches the path set for {uid}"
        );
    }
    // The two endpoints are on the path.
    assert!(styles.get(FCT).unwrap().on_path, "root is on the path");
    assert!(styles.get(&cur).unwrap().on_path, "cursor is on the path");

    // Cursor home → no on_path anywhere.
    a.reset_lineage_cursor();
    let styles_home = a.lineage_styles(&lay);
    assert!(
        styles_home.values().all(|c| !c.on_path),
        "a home cursor highlights no path"
    );
}

#[test]
fn focus_dim_is_set_only_off_root_and_only_for_off_path_nodes() {
    // Focus dim is lens-INDEPENDENT (no lens active here): with the cursor at
    // home NOTHING is dimmed; with an off-root cursor, every node NOT on the
    // root↔cursor path is dimmed and the path nodes are not.
    let mut a = app();
    a.select_by_unique_id(FCT);

    // Home cursor → empty path → no dim anywhere (the diagram reads normally).
    let lay_home = crate::layout(&a.lineage_display_subgraph());
    let styles_home = a.lineage_styles(&lay_home);
    assert!(
        styles_home.values().all(|c| !c.dimmed),
        "a home cursor dims nothing"
    );

    // Off-root cursor → dim partitions exactly on the path set.
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur, FCT, "precondition: cursor walked off the root");
    let path = a.lineage_path_set();
    assert!(!path.is_empty(), "off-root path is non-empty");
    let lay = crate::layout(&a.lineage_display_subgraph());
    let styles = a.lineage_styles(&lay);
    for (uid, attr) in &styles {
        assert_eq!(
            attr.dimmed,
            !path.contains(uid),
            "off-path nodes are dimmed, path nodes are not ({uid})"
        );
    }
    // The two endpoints are on the path, so never dimmed.
    assert!(!styles.get(FCT).unwrap().dimmed, "root never dims");
    assert!(!styles.get(&cur).unwrap().dimmed, "cursor never dims");
    // At least one off-path node exists in fct's subgraph and IS dimmed.
    assert!(
        styles.values().any(|c| c.dimmed),
        "fct's subgraph has off-path nodes that dim"
    );
}

#[test]
fn cycle_lens_advances_the_lens_as_a_noop_action() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.ui_state.selected();
    assert_eq!(a.ui_state.lens(), LineageLens::Off, "lens starts Off");
    let out = apply_action(&mut a, Action::CycleLens);
    assert!(
        !out.quit && out.effects.is_empty(),
        "cycle is a no-op action"
    );
    assert!(matches!(a.mode, Mode::Selection), "stays in Selection");
    assert_eq!(
        a.ui_state.lens(),
        LineageLens::Coverage,
        "advanced to Coverage"
    );
    // Cycle through the rest and back to Off. The fixture has lint findings so
    // the Lint slot joins the ring (no baseline/run_results → Diff and the run
    // lenses are skipped); PrimaryKey is data-free and never skipped.
    for expected in [
        LineageLens::DegreeHeat,
        LineageLens::Layer,
        LineageLens::LayerViolation,
        LineageLens::Lint,
        LineageLens::PrimaryKey,
        LineageLens::Off,
    ] {
        apply_action(&mut a, Action::CycleLens);
        assert_eq!(a.ui_state.lens(), expected);
    }
    assert_eq!(a.ui_state.selected(), before, "selection never moved");
}

#[test]
fn toggle_minimap_flips_the_flag_as_a_noop_action() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.ui_state.selected();
    assert!(
        !a.ui_state.minimap_visible(),
        "minimap starts off (default)"
    );
    let out = apply_action(&mut a, Action::ToggleMinimap);
    assert!(
        !out.quit && out.effects.is_empty(),
        "toggle is a no-op action"
    );
    assert!(matches!(a.mode, Mode::Selection), "stays in Selection");
    assert!(a.ui_state.minimap_visible(), "the flag flipped on");
    apply_action(&mut a, Action::ToggleMinimap);
    assert!(!a.ui_state.minimap_visible(), "and flips back off");
    assert_eq!(a.ui_state.selected(), before, "selection never moved");
}

#[test]
fn pane_width_keys_step_the_saved_list_width_by_four() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.ui_state.selected();
    assert_eq!(
        a.ui_state.list_width(),
        DEFAULT_LIST_WIDTH,
        "34-col default"
    );

    let out = apply_action(&mut a, Action::PaneWiden);
    assert!(
        !out.quit && out.effects.is_empty(),
        "a pure view-pref write"
    );
    assert!(matches!(a.mode, Mode::Selection));
    assert_eq!(a.ui_state.list_width(), 38, "`>` widens by the 4-col step");
    apply_action(&mut a, Action::PaneNarrow);
    apply_action(&mut a, Action::PaneNarrow);
    assert_eq!(a.ui_state.list_width(), 30, "`<` narrows by the same step");
    assert_eq!(a.ui_state.selected(), before, "selection never moved");
}

#[test]
fn pane_narrow_stops_at_the_floor_and_reset_restores_defaults() {
    let mut a = app();
    // `<` past the floor (18 cols) clamps, never wraps.
    for _ in 0..10 {
        apply_action(&mut a, Action::PaneNarrow);
    }
    assert_eq!(
        a.ui_state.list_width(),
        MIN_LIST_WIDTH,
        "the saved width floors, it never wraps"
    );
    // `=` restores BOTH panes' defaults — the inspector's saved value too,
    // though its pane isn't drawn yet.
    a.ui_state.set_inspector_width(60);
    apply_action(&mut a, Action::PaneWidthsReset);
    assert_eq!(a.ui_state.list_width(), DEFAULT_LIST_WIDTH);
    assert_eq!(a.ui_state.inspector_width(), DEFAULT_INSPECTOR_WIDTH);
}

#[test]
fn bookmark_toggle_inserts_then_removes_on_selected_uid() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert!(a.bookmarks.is_empty(), "no bookmarks initially");
    apply_action(&mut a, Action::BookmarkToggle);
    assert!(a.bookmarks.contains(FCT), "first toggle adds the bookmark");
    apply_action(&mut a, Action::BookmarkToggle);
    assert!(!a.bookmarks.contains(FCT), "second toggle removes it");
    assert!(a.bookmarks.is_empty());
}

#[test]
fn bookmark_toggle_is_a_noop_when_nothing_selected() {
    // An empty filter leaves no selectable model, so selected_unique_id is
    // None and the toggle does nothing (no panic, no insert).
    let mut a = app();
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: "zzzzzz".into(), // matches no model
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    a.refilter();
    assert!(a.list_sel_uid().is_none(), "filter matched nothing");
    apply_action(&mut a, Action::BookmarkToggle);
    assert!(
        a.bookmarks.is_empty(),
        "toggle with no selection is a no-op"
    );
}

#[test]
fn bookmark_cycle_wraps_to_next_bookmarked_model() {
    let mut a = app();
    // Bookmark two models; cycling from one lands on the other, and again
    // wraps back. Use the flat models list to pick deterministic neighbours.
    let ids: Vec<String> = a
        .model_list
        .models
        .iter()
        .map(|m| m.unique_id.clone())
        .collect();
    let first = ids[1].clone(); // not index 0, so the wrap is exercised
    let second = ids[5].clone();
    a.bookmarks.insert(first.clone());
    a.bookmarks.insert(second.clone());
    a.select_by_unique_id(&first);
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(second.as_str()),
        "cycle moves to the next bookmarked model"
    );
    // From the later one, cycling wraps forward back to the earlier one.
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(first.as_str()),
        "cycle wraps around to the first bookmark"
    );
}

#[test]
fn bookmark_cycle_is_a_noop_when_set_empty() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    let before = a.selected_unique_id();
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(a.selected_unique_id(), before, "no bookmarks ⇒ no move");
}

#[test]
fn reload_prunes_vanished_bookmarks_and_keeps_survivors() {
    let mut a = app();
    // One real id (survives reload) and one stale id (pruned).
    a.bookmarks.insert(FCT.to_string());
    a.bookmarks
        .insert("model.jaffle_finance.gone_forever".to_string());
    a.reload().expect("reload ok");
    assert!(a.bookmarks.contains(FCT), "surviving id kept");
    assert!(
        !a.bookmarks.contains("model.jaffle_finance.gone_forever"),
        "vanished id pruned"
    );
    assert_eq!(a.bookmarks.len(), 1);
}

#[test]
fn sort_cycle_advances_mode_and_preserves_selection_by_uid() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert_eq!(a.sort, SortMode::Layer, "starts in Layer");
    apply_action(&mut a, Action::SortCycle);
    assert_eq!(a.sort, SortMode::Downstream, "cycles Layer→Downstream");
    // Selection is re-resolved BY uid across the rebuild — same node, even if
    // its row moved within its layer group.
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "selection survives the sort rebuild by unique_id"
    );
    apply_action(&mut a, Action::SortCycle);
    assert_eq!(a.sort, SortMode::Tests);
    apply_action(&mut a, Action::SortCycle);
    assert_eq!(a.sort, SortMode::Layer, "wraps back to Layer");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "still the same node after a full cycle"
    );
}

#[test]
fn sort_cycle_refilters_in_the_new_order_during_search() {
    // With a list search active, SortCycle rebuilds the full list AND the
    // filtered view, keeping the selection by uid. The active list reorders.
    let mut a = app();
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: "pos".into(),
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    a.refilter();
    let before: Vec<String> = a
        .active_list()
        .models
        .iter()
        .map(|m| m.unique_id.clone())
        .collect();
    assert!(before.len() > 1, "search matched several models");
    let sel = a.selected_unique_id();
    apply_action(&mut a, Action::SortCycle);
    // Still filtered (search mode preserved) and selection survives by uid.
    assert!(a.filter.is_some(), "filter rebuilt, search still active");
    assert_eq!(a.selected_unique_id(), sel, "selection unchanged by uid");
    let after: Vec<String> = a
        .active_list()
        .models
        .iter()
        .map(|m| m.unique_id.clone())
        .collect();
    // Same membership (a multiset), regardless of order.
    let mut b = before.clone();
    let mut af = after.clone();
    b.sort();
    af.sort();
    assert_eq!(b, af, "filtered membership is unchanged by the sort");
}
