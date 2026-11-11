use super::*;
use crate::manifest::{RawContract, RawManifest, RawNode, RawSource, RawTestMetadata};
use crate::project_config::{project_config_path, ProjectConfig, PROJECT_CONFIG_NAME};
use std::collections::HashMap;

/// Parse the `lint:` section out of `.dbtl.yml` text (the parse's own tests
/// live in `project_config` — here it's just a config builder).
fn config_from(yaml: &str) -> LintConfig {
    ProjectConfig::from_yaml(yaml).unwrap().lint
}

/// A manifest assembled from just the interesting fields — the remaining
/// maps default empty. `child_map` is derived from `edges` (parent, child)
/// pairs so each test writes its graph ONCE.
fn manifest(
    nodes: Vec<(String, RawNode)>,
    sources: Vec<(String, RawSource)>,
    edges: &[(&str, &str)],
) -> RawManifest {
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    for (p, c) in edges {
        parent_map
            .entry((*c).to_string())
            .or_default()
            .push((*p).to_string());
        child_map
            .entry((*p).to_string())
            .or_default()
            .push((*c).to_string());
    }
    RawManifest {
        metadata: Default::default(),
        nodes: nodes.into_iter().collect(),
        sources: sources.into_iter().collect(),
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

/// A model node literal; `path` carries the layer dir (`staging/x.sql`).
fn model(name: &str, path: &str) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(path.into()),
            ..Default::default()
        },
    )
}

/// A source node literal — `source.p.<src>.<tbl>` like a real manifest.
fn source(name: &str) -> (String, RawSource) {
    (
        format!("source.p.{name}"),
        RawSource {
            name: name.into(),
            resource_type: "source".into(),
            ..Default::default()
        },
    )
}

