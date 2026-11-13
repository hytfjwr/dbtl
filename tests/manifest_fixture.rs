//! Integration tests against the committed fixture (the synthetic
//! jaffle_finance manifest), asserting frozen expected values.

mod common;
use common::fixture_dag;
use std::collections::{BTreeMap, HashSet};

use dbtl::{
    column_coverage_counts, has_pk_guarantee, load_manifest, run_rules, LintConfig, LintRule,
    RuleEnabled, Severity,
};

const INVALID: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/invalid.json");
const MISSING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/does_not_exist.json"
);

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---- merged map composition (frozen counts) ----

#[test]
fn merged_map_has_expected_counts() {
    let dag = fixture_dag();
    assert_eq!(dag.count_by_resource_type("model"), 45, "model count");
    assert_eq!(dag.count_by_resource_type("source"), 38, "source count");
    assert_eq!(dag.count_by_resource_type("seed"), 7, "seed count");
    assert_eq!(dag.count_by_resource_type("snapshot"), 1, "snapshot count");
    assert_eq!(dag.count_by_resource_type("exposure"), 2, "exposure count");
    assert_eq!(dag.len(), 93, "total merged entries");
}

#[test]
fn merged_map_excludes_test_and_operation() {
    let dag = fixture_dag();
    assert_eq!(dag.count_by_resource_type("test"), 0, "no test nodes");
    assert_eq!(
        dag.count_by_resource_type("operation"),
        0,
        "no operation nodes"
    );
    // Spot-check by scanning every entry: nothing is test/operation.
    for node in dag.nodes().values() {
        assert!(
            node.resource_type != "test" && node.resource_type != "operation",
            "found excluded node: {} ({})",
            node.unique_id,
            node.resource_type
        );
    }
}

// ---- lineage transitive closures (frozen expected sets) ----

#[test]
fn closure_upstream_only_pos_txn() {
    let dag = fixture_dag();
    let start = "model.jaffle_finance.pos_txn";
    let expected_up = set(&[
        "model.jaffle_finance.pos_files__assignment",
        "seed.jaffle_finance.pos_prod_aws_store_master",
        "source.jaffle_finance.lake_jaffle_payment.pos_txn",
        "source.jaffle_finance.lake_jaffle_payment.pos_shp",
        "source.jaffle_finance.lake_jaffle_payment.pos_cat",
        "source.jaffle_finance.lake_jaffle_payment.pos_rcv",
        "source.jaffle_finance.lake_jaffle_payment.pos_pay",
    ]);
    assert_eq!(dag.upstream(start), expected_up, "pos_txn upstream");
    assert!(dag.downstream(start).is_empty(), "pos_txn downstream empty");
}

#[test]
fn closure_downstream_only_shoppers_source_multihop() {
    let dag = fixture_dag();
    let start = "source.jaffle_finance.dev_lake_jaffle_payment.shoppers";
    let expected_down = set(&[
        "model.jaffle_finance.stg_payment__shoppers",
        "model.jaffle_finance.int_shoppers__combined",
    ]);
    assert!(
        dag.upstream(start).is_empty(),
        "shoppers source upstream empty"
    );
    assert_eq!(
        dag.downstream(start),
        expected_down,
        "shoppers source downstream (2 hops)"
    );
}

#[test]
fn closure_both_stg_payment_shoppers() {
    let dag = fixture_dag();
    let start = "model.jaffle_finance.stg_payment__shoppers";
    let expected_up = set(&[
        "seed.jaffle_finance.source_datetime_policy",
        "source.jaffle_finance.dev_lake_jaffle_payment.shoppers",
    ]);
    let expected_down = set(&["model.jaffle_finance.int_shoppers__combined"]);
    assert_eq!(
        dag.upstream(start),
        expected_up,
        "stg_payment__shoppers upstream"
    );
    assert_eq!(
        dag.downstream(start),
        expected_down,
        "stg_payment__shoppers downstream"
    );
}

