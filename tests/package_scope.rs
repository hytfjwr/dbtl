//! Package-scope parity: a manifest carrying installed-package (`dbt deps`)
//! nodes must — under the DEFAULT root scope — produce the same graph and the
//! same metrics as source mode parsing the project (which never sees
//! `dbt_packages/`). `--include-packages` restores the old whole-manifest
//! graph for lineage/docs while every metric still counts the root project.
//!
//! The fixture pair is hand-written: `packages_manifest.json` carries
//! `package_name`s the way `dbt compile` records them (root `acme_analytics`
//! plus the installed `audit_tools` package), and `packages_project/` mirrors
//! the root half node-for-node, so the root-scoped manifest graph and the
//! source graph must agree field-for-field — the same contract
//! `tests/consistency.rs` pins for the dbt-generated fixtures.
//!
//! Fixture topology (manifest mode, unfiltered):
//!
//! ```text
//!   raw.orders ──> stg_orders ──> fct_orders ──> ops_dashboard   (tested)
//!       │
//!       └────────> stg_metrics ───────────────┐
//!                                             v
//!   [audit_tools] telemetry.events ─> audit_events ─> audit_rollups ─> audit_health
//!                                        ^                (package side)
//!                                        └── stg_audit_events (root model fed
//!                                            by a package model)
//!   raw.customers ─> dim_customers  (orphan everywhere)
//! ```
//!
//! Metrics the tests pin: coverage 1/5 (only `fct_orders` carries tests);
//! orphan models `dim_customers` + `stg_audit_events` always, plus
//! `stg_metrics` when the package consumer is out of scope.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use dbtl::{
    build_model_list, coverage_counts, generate_docs, load_dag, load_dag_from_source, Dag,
    NodeInfo, PackageScope, SortMode,
};

const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/packages_manifest.json"
);
const PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/packages_project"
);

/// The unfiltered manifest Dag — the `All` scope shape (12 kept nodes: 7
/// models, 3 sources, 2 exposures; test nodes are excluded as always).
fn manifest_full() -> Dag {
    load_dag(MANIFEST).expect("packages manifest loads")
}

/// The default-scope manifest Dag — what the CLI/TUI actually loads now.
fn manifest_root() -> Dag {
    PackageScope::Root.apply(manifest_full())
}

/// The source-mode Dag of the mirrored project (8 nodes — package-free by
/// construction, like every source-mode graph).
fn source_dag() -> Dag {
    load_dag_from_source(PROJECT).expect("packages project parses")
}

