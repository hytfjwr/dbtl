use super::*;

// ---- run_results: lenses + weighted critical path ---------------------------

#[test]
fn run_status_lens_tints_by_last_run_outcome() {
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    load_run_results(
        &mut a,
        r#"{"metadata":{"generated_at":"2024-05-01T00:00:00Z"},"results":[
            {"unique_id":"model.p.stg_a","status":"error","execution_time":2.0},
            {"unique_id":"model.p.fct_b","status":"warn","execution_time":5.0},
            {"unique_id":"model.p.mart_c","status":"skipped","execution_time":null}
        ]}"#,
    );
    let none = std::collections::HashSet::new();
    let no_lint = std::collections::HashMap::new();
    let uid = "model.p.stg_a";
    let node = a.dag.get(uid);
    assert_eq!(
        a.lens_tint(crate::LineageLens::RunStatus, uid, node, &none, &no_lint),
        crate::LensTint::Violation,
        "error → violation red"
    );
    assert_eq!(
        a.lens_tint(
            crate::LineageLens::RunStatus,
            "model.p.fct_b",
            a.dag.get("model.p.fct_b"),
            &none,
            &no_lint
        ),
        crate::LensTint::RunWarn,
        "warn → yellow"
    );
    assert_eq!(
        a.lens_tint(
            crate::LineageLens::RunStatus,
            "model.p.mart_c",
            a.dag.get("model.p.mart_c"),
            &none,
            &no_lint
        ),
        crate::LensTint::RunSkip,
        "skipped → muted"
    );
}

#[test]
fn run_time_lens_buckets_by_execution_seconds() {
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    load_run_results(
        &mut a,
        r#"{"results":[
            {"unique_id":"model.p.stg_a","status":"success","execution_time":0.5},
            {"unique_id":"model.p.fct_b","status":"success","execution_time":30.0},
            {"unique_id":"model.p.mart_c","status":"success","execution_time":120.0}
        ]}"#,
    );
    let none = std::collections::HashSet::new();
    let no_lint = std::collections::HashMap::new();
    let tint = |uid: &str| {
        a.lens_tint(
            crate::LineageLens::RunTime,
            uid,
            a.dag.get(uid),
            &none,
            &no_lint,
        )
    };
    assert_eq!(tint("model.p.stg_a"), crate::LensTint::None, "sub-second");
    assert_eq!(tint("model.p.fct_b"), crate::LensTint::HeatMid, "30s");
    assert_eq!(tint("model.p.mart_c"), crate::LensTint::HeatHigh, "2min");
}

#[test]
fn run_lenses_are_skipped_in_the_cycle_without_results() {
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    assert!(a.run_results().is_none());
    // Walk the whole cycle: with no run_results the Run lenses never land.
    for _ in 0..10 {
        crate::apply_action(&mut a, crate::Action::CycleLens);
        assert!(!matches!(
            a.ui_state.lens(),
            crate::LineageLens::RunStatus | crate::LineageLens::RunTime
        ));
    }
}

#[test]
fn run_status_label_reports_coverage_and_date() {
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    assert_eq!(a.run_status_label(), None, "no results → no segment");
    load_run_results(
        &mut a,
        r#"{"metadata":{"generated_at":"2024-05-01T12:00:00Z"},"results":[
            {"unique_id":"model.p.stg_a","status":"success","execution_time":1.0},
            {"unique_id":"model.p.unknown","status":"success","execution_time":1.0}
        ]}"#,
    );
    assert_eq!(
        a.run_status_label().as_deref(),
        Some("run 1/3 @2024-05-01"),
        "1 of the dag's 3 nodes covered (the stray uid doesn't count)"
    );
}

#[test]
fn weighted_chain_prefers_the_heavy_short_path_over_a_cheap_long_one() {
    // stg_a -> fct_b -> mart_c (3 hops) versus the fixture also being a chain —
    // here the whole dag IS one chain, so weight picks the same chain but the
    // score shows it: the runtimes sum, not the hop count.
    let dag = run_dag();
    let (path, total) = super::analysis::weighted_chain(&dag, |uid| match uid {
        "model.p.stg_a" => 10.0,
        "model.p.fct_b" => 20.0,
        _ => 0.0,
    });
    assert_eq!(
        path,
        vec![
            "stg_a".to_string(),
            "fct_b".to_string(),
            "mart_c".to_string()
        ]
    );
    assert_eq!(total, 30.0, "the runtime total, not the depth");
    // And the unweighted wrapper still reports hop-count chains.
    assert_eq!(super::analysis::longest_chain(&dag).len(), 3);
}

#[test]
fn stats_view_carries_the_runtime_critical_path_when_results_load() {
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    let sv = a.compute_stats_view();
    assert!(
        sv.critical_path_runtime.is_none(),
        "no run_results → no runtime path"
    );
    load_run_results(
        &mut a,
        r#"{"results":[
            {"unique_id":"model.p.stg_a","status":"success","execution_time":10.0},
            {"unique_id":"model.p.fct_b","status":"success","execution_time":20.0},
            {"unique_id":"model.p.mart_c","status":"success","execution_time":3.0}
        ]}"#,
    );
    let sv = a.compute_stats_view();
    let (path, total) = sv.critical_path_runtime.expect("runtime path present");
    assert_eq!(path.len(), 3);
    assert_eq!(total, "33.0s", "10+20+3 seconds");
}

#[test]
fn path_anchor_reroots_the_path_highlight() {
    // stg_finance__fiscal_years -> int_fiscal_years__combined -> dim_fiscal_years
    let mut a = app();
    assert!(a.select_by_unique_id("model.jaffle_finance.stg_finance__fiscal_years"));
    apply_action(&mut a, Action::PathToggle);
    assert!(a.tab().path_anchor.is_some(), "p anchors the path start");
    let _ = a.take_notice();
    // Navigate to the downstream end: cursor re-homes to the new root.
    assert!(a.select_by_unique_id("model.jaffle_finance.dim_fiscal_years"));
    let path = a.lineage_path_set();
    assert!(
        path.contains("model.jaffle_finance.stg_finance__fiscal_years"),
        "anchor in path"
    );
    assert!(
        path.contains("model.jaffle_finance.int_fiscal_years__combined"),
        "middle hop in path"
    );
    assert!(
        path.contains("model.jaffle_finance.dim_fiscal_years"),
        "target in path"
    );
    apply_action(&mut a, Action::PathToggle);
    assert!(a.tab().path_anchor.is_none(), "p again clears the anchor");
}

#[test]
fn full_text_search_matches_desc_columns_and_sql() {
    let mut a = app();
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: "?old_file_id".into(), // a column on pos_files__assignment only
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    a.refilter();
    let ids: Vec<String> = (0..a.active_list().len())
        .filter_map(|i| a.active_list().model_at(i))
        .map(|m| m.unique_id.clone())
        .collect();
    assert_eq!(
        ids,
        vec!["model.jaffle_finance.pos_files__assignment"],
        "column hit: {ids:?}"
    );
}