/// A generic-test node literal attached to `target` (kind + column).
fn test_on(target: &str, kind: &str, column: &str) -> (String, RawNode) {
    (
        format!("test.p.{kind}_{target}_{column}"),
        RawNode {
            name: format!("{kind}_{column}"),
            resource_type: "test".into(),
            attached_node: Some(format!("model.p.{target}")),
            column_name: Some(column.into()),
            test_metadata: Some(RawTestMetadata {
                name: Some(kind.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
}

/// The uids flagged by `rule` in `findings`, sorted for comparison.
fn flagged(findings: &[Finding], rule: LintRule) -> Vec<&str> {
    let mut uids: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == rule)
        .map(|f| f.node_uid.as_str())
        .collect();
    uids.sort_unstable();
    uids
}

fn findings_of(m: &RawManifest, config: &LintConfig) -> Vec<Finding> {
    run_rules(LintInput::new(&Dag::build(m), config))
}

#[test]
fn default_config_enables_every_rule() {
    let cfg = LintConfig::default();
    for rule in LintRule::ALL {
        assert!(cfg.enabled(rule), "{} must default to enabled", rule.name());
    }
    assert_eq!(
        cfg.int_param(LintRule::ModelFanout, "threshold"),
        Some(crate::PR_HUB_MIN_DOWNSTREAM)
    );
    assert_eq!(cfg.int_param(LintRule::SourceFanout, "threshold"), Some(2));
}

#[test]
fn yaml_disables_rules_and_overrides_thresholds() {
    let cfg = config_from(
        r#"lint:
  rules:
    undocumented_models: false
    source_fanout: false
  model_fanout_threshold: 3
  naming_prefixes:
    staging: [stg_, base_]
"#,
    );
    assert!(!cfg.enabled(LintRule::UndocumentedModels));
    assert!(!cfg.enabled(LintRule::SourceFanout));
    assert!(cfg.enabled(LintRule::RootModels));
    assert_eq!(cfg.int_param(LintRule::ModelFanout, "threshold"), Some(3));
    // Per-layer merge: staging overridden, intermediate/marts keep defaults.
    assert_eq!(
        cfg.naming_prefixes["staging"],
        vec!["stg_".to_string(), "base_".to_string()]
    );
    assert_eq!(
        cfg.naming_prefixes["intermediate"],
        vec!["int_".to_string()]
    );
}

#[test]
fn direct_join_to_source_flags_source_and_model_mix() {
    // bad joins src + stg_a; good joins only stg_a.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("bad", "staging/bad.sql"),
            model("good", "marts/good.sql"),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.stg_a"),
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
            ("model.p.stg_a", "model.p.good"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::DirectJoinToSource),
        vec!["model.p.bad"]
    );
}

#[test]
fn multiple_sources_joined_needs_two_sources() {
    let m = manifest(
        vec![
            model("stg_two", "staging/stg_two.sql"),
            model("stg_one", "staging/stg_one.sql"),
        ],
        vec![source("a.t"), source("b.t")],
        &[
            ("source.p.a.t", "model.p.stg_two"),
            ("source.p.b.t", "model.p.stg_two"),
            ("source.p.a.t", "model.p.stg_one"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::MultipleSourcesJoined),
        vec!["model.p.stg_two"]
    );
}

#[test]
fn source_fanout_flags_source_with_two_model_children() {
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("stg_b", "staging/stg_b.sql"),
        ],
        vec![source("s.hot"), source("s.cold")],
        &[
            ("source.p.s.hot", "model.p.stg_a"),
            ("source.p.s.hot", "model.p.stg_b"),
            ("source.p.s.cold", "model.p.stg_b"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(flagged(&f, LintRule::SourceFanout), vec!["source.p.s.hot"]);
}

#[test]
fn model_fanout_flags_leaf_children_at_threshold() {
    // hub feeds 2 leaf models; default threshold is 5 → no finding. With a
    // config of 2 the same graph flags hub. int_* keeps the names legal.
    let m = manifest(
        vec![
            model("int_hub", "intermediate/int_hub.sql"),
            model("fct_a", "marts/fct_a.sql"),
            model("fct_b", "marts/fct_b.sql"),
        ],
        vec![],
        &[
            ("model.p.int_hub", "model.p.fct_a"),
            ("model.p.int_hub", "model.p.fct_b"),
        ],
    );
    let base = Dag::build(&m);
    let default = run_rules(LintInput::new(&base, &LintConfig::default()));
    assert!(flagged(&default, LintRule::ModelFanout).is_empty());
    let cfg = config_from("lint:\n  model_fanout_threshold: 2\n");
    let tightened = run_rules(LintInput::new(&base, &cfg));
    assert_eq!(
        flagged(&tightened, LintRule::ModelFanout),
        vec!["model.p.int_hub"]
    );
}

#[test]
fn root_models_flags_parentless_models_only() {
    let m = manifest(
        vec![
            model("stg_root", "staging/stg_root.sql"),
            model("int_child", "intermediate/int_child.sql"),
        ],
        vec![],
        &[("model.p.stg_root", "model.p.int_child")],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(flagged(&f, LintRule::RootModels), vec!["model.p.stg_root"]);
}

#[test]
fn rejoining_upstream_flags_the_redundant_hop() {
    // a -> b -> c AND a -> c: b is a redundant hop. a -> d -> e with NO
    // a -> e shortcut: d is not flagged.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("int_b", "intermediate/int_b.sql"),
            model("fct_c", "marts/fct_c.sql"),
            model("int_d", "intermediate/int_d.sql"),
            model("fct_e", "marts/fct_e.sql"),
        ],
        vec![],
        &[
            ("model.p.stg_a", "model.p.int_b"),
            ("model.p.int_b", "model.p.fct_c"),
            ("model.p.stg_a", "model.p.fct_c"),
            ("model.p.stg_a", "model.p.int_d"),
            ("model.p.int_d", "model.p.fct_e"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::RejoiningUpstream)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.p.int_b");
    // related is the sorted {C} ∪ {A…} set, not positional.
    assert_eq!(
        hits[0].related,
        vec!["model.p.fct_c".to_string(), "model.p.stg_a".to_string()]
    );
}

#[test]
fn rejoining_upstream_counts_only_data_node_children_and_endpoints() {
    // evaluator's single_use_resources semantics:
    //  - source→B→C + source→C: B flags (A need not be a model).
    //  - an exposure child of B does NOT break "single use" (endpoints
    //    don't count as a use).
    //  - two bypassing parents still produce ONE finding (dedup per B).
    let mut m = manifest(
        vec![
            model("int_b", "intermediate/int_b.sql"),
            model("fct_c", "marts/fct_c.sql"),
            model("stg_a", "staging/stg_a.sql"),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.int_b"),
            ("model.p.int_b", "model.p.fct_c"),
            ("source.p.s.raw", "model.p.fct_c"),
            ("model.p.stg_a", "model.p.int_b"),
            ("model.p.stg_a", "model.p.fct_c"),
            ("model.p.int_b", "exposure.p.exp"),
        ],
    );
    m.exposures.insert(
        "exposure.p.exp".to_string(),
        crate::manifest::RawExposure {
            name: "exp".into(),
            ..Default::default()
        },
    );
    let findings = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.rule == LintRule::RejoiningUpstream)
        .collect();
    assert_eq!(hits.len(), 1, "exposure child + two A's → one finding");
    assert_eq!(hits[0].node_uid, "model.p.int_b");
    assert_eq!(
        hits[0].related,
        vec![
            "model.p.fct_c".to_string(),
            "model.p.stg_a".to_string(),
            "source.p.s.raw".to_string()
        ]
    );
}

#[test]
fn rejoining_upstream_flags_a_snapshot_relay() {
    // A→B→C + A→C where the redundant hop B is a snapshot (evaluator
    // resource kinds aren't model-only).
    let mut m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("fct_c", "marts/fct_c.sql"),
        ],
        vec![],
        &[
            ("model.p.stg_a", "snapshot.p.snap_b"),
            ("snapshot.p.snap_b", "model.p.fct_c"),
            ("model.p.stg_a", "model.p.fct_c"),
        ],
    );
    m.nodes.insert(
        "snapshot.p.snap_b".to_string(),
        RawNode {
            name: "snap_b".into(),
            resource_type: "snapshot".into(),
            path: Some("snapshots/snap_b.sql".into()),
            ..Default::default()
        },
    );
    let findings = findings_of(&m, &LintConfig::default());
    let hits = flagged(&findings, LintRule::RejoiningUpstream);
    assert_eq!(hits, vec!["snapshot.p.snap_b"]);
}

#[test]
fn marts_depend_on_source_scopes_to_marts_and_intermediate() {
    // staging MAY read sources; a marts/intermediate direct read flags.
    let m = manifest(
        vec![
            model("stg_ok", "staging/stg_ok.sql"),
            model("fct_bad", "marts/fct_bad.sql"),
            model("int_bad", "intermediate/int_bad.sql"),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.stg_ok"),
            ("source.p.s.raw", "model.p.fct_bad"),
            ("source.p.s.raw", "model.p.int_bad"),
            ("model.p.stg_ok", "model.p.fct_bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::MartsDependOnSource);
    got.sort_unstable();
    assert_eq!(got, vec!["model.p.fct_bad", "model.p.int_bad"]);
}

#[test]
fn marts_depend_on_source_resolves_the_layer_prefix_first() {
    // evaluator model_type semantics: staging/fct_x IS a marts model
    // (prefix beats folder) — under-flag fix; reports/x is 'other'.
    let m = manifest(
        vec![
            model("fct_x", "staging/fct_x.sql"),
            model("plain", "marts/plain.sql"),
            model("rep_x", "reports/rep_x.sql"),
            model("stg_ok", "staging/stg_ok.sql"),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.fct_x"),
            ("source.p.s.raw", "model.p.plain"),
            ("source.p.s.raw", "model.p.rep_x"),
            ("source.p.s.raw", "model.p.stg_ok"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::MartsDependOnSource);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.fct_x", "model.p.plain"],
        "fct_ in staging/ is marts (prefix-first); reports/ is 'other'"
    );
}

#[test]
fn missing_primary_key_tests_accepts_same_column_pair_or_combination() {
    // tested: unique+not_null on `id` → OK. combo: unique_combination → OK.
    // split: unique on a, not_null on b → still missing. bare: nothing.
    let m = manifest(
        vec![
            model("stg_tested", "staging/stg_tested.sql"),
            model("stg_combo", "staging/stg_combo.sql"),
            model("stg_split", "staging/stg_split.sql"),
            model("stg_bare", "staging/stg_bare.sql"),
            test_on("stg_tested", "unique", "id"),
            test_on("stg_tested", "not_null", "id"),
            test_on("stg_split", "unique", "a"),
            test_on("stg_split", "not_null", "b"),
            (
                "test.p.ucombo".to_string(),
                RawNode {
                    name: "ucombo".into(),
                    resource_type: "test".into(),
                    attached_node: Some("model.p.stg_combo".into()),
                    test_metadata: Some(RawTestMetadata {
                        name: Some("unique_combination_of_columns".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ),
        ],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::MissingPrimaryKeyTests);
    got.sort_unstable();
    assert_eq!(got, vec!["model.p.stg_bare", "model.p.stg_split"]);
}

#[test]
fn public_models_without_contract_flags_public_uncontracted() {
    let mut nodes = vec![
        model("dim_pub", "marts/dim_pub.sql"),
        model("dim_contract", "marts/dim_contract.sql"),
        model("dim_private", "marts/dim_private.sql"),
    ];
    nodes[0].1.access = Some("public".into());
    nodes[1].1.access = Some("public".into());
    nodes[1].1.contract = Some(RawContract { enforced: true });
    nodes[2].1.access = Some("private".into());
    let m = manifest(nodes, vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::PublicModelsWithoutContract),
        vec!["model.p.dim_pub"]
    );
}

#[test]
fn undocumented_models_flags_missing_and_blank_descriptions() {
    let mut nodes = vec![
        model("stg_doc", "staging/stg_doc.sql"),
        model("stg_bare", "staging/stg_bare.sql"),
        model("stg_blank", "staging/stg_blank.sql"),
    ];
    nodes[0].1.description = Some("documented".into());
    nodes[2].1.description = Some("   \n".into()); // cleaned to None at build
    let m = manifest(nodes, vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::UndocumentedModels);
    got.sort_unstable();
    assert_eq!(got, vec!["model.p.stg_bare", "model.p.stg_blank"]);
}

#[test]
fn naming_conventions_resolves_the_type_prefix_first() {
    // evaluator's coalesce(model_type_prefix, model_type_folder, 'other'):
    //  - staging/fct_x → marts type, fct_ is appropriate → NOT flagged
    //    (over-flag fix; the folder mismatch is a different evaluator rule
    //    — model_directories — which dbtl doesn't have).
    //  - marts/plain → folder type, no prefix → flagged.
    //  - reports/x → 'other', which is prefix-free by default (deliberate
    //    divergence: the evaluator expects rpt_ out of the box).
    let m = manifest(
        vec![
            model("fct_x", "staging/fct_x.sql"),
            model("plain", "marts/plain.sql"),
            model("rep", "reports/rep.sql"),
            model("stg_ok", "staging/stg_ok.sql"),
        ],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::NamingConventions),
        vec!["model.p.plain"],
        "prefix-first: staging/fct_x and reports/rep pass, marts/plain fails"
    );
}

#[test]
fn naming_conventions_other_layer_is_opt_in_via_config() {
    // Under-flag fix, opt-in: mapping `other:` makes unclassified models
    // need a prefix — evaluator-parity `rpt_` enforcement.
    let m = manifest(
        vec![
            model("rpt_ok", "reports/rpt_ok.sql"),
            model("plain", "reports/plain.sql"),
            model("util_x", "base/util_x.sql"),
        ],
        vec![],
        &[],
    );
    let cfg = config_from("lint:\n  naming_prefixes:\n    other: [rpt_]\n");
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::NamingConventions);
    got.sort_unstable();
    // rpt_ok carries rpt_ (a default marts prefix — it resolves to the
    // marts type and passes there); the prefix-less reports/ and base/
    // models resolve to 'other' and flag.
    assert_eq!(got, vec!["model.p.plain", "model.p.util_x"]);
}

#[test]
fn layer_violation_reuses_the_shared_predicate() {
    // stg_a depending on marts' fct_bad is the backward edge; stg->fct is
    // fine. node_uid is the model doing the bad dependency (the child).
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("fct_a", "marts/fct_a.sql"),
            model("fct_bad", "marts/fct_bad.sql"),
        ],
        vec![],
        &[
            ("model.p.stg_a", "model.p.fct_a"),
            ("model.p.fct_bad", "model.p.stg_a"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::LayerViolation)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.p.stg_a");
    assert_eq!(hits[0].related, vec!["model.p.fct_bad".to_string()]);
}

#[test]
fn disabled_rule_produces_no_findings() {
    let m = manifest(vec![model("stg_root", "staging/stg_root.sql")], vec![], &[]);
    let cfg = config_from("lint:\n  rules:\n    root_models: false\n");
    let f = findings_of(&m, &cfg);
    assert!(flagged(&f, LintRule::RootModels).is_empty());
    // Other rules still ran — undocumented_models fires on the same node.
    assert_eq!(
        flagged(&f, LintRule::UndocumentedModels),
        vec!["model.p.stg_root"]
    );
}

#[test]
fn findings_are_deterministic_and_rule_ordered() {
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("bad_name", "marts/bad_name.sql"),
        ],
        vec![source("s.raw")],
        &[("source.p.s.raw", "model.p.stg_a")],
    );
    let dag = Dag::build(&m);
    let a = run_rules(LintInput::new(&dag, &LintConfig::default()));
    let b = run_rules(LintInput::new(&dag, &LintConfig::default()));
    assert_eq!(a, b, "findings must be identical across runs");
    let rule_order: Vec<LintRule> = a.iter().map(|f| f.rule).collect();
    // The emitted order must follow LintRule::ALL — the ALL index of
    // consecutive findings is non-decreasing.
    let idx = |r: LintRule| LintRule::ALL.iter().position(|x| *x == r).unwrap();
    for w in rule_order.windows(2) {
        assert!(idx(w[0]) <= idx(w[1]));
    }
}

#[test]
fn installed_package_nodes_are_not_flagged() {
    // model.pkg.other is an installed package's model: it must not flag
    // even though it has no parents/docs. PackageScope::All keeps it in
    // the graph; root-project scoping must still exclude it.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            (
                "model.pkg.other".to_string(),
                RawNode {
                    name: "other".into(),
                    resource_type: "model".into(),
                    path: Some("staging/other.sql".into()),
                    package_name: Some("pkg".into()),
                    ..Default::default()
                },
            ),
        ],
        vec![],
        &[],
    );
    let mut m = m;
    m.metadata.project_name = Some("p".into());
    let dag = Dag::build(&m);
    assert!(dag.contains("model.pkg.other"), "package node kept in dag");
    let f = run_rules(LintInput::new(&dag, &LintConfig::default()));
    for rule in LintRule::ALL {
        assert!(
            flagged(&f, rule)
                .iter()
                .all(|u| !u.starts_with("model.pkg.")),
            "{} flagged an installed-package node",
            rule.name()
        );
    }
}

#[test]
fn finding_key_distinguishes_edge_from_node_findings() {
    // Node findings key on (rule, node): changed related sets don't make a
    // pre-existing violation look "new" under --diff.
    let node_finding = Finding::new(
        LintRule::DirectJoinToSource,
        "model.p.x",
        vec!["source.p.s.t".into()],
        "",
    );
    assert_eq!(
        node_finding.key(),
        (LintRule::DirectJoinToSource, "model.p.x", &[][..])
    );
    let edge_finding = Finding::new(
        LintRule::LayerViolation,
        "model.p.c",
        vec!["model.p.p".into()],
        "",
    );
    assert_eq!(
        edge_finding.key(),
        (
            LintRule::LayerViolation,
            "model.p.c",
            &["model.p.p".to_string()][..]
        )
    );
}

#[test]
fn project_config_path_prefers_the_project_root_then_target() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("target").join("manifest.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    // No .dbtl.yml anywhere → None.
    assert_eq!(project_config_path(Some(&manifest), None), None);
    // Standard layout `<root>/target/manifest.json`: the PROJECT ROOT's
    // file wins (it is the conventional config home)…
    let root_cfg = dir.path().join(PROJECT_CONFIG_NAME);
    std::fs::write(&root_cfg, "lint: {}\n").unwrap();
    let target_cfg = dir.path().join("target").join(PROJECT_CONFIG_NAME);
    std::fs::write(&target_cfg, "lint: {}\n").unwrap();
    assert_eq!(
        project_config_path(Some(&manifest), None),
        Some(root_cfg.clone()),
        "project root outranks target/ under the standard layout"
    );
    // …and target/ is only an escape hatch when the root has none.
    std::fs::remove_file(&root_cfg).unwrap();
    assert_eq!(project_config_path(Some(&manifest), None), Some(target_cfg));
    // Source mode looks in the project dir itself (it IS the root).
    let proj = tempfile::tempdir().unwrap();
    let cfg = proj.path().join(PROJECT_CONFIG_NAME);
    std::fs::write(&cfg, "lint: {}\n").unwrap();
    assert_eq!(project_config_path(None, Some(proj.path())), Some(cfg));
}

#[test]
fn project_config_path_never_climbs_past_a_nonstandard_manifest_dir() {
    // `--manifest /tmp/x/manifest.json` must NOT see /tmp/.dbtl.yml —
    // the parent hop exists only for the `<root>/target/` layout.
    let parent = tempfile::tempdir().unwrap();
    let x = parent.path().join("x");
    std::fs::create_dir_all(&x).unwrap();
    let manifest = x.join("manifest.json");
    let foreign = parent.path().join(PROJECT_CONFIG_NAME);
    std::fs::write(&foreign, "lint: {}\n").unwrap();
    assert_eq!(
        project_config_path(Some(&manifest), None),
        None,
        "a parent's .dbtl.yml must not leak into a non-target manifest dir"
    );
    // Its own dir still works.
    let own = x.join(PROJECT_CONFIG_NAME);
    std::fs::write(&own, "lint: {}\n").unwrap();
    assert_eq!(project_config_path(Some(&manifest), None), Some(own));
}

// ---- the registry (drift guards) ------------------------------------------

#[test]
fn registry_rows_match_all_one_for_one_in_order() {
    assert_eq!(REGISTRY.len(), LintRule::ALL.len());
    for (rule, row) in LintRule::ALL.iter().zip(REGISTRY) {
        assert_eq!(*rule, row.rule);
    }
    // ids are unique and equal to the rule's serialization name — the
    // `.dbtl.yml` key, `--rules` argument and baseline `rule` field are the
    // same string by construction.
    let mut ids = std::collections::BTreeSet::new();
    for row in REGISTRY {
        assert!(ids.insert(row.id), "duplicate id {}", row.id);
        assert_eq!(
            serde_json::to_string(&row.rule).unwrap(),
            format!("\"{}\"", row.id),
            "registry id {} is the serde name",
            row.id
        );
        assert_eq!(row.rule.name(), row.id);
        assert_eq!(LintRule::from_name(row.id), Some(row.rule));
    }
}

#[test]
fn registry_rows_carry_the_fixed_metadata() {
    // The 12 ids are a fixed contract — config keys, --rules args and the
    // baseline format all rely on them.
    let ids: Vec<&str> = REGISTRY.iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        vec![
            "direct_join_to_source",
            "multiple_sources_joined",
            "source_fanout",
            "model_fanout",
            "root_models",
            "rejoining_upstream",
            "marts_depend_on_source",
            "missing_primary_key_tests",
            "public_models_without_contract",
            "undocumented_models",
            "naming_conventions",
            "layer_violation",
        ]
    );
    for row in REGISTRY {
        // Every current rule gates at error severity and reads the graph only.
        assert_eq!(row.default_severity, Severity::Error, "{}", row.id);
        assert_eq!(row.requires, Requires::Graph, "{}", row.id);
        assert!(
            !row.summary.is_empty() && !row.summary.contains('\n'),
            "{} summary is one line",
            row.id
        );
        assert!(!row.rationale.trim().is_empty(), "{} rationale", row.id);
        assert!(!row.example.trim().is_empty(), "{} example", row.id);
        assert_eq!(row.rule.description(), row.summary);
    }
}

#[test]
fn registry_params_are_the_declared_config_keys_with_matching_defaults() {
    // The two fanout rules declare `threshold`; nothing else declares a
    // param — and the unconfigured `LintConfig` resolves the declared
    // defaults.
    assert_eq!(LintRule::ModelFanout.info().params.len(), 1);
    assert_eq!(LintRule::SourceFanout.info().params.len(), 1);
    for row in REGISTRY {
        for p in row.params {
            match p.kind {
                ParamKind::Int { min, default } => {
                    assert_eq!(
                        LintConfig::default().int_param(row.rule, p.name),
                        Some(default),
                        "{}.{}",
                        row.id,
                        p.name
                    );
                    assert!(min <= default, "{}.{}", row.id, p.name);
                }
            }
        }
    }
    // A rule with no params resolves nothing.
    assert_eq!(
        LintConfig::default().int_param(LintRule::RootModels, "threshold"),
        None
    );
}

// ---- the finding envelope ---------------------------------------------------

#[test]
fn findings_carry_severity_location_and_baselined() {
    let mut node = model("stg_root", "staging/stg_root.sql");
    node.1.original_file_path = Some("models/staging/stg_root.sql".into());
    let mut bare = model("stg_bare", "staging/stg_bare.sql");
    bare.1.original_file_path = None;
    let m = manifest(vec![node, bare], vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let g = f
        .iter()
        .find(|f| f.node_uid == "model.p.stg_root" && f.rule == LintRule::RootModels)
        .unwrap();
    assert_eq!(g.severity, Severity::Error);
    assert_eq!(
        g.location,
        Some(Location {
            file: "models/staging/stg_root.sql".into(),
            line: None
        }),
        "location is the node's original_file_path"
    );
    assert!(!g.baselined);
    // No original_file_path → no location.
    let g2 = f
        .iter()
        .find(|f| f.node_uid == "model.p.stg_bare" && f.rule == LintRule::RootModels)
        .unwrap();
    assert_eq!(g2.location, None);
    // A configured severity lands on the finding (the envelope is filled in
    // run_rules, not by the rule).
    let cfg = config_from("lint:\n  rules:\n    root_models: {severity: warn}\n");
    let f = findings_of(&m, &cfg);
    let g = f
        .iter()
        .find(|f| f.node_uid == "model.p.stg_root" && f.rule == LintRule::RootModels)
        .unwrap();
    assert_eq!(g.severity, Severity::Warn);
}

#[test]
fn severity_off_disables_the_rule() {
    // `{severity: off}` and `false` both land as "off" — the finding is
    // dropped, not merely re-labelled.
    let m = manifest(vec![model("stg_root", "staging/stg_root.sql")], vec![], &[]);
    for yaml in [
        "lint:\n  rules:\n    root_models: {severity: off}\n",
        "lint:\n  rules:\n    root_models: false\n",
    ] {
        let cfg = config_from(yaml);
        assert!(!cfg.enabled(LintRule::RootModels));
        let f = findings_of(&m, &cfg);
        assert!(flagged(&f, LintRule::RootModels).is_empty());
        assert_eq!(
            flagged(&f, LintRule::UndocumentedModels),
            vec!["model.p.stg_root"]
        );
    }
}

#[test]
fn finding_json_carries_the_envelope() {
    let f = Finding::new(
        LintRule::RootModels,
        "model.p.x",
        vec![],
        "x has no parents",
    );
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["rule"], "root_models");
    assert_eq!(v["severity"], "error");
    assert_eq!(v["baselined"], false);
    assert!(
        v.get("location").is_none(),
        "location is omitted from JSON when absent"
    );
    let mut f2 = f.clone();
    f2.location = Some(Location {
        file: "models/x.sql".into(),
        line: None,
    });
    let v2 = serde_json::to_value(&f2).unwrap();
    assert_eq!(v2["location"]["file"], "models/x.sql");
    assert!(v2["location"].get("line").is_none());
}

// ---- per-rule excludes -------------------------------------------------------

#[test]
fn exclude_unions_glob_path_tag_and_exact_names() {
    // root_models flags every parentless model — five of them here.
    let mut tagged = model("stg_tagged", "staging/stg_tagged.sql");
    tagged.1.tags = vec!["wip".to_string()];
    let m = manifest(
        vec![
            model("stg_legacy_a", "staging/stg_legacy_a.sql"),
            tagged,
            model("stg_exact", "staging/stg_exact.sql"),
            model("plain_kept", "marts/plain_kept.sql"),
            model("fct_kept", "marts/fct_kept.sql"),
        ],
        vec![],
        &[],
    );
    let cfg = config_from(
        "lint:\n  rules:\n    root_models:\n      exclude: [\"stg_legacy_*\", \"stg_exact\", \"path:marts/plain\", \"tag:wip\"]\n",
    );
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::RootModels),
        vec!["model.p.fct_kept"],
        "the exclude spec is a union: glob name, exact name, path:, tag:"
    );
    // Exclusion is PER-RULE — the excluded models still flag elsewhere.
    assert!(
        flagged(&f, LintRule::UndocumentedModels).contains(&"model.p.stg_legacy_a"),
        "excluding root_models doesn't blind other rules"
    );
}

