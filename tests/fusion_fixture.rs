//! Integration tests against the committed hand-written dbt Fusion fixture
//! (`tests/fixtures/fusion_manifest.json`): a v12 manifest carrying
//! `resource_type: "check"` validation nodes (excluded like `test` —
//! payload dropped entirely) and resource types dbtl does not classify
//! (`notebook`, `rpc` — kept as generic graph nodes, reported as one
//! stderr warning per kind).

use std::path::PathBuf;
use std::process::Command;

use dbtl::{
    build_model_list, compute_diff, generate_docs, load_dag, load_dag_with_warnings, App, Dag,
    SortMode,
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fusion_manifest.json"
);
/// The jaffle_finance fixture — an independent Dag to diff the fusion one
/// against, so `check` leaking into a diff listing would be visible.
const JAFFE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fusion fixture manifest must load")
}

// ---- frozen graph composition: check excluded, unknown kept ----

#[test]
fn merged_map_excludes_check_and_test_but_keeps_unknown_types() {
    let dag = fixture_dag();
    // Buildable + surface resources.
    assert_eq!(dag.count_by_resource_type("model"), 3, "model count");
    assert_eq!(dag.count_by_resource_type("source"), 2, "source count");
    assert_eq!(dag.count_by_resource_type("exposure"), 1, "exposure count");
    assert_eq!(
        dag.count_by_resource_type("semantic_model"),
        1,
        "semantic_model count"
    );
    assert_eq!(dag.count_by_resource_type("metric"), 1, "metric count");
    // Unknown resource types stay in the graph as generic nodes.
    assert_eq!(dag.count_by_resource_type("notebook"), 1, "notebook count");
    assert_eq!(dag.count_by_resource_type("rpc"), 2, "rpc count");
    // check + test are excluded entirely.
    assert_eq!(dag.count_by_resource_type("check"), 0, "no check nodes");
    assert_eq!(dag.count_by_resource_type("test"), 0, "no test nodes");
    assert_eq!(dag.len(), 11, "total merged entries");
}

#[test]
fn check_payload_is_dropped_no_node_no_side_map_no_edge() {
    let dag = fixture_dag();
    for uid in [
        "check.fusion_shop.stg_orders_row_count",
        "check.fusion_shop.fct_orders_freshness",
    ] {
        assert!(!dag.contains(uid), "{uid} must not be a node");
        assert!(dag.get(uid).is_none(), "{uid}: no NodeInfo");
        assert!(dag.detail(uid).is_none(), "{uid}: no detail payload");
        assert!(dag.raw_code(uid).is_none(), "{uid}: no raw SQL kept");
        assert!(dag.compiled(uid).is_none(), "{uid}: no compiled SQL kept");
        assert!(dag.tests(uid).is_empty(), "{uid}: no tests entry");
    }
    // No pruned edge may reference an excluded check node — not as a parent,
    // not as a transit hop (stg_orders -> check -> downstream stays broken).
    for (parent, child) in dag.edges() {
        assert!(
            !parent.starts_with("check.") && !child.starts_with("check."),
            "check node leaked into an edge: {parent} -> {child}"
        );
    }
    // A check does NOT inflate the tests side map either: fct_orders carries
    // exactly the one generic not_null test, never a "check" kind.
    let tests = dag.tests("model.fusion_shop.fct_orders");
    assert_eq!(tests.len(), 1, "only the generic test is captured");
    assert_eq!(tests[0].kind, "not_null");
    let fct = dag.get("model.fusion_shop.fct_orders").expect("fct node");
    assert_eq!(fct.test_count, 1, "test_count counts the generic test only");
    // And it never appears in a lineage subgraph.
    let sg = dag.subgraph("model.fusion_shop.fct_orders");
    assert!(
        sg.nodes.iter().all(|n| !n.unique_id.starts_with("check.")),
        "check node leaked into the fct_orders lineage"
    );
    assert!(
        sg.edges
            .iter()
            .all(|e| !e.parent.starts_with("check.") && !e.child.starts_with("check.")),
        "check node leaked into lineage edges"
    );
}

#[test]
fn check_never_reaches_the_model_list() {
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    assert!(
        list.models
            .iter()
            .all(|m| !m.unique_id.starts_with("check.")),
        "check node leaked into the left pane"
    );
    assert_eq!(list.len(), 3, "the three models are the whole list");
}

#[test]
fn check_never_reaches_the_stats_dashboard() {
    let dag = fixture_dag();
    let app = App::new(dag.clone(), PathBuf::from(FIXTURE));
    let stats = app.compute_stats_view();
    assert!(
        stats
            .by_resource_type
            .iter()
            .all(|(kind, _)| kind != "check" && kind != "test"),
        "excluded kind leaked into the stats breakdown: {:?}",
        stats.by_resource_type
    );
    let sum: usize = stats.by_resource_type.iter().map(|(_, n)| n).sum();
    assert_eq!(sum, dag.len(), "stats breakdown must sum to the node count");
}

#[test]
fn check_never_reaches_generated_docs() {
    let dag = fixture_dag();
    let out = generate_docs(&dag, "fusion_shop", "fusion_manifest.json");
    assert_eq!(out.len(), dag.len() + 1, "one page per node + README");
    for (path, contents) in &out {
        assert!(
            !path.starts_with("models/check."),
            "a check node got a docs page: {path}"
        );
        assert!(
            !contents.contains("check.fusion_shop."),
            "{path} references a check unique_id"
        );
    }
    let readme = &out
        .iter()
        .find(|(p, _)| p == "README.md")
        .expect("README")
        .1;
    assert!(!readme.contains("- check:"), "check in the breakdown");
    assert!(
        !readme.contains("fct_orders_freshness") && !readme.contains("stg_orders_row_count"),
        "a check node leaked into the README"
    );
}

