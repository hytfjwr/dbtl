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

mod common;
use common::{consistency::node_map, dbtl_cmd, run};
use std::collections::BTreeSet;

use dbtl::{
    build_model_list, coverage_counts, generate_docs, layer_violation_edges, load_dag,
    load_dag_from_source, Dag, PackageScope, SortMode,
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

/// A minimal manifest.json body: `nodes` + `sources` + the two adjacency maps
/// are the only required keys. Each `uid` becomes a bare model node — enough
/// for the load path, the scope predicates, and `dbtl check`.
fn manifest_json(project_name: Option<&str>, uids: &[&str]) -> String {
    let nodes: serde_json::Map<String, serde_json::Value> = uids
        .iter()
        .map(|uid| {
            (
                uid.to_string(),
                serde_json::json!({
                    "name": uid.rsplit('.').next().unwrap_or(uid),
                    "resource_type": "model",
                }),
            )
        })
        .collect();
    let mut manifest = serde_json::json!({
        "nodes": nodes,
        "sources": {},
        "parent_map": {},
        "child_map": {},
    });
    if let Some(p) = project_name {
        manifest["metadata"] = serde_json::json!({ "project_name": p });
    }
    manifest.to_string()
}

/// Write [`manifest_json`] to `<dir>/manifest.json` and return its path.
fn write_manifest(dir: &std::path::Path, project_name: Option<&str>, uids: &[&str]) -> String {
    let path = dir.join("manifest.json");
    std::fs::write(&path, manifest_json(project_name, uids)).expect("write manifest");
    path.to_string_lossy().into_owned()
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
        coverage_counts(&m, None),
        (1, 5),
        "root scope: 1 tested (fct_orders) of 5 testable models"
    );
    assert_eq!(
        coverage_counts(&m, None),
        coverage_counts(&s, None),
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
        coverage_counts(&all, None),
        coverage_counts(&root, None),
        "coverage_counts is root-scoped even on the full graph"
    );
    assert_eq!(coverage_counts(&all, None), (1, 5));

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
    //
    // The bare invocation reaches `ratatui::try_init()`: on a host WITHOUT a
    // console it exits with "no TTY", but a Windows CI runner HAS a console
    // (crossterm opens CONIN$ — stdin redirection cannot hide it), so the
    // child parks on the event loop. The deprecation warning is emitted
    // BEFORE terminal init, so read it off stderr and kill the child if it
    // happens to find a console instead of exiting.
    let mut child = dbtl_cmd()
        .args(["--manifest", MANIFEST, "--project-only"])
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

// ---------- the global flag + the warning surface ----------

#[test]
fn include_packages_flag_works_before_the_subcommand() {
    // The flag is GLOBAL: `dbtl --include-packages check` must widen exactly
    // like `dbtl check --include-packages` — a subcommand-scoped flag would
    // leave the top-level form parsed but silently ignored.
    for args in [
        ["--include-packages", "check", "--manifest", MANIFEST].as_slice(),
        ["check", "--manifest", MANIFEST, "--include-packages"].as_slice(),
    ] {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} runs, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("scope: all packages (metrics count the root project only)"),
            "{args:?} reaches the scope in both placements, got: {stdout}"
        );
    }
}

#[test]
fn project_only_conflicts_with_include_packages() {
    // Same level AND cross level: `--include-packages` is global, so the
    // conflict must hold wherever each flag was typed — not just when both
    // sit in the same argv segment.
    for args in [
        [
            "--project-only",
            "--include-packages",
            "--manifest",
            MANIFEST,
        ]
        .as_slice(),
        [
            "--project-only",
            "check",
            "--manifest",
            MANIFEST,
            "--include-packages",
        ]
        .as_slice(),
    ] {
        let out = run(args);
        assert!(!out.status.success(), "{args:?} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cannot be used with '--include-packages'"),
            "{args:?} reports the conflict, got: {stderr}"
        );
    }

    // `--list-themes` never loads a project, so a scope flag alongside it is
    // noise — the deprecated flag is declared to conflict with it too.
    let out = run(&["--project-only", "--list-themes"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with '--list-themes'"),
        "the query-flag conflict, got: {stderr}"
    );
}

