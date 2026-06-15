//! Integration tests against the committed fixture (the synthetic
//! jaffle_finance manifest), asserting frozen expected values.

use std::collections::HashSet;

use dbtl::{load_dag, load_manifest, Dag};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");
const INVALID: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/invalid.json");
const MISSING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/does_not_exist.json"
);

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fixture manifest must load")
}

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
