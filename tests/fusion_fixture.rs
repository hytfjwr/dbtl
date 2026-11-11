//! Integration tests against the committed hand-written dbt Fusion fixture
//! (`tests/fixtures/fusion_manifest.json`): a v12 manifest carrying
//! `resource_type: "check"` validation nodes (excluded like `test` —
//! payload dropped entirely), real dbt types dbtl does not classify
//! (`rpc`, `sql_operation`, `function`, `fixture` — kept as generic graph
//! nodes, reported as `unsupported` warnings), a type outside the schema
//! entirely (`notebook` — kept, `unknown` warning), a Fusion-shaped
//! `unit_tests` entry (folded into the tests side map), a non-empty
//! top-level `functions` map (warned, never drawn), and the Fusion-only
//! top-level maps (`docs`/`groups`/`group_map`/`selectors`/`classifiers`/
//! `disabled` — ignored by construction).

mod common;
// The jaffle_finance fixture is an independent Dag to diff the fusion one
// against, so `check` leaking into a diff listing would be visible.
use common::{dbtl_cmd, FIXTURE_MANIFEST as JAFFE};
use std::path::PathBuf;

use dbtl::{
    build_model_list, compute_diff, generate_docs, load_dag, load_dag_with_warnings, App, Dag,
    SortMode,
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fusion_manifest.json"
);

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fusion fixture manifest must load")
}

// ---- frozen graph composition: check excluded, unknown kept ----

#[test]
fn merged_map_excludes_check_and_test_but_keeps_unhandled_types() {
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
    // Unhandled resource types stay in the graph as generic nodes — real
    // dbt types (rpc/sql_operation/function/fixture) exactly like a truly
    // unknown one (notebook).
    assert_eq!(dag.count_by_resource_type("notebook"), 1, "notebook count");
    assert_eq!(dag.count_by_resource_type("rpc"), 2, "rpc count");
    assert_eq!(
        dag.count_by_resource_type("sql_operation"),
        1,
        "sql_operation count"
    );
    assert_eq!(dag.count_by_resource_type("function"), 1, "function count");
    assert_eq!(dag.count_by_resource_type("fixture"), 1, "fixture count");
    // check + test are excluded entirely.
    assert_eq!(dag.count_by_resource_type("check"), 0, "no check nodes");
    assert_eq!(dag.count_by_resource_type("test"), 0, "no test nodes");
    assert_eq!(dag.len(), 14, "total merged entries");
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
    // the generic not_null test plus the `unit_tests`-map unit test — never
    // a "check" kind.
    let tests = dag.tests("model.fusion_shop.fct_orders");
    assert_eq!(tests.len(), 2, "generic + unit tests are captured");
    assert_eq!(tests[0].kind, "not_null", "generic test sorts first");
    assert_eq!(
        tests[1].kind, "unit",
        "the unit_tests-map entry is folded in"
    );
    let fct = dag.get("model.fusion_shop.fct_orders").expect("fct node");
    assert_eq!(fct.test_count, 2, "test_count counts generic + unit tests");
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
    let out = generate_docs(
        &dag,
        "fusion_shop",
        "fusion_manifest.json",
        dbtl::PackageScope::Root,
    );
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

// ---- unhandled resource types: kept + one warning per kind ----

#[test]
fn unhandled_types_warn_once_per_kind_split_by_schema_membership() {
    let (dag, warnings) = load_dag_with_warnings(FIXTURE).expect("fixture loads");
    // Sorted by kind (fixture < function < notebook < rpc < sql_operation):
    // real dbt types report `unsupported`, a type outside the schema
    // (`notebook`) reports `unknown`, and `check` is a KNOWN excluded type
    // so it must NOT warn. The `functions`-map line rides last.
    assert_eq!(
        warnings,
        vec![
            "unsupported resource_type 'fixture' (1 node): kept as generic graph nodes".to_string(),
            "unsupported resource_type 'function' (1 node): kept as generic graph nodes"
                .to_string(),
            "unknown resource_type 'notebook' (1 node): kept as generic graph nodes".to_string(),
            "unsupported resource_type 'rpc' (2 nodes): kept as generic graph nodes".to_string(),
            "unsupported resource_type 'sql_operation' (1 node): kept as generic graph nodes"
                .to_string(),
            "'functions' map (1 function): not displayed".to_string(),
        ],
        "one warning per unhandled kind + the functions map, pluralized"
    );
    // The plain loader still works (warnings are discarded there).
    assert_eq!(dag.len(), 14);
}

#[test]
fn unhandled_types_stay_in_the_graph_with_their_edges() {
    let dag = fixture_dag();
    assert!(dag.contains("notebook.fusion_shop.weekly_report"));
    assert!(dag.contains("rpc.fusion_shop.rebuild_mart"));
    assert!(dag.contains("rpc.fusion_shop.noop"));
    assert!(dag.contains("sql_operation.fusion_shop.rebuild_index"));
    assert!(dag.contains("function.fusion_shop.order_total"));
    assert!(dag.contains("fixture.fusion_shop.orders_input"));
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
        (
            "model.fusion_shop.fct_orders",
            "sql_operation.fusion_shop.rebuild_index",
        ),
    ] {
        assert!(
            edges.iter().any(|e| e.0 == parent && e.1 == child),
            "kept unhandled node lost its edge {parent} -> {child}"
        );
    }
    // fct_orders' kept children: notebook + rpc + sql_operation +
    // semantic_model + exposure — the check/test/unit_test children were
    // pruned, so the badge reads 5.
    let fct = dag.get("model.fusion_shop.fct_orders").expect("fct node");
    assert_eq!(fct.direct_down, 5, "fct_orders kept-child count");
}