#[test]
fn exclude_on_an_edge_rule_matches_the_child() {
    // layer_violation's node_uid is the CHILD — excluding the child drops
    // the finding; excluding the parent does not.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("fct_bad", "marts/fct_bad.sql"),
        ],
        vec![],
        &[("model.p.fct_bad", "model.p.stg_a")],
    );
    assert_eq!(
        flagged(
            &findings_of(&m, &LintConfig::default()),
            LintRule::LayerViolation
        ),
        vec!["model.p.stg_a"],
        "sanity: the violation fires without excludes"
    );
    let cfg = config_from("lint:\n  rules:\n    layer_violation: {exclude: [\"stg_a\"]}\n");
    assert!(flagged(&findings_of(&m, &cfg), LintRule::LayerViolation).is_empty());
    let cfg = config_from("lint:\n  rules:\n    layer_violation: {exclude: [\"fct_bad\"]}\n");
    assert_eq!(
        flagged(&findings_of(&m, &cfg), LintRule::LayerViolation),
        vec!["model.p.stg_a"],
        "the parent isn't the finding's judged side"
    );
}

#[test]
fn exclude_drops_whole_findings_without_reordering() {
    // Two violations on the same rule; excluding one keeps the other in
    // place — findings are removed whole, never re-sorted.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("stg_b", "staging/stg_b.sql"),
            model("stg_c", "staging/stg_c.sql"),
        ],
        vec![],
        &[],
    );
    let cfg = config_from("lint:\n  rules:\n    root_models: {exclude: [\"stg_b\"]}\n");
    let f = findings_of(&m, &cfg);
    let positions: Vec<usize> = f
        .iter()
        .enumerate()
        .filter(|(_, f)| f.rule == LintRule::RootModels)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        flagged(&f, LintRule::RootModels),
        vec!["model.p.stg_a", "model.p.stg_c"]
    );
    // And the survivors appear in uid order — the same positions they held
    // under the default config (minus the dropped slot).
    let all = findings_of(&m, &LintConfig::default());
    let all_pos: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, f)| f.rule == LintRule::RootModels && f.node_uid != "model.p.stg_b")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(positions.len(), all_pos.len());
    let survivors: Vec<&str> = f
        .iter()
        .filter(|f| f.rule == LintRule::RootModels)
        .map(|f| f.node_uid.as_str())
        .collect();
    assert_eq!(survivors, vec!["model.p.stg_a", "model.p.stg_c"]);
}