#[test]
fn closure_deep_multihop_fct_subscription_process() {
    let dag = fixture_dag();
    let start = "model.jaffle_finance.fct_subscription_process";
    let expected_up = set(&[
        "model.jaffle_finance.dim_fiscal_years",
        "model.jaffle_finance.dim_supplier_departments",
        "model.jaffle_finance.dim_suppliers",
        "model.jaffle_finance.dim_delivery_lanes",
        "model.jaffle_finance.int_fiscal_years__combined",
        "model.jaffle_finance.int_supplier_departments__combined",
        "model.jaffle_finance.int_suppliers__combined",
        "model.jaffle_finance.int_delivery_lanes__combined",
        "model.jaffle_finance.int_subscriptions__combined",
        "model.jaffle_finance.stg_finance__fiscal_years",
        "model.jaffle_finance.stg_masterdata__companies",
        "model.jaffle_finance.stg_masterdata__deliveries",
        "model.jaffle_finance.stg_masterdata__delivery_groups",
        "model.jaffle_finance.stg_payment__supplier_departments",
        "model.jaffle_finance.stg_payment__suppliers",
        "model.jaffle_finance.stg_payment__subscriptions",
        "model.jaffle_finance.stg_payment__warehouses",
        "seed.jaffle_finance.source_datetime_policy",
        "snapshot.jaffle_finance.delivery_lanes_snapshot",
        "source.jaffle_finance.dev_lake_jaffle_finance.fiscal_years",
        "source.jaffle_finance.dev_lake_jaffle_masterdata.companies",
        "source.jaffle_finance.dev_lake_jaffle_masterdata.deliveries",
        "source.jaffle_finance.dev_lake_jaffle_masterdata.delivery_groups",
        "source.jaffle_finance.dev_lake_jaffle_payment.supplier_departments",
        "source.jaffle_finance.dev_lake_jaffle_payment.suppliers",
        "source.jaffle_finance.dev_lake_jaffle_payment.subscriptions",
        "source.jaffle_finance.dev_lake_jaffle_payment.warehouses",
    ]);
    let expected_down = set(&[
        "model.jaffle_finance.fct_delivery_monthly_snapshot",
        "model.jaffle_finance.rpt_delivery_base_metrics",
        // The two fixture exposures: the notebook hangs directly off fct, the
        // dashboard rides behind both downstream models.
        "exposure.jaffle_finance.subscription_churn_notebook",
        "exposure.jaffle_finance.delivery_kpi_dashboard",
    ]);
    assert_eq!(dag.upstream(start).len(), 27, "fct upstream count");
    assert_eq!(dag.upstream(start), expected_up, "fct upstream set");
    assert_eq!(dag.downstream(start), expected_down, "fct downstream set");
}

// ---- static lint findings, frozen against the fixture ----
//
// The counts were computed by hand against the fixture's structure: the
// `utilities/pos_prep` models each join a raw POS source with
// `pos_files__assignment` (16 direct_join_to_source), 4 POS sources each feed
// their own `pos_*` model plus the assignment model (source_fanout), the
// assignment model joins 4 sources and feeds 16 leaf pos_* models
// (multiple_sources_joined / model_fanout), and
// `fct_delivery_shopper_monthly_visits`,
// `fct_delivery_monthly_snapshot` and `fct_subscription_process` are
// redundant hops — each's only data-node child is a marts model also fed
// directly by the hop's own parents (their other "children" are exposures /
// non-model consumers that don't count as a use — rejoining_upstream).
// The two `int_*__combined` models carry no
// unique+not_null pair — the fixture's only ERROR-default findings
// (missing_primary_key_tests); the structure rules above all report at
// warn now. Everything else scores 0: all models are documented, none
// declare access/contract, every model has a parent, no marts or
// intermediate model reads a source, no backward edge exists, and
// `utilities/` has no default naming prefix.

/// A `LintConfig` running ONLY the rules that gate out of the box — the
/// ones declared at error severity. Every other rule defaults to warn and
/// is switched off here so the frozen gate counts can't churn when the
/// rule set grows; their own fixture counts are pinned separately.
fn gating_lint_config() -> LintConfig {
    let mut cfg = LintConfig::default();
    for rule in LintRule::ALL {
        if rule.info().default_severity != Severity::Error {
            cfg.rules.insert(rule, RuleEnabled::Off);
        }
    }
    cfg
}

#[test]
fn lint_findings_match_frozen_fixture_counts() {
    let dag = fixture_dag();
    let findings = run_rules(dbtl::LintInput::new(&dag, &gating_lint_config())).findings;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &findings {
        *counts.entry(f.rule.name()).or_default() += 1;
    }
    let expected: BTreeMap<&str, usize> = BTreeMap::from([("missing_primary_key_tests", 2)]);
    assert_eq!(
        counts, expected,
        "per-rule finding counts drifted from the frozen fixture values"
    );
    assert_eq!(findings.len(), 2, "total findings");
    // The other error-default rules score zero here (unknown_column_reference
    // would skip — this input is graph-only — and no fixture model declares
    // a contract).
    for rule in [
        LintRule::PublicModelsWithoutContract,
        LintRule::UnknownColumnReference,
    ] {
        assert!(
            findings.iter().all(|f| f.rule != rule),
            "{} must not fire on this fixture",
            rule.name()
        );
    }
}