// ---- docs README breakdown: enumerate every type, sum == total ----

#[test]
fn readme_breakdown_enumerates_every_resource_type_and_sums_to_total() {
    let dag = fixture_dag();
    let out = generate_docs(
        &dag,
        "fusion_shop",
        "fusion_manifest.json",
        dbtl::PackageScope::Root,
    );
    let readme = &out
        .iter()
        .find(|(p, _)| p == "README.md")
        .expect("README")
        .1;
    // Every kept resource_type appears as a `- <kind>: <n>` line — the
    // unlisted kinds (semantic_model/metric/notebook/rpc plus the newly
    // classified sql_operation/function/fixture) are no longer missing
    // from the breakdown.
    for line in [
        "- total nodes: 14",
        "- model: 3",
        "- source: 2",
        "- exposure: 1",
        "- fixture: 1",
        "- function: 1",
        "- metric: 1",
        "- notebook: 1",
        "- rpc: 2",
        "- semantic_model: 1",
        "- sql_operation: 1",
    ] {
        assert!(readme.contains(line), "README breakdown missing {line:?}");
    }
    // And the counts literally sum to the reported total.
    let summary = readme.split("## Summary").nth(1).expect("Summary section");
    let summary = summary.split("## Test coverage").next().unwrap();
    let mut total = None;
    let mut sum = 0usize;
    for l in summary
        .lines()
        .filter(|l| l.starts_with("- ") && !l.starts_with("- scope:"))
    {
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
fn docs_cli_prints_one_warning_per_unhandled_kind_to_stderr() {
    let out_dir = std::env::temp_dir().join(format!("dbtl_fusion_docs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let result = dbtl_cmd()
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
    for warning in [
        "warning: unsupported resource_type 'fixture' (1 node)",
        "warning: unsupported resource_type 'function' (1 node)",
        "warning: unknown resource_type 'notebook' (1 node)",
        "warning: unsupported resource_type 'rpc' (2 nodes)",
        "warning: unsupported resource_type 'sql_operation' (1 node)",
        "warning: 'functions' map (1 function): not displayed",
    ] {
        assert!(
            stderr.contains(warning),
            "stderr missing {warning:?}, got: {stderr:?}"
        );
    }
    assert!(
        !stderr.contains("check"),
        "the excluded check type must not warn, got: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Routing startup warnings into the TUI's notification log must
/// not change what reaches stderr on the TUI path. Warnings are printed by
/// `resolve_source` BEFORE terminal init.
///
/// The bare invocation reaches `ratatui::try_init()`: on a host WITHOUT a
/// console it exits with "no TTY", but a Windows CI runner HAS a console
/// (crossterm opens CONIN$ — stdin redirection cannot hide it), so the child
/// parks on the event loop. Poll `try_wait` and kill a parked child instead
/// of blocking on `output()` (the pattern `package_scope.rs` uses for the
/// same reason).
#[test]
fn tui_path_still_prints_startup_warnings_to_stderr() {
    let mut child = dbtl_cmd()
        .args(["--manifest", FIXTURE])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn dbtl");
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        use std::io::Read;
        let _ = std::io::BufReader::new(stderr_pipe).read_to_string(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if child.try_wait().expect("poll child").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if child.try_wait().expect("poll child").is_none() {
        child.kill().expect("kill parked TUI child");
    }
    let _ = child.wait();
    let stderr = reader.join().expect("stderr reader");
    for warning in [
        "warning: unsupported resource_type 'fixture' (1 node)",
        "warning: unsupported resource_type 'function' (1 node)",
        "warning: unknown resource_type 'notebook' (1 node)",
        "warning: unsupported resource_type 'rpc' (2 nodes)",
        "warning: unsupported resource_type 'sql_operation' (1 node)",
        "warning: 'functions' map (1 function): not displayed",
    ] {
        assert!(
            stderr.contains(warning),
            "stderr missing {warning:?}, got: {stderr:?}"
        );
    }
}

// ---- Fusion shape tolerance: extra maps/fields parse and are ignored ----

#[test]
fn fusion_only_maps_and_node_extras_are_tolerated() {
    // The fixture carries the Fusion-only top-level maps (`docs`/`groups`/
    // `group_map`/`selectors`/`classifiers`/`disabled`), the
    // `depends_on.nodes_with_ref_location` extra key, and a non-empty
    // `functions` map — `fixture_dag()` loading at all is the parse proof.
    let dag = fixture_dag();
    for uid in [
        "doc.fusion_shop.orders_overview",
        "group.fusion_shop.finance",
        // The `disabled`-map model is parked by dbt — never a graph node.
        "model.fusion_shop.legacy_orders",
        // `functions`-map UDFs are not graph nodes (they never were).
        "function.fusion_shop.safe_divide",
    ] {
        assert!(!dag.contains(uid), "{uid} must not be a graph node");
    }
    // The Fusion-shaped `unit_tests` entry (given/expect/overrides/
    // tested_node_unique_id/…) lands in the tests side map keyed by its
    // `unit_test.<pkg>.<model>.<name>` uid.
    let tests = dag.tests("model.fusion_shop.fct_orders");
    assert!(
        tests
            .iter()
            .any(|t| t.kind == "unit" && t.name == "order_amounts_positive"),
        "the unit_tests entry should fold into the tests side map: {tests:?}"
    );
}

#[test]
fn functions_map_is_warned_not_drawn() {
    let (dag, warnings) = load_dag_with_warnings(FIXTURE).expect("fixture loads");
    // Exactly one line for the whole map — the UDF is not drawn, and the
    // warning says so rather than dropping it silently.
    assert!(
        warnings
            .iter()
            .any(|w| w == "'functions' map (1 function): not displayed"),
        "functions warning missing: {warnings:?}"
    );
    assert!(!dag.contains("function.fusion_shop.safe_divide"));
    // … while a `function` entry inside `nodes` (the Core v12 union member)
    // IS kept as a generic node — map-path vs nodes-path is deliberate.
    assert!(dag.contains("function.fusion_shop.order_total"));
}

#[test]
fn nameless_unit_test_entry_still_parses() {
    // `RawUnitTest.name` is defaulted, not required: a unit_tests entry
    // missing it (a shape divergence like dbt-core #15900's) must not fail
    // the whole manifest parse.
    let json = r#"{
        "nodes": {},
        "sources": {},
        "unit_tests": {
            "unit_test.p.m.t": {"model": "m"}
        },
        "parent_map": {},
        "child_map": {}
    }"#;
    let manifest: dbtl::RawManifest =
        serde_json::from_str(json).expect("a nameless unit_tests entry parses");
    let ut = &manifest.unit_tests["unit_test.p.m.t"];
    assert_eq!(ut.name, "", "missing name defaults");
    assert_eq!(ut.model.as_deref(), Some("m"));
}