fn node_map(dag: &Dag) -> BTreeMap<String, NodeInfo> {
    dag.nodes()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Root-project orphan model names — the predicate the stats dashboard and
/// the docs README share (`is_root_project && model && direct_down == 0`).
fn orphan_names(dag: &Dag) -> Vec<&str> {
    let mut v: Vec<&str> = dag
        .nodes()
        .values()
        .filter(|n| dag.is_root_project(n) && n.resource_type == "model" && n.direct_down == 0)
        .map(|n| n.name.as_str())
        .collect();
    v.sort_unstable();
    v
}

fn readme_of(files: &[(String, String)]) -> String {
    files
        .iter()
        .find(|(p, _)| p == "README.md")
        .map(|(_, c)| c.clone())
        .expect("generated set contains README.md")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dbtl"))
        .args(args)
        .output()
        .expect("failed to run dbtl")
}

// ---------- the default scope: root project only ----------

#[test]
fn default_scope_prunes_installed_packages() {
    let full = manifest_full();
    assert!(
        full.contains("model.audit_tools.audit_events")
            && full.contains("model.audit_tools.audit_rollups")
            && full.contains("source.audit_tools.telemetry.events")
            && full.contains("exposure.audit_tools.audit_health"),
        "the unfiltered manifest keeps installed-package nodes"
    );

    let root = manifest_root();
    let ids: BTreeSet<&String> = root.nodes().keys().collect();
    assert_eq!(
        ids.len(),
        8,
        "the root scope keeps exactly the project's own nodes"
    );
    assert!(
        !ids.iter()
            .any(|u| u.split('.').nth(1) == Some("audit_tools")),
        "no installed-package node survives the default scope"
    );
    // And the recorded root project is what the predicate scopes by.
    assert_eq!(root.project_name(), Some("acme_analytics"));
}

#[test]
fn root_scope_graph_matches_source_mode() {
    // The core contract: under the default scope, manifest mode sees exactly
    // the graph source mode parses — same uids, same NodeInfo fields
    // (edges/degrees included: package endpoints are pruned like the
    // unresolved refs source mode drops).
    let (m, s) = (manifest_root(), source_dag());
    let mm = node_map(&m);
    let sm = node_map(&s);
    assert_eq!(
        mm.keys().collect::<BTreeSet<_>>(),
        sm.keys().collect::<BTreeSet<_>>(),
        "root-scoped manifest and source node sets diverge"
    );
    for (uid, mi) in &mm {
        assert_eq!(mi, &sm[uid], "NodeInfo diverges for {uid}");
    }
}

#[test]
fn scoped_metrics_match_source_mode() {
    // Coverage + orphans under the default scope must equal the source-mode
    // numbers — the "manifest vs source disagree on coverage" bug this fixes.
    let (m, s) = (manifest_root(), source_dag());
    assert_eq!(
        coverage_counts(&m),
        (1, 5),
        "root scope: 1 tested (fct_orders) of 5 testable models"
    );
    assert_eq!(
        coverage_counts(&m),
        coverage_counts(&s),
        "coverage diverges between manifest and source modes"
    );
    assert_eq!(
        orphan_names(&m),
        ["dim_customers", "stg_audit_events", "stg_metrics"],
        "scoped orphan models — stg_metrics' only consumer is a package node"
    );
    assert_eq!(
        orphan_names(&m),
        orphan_names(&s),
        "orphan sets diverge between manifest and source modes"
    );
    // The left pane's model list agrees too — same models, same grouping.
    let groups = |dag: &Dag| -> Vec<String> {
        build_model_list(dag, SortMode::Layer)
            .groups
            .iter()
            .flat_map(|g| g.models.iter().map(|n| n.unique_id.clone()))
            .collect()
    };
    assert_eq!(groups(&m), groups(&s), "model list diverges");
}

#[test]
fn docs_summary_matches_source_mode() {
    // The generated README must be byte-identical across modes under the
    // default scope — summary bullet, scope line, coverage, orphans, node
    // table, lineage (same label for both so provenance can't differ).
    let (m, s) = (manifest_root(), source_dag());
    let out_m = generate_docs(&m, "acme_analytics", "fixture", PackageScope::Root);
    let out_s = generate_docs(&s, "acme_analytics", "fixture", PackageScope::Root);
    assert_eq!(
        readme_of(&out_m),
        readme_of(&out_s),
        "generated README diverges between manifest and source modes"
    );
    let readme = readme_of(&out_m);
    assert!(
        readme.contains("- scope: root project only"),
        "the README declares the scope, got:\n{readme}"
    );
    assert!(
        readme.contains("- tested: 1 / 5 testable resources"),
        "root-scoped coverage in the README"
    );
    // The scope line is inside the Summary block, near the top.
    let summary_at = readme.find("## Summary").expect("summary section");
    let scope_at = readme.find("- scope:").expect("scope bullet");
    assert!(scope_at > summary_at && scope_at - summary_at < 200);
}

// ---------- --include-packages: context in the graph, metrics stay scoped ----------

#[test]
fn include_packages_keeps_package_nodes_as_context() {
    let all = manifest_full();
    let root = manifest_root();

    // The full graph: package nodes present for lineage/docs (the old
    // whole-manifest behaviour --project-only used to strip).
    assert_eq!(all.len(), 12, "unfiltered manifest keeps every kept node");

    // Metrics STILL count only the root project — coverage is identical to
    // the scoped graph and to source mode, package test nodes don't count.
    assert_eq!(
        coverage_counts(&all),
        coverage_counts(&root),
        "coverage_counts is root-scoped even on the full graph"
    );
    assert_eq!(coverage_counts(&all), (1, 5));

    // Orphan models: stg_metrics IS consumed inside the full graph (by the
    // package rollup), so it's not an orphan under the wide scope — but the
    // count is still root-only (audit_rollups, a package orphan itself,
    // never appears).
    let orphans = orphan_names(&all);
    assert_eq!(
        orphans,
        ["dim_customers", "stg_audit_events"],
        "full-graph orphans are still root-scoped"
    );

    // Docs under --include-packages: package nodes get pages, the README
    // declares the scope, and the stats block stays root-scoped.
    let out = generate_docs(&all, "acme_analytics", "fixture", PackageScope::All);
    assert!(
        out.iter()
            .any(|(p, _)| p.contains("model.audit_tools.audit_events")),
        "package nodes get their own pages under --include-packages"
    );
    let readme = readme_of(&out);
    assert!(
        readme.contains(
            "- scope: all packages (4 installed-package nodes; metrics count the root project only)"
        ),
        "the All-scope bullet names the package count, got:\n{readme}"
    );
    assert!(
        readme.contains("- tested: 1 / 5 testable resources"),
        "README coverage stays root-scoped under --include-packages"
    );
    assert!(
        readme.contains("- total nodes: 12"),
        "the node table covers every package (context is visible)"
    );
}

#[test]
fn include_packages_keeps_package_edges_in_lineage() {
    // Package nodes are real dependency context: stg_metrics' only consumer is
    // a package model, so its subgraph under the wide scope shows that edge —
    // while the root scope shows the same shape source mode shows (no
    // consumer). Both are honest readouts of their respective graphs.
    let all = manifest_full();
    assert_eq!(
        all.get("model.acme_analytics.stg_metrics")
            .map(|n| n.direct_down),
        Some(1),
        "package consumer visible under --include-packages"
    );
    assert!(all
        .downstream("model.acme_analytics.stg_metrics")
        .contains("model.audit_tools.audit_rollups"));
    let root = manifest_root();
    assert_eq!(
        root.get("model.acme_analytics.stg_metrics")
            .map(|n| n.direct_down),
        Some(0),
        "the same edge pruned under the default scope"
    );
}

// ---------- CLI surfaces ----------

#[test]
fn check_reports_scope_and_counts_root_only() {
    let m = MANIFEST;
    let out = run(&["check", "--manifest", m]);
    assert!(
        out.status.success(),
        "check passes, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("scope: root project only"),
        "the scope line leads the check report, got: {stdout}"
    );
    assert!(
        stdout.contains("PASS coverage: 1/5 (20%)"),
        "coverage counts the root project only, got: {stdout}"
    );

    let out = run(&["check", "--manifest", m, "--include-packages"]);
    assert!(
        out.status.success(),
        "check passes under --include-packages, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("scope: all packages (metrics count the root project only)"),
        "the scope line widens verbatim, got: {stdout}"
    );
    assert!(
        stdout.contains("PASS coverage: 1/5 (20%)"),
        "coverage stays root-scoped under --include-packages, got: {stdout}"
    );
}

#[test]
fn check_source_mode_matches_manifest_mode() {
    // The headline parity: `check` on the mirrored project dir prints the
    // same coverage + scope as on the manifest.
    let out = run(&["check", "--source", PROJECT]);
    assert!(
        out.status.success(),
        "check passes in source mode, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("scope: root project only"));
    assert!(
        stdout.contains("PASS coverage: 1/5 (20%)"),
        "source-mode coverage == manifest-mode coverage, got: {stdout}"
    );
}

#[test]
fn include_packages_in_source_mode_warns_but_works() {
    // Source mode never parses dbt_packages — the flag is a no-op there and
    // the warning says so instead of silently dropping it.
    let out = run(&["check", "--source", PROJECT, "--include-packages"]);
    assert!(
        out.status.success(),
        "check still passes, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--include-packages has no effect in source mode"),
        "the no-op warning explains itself, got stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS coverage: 1/5 (20%)"));
}