#[test]
fn check_never_reaches_the_diff() {
    let dag = fixture_dag();
    assert!(
        compute_diff(&dag, &dag).is_empty(),
        "a Dag diffs clean against itself"
    );
    // Against an unrelated baseline: nothing the fusion manifest holds under
    // `check`/`test` can show up as added or as an edge endpoint name.
    let jaffle = load_dag(JAFFE).expect("jaffle fixture loads");
    let diff = compute_diff(&jaffle, &dag);
    assert!(
        diff.added.iter().all(|uid| !uid.starts_with("check.")),
        "check uid in diff.added: {:?}",
        diff.added
    );
    assert!(
        diff.removed.iter().all(|(_, rt)| rt != "check"),
        "check node in diff.removed: {:?}",
        diff.removed
    );
    for (p, c) in diff.edges_added.iter().chain(&diff.edges_removed) {
        assert!(
            !p.contains("freshness") && !c.contains("freshness") && !p.contains("row_count"),
            "check node name leaked into a diff edge: {p} -> {c}"
        );
    }
}

// ---- unknown resource types: kept + one warning per kind ----

#[test]
fn unknown_resource_types_warn_once_per_kind_and_say_how_they_are_handled() {
    let (dag, warnings) = load_dag_with_warnings(FIXTURE).expect("fixture loads");
    // Exactly two kinds — sorted (`notebook` < `rpc`); `check` is a KNOWN
    // excluded type, so it must NOT warn.
    assert_eq!(
        warnings,
        vec![
            "unknown resource_type 'notebook' (1 node): kept as generic graph nodes".to_string(),
            "unknown resource_type 'rpc' (2 nodes): kept as generic graph nodes".to_string(),
        ],
        "one warning per unknown kind, pluralized"
    );
    // The plain loader still works (warnings are discarded there).
    assert_eq!(dag.len(), 11);
}

#[test]
fn unknown_types_stay_in_the_graph_with_their_edges() {
    let dag = fixture_dag();
    assert!(dag.contains("notebook.fusion_shop.weekly_report"));
    assert!(dag.contains("rpc.fusion_shop.rebuild_mart"));
    assert!(dag.contains("rpc.fusion_shop.noop"));
    let edges = dag.edges();
    for (parent, child) in [
        (
            "model.fusion_shop.fct_orders",
            "notebook.fusion_shop.weekly_report",
        ),
        (
            "model.fusion_shop.fct_orders",
            "rpc.fusion_shop.rebuild_mart",
        ),
    ] {
        assert!(
            edges.iter().any(|e| e.0 == parent && e.1 == child),
            "kept unknown node lost its edge {parent} -> {child}"
        );
    }
    // fct_orders' kept children: notebook + rpc + semantic_model + exposure —
    // the check and test children were pruned, so the badge reads 4.
    let fct = dag.get("model.fusion_shop.fct_orders").expect("fct node");
    assert_eq!(fct.direct_down, 4, "fct_orders kept-child count");
}

// ---- docs README breakdown: enumerate every type, sum == total ----

#[test]
fn readme_breakdown_enumerates_every_resource_type_and_sums_to_total() {
    let dag = fixture_dag();
    let out = generate_docs(&dag, "fusion_shop", "fusion_manifest.json");
    let readme = &out
        .iter()
        .find(|(p, _)| p == "README.md")
        .expect("README")
        .1;
    // Every kept resource_type appears as a `- <kind>: <n>` line — the
    // unlisted kinds (semantic_model/metric/notebook/rpc) are no longer
    // missing from the breakdown.
    for line in [
        "- total nodes: 11",
        "- model: 3",
        "- source: 2",
        "- exposure: 1",
        "- semantic_model: 1",
        "- metric: 1",
        "- notebook: 1",
        "- rpc: 2",
    ] {
        assert!(readme.contains(line), "README breakdown missing {line:?}");
    }
    // And the counts literally sum to the reported total.
    let summary = readme.split("## Summary").nth(1).expect("Summary section");
    let summary = summary.split("## Test coverage").next().unwrap();
    let mut total = None;
    let mut sum = 0usize;
    for l in summary.lines().filter(|l| l.starts_with("- ")) {
        let (kind, n) = l
            .trim_start_matches("- ")
            .split_once(':')
            .expect("breakdown line is '<kind>: <n>'");
        let n: usize = n.trim().parse().expect("count parses");
        if kind == "total nodes" {
            total = Some(n);
        } else {
            sum += n;
        }
    }
    assert_eq!(total, Some(dag.len()), "total line matches dag.len()");
    assert_eq!(sum, dag.len(), "breakdown sums to the total");
}

// ---- stderr plumbing: the `dbtl docs` path prints the warnings ----

#[test]
fn docs_cli_prints_one_warning_per_unknown_kind_to_stderr() {
    let out_dir = std::env::temp_dir().join(format!("dbtl_fusion_docs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let result = Command::new(env!("CARGO_BIN_EXE_dbtl"))
        .arg("docs")
        .arg("--manifest")
        .arg(FIXTURE)
        .arg("--out")
        .arg(&out_dir)
        .arg("--quiet")
        .output()
        .expect("failed to run dbtl docs");
    assert!(
        result.status.success(),
        "docs exits 0, stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    for kind in ["'notebook' (1 node)", "'rpc' (2 nodes)"] {
        assert!(
            stderr.contains(&format!("warning: unknown resource_type {kind}")),
            "stderr missing the {kind} warning, got: {stderr:?}"
        );
    }
    assert!(
        !stderr.contains("check"),
        "the excluded check type must not warn, got: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&out_dir);
}