#[test]
fn warn_severity_rules_match_frozen_fixture_counts() {
    // The warn-by-default rules' counts on the same fixture, pinned
    // separately from the gate set (which is switched off here). Verified
    // by hand: the five structure rules named above plus 11 sources feeding
    // nothing at all — not even an exposure; 6 incremental models declare no
    // unique_key; none of its 38 sources carries a freshness block; and
    // `int_shoppers__combined` is an unused intermediate (its only
    // "child" is a unique test — not a consumer) — nothing else on this
    // graph matches a warn rule.
    let dag = fixture_dag();
    let mut cfg = LintConfig::default();
    for rule in LintRule::ALL {
        if rule.info().default_severity == Severity::Error {
            cfg.rules.insert(rule, RuleEnabled::Off);
        }
    }
    let outcome = run_rules(dbtl::LintInput::new(&dag, &cfg));
    // Graph-only input: every enabled SQL/catalog-backed rule reports
    // skipped — informational only, adding nothing to `findings`.
    // (unknown_column_reference, trailing_semicolon and
    // duplicate_output_columns are error-default, so they're switched
    // off above and never reach the skip list.)
    let skipped: Vec<_> = outcome.skipped.iter().map(|s| s.rule.name()).collect();
    assert_eq!(
        skipped,
        [
            "select_star",
            "hard_coded_references",
            "cartesian_join",
            "forbidden_patterns",
            "warehouse_type_drift",
            "columns_missing_in_warehouse",
            "undeclared_warehouse_columns",
            "unused_cte",
            "order_by_without_limit",
        ],
        "the enabled aux-backed rules skip in registry order"
    );
    let findings = outcome.findings;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &findings {
        *counts.entry(f.rule.name()).or_default() += 1;
    }
    let expected: BTreeMap<&str, usize> = BTreeMap::from([
        ("direct_join_to_source", 16),
        ("multiple_sources_joined", 1),
        ("source_fanout", 4),
        ("model_fanout", 1),
        ("rejoining_upstream", 3),
        ("incremental_without_unique_key", 6),
        ("sources_without_freshness", 38),
        ("unused_sources", 11),
        ("unused_models", 1),
    ]);
    assert_eq!(
        counts, expected,
        "the warn-severity rules' fixture counts drifted"
    );
    assert!(
        findings.iter().all(|f| f.severity == Severity::Warn),
        "warn-by-default rules emit warn findings"
    );
}

// ---- Dag side maps (detail + tests), frozen against the fixture ----

#[test]
fn detail_materialized_is_frozen_for_known_models() {
    let dag = fixture_dag();
    // pos_txn is a table (verified via jq against the fixture).
    let txn = dag
        .detail("model.jaffle_finance.pos_txn")
        .expect("pos_txn has detail");
    assert_eq!(
        txn.materialized.as_deref(),
        Some("table"),
        "pos_txn materialized"
    );
    assert!(
        !txn.columns.is_empty(),
        "pos_txn exposes at least one column"
    );
    assert!(
        txn.columns.iter().any(|c| c.name == "id"),
        "pos_txn has an 'id' column"
    );
    // Sources carry no materialization.
    let src = dag
        .detail("source.jaffle_finance.dev_lake_jaffle_payment.shoppers")
        .expect("shoppers source has detail");
    assert_eq!(src.materialized, None, "a source has no materialization");
}

#[test]
fn every_model_has_a_known_materialization() {
    let dag = fixture_dag();
    for node in dag.nodes().values().filter(|n| n.resource_type == "model") {
        let m = dag
            .detail(&node.unique_id)
            .and_then(|d| d.materialized.clone())
            .unwrap_or_default();
        assert!(
            matches!(m.as_str(), "table" | "view" | "incremental"),
            "model {} has unexpected materialization {m:?}",
            node.unique_id
        );
    }
}

#[test]
fn tests_are_captured_for_a_known_seed() {
    let dag = fixture_dag();
    // The fiscal_years seed has an accepted_values test on fiscal_year_start_month.
    let tests = dag.tests("seed.jaffle_finance.fiscal_years");
    assert!(!tests.is_empty(), "fiscal_years seed has captured tests");
    assert!(
        tests.iter().any(|t| t.kind == "accepted_values"
            && t.column_name.as_deref() == Some("fiscal_year_start_month")),
        "accepted_values on fiscal_year_start_month is captured, got {tests:?}"
    );
    // Tests never become DAG nodes (count stays 0) — capture is pre-prune only.
    assert_eq!(
        dag.count_by_resource_type("test"),
        0,
        "tests excluded from the DAG"
    );
}