// ---- meta.dbtl.ignore ---------------------------------------------------------

#[test]
fn meta_dbtl_ignore_suppresses_listed_rules_and_all() {
    let mk = |name: &str, meta: serde_json::Value| {
        (
            format!("model.p.{name}"),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(format!("staging/{name}.sql")),
                meta: meta.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            },
        )
    };
    let m = manifest(
        vec![
            mk(
                "stg_a",
                serde_json::json!({"dbtl": {"ignore": ["root_models"]}}),
            ),
            mk("stg_b", serde_json::json!({"dbtl": {"ignore": "all"}})),
            mk(
                "stg_c",
                serde_json::json!({"dbtl": {"ignore": ["not_a_rule", 42]}}),
            ),
            mk("stg_d", serde_json::json!({"dbtl": "not-an-object"})),
        ],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    let roots = flagged(&f, LintRule::RootModels);
    let undoc = flagged(&f, LintRule::UndocumentedModels);
    // Rule-scoped: root_models is suppressed on stg_a; other rules still see it.
    assert!(!roots.contains(&"model.p.stg_a"));
    assert!(undoc.contains(&"model.p.stg_a"));
    // "all": every rule on stg_b is suppressed.
    assert!(!roots.contains(&"model.p.stg_b"));
    assert!(!undoc.contains(&"model.p.stg_b"));
    // Unknown ids / non-strings suppress nothing (a stale id isn't an error).
    assert!(roots.contains(&"model.p.stg_c"));
    // A malformed dbtl value suppresses nothing — suppression is opt-in.
    assert!(roots.contains(&"model.p.stg_d"));
    // "all" is the STRING form — inside the list it's just an unknown id.
    let m = manifest(
        vec![mk(
            "stg_e",
            serde_json::json!({"dbtl": {"ignore": ["all"]}}),
        )],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert!(
        flagged(&f, LintRule::RootModels).contains(&"model.p.stg_e"),
        "[\"all\"] is a list of ids; 'all' isn't one"
    );
}

#[test]
fn meta_suppression_matches_the_edge_finding_child() {
    // layer_violation keys on the child — ignore metadata on the CHILD
    // suppresses the edge finding; the parent's doesn't.
    let mut stg = model("stg_a", "staging/stg_a.sql");
    stg.1.meta = serde_json::json!({"dbtl": {"ignore": ["layer_violation"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let m = manifest(
        vec![stg, model("fct_bad", "marts/fct_bad.sql")],
        vec![],
        &[("model.p.fct_bad", "model.p.stg_a")],
    );
    assert!(
        flagged(
            &findings_of(&m, &LintConfig::default()),
            LintRule::LayerViolation
        )
        .is_empty(),
        "child meta.dbtl.ignore suppresses the edge finding"
    );
    let mut fct = model("fct_bad", "marts/fct_bad.sql");
    fct.1.meta = serde_json::json!({"dbtl": {"ignore": ["layer_violation"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let m = manifest(
        vec![model("stg_a", "staging/stg_a.sql"), fct],
        vec![],
        &[("model.p.fct_bad", "model.p.stg_a")],
    );
    assert_eq!(
        flagged(
            &findings_of(&m, &LintConfig::default()),
            LintRule::LayerViolation
        ),
        vec!["model.p.stg_a"],
        "parent-side ignore doesn't suppress the child's violation"
    );
}

#[test]
fn dbtl_meta_stringifies_identically_in_manifest_and_source_modes() {
    // Both loaders land `meta.dbtl` on NodeDetail as the same compact-JSON
    // string — suppression only reads the string, so the two modes agree.
    let mut node = model("stg_x", "staging/stg_x.sql");
    node.1.meta = serde_json::json!({"dbtl": {"ignore": ["root_models"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let dag = Dag::build(&manifest(vec![node], vec![], &[]));
    let manifest_side = dag.detail("model.p.stg_x").unwrap().meta["dbtl"].clone();
    assert_eq!(manifest_side, r#"{"ignore":["root_models"]}"#);

    // Source mode: `+meta` folder inheritance from dbt_project.yml lands
    // the same string…
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nmodels:\n  p:\n    +meta:\n      dbtl:\n        ignore: [root_models]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("models/stg_y.sql"), "select 1").unwrap();
    // …and so does an in-file `config(meta=...)` (it wins over `+meta` for
    // the `dbtl` key on that model).
    std::fs::write(
        dir.path().join("models/stg_z.sql"),
        "{{ config(meta={'dbtl': {'ignore': ['undocumented_models']}}) }}\nselect 1\n",
    )
    .unwrap();
    let sm = crate::manifest_from_source(dir.path()).unwrap();
    let sdag = Dag::build(&sm);
    assert_eq!(
        sdag.detail("model.p.stg_y").unwrap().meta["dbtl"],
        r#"{"ignore":["root_models"]}"#
    );
    assert_eq!(
        sdag.detail("model.p.stg_z").unwrap().meta["dbtl"],
        r#"{"ignore":["undocumented_models"]}"#
    );
    // …and suppression actually fires under source mode, on both.
    let f = run_rules(LintInput::new(&sdag, &LintConfig::default()));
    assert!(
        !flagged(&f, LintRule::RootModels).contains(&"model.p.stg_y"),
        "+meta dbtl.ignore suppresses root_models in source mode"
    );
    assert!(
        !flagged(&f, LintRule::UndocumentedModels).contains(&"model.p.stg_z"),
        "config(meta=) dbtl.ignore suppresses undocumented_models in source mode"
    );
}

// ---- the baseline -------------------------------------------------------------

#[test]
fn baseline_round_trips_and_flags_exact_keys() {
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("fct_bad", "marts/fct_bad.sql"),
        ],
        vec![],
        &[("model.p.fct_bad", "model.p.stg_a")],
    );
    let mut findings = findings_of(&m, &LintConfig::default());
    let baseline = Baseline::from_findings(&findings);
    let text = baseline.to_json();
    // Schema-stamped, newline-terminated, byte-stable.
    assert!(text.starts_with("{\n  \"schema\": \"dbtl.lint-baseline/v1\""));
    assert!(text.ends_with("}\n"));
    assert_eq!(Baseline::from_findings(&findings).to_json(), text);
    // Input order doesn't matter: reversed findings produce the same file.
    let mut rev = findings.clone();
    rev.reverse();
    assert_eq!(Baseline::from_findings(&rev).to_json(), text);
    // Round trip: every finding baselines.
    let back = Baseline::from_json(&text).unwrap();
    back.apply(&mut findings);
    assert!(findings.iter().all(|f| f.baselined));
    // A stale entry (a rule that no longer fires) changes nothing.
    let b2 = Baseline::from_findings(&[Finding::new(
        LintRule::LayerViolation,
        "model.p.stg_a",
        vec!["model.p.gone".into()],
        "",
    )]);
    let mut one = findings.clone();
    for f in &mut one {
        f.baselined = false;
    }
    b2.apply(&mut one);
    assert!(
        one.iter().all(|f| !f.baselined),
        "a non-matching entry baselines nothing"
    );
}

#[test]
fn baseline_requires_the_edge_endpoints_to_match() {
    // Edge findings key on (rule, child, related): same node, different
    // related set → NOT baselined.
    let edge = Finding::new(
        LintRule::RejoiningUpstream,
        "model.p.b",
        vec!["model.p.a".into(), "model.p.c".into()],
        "",
    );
    let baseline = Baseline::from_findings(std::slice::from_ref(&edge));
    let mut got = vec![
        Finding::new(
            LintRule::RejoiningUpstream,
            "model.p.b",
            vec!["model.p.x".into()],
            "",
        ),
        edge.clone(),
    ];
    baseline.apply(&mut got);
    assert!(!got[0].baselined, "a different edge is a different finding");
    assert!(got[1].baselined);
    // Node findings key on (rule, node) only — related changes still match.
    let node_f = Finding::new(
        LintRule::DirectJoinToSource,
        "model.p.x",
        vec!["source.p.s.t".into()],
        "",
    );
    let nb = Baseline::from_findings(&[node_f]);
    let mut nf = vec![Finding::new(
        LintRule::DirectJoinToSource,
        "model.p.x",
        vec![],
        "",
    )];
    nb.apply(&mut nf);
    assert!(nf[0].baselined);
    // from_findings dedupes.
    let f = Finding::new(LintRule::RootModels, "model.p.x", vec![], "");
    assert_eq!(Baseline::from_findings(&[f.clone(), f]).len(), 1);
}

#[test]
fn baseline_parse_normalizes_related() {
    // A hand-written baseline may carry `related` unsorted or duplicated —
    // it still has to match the edge finding's canonical key, and serialize
    // back in canonical order.
    let text = r#"{
  "schema": "dbtl.lint-baseline/v1",
  "findings": [
    {"rule": "rejoining_upstream", "node": "model.p.b",
     "related": ["model.p.c", "model.p.a", "model.p.c"]}
  ]
}"#;
    let baseline = Baseline::from_json(text).unwrap();
    assert_eq!(baseline.len(), 1, "the duplicate collapses");
    let canonical = Baseline::from_findings(&[Finding::new(
        LintRule::RejoiningUpstream,
        "model.p.b",
        vec!["model.p.a".into(), "model.p.c".into()],
        "",
    )]);
    assert_eq!(baseline.to_json(), canonical.to_json());
    let mut got = vec![Finding::new(
        LintRule::RejoiningUpstream,
        "model.p.b",
        vec!["model.p.a".into(), "model.p.c".into()],
        "",
    )];
    baseline.apply(&mut got);
    assert!(
        got[0].baselined,
        "an unsorted baseline entry still matches the edge"
    );
}

#[test]
fn baseline_rejects_bad_schema_and_malformed_json() {
    assert!(Baseline::from_json("not json").is_err());
    assert!(Baseline::from_json("{}").is_err(), "schema is required");
    let err = Baseline::from_json(r#"{"schema":"other/v9","findings":[]}"#).unwrap_err();
    assert!(
        err.contains("other/v9") && err.contains(BASELINE_SCHEMA),
        "{err} names both the found and expected schema"
    );
}