#[test]
fn select_miss_hints_at_the_hidden_package_scope() {
    // `audit_rollups` IS in the manifest — but in the installed package, which
    // the default scope pruned. The error names the escape hatch.
    let out = run(&["--manifest", MANIFEST, "--select", "audit_rollups"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "{stderr}");
    assert!(
        stderr.contains("--include-packages"),
        "the hint names the flag: {stderr}"
    );
}

#[test]
fn unrecorded_multi_package_manifest_warns_about_the_root_guess() {
    // No `metadata.project_name` + several packages → the frequency fallback
    // is a GUESS; the load must say so on stderr rather than silently picking
    // whichever package happens to outnumber the root project.
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = write_manifest(
        dir.path(),
        None,
        &["model.aaa.a", "model.bbb.b", "model.bbb.c"],
    );
    let out = run(&["check", "--manifest", &manifest]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("metadata.project_name") && stderr.contains("bbb"),
        "the ambiguous-root warning names the guess, got: {stderr}"
    );
}

#[test]
fn deps_only_manifest_warns_that_the_scope_emptied_the_graph() {
    // Recorded root contributes NO nodes: an honestly empty graph plus the
    // warning that explains where every node went — never a silent re-scope
    // to some other package.
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = write_manifest(
        dir.path(),
        Some("zzz_proj"),
        &["model.aaa.a", "model.aaa.b"],
    );
    let out = run(&["check", "--manifest", &manifest]);
    // `check` passes on the empty graph (0/0 coverage); the warning is the
    // contract being tested.
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no nodes belong to the root project 'zzz_proj'"),
        "the emptied-graph warning, got: {stderr}"
    );
}

#[test]
fn layer_violation_requires_both_endpoints_in_the_root_project() {
    // The documented scope rule: BOTH endpoints must be root models — a
    // package node's `path` ranks against the PACKAGE's own layer taxonomy,
    // so a package→root edge ranking "backwards" would flag every package
    // consumer as a violation. Such edges are dependency context, never
    // your project's health — under either scope.
    let manifest = r#"{
        "metadata": {"project_name": "pkg"},
        "nodes": {
            "model.pkg.stg_a":    {"name": "stg_a",   "resource_type": "model", "path": "staging/stg_a.sql"},
            "model.pkg.mart_p":   {"name": "mart_p",  "resource_type": "model", "path": "marts/mart_p.sql"},
            "model.pkg.stg_b":    {"name": "stg_b",   "resource_type": "model", "path": "staging/stg_b.sql"},
            "model.dep.mart_p":   {"name": "mart_p",  "resource_type": "model", "path": "marts/mart_p.sql"},
            "model.pkg.mart_c":   {"name": "mart_c",  "resource_type": "model", "path": "marts/mart_c.sql"},
            "model.dep.stg_c":    {"name": "stg_c",   "resource_type": "model", "path": "staging/stg_c.sql"}
        },
        "sources": {},
        "parent_map": {
            "model.pkg.stg_a": ["model.pkg.mart_p"],
            "model.pkg.stg_b": ["model.dep.mart_p"],
            "model.dep.stg_c": ["model.pkg.mart_c"]
        },
        "child_map": {
            "model.pkg.mart_p": ["model.pkg.stg_a"],
            "model.dep.mart_p": ["model.pkg.stg_b"],
            "model.pkg.mart_c": ["model.dep.stg_c"]
        }
    }"#;
    let raw: dbtl::RawManifest = serde_json::from_str(manifest).expect("synthesized manifest");
    let dag = Dag::build(&raw);

    let violations = layer_violation_edges(&dag);
    assert_eq!(
        violations,
        vec![(
            "model.pkg.mart_p".to_string(),
            "model.pkg.stg_a".to_string()
        )],
        "only the root↔root backward edge reports — the package-parent and \
         package-child violations are dependency context, not project health"
    );

    // And under the default scope the answer is the same: the root↔root
    // violation survives the prune.
    let (scoped, _) = PackageScope::Root.apply_with_warnings(dag);
    assert_eq!(
        layer_violation_edges(&scoped),
        vec![(
            "model.pkg.mart_p".to_string(),
            "model.pkg.stg_a".to_string()
        )],
        "the default scope keeps the root↔root violation"
    );
}

#[test]
fn docs_all_scope_with_zero_package_nodes_omits_the_count() {
    // A single-package Dag under --include-packages: "(0 installed-package
    // nodes)" would read as a bug — the bullet degrades to the bare scope
    // label instead of quoting a meaningless zero.
    let files = generate_docs(&manifest_root(), "pkg", "fixture", PackageScope::All);
    let readme = readme_of(&files);
    assert!(
        readme.contains("- scope: all packages (metrics count the root project only)"),
        "the bare label when there is nothing extra, got:\n{readme}"
    );
    assert!(
        !readme.contains("0 installed-package nodes"),
        "the meaningless zero is never written, got:\n{readme}"
    );
}