// ---- PK guarantees + column coverage + relationships metadata ----

#[test]
fn pk_guarantee_set_is_frozen() {
    let dag = fixture_dag();
    // Hand-verified against the fixture: every model/seed/snapshot's
    // `has_pk_guarantee` verdict. The only testables WITHOUT a guarantee are
    // the two int_*__combined models (the `missing_primary_key_tests` lint's
    // 2 findings — the rule is model-scoped) plus the lone snapshot.
    let mut guaranteed = 0usize;
    let mut missing: Vec<&str> = Vec::new();
    for n in dag.nodes().values() {
        if !matches!(n.resource_type.as_str(), "model" | "seed" | "snapshot") {
            continue;
        }
        if has_pk_guarantee(dag.tests(&n.unique_id)) {
            guaranteed += 1;
        } else {
            missing.push(n.unique_id.as_str());
        }
    }
    missing.sort();
    assert_eq!(
        missing,
        vec![
            "model.jaffle_finance.int_fiscal_years__combined",
            "model.jaffle_finance.int_shoppers__combined",
            "snapshot.jaffle_finance.delivery_lanes_snapshot",
        ],
        "the PK-guarantee-missing set drifted"
    );
    assert_eq!(guaranteed, 50, "PK-guaranteed testable count drifted");
}

#[test]
fn column_coverage_counts_is_frozen() {
    let dag = fixture_dag();
    // Declared columns carrying >=1 attached test / declared columns, over the
    // root-project testable set — the SAME numbers the stats dashboard and the
    // docs README quote.
    assert_eq!(column_coverage_counts(&dag, None), (212, 322));
}

#[test]
fn relationships_tests_resolve_their_targets() {
    let dag = fixture_dag();
    // pos_ord's relationships test points at the pos_pay source — the
    // `to:` dep is resolved out of `depends_on` minus the attached node.
    let ord = dag.tests("model.jaffle_finance.pos_ord");
    let rel = ord
        .iter()
        .find(|t| t.kind == "relationships")
        .expect("pos_ord has a relationships test");
    assert_eq!(
        rel.ref_uid.as_deref(),
        Some("source.jaffle_finance.lake_jaffle_payment.pos_pay")
    );
    assert_eq!(rel.ref_name.as_deref(), Some("pos_pay"));
    // A model->model relationship resolves the same way (ref_name = the
    // target's display name once the whole graph exists).
    let dept = dag.tests("model.jaffle_finance.dim_supplier_departments");
    let rel = dept
        .iter()
        .find(|t| t.kind == "relationships")
        .expect("dim_supplier_departments has a relationships test");
    assert_eq!(
        rel.ref_uid.as_deref(),
        Some("model.jaffle_finance.dim_suppliers")
    );
    assert_eq!(rel.ref_name.as_deref(), Some("dim_suppliers"));
    // Non-relationships tests carry no target.
    let nn = dept
        .iter()
        .find(|t| t.kind == "not_null")
        .expect("a not_null test exists");
    assert_eq!(nn.ref_uid, None);
}

// ---- Error handling: load_manifest returns Err (never panics) ----

#[test]
fn load_manifest_missing_path_returns_err() {
    let result = load_manifest(MISSING);
    assert!(result.is_err(), "missing path must be Err");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("read") || msg.contains("manifest file"),
        "error should name the file-read cause, got: {msg}"
    );
}

#[test]
fn load_manifest_invalid_json_returns_err() {
    let result = load_manifest(INVALID);
    assert!(result.is_err(), "invalid JSON must be Err");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("parse") || msg.contains("JSON"),
        "error should name the parse cause, got: {msg}"
    );
}

// ---- whole-graph overview subgraph ----

#[test]
fn full_subgraph_covers_the_whole_dag() {
    let dag = fixture_dag();
    let sg = dag.full_subgraph();
    assert_eq!(sg.nodes.len(), dag.len(), "every kept node is present");
    assert!(
        sg.nodes.windows(2).all(|w| w[0].unique_id < w[1].unique_id),
        "nodes are sorted by unique_id"
    );
    assert_eq!(
        sg.edges.len(),
        dag.edges().len(),
        "every pruned edge is present"
    );
    assert!(sg.selected.is_empty(), "no node is emphasised");
}