#[test]
fn project_only_is_a_deprecated_no_op() {
    // Backwards compatibility: the flag still parses and changes nothing (the
    // default scope is already root-only) — but it must NOT be silent, on the
    // TUI path and on subcommands alike.
    let out = run(&["--manifest", MANIFEST, "--project-only"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--project-only is deprecated"),
        "deprecated flag warns (the TUI load path), got stderr: {stderr}"
    );

    // Top-level flags precede the subcommand: `--project-only check` still
    // parses, the warning fires, and the scope stays root-only.
    let out = run(&["--project-only", "check", "--manifest", MANIFEST]);
    assert!(
        out.status.success(),
        "check passes, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--project-only is deprecated"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PASS coverage: 1/5 (20%)"),
        "the no-op leaves the root scope untouched, got: {stdout}"
    );
}

#[test]
fn docs_readme_declares_the_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out_dir = dir.path().join("docs");
    let out = run(&[
        "docs",
        "--manifest",
        MANIFEST,
        "--out",
        out_dir.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "docs generates, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let readme = std::fs::read_to_string(out_dir.join("README.md")).expect("README written");
    assert!(readme.contains("- scope: root project only"));
    assert!(
        !std::fs::read_dir(out_dir.join("models"))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("audit_tools")),
        "default scope writes no package pages"
    );

    let out_dir2 = dir.path().join("docs_all");
    let out = run(&[
        "docs",
        "--manifest",
        MANIFEST,
        "--out",
        out_dir2.to_str().unwrap(),
        "--include-packages",
    ]);
    assert!(out.status.success());
    let readme = std::fs::read_to_string(out_dir2.join("README.md")).expect("README written");
    assert!(
        readme.contains("- scope: all packages (4 installed-package nodes"),
        "the widened scope is declared, got:\n{readme}"
    );
    assert!(
        std::fs::read_dir(out_dir2.join("models"))
            .unwrap()
            .flatten()
            .any(|e| e
                .file_name()
                .to_string_lossy()
                .contains("model.audit_tools")),
        "package nodes get pages under --include-packages"
    );
}

#[test]
fn impact_report_carries_the_scope() {
    // `dbtl impact` — the reviewer-facing report — states the scope in its
    // header and serializes it in JSON, so a pasted pack declares itself.
    let out = run(&[
        "impact",
        "--manifest",
        MANIFEST,
        "--diff",
        MANIFEST,
        "--include-packages",
    ]);
    assert!(
        out.status.success(),
        "impact runs, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("- scope: all packages (metrics count the root project only)"),
        "markdown header states the scope, got: {stdout}"
    );

    let out = run(&[
        "impact",
        "--manifest",
        MANIFEST,
        "--diff",
        MANIFEST,
        "--format",
        "json",
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    assert_eq!(v["scope"].as_str(), Some("root"), "scope serializes");
}
