use super::*;
use crate::manifest::{RawConfig, RawTestMetadata};

fn sample_manifest() -> RawManifest {
    // a -> b -> c, plus a test node hanging off b (must not bridge). The test
    // carries metadata so the pre-prune tests-capture is exercised too.
    let nodes = HashMap::from([
        (
            "model.p.a".to_string(),
            RawNode {
                name: "a".into(),
                resource_type: "model".into(),
                path: Some("staging/a.sql".into()),
                ..Default::default()
            },
        ),
        (
            "model.p.b".to_string(),
            RawNode {
                name: "b".into(),
                resource_type: "model".into(),
                path: Some("marts/b.sql".into()),
                config: RawConfig {
                    materialized: Some("view".into()),
                },
                ..Default::default()
            },
        ),
        (
            "test.p.t".to_string(),
            RawNode {
                name: "t".into(),
                resource_type: "test".into(),
                attached_node: Some("model.p.b".into()),
                column_name: Some("id".into()),
                test_metadata: Some(RawTestMetadata {
                    name: Some("not_null".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ]);
    let sources = HashMap::from([(
        "source.p.s.c".to_string(),
        RawSource {
            name: "c".into(),
            resource_type: "source".into(),
            path: None,
            ..Default::default()
        },
    )]);
    // child_map: a -> b -> t (test). If we traversed raw + filtered after,
    // a -> b -> t would still stop at t; but the prune-first guarantee is
    // that t is never a transit point. Add c as an upstream of a.
    let parent_map = HashMap::from([
        ("model.p.a".to_string(), vec!["source.p.s.c".to_string()]),
        ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
        ("test.p.t".to_string(), vec!["model.p.b".to_string()]),
    ]);
    let child_map = HashMap::from([
        ("source.p.s.c".to_string(), vec!["model.p.a".to_string()]),
        ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
        ("model.p.b".to_string(), vec!["test.p.t".to_string()]),
    ]);
    RawManifest {
        metadata: Default::default(),
        nodes,
        sources,
        exposures: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
    }
}

#[test]
fn merge_excludes_test_and_operation() {
    let dag = Dag::build(&sample_manifest());
    // model a, model b, source c == 3 kept; test t excluded.
    assert_eq!(dag.len(), 3);
    assert!(dag.contains("model.p.a"));
    assert!(dag.contains("source.p.s.c"));
    assert!(!dag.contains("test.p.t"));
    assert_eq!(dag.count_by_resource_type("test"), 0);
}

#[test]
fn analysis_nodes_are_excluded_like_tests() {
    // manifest mode is the only loader that ever sees `analysis` nodes
    // (source.rs never parses `analyses/`); excluding them here keeps the
    // two load paths' graphs identical.
    let nodes = HashMap::from([
        (
            "model.p.a".to_string(),
            RawNode {
                name: "a".into(),
                resource_type: "model".into(),
                ..Default::default()
            },
        ),
        (
            "analysis.p.x".to_string(),
            RawNode {
                name: "x".into(),
                resource_type: "analysis".into(),
                ..Default::default()
            },
        ),
    ]);
    let parent_map = HashMap::from([("analysis.p.x".to_string(), vec!["model.p.a".to_string()])]);
    let child_map = HashMap::from([("model.p.a".to_string(), vec!["analysis.p.x".to_string()])]);
    let dag = Dag::build(&RawManifest {
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
    });
    assert!(dag.get("analysis.p.x").is_none());
    assert_eq!(dag.count_by_resource_type("analysis"), 0);
    assert!(dag.upstream("model.p.a").is_empty());
    assert!(dag.downstream("model.p.a").is_empty());
}

#[test]
fn closure_is_multihop_and_excludes_self() {
    let dag = Dag::build(&sample_manifest());
    // c -> a -> b downstream.
    let down = dag.downstream("source.p.s.c");
    assert_eq!(
        down,
        HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()])
    );
    assert!(!down.contains("source.p.s.c"));
    // b upstream is a and c.
    let up = dag.upstream("model.p.b");
    assert_eq!(
        up,
        HashSet::from(["model.p.a".to_string(), "source.p.s.c".to_string()])
    );
}

#[test]
fn excluded_node_is_not_a_transit_hop() {
    let dag = Dag::build(&sample_manifest());
    // b's downstream child in raw child_map is the test node only, so after
    // exclusion downstream(b) is empty (the test is dropped, not bridged).
    assert!(dag.downstream("model.p.b").is_empty());
}

#[test]
fn tests_are_captured_pre_prune_without_bridging_topology() {
    // The test node `t` is attached to model b. It must be recorded in the
    // tests side map for b, yet must NOT appear as a node nor bridge lineage:
    // downstream(b) stays empty.
    let dag = Dag::build(&sample_manifest());
    let b_tests = dag.tests("model.p.b");
    assert_eq!(b_tests.len(), 1, "b has one captured test");
    assert_eq!(b_tests[0].kind, "not_null");
    assert_eq!(b_tests[0].column_name.as_deref(), Some("id"));
    assert!(!dag.contains("test.p.t"), "test node is not a DAG node");
    assert!(
        dag.downstream("model.p.b").is_empty(),
        "test never bridges lineage"
    );
    assert!(dag.tests("model.p.a").is_empty(), "no tests on a");
}

#[test]
fn detail_surfaces_materialized_from_config() {
    let dag = Dag::build(&sample_manifest());
    let b = dag.detail("model.p.b").expect("b has detail");
    assert_eq!(b.materialized.as_deref(), Some("view"));
    // A source has no materialization.
    assert_eq!(dag.detail("source.p.s.c").unwrap().materialized, None);
    // Unknown node has no detail.
    assert!(dag.detail("model.p.nope").is_none());
}

#[test]
fn closure_is_cycle_safe() {
    // a <-> b cycle.
    let nodes = HashMap::from([
        (
            "model.p.a".to_string(),
            RawNode {
                name: "a".into(),
                resource_type: "model".into(),
                ..Default::default()
            },
        ),
        (
            "model.p.b".to_string(),
            RawNode {
                name: "b".into(),
                resource_type: "model".into(),
                ..Default::default()
            },
        ),
    ]);
    let child_map = HashMap::from([
        ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
        ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
    ]);
    let manifest = RawManifest {
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::new(),
        child_map,
        macros: HashMap::new(),
    };
    let dag = Dag::build(&manifest);
    let down = dag.downstream("model.p.a");
    // Must terminate; self excluded, b included.
    assert_eq!(down, HashSet::from(["model.p.b".to_string()]));
}

#[test]
fn unknown_start_returns_empty() {
    let dag = Dag::build(&sample_manifest());
    assert!(dag.upstream("model.p.nonexistent").is_empty());
    assert!(dag.downstream("model.p.nonexistent").is_empty());
}

#[test]
fn subgraph_collects_nodes_and_edges() {
    // c -> a -> b (source c, models a/b). Subgraph of `a` is {c, a, b} with
    // edges c->a and a->b.
    let dag = Dag::build(&sample_manifest());
    let sg = dag.subgraph("model.p.a");
    let ids: HashSet<&str> = sg.nodes.iter().map(|n| n.unique_id.as_str()).collect();
    assert_eq!(
        ids,
        HashSet::from(["source.p.s.c", "model.p.a", "model.p.b"]),
        "subgraph of a is its full lineage"
    );
    assert_eq!(sg.selected, "model.p.a");
    // Both dependency edges present, endpoints in the set.
    let edge_pairs: HashSet<(&str, &str)> = sg
        .edges
        .iter()
        .map(|e| (e.parent.as_str(), e.child.as_str()))
        .collect();
    assert_eq!(
        edge_pairs,
        HashSet::from([("source.p.s.c", "model.p.a"), ("model.p.a", "model.p.b")]),
        "edges are the two parent->child dependencies"
    );
}

#[test]
fn subgraph_is_deterministic_and_sorted() {
    // Two calls must produce bit-identical Subgraphs (HashSet closures are
    // unordered; subgraph() must sort to a stable total order).
    let dag = Dag::build(&sample_manifest());
    let a = dag.subgraph("model.p.b");
    let b = dag.subgraph("model.p.b");
    assert_eq!(a, b, "subgraph must be deterministic across calls");
    // nodes sorted by unique_id; edges sorted by (parent, child).
    let node_ids: Vec<&str> = a.nodes.iter().map(|n| n.unique_id.as_str()).collect();
    let mut sorted = node_ids.clone();
    sorted.sort_unstable();
    assert_eq!(node_ids, sorted, "nodes sorted by unique_id");
}

#[test]
fn exposures_join_as_terminator_leaves_with_side_map_payload() {
    // An exposure declared via depends_on ONLY (the source-mode shape: no
    // parent_map/child_map entries) still joins the kept set as a leaf:
    // parents from depends_on, downstream closure reaches it, and it never
    // gains children. The kind/owner/url land in the side map; a dangling
    // dep is dropped like an unresolved ref.
    use crate::manifest::{RawExposure, RawExposureOwner};
    let mut manifest = sample_manifest();
    manifest.exposures.insert(
        "exposure.p.dash".to_string(),
        RawExposure {
            name: "dash".into(),
            exposure_type: Some("dashboard".into()),
            owner: RawExposureOwner {
                name: Some("Finance".into()),
                email: Some("fin@example.com".into()),
            },
            url: Some("https://bi.example.com/1".into()),
            description: Some("Weekly overview.\n".into()),
            depends_on: crate::RawDependsOn {
                nodes: vec![
                    "model.p.b".to_string(),
                    "model.p.missing".to_string(), // dangling: dropped
                ],
                macros: vec![],
            },
            ..Default::default()
        },
    );
    let dag = Dag::build(&manifest);
    let exp = "exposure.p.dash";
    assert_eq!(dag.count_by_resource_type("exposure"), 1);
    let info = dag.get(exp).expect("exposure is a kept node");
    assert_eq!(info.resource_type, "exposure");
    assert_eq!(
        (info.direct_up, info.direct_down),
        (1, 0),
        "leaf with 1 parent"
    );
    assert_eq!(
        dag.upstream(exp),
        HashSet::from([
            "model.p.b".to_string(),
            "model.p.a".to_string(),
            "source.p.s.c".to_string(),
        ]),
        "the dangling dep never became an edge"
    );
    assert!(dag.downstream(exp).is_empty(), "exposures have no children");
    assert!(
        dag.downstream("model.p.a").contains(exp),
        "the exposure is reachable downstream of its ancestors"
    );
    let payload = dag.exposure(exp).expect("side-map payload");
    assert_eq!(payload.exposure_type.as_deref(), Some("dashboard"));
    assert_eq!(payload.owner_name.as_deref(), Some("Finance"));
    assert_eq!(payload.owner_email.as_deref(), Some("fin@example.com"));
    assert_eq!(payload.url.as_deref(), Some("https://bi.example.com/1"));
    assert!(
        dag.exposure("model.p.b").is_none(),
        "models have no payload"
    );
    let detail = dag.detail(exp).expect("exposure detail");
    assert_eq!(detail.description.as_deref(), Some("Weekly overview."));
    assert_eq!(detail.materialized, None);
}

#[test]
fn exposure_edges_from_parent_map_and_depends_on_are_deduplicated() {
    // A real manifest records exposure edges in parent_map/child_map AND in
    // depends_on; the merge must not double-count them (direct_up == 1).
    use crate::manifest::RawExposure;
    let mut manifest = sample_manifest();
    manifest.exposures.insert(
        "exposure.p.dash".to_string(),
        RawExposure {
            name: "dash".into(),
            depends_on: crate::RawDependsOn {
                nodes: vec!["model.p.b".to_string()],
                macros: vec![],
            },
            ..Default::default()
        },
    );
    manifest
        .parent_map
        .insert("exposure.p.dash".to_string(), vec!["model.p.b".to_string()]);
    manifest
        .child_map
        .entry("model.p.b".to_string())
        .or_default()
        .push("exposure.p.dash".to_string());
    let dag = Dag::build(&manifest);
    let info = dag.get("exposure.p.dash").unwrap();
    assert_eq!(
        info.direct_up, 1,
        "parent_map + depends_on dedup to one edge"
    );
    let b = dag.get("model.p.b").unwrap();
    assert_eq!(b.direct_down, 1, "child side deduplicated too");
}

#[test]
fn subgraph_unknown_node_is_empty() {
    let dag = Dag::build(&sample_manifest());
    let sg = dag.subgraph("model.p.nope");
    assert!(sg.nodes.is_empty());
    assert!(sg.edges.is_empty());
    assert_eq!(sg.selected, "model.p.nope");
}

#[test]
fn filtered_prunes_nodes_edges_and_side_maps() {
    // model.p.a -> model.p.b -> model.q.c: keep only package p.
    let mut manifest = sample_manifest();
    manifest.nodes.insert(
        "model.q.c".to_string(),
        crate::RawNode {
            name: "c".into(),
            resource_type: "model".into(),
            package_name: Some("q".into()),
            ..Default::default()
        },
    );
    manifest
        .parent_map
        .insert("model.q.c".to_string(), vec!["model.p.b".to_string()]);
    manifest
        .child_map
        .entry("model.p.b".to_string())
        .or_default()
        .push("model.q.c".to_string());
    let dag = Dag::build(&manifest);
    assert_eq!(dag.primary_package().as_deref(), Some("p"));

    let f = dag.filtered(|n| n.unique_id.starts_with("model.p."));
    assert!(f.get("model.q.c").is_none(), "foreign package dropped");
    assert!(
        f.edges()
            .iter()
            .all(|(p, c)| f.get(p).is_some() && f.get(c).is_some()),
        "no edge endpoint dangles into the pruned set"
    );
    assert!(
        f.get("model.p.b").unwrap().direct_down == 0,
        "b's dropped child no longer counts"
    );
}

#[test]
fn semantic_resources_and_unit_tests_attach() {
    let mut manifest = sample_manifest();
    manifest.semantic_models.insert(
        "semantic_model.p.orders".to_string(),
        crate::manifest::RawSemanticResource {
            name: "orders".into(),
            depends_on: crate::RawDependsOn {
                nodes: vec!["model.p.b".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    manifest.metrics.insert(
        "metric.p.order_total".to_string(),
        crate::manifest::RawSemanticResource {
            name: "order_total".into(),
            depends_on: crate::RawDependsOn {
                nodes: vec!["semantic_model.p.orders".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    manifest.unit_tests.insert(
        "unit_tests.p.b.test_b".to_string(),
        crate::manifest::RawUnitTest {
            name: "test_b".into(),
            model: Some("b".into()),
            ..Default::default()
        },
    );
    let dag = Dag::build(&manifest);
    let sem = dag.get("semantic_model.p.orders").unwrap();
    assert_eq!(sem.resource_type, "semantic_model");
    assert_eq!(sem.direct_up, 1, "semantic model hangs off model.p.b");
    let met = dag.get("metric.p.order_total").unwrap();
    assert_eq!(met.direct_up, 1, "metric chains off the semantic model");
    assert_eq!(
        dag.get("model.p.b").unwrap().direct_down,
        1,
        "semantic consumer counts as downstream"
    );
    assert!(
        dag.tests("model.p.b")
            .iter()
            .any(|t| t.kind == "unit" && t.name == "test_b"),
        "unit test recorded on its model"
    );
}

/// A three-package manifest where the uid-segment heuristics DISAGREE —
/// `aaa_pkg` is the lexicographic min (1 model), `mmm_pkg` the most
/// frequent (3 models), `zzz_proj` the recorded root project (1 model) —
/// parameterized by `metadata.project_name` so each test picks
/// recorded-vs-fallback. The min/most-frequent divergence matters: a
/// fixture where they coincide could not detect the wrong one being used.
fn multi_package_manifest(root_project: Option<&str>) -> RawManifest {
    let mut nodes = HashMap::new();
    let mut model = |uid: &str, name: &str| {
        nodes.insert(
            uid.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                ..Default::default()
            },
        );
    };
    model("model.zzz_proj.root_model", "root_model");
    model("model.aaa_pkg.pkg_a", "pkg_a");
    for i in 0..3 {
        model(&format!("model.mmm_pkg.pkg_{i}"), &format!("mmm_{i}"));
    }
    RawManifest {
        metadata: crate::manifest::RawMetadata {
            project_name: root_project.map(str::to_string),
            ..Default::default()
        },
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
    }
}

#[test]
fn project_name_prefers_root_project_metadata() {
    // `aaa_pkg` sorts BEFORE `zzz_proj` and `mmm_pkg` outnumbers it — but
    // metadata says the root project is `zzz_proj`, and that wins for both
    // the display name and the `--project-only` scope.
    let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
    assert_eq!(dag.project_name(), Some("zzz_proj"));
    assert_eq!(dag.primary_package().as_deref(), Some("zzz_proj"));
}

#[test]
fn project_name_and_primary_package_fall_back_to_uid_heuristics() {
    // Manifests recording no `metadata.project_name` keep the legacy
    // inference — and the two heuristics must not be conflated: the
    // display name is the lexicographic min (`aaa_pkg`), the owning
    // package the most frequent (`mmm_pkg`).
    let dag = Dag::build(&multi_package_manifest(None));
    assert_eq!(dag.project_name(), Some("aaa_pkg"));
    assert_eq!(dag.primary_package().as_deref(), Some("mmm_pkg"));
}

#[test]
fn blank_project_name_metadata_falls_back_too() {
    // An empty or whitespace-only `project_name` is as good as absent —
    // the fallback answers, not the blank string.
    for blank in ["", "  "] {
        let dag = Dag::build(&multi_package_manifest(Some(blank)));
        assert_eq!(dag.project_name(), Some("aaa_pkg"), "blank {blank:?}");
        assert_eq!(
            dag.primary_package().as_deref(),
            Some("mmm_pkg"),
            "blank {blank:?}"
        );
    }
}

#[test]
fn primary_package_names_root_even_when_root_has_no_nodes() {
    // A deps-only manifest records a root project that contributes NO
    // nodes: `primary_package` still names it honestly (it is a fact
    // about the manifest, not a vote over surviving uids) — and a
    // `--project-only` filter therefore yields an EMPTY graph, which
    // `cli.rs` surfaces as a warning rather than silently re-scoping to a
    // foreign package.
    let mut manifest = multi_package_manifest(Some("zzz_proj"));
    manifest.nodes.remove("model.zzz_proj.root_model");
    let dag = Dag::build(&manifest);
    assert_eq!(dag.primary_package().as_deref(), Some("zzz_proj"));
    assert!(dag
        .filtered(|n| n.unique_id.split('.').nth(1) == Some("zzz_proj"))
        .is_empty());
}

#[test]
fn filtered_keeps_the_root_project_name() {
    // `--project-only` filters to the root package; the filtered Dag must
    // still name itself `zzz_proj` (metadata is a property of the source
    // manifest, not of the surviving uid set).
    let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
    let f = dag.filtered(|n| n.unique_id.split('.').nth(1) == Some("zzz_proj"));
    assert_eq!(f.len(), 1);
    assert_eq!(f.project_name(), Some("zzz_proj"));
    assert_eq!(f.primary_package().as_deref(), Some("zzz_proj"));
}

// ---- PackageScope::apply_with_warnings -------------------------------

#[test]
fn apply_with_warnings_flags_the_ambiguous_root_guess() {
    // No recorded `metadata.project_name` + several packages → the
    // frequency fallback is a guess, and the warning names it — under BOTH
    // scopes (`All` skips the prune but every root-scoped metric still
    // trusts the same guess). Here `mmm_pkg` wins on frequency while
    // `aaa_pkg` sorts first — the guess is real.
    let dag = Dag::build(&multi_package_manifest(None));
    let (scoped, w) = PackageScope::Root.apply_with_warnings(dag);
    assert_eq!(w.len(), 1, "one ambiguity warning, got {w:?}");
    assert!(w[0].contains("metadata.project_name"), "{w:?}");
    assert!(
        w[0].contains("mmm_pkg"),
        "the warning names the guess: {w:?}"
    );
    assert!(
        scoped
            .nodes()
            .keys()
            .all(|u| u.split('.').nth(1) == Some("mmm_pkg")),
        "the guessed package scopes the pruned graph"
    );

    let dag = Dag::build(&multi_package_manifest(None));
    let (_, w) = PackageScope::All.apply_with_warnings(dag);
    assert_eq!(w.len(), 1, "the same guess is warned under All");

    // Recorded metadata is not a guess — no warning either way.
    let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
    let (_, w) = PackageScope::Root.apply_with_warnings(dag);
    assert!(w.is_empty(), "a recorded root never warns, got {w:?}");
    let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
    let (_, w) = PackageScope::All.apply_with_warnings(dag);
    assert!(w.is_empty());
}

#[test]
fn apply_with_warnings_reports_the_emptied_graph() {
    // A deps-only manifest (recorded root contributes NO nodes): the Root
    // scope yields an honestly empty graph plus the warning the CLI prints
    // and `App::reload` toasts — never a silent re-scope.
    let mut manifest = multi_package_manifest(Some("zzz_proj"));
    manifest.nodes.remove("model.zzz_proj.root_model");
    let dag = Dag::build(&manifest);
    let (scoped, w) = PackageScope::Root.apply_with_warnings(dag);
    assert!(scoped.is_empty());
    assert!(
        w.iter()
            .any(|m| m.contains("no nodes belong to the root project 'zzz_proj'")),
        "the emptied-graph warning, got {w:?}"
    );
}

#[test]
fn apply_with_warnings_is_quiet_on_a_normal_root_scope() {
    let dag = Dag::build(&multi_package_manifest(Some("zzz_proj")));
    let (scoped, w) = PackageScope::Root.apply_with_warnings(dag);
    assert_eq!(scoped.len(), 1, "only the root project's model survives");
    assert!(w.is_empty(), "a clean prune warns about nothing, got {w:?}");
}

// ---- compiled side map ingestion (target/compiled/) -------------------
// `Dag::ingest_compiled` driven straight on tempdirs; the wider
// load-path coverage lives in tests/compiled_fixture.rs.

fn stamp_mtime(path: &Path, secs_ago: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago))
        .unwrap();
}

#[test]
fn ingest_compiled_installed_package_resolves_only_package_dirs() {
    // An installed-package node's original_file_path is PACKAGE-root-
    // relative: its source lives under `dbt_packages/<pkg>/` or
    // `dbt_internal_packages/<pkg>/` — never `<project_root>/<ofp>`. A
    // local file sharing the path must not influence staleness.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let compiled_dir = root.join("target/compiled");
    let cfile = compiled_dir.join("dep/models/m.sql");
    std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
    std::fs::write(&cfile, "select 1").unwrap();
    stamp_mtime(&cfile, 3600); // expansion is old

    // An unrelated project file at <root>/models/m.sql, NEWER than the
    // expansion: a local node would read it as the source; `dep` must not.
    std::fs::create_dir_all(root.join("models")).unwrap();
    let local_src = root.join("models/m.sql");
    std::fs::write(&local_src, "select unrelated").unwrap();
    stamp_mtime(&local_src, 60);

    let mut manifest = sample_manifest();
    manifest.nodes.insert(
        "model.dep.m".to_string(),
        crate::RawNode {
            name: "m".into(),
            resource_type: "model".into(),
            package_name: Some("dep".into()),
            original_file_path: Some("models/m.sql".into()),
            ..Default::default()
        },
    );
    let mut dag = Dag::build(&manifest);
    dag.ingest_compiled(&compiled_dir, Some(root));
    let c = dag
        .compiled("model.dep.m")
        .expect("compiled file resolves under dep/");
    assert!(
        !c.stale,
        "a project-root file must not mark an installed package stale"
    );

    // The package checkout IS the real source — newer means stale.
    // (ingest skips uids already in the map, so rebuild for round two.)
    let pkg_src = root.join("dbt_packages/dep/models/m.sql");
    std::fs::create_dir_all(pkg_src.parent().unwrap()).unwrap();
    std::fs::write(&pkg_src, "select 2").unwrap();
    stamp_mtime(&pkg_src, 30);
    let mut dag = Dag::build(&manifest);
    dag.ingest_compiled(&compiled_dir, Some(root));
    assert!(
        dag.compiled("model.dep.m").unwrap().stale,
        "dbt_packages source newer than expansion is stale"
    );
}

#[test]
fn ingest_compiled_loads_python_models() {
    // dbt keeps a `.py` artifact under compiled/ for Python models
    // (Snowpark/Databricks) — same <pkg>/<ofp> layout, different suffix.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let compiled_dir = root.join("target/compiled");
    let cfile = compiled_dir.join("p/models/py_mod.py");
    std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
    std::fs::write(&cfile, "def model(dbt, session):\n    return dbt.ref('a')").unwrap();

    let mut manifest = sample_manifest();
    manifest.nodes.insert(
        "model.p.py_mod".to_string(),
        crate::RawNode {
            name: "py_mod".into(),
            resource_type: "model".into(),
            package_name: Some("p".into()),
            original_file_path: Some("models/py_mod.py".into()),
            ..Default::default()
        },
    );
    let mut dag = Dag::build(&manifest);
    dag.ingest_compiled(&compiled_dir, Some(root));
    assert_eq!(
        dag.compiled_code("model.p.py_mod"),
        Some("def model(dbt, session):\n    return dbt.ref('a')"),
        "the .py suffix must not exclude Python models"
    );
}

#[test]
fn ingest_compiled_empty_package_name_uses_uid_segment() {
    // `package_name: ""` never occurs in real dbt output, but a
    // hand-rolled manifest could carry one — an empty string must fall
    // through to the uid's project segment like a missing field.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let compiled_dir = root.join("target/compiled");
    let cfile = compiled_dir.join("p/models/e.sql");
    std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
    std::fs::write(&cfile, "select 3").unwrap();

    let mut manifest = sample_manifest();
    manifest.nodes.insert(
        "model.p.e".to_string(),
        crate::RawNode {
            name: "e".into(),
            resource_type: "model".into(),
            package_name: Some(String::new()),
            original_file_path: Some("models/e.sql".into()),
            ..Default::default()
        },
    );
    let mut dag = Dag::build(&manifest);
    dag.ingest_compiled(&compiled_dir, Some(root));
    assert_eq!(dag.compiled_code("model.p.e"), Some("select 3"));
}

// ---- Issue 11: the shared PK predicate + relationships metadata ----

fn ti(kind: &str, col: Option<&str>) -> TestInfo {
    TestInfo {
        name: String::new(),
        kind: kind.into(),
        column_name: col.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn has_pk_guarantee_needs_a_same_column_pair_or_combination() {
    // Same-column unique + not_null qualifies.
    assert!(has_pk_guarantee(&[
        ti("unique", Some("id")),
        ti("not_null", Some("id")),
    ]));
    // A split pair — unique(a) + not_null(b) — does NOT.
    assert!(!has_pk_guarantee(&[
        ti("unique", Some("a")),
        ti("not_null", Some("b")),
    ]));
    // unique_combination_of_columns alone qualifies, including a
    // package-namespaced spelling.
    assert!(has_pk_guarantee(&[ti(
        "unique_combination_of_columns",
        None
    )]));
    assert!(has_pk_guarantee(&[ti(
        "dbt_utils.unique_combination_of_columns",
        None
    )]));
    // A lone unique or not_null never qualifies; an empty set trivially not.
    assert!(!has_pk_guarantee(&[ti("unique", Some("id"))]));
    assert!(!has_pk_guarantee(&[ti("not_null", Some("id"))]));
    assert!(!has_pk_guarantee(&[]));
}

#[test]
fn relationships_tests_capture_their_target_and_field() {
    // A relationships test node: attached to m, depends_on carries BOTH
    // the attached node and the `to:` target; kwargs carries `field`.
    let mut manifest = sample_manifest();
    manifest.nodes.insert(
        "test.p.rel".to_string(),
        RawNode {
            name: "rel".into(),
            resource_type: "test".into(),
            attached_node: Some("model.p.b".into()),
            column_name: Some("a_id".into()),
            test_metadata: Some(RawTestMetadata {
                name: Some("relationships".into()),
                kwargs: Some(serde_json::Map::from_iter([(
                    "field".to_string(),
                    serde_json::Value::String("id".to_string()),
                )])),
            }),
            depends_on: crate::RawDependsOn {
                macros: vec![],
                nodes: vec!["model.p.b".to_string(), "model.p.a".to_string()],
            },
            ..Default::default()
        },
    );
    let dag = Dag::build(&manifest);
    let tests = dag.tests("model.p.b");
    let rel = tests
        .iter()
        .find(|t| t.kind == "relationships")
        .expect("the relationships test is captured");
    assert_eq!(rel.column_name.as_deref(), Some("a_id"));
    assert_eq!(rel.ref_uid.as_deref(), Some("model.p.a"));
    assert_eq!(rel.ref_name.as_deref(), Some("a"));
    assert_eq!(rel.ref_column.as_deref(), Some("id"));
    // The sibling not_null test has no target.
    let nn = tests.iter().find(|t| t.kind == "not_null").unwrap();
    assert_eq!(nn.ref_uid, None);
    assert_eq!(nn.ref_column, None);
}

#[test]
fn unit_tests_carry_their_shape() {
    // A unit_test entry attaches to its model by uid-shape guess; the
    // shape counts given inputs + inline row counts.
    let mut manifest = sample_manifest();
    manifest.unit_tests.insert(
        "unit_test.p.b.my_unit".to_string(),
        crate::RawUnitTest {
            name: "my_unit".into(),
            model: Some("b".into()),
            given: vec![
                crate::RawUnitTestIo {
                    input: Some("ref('a')".into()),
                    rows: Some(serde_json::json!([{"id": 1}, {"id": 2}])),
                },
                crate::RawUnitTestIo {
                    input: Some("source('s','c')".into()),
                    rows: Some(serde_json::json!([{"id": 3}])),
                },
            ],
            expect: Some(crate::RawUnitTestIo {
                input: None,
                rows: Some(serde_json::json!([{"id": 1}])),
            }),
        },
    );
    let dag = Dag::build(&manifest);
    let tests = dag.tests("model.p.b");
    let unit = tests
        .iter()
        .find(|t| t.kind == "unit")
        .expect("the unit test is captured");
    assert_eq!(unit.name, "my_unit");
    let shape = unit.unit.as_ref().expect("unit shape present");
    assert_eq!(shape.inputs, 2);
    assert_eq!(shape.given_rows, Some(3));
    assert_eq!(shape.expect_rows, Some(1));
}

#[test]
fn same_named_relationships_tests_sort_by_their_target() {
    // Determinism guard: two `relationships` tests on the SAME column of
    // the SAME model — source mode names both `relationships_<node>_<col>`,
    // so the (kind, column_name, name) key ties and the STABLE sort would
    // leak `manifest.nodes` HashMap iteration order (random per build)
    // into the tests vec — flaking `docs --check` diffs. `ref_uid` breaks
    // the tie; rebuild repeatedly so a broken key can't pass by luck.
    let build = || {
        let mut manifest = sample_manifest();
        for name in ["x", "y"] {
            manifest.nodes.insert(
                format!("model.p.{name}"),
                RawNode {
                    name: name.into(),
                    resource_type: "model".into(),
                    ..Default::default()
                },
            );
        }
        for (key, to) in [("test.p.rel_y", "model.p.y"), ("test.p.rel_x", "model.p.x")] {
            manifest.nodes.insert(
                key.to_string(),
                RawNode {
                    // The shared, synthesized-style name — the collision
                    // the sort key has to absorb.
                    name: "relationships_b_a_id".into(),
                    resource_type: "test".into(),
                    attached_node: Some("model.p.b".into()),
                    column_name: Some("a_id".into()),
                    test_metadata: Some(RawTestMetadata {
                        name: Some("relationships".into()),
                        kwargs: Some(serde_json::Map::from_iter([(
                            "field".to_string(),
                            serde_json::Value::String("id".to_string()),
                        )])),
                    }),
                    depends_on: crate::RawDependsOn {
                        macros: vec![],
                        nodes: vec!["model.p.b".to_string(), to.to_string()],
                    },
                    ..Default::default()
                },
            );
        }
        Dag::build(&manifest)
    };
    for _ in 0..16 {
        let dag = build();
        let rels: Vec<Option<&str>> = dag
            .tests("model.p.b")
            .iter()
            .filter(|t| t.kind == "relationships")
            .map(|t| t.ref_uid.as_deref())
            .collect();
        assert_eq!(
            rels,
            vec![Some("model.p.x"), Some("model.p.y")],
            "ref_uid breaks the (kind, column, name) tie deterministically"
        );
    }
}
