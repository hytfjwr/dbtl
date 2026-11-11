use super::*;
use crate::manifest::{RawContract, RawManifest, RawNode, RawSource, RawTestMetadata};
use crate::project_config::{project_config_path, ProjectConfig, PROJECT_CONFIG_NAME};
use crate::{Catalog, SqlDialect, SqlParseResolver};
use std::collections::HashMap;
use std::rc::Rc;

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
    run_rules(LintInput::new(&Dag::build(m), config)).findings
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
    let default = run_rules(LintInput::new(&base, &LintConfig::default())).findings;
    assert!(flagged(&default, LintRule::ModelFanout).is_empty());
    let cfg = config_from("lint:\n  model_fanout_threshold: 2\n");
    let tightened = run_rules(LintInput::new(&base, &cfg)).findings;
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
    let a = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let b = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
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
    // even though it has no parents/docs. It is also an incremental model
    // without a unique_key, and source.pkg.s.t a source without freshness —
    // both would trip the new warn rules if root-project scoping broke.
    // PackageScope::All keeps them in the graph; scoping must still exclude
    // them.
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
                    config: crate::manifest::RawConfig {
                        materialized: Some("incremental".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
        ],
        vec![(
            "source.pkg.s.t".to_string(),
            RawSource {
                name: "t".into(),
                resource_type: "source".into(),
                package_name: Some("pkg".into()),
                ..Default::default()
            },
        )],
        &[],
    );
    let mut m = m;
    m.metadata.project_name = Some("p".into());
    let dag = Dag::build(&m);
    assert!(dag.contains("model.pkg.other"), "package node kept in dag");
    assert!(dag.contains("source.pkg.s.t"), "package source kept in dag");
    let f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    for rule in LintRule::ALL {
        assert!(
            flagged(&f, rule)
                .iter()
                .all(|u| !u.starts_with("model.pkg.") && !u.starts_with("source.pkg.")),
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
    // The 32 ids are a fixed contract — config keys, --rules args and the
    // baseline format all rely on them. The original twelve lead; rules
    // added later append in the registry's order so the emitted sequence
    // stays stable.
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
            "unused_sources",
            "duplicate_sources",
            "staging_depends_on_staging",
            "too_many_joins",
            "chained_views",
            "exposure_parents_materializations",
            "exposures_dependent_on_private_models",
            "undocumented_sources",
            "undocumented_columns",
            "undocumented_public_models",
            "model_directories",
            "required_meta_keys",
            "contract_columns_without_type",
            "incremental_without_unique_key",
            "sources_without_freshness",
            "select_star",
            "hard_coded_references",
            "cartesian_join",
            "unknown_column_reference",
            "forbidden_patterns",
            "warehouse_type_drift",
            "columns_missing_in_warehouse",
        ]
    );
    // The original twelve gate at error severity (that was the only
    // behaviour); rules added later default to warn so they never silently
    // gate a project that didn't opt in.
    let error_by_default = [
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
    ];
    // `requires` is a fixed contract too — only the SQL-scanning and
    // catalog-reading rules ask for more than the graph.
    let sql_rules = [
        "select_star",
        "hard_coded_references",
        "cartesian_join",
        "unknown_column_reference",
        "forbidden_patterns",
    ];
    let catalog_rules = ["warehouse_type_drift", "columns_missing_in_warehouse"];
    for row in REGISTRY {
        let expected = if error_by_default.contains(&row.id) {
            Severity::Error
        } else {
            Severity::Warn
        };
        assert_eq!(row.default_severity, expected, "{}", row.id);
        let expected_req = if sql_rules.contains(&row.id) {
            Requires::Sql
        } else if catalog_rules.contains(&row.id) {
            Requires::Catalog
        } else {
            Requires::Graph
        };
        assert_eq!(row.requires, expected_req, "{}", row.id);
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
    // Every declared param resolves its registry default under the
    // unconfigured `LintConfig` — integers and string lists alike.
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
                ParamKind::StringList { default, allowed } => {
                    assert_eq!(
                        LintConfig::default().str_list_param(row.rule, p.name),
                        Some(default.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
                        "{}.{}",
                        row.id,
                        p.name
                    );
                    if let Some(allowed) = allowed {
                        assert!(
                            default.iter().all(|d| allowed.contains(d)),
                            "{}.{}'s default sits outside its allowed set",
                            row.id,
                            p.name
                        );
                    }
                }
                ParamKind::RegexList { default } => {
                    assert_eq!(
                        LintConfig::default().str_list_param(row.rule, p.name),
                        Some(default.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
                        "{}.{}",
                        row.id,
                        p.name
                    );
                    for d in default {
                        assert!(
                            regex::Regex::new(d).is_ok(),
                            "{}.{}'s default '{d}' must compile",
                            row.id,
                            p.name
                        );
                    }
                }
            }
        }
    }
    // A rule with no params resolves nothing.
    assert_eq!(
        LintConfig::default().int_param(LintRule::RootModels, "threshold"),
        None
    );
    assert_eq!(
        LintConfig::default().str_list_param(LintRule::RootModels, "keys"),
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
    let f = run_rules(LintInput::new(&sdag, &LintConfig::default())).findings;
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

// ---- the graph/metadata rules ---------------------------------------------
//
// One positive + one negative case per rule minimum. Helpers shared by
// these tests:

/// A source literal with warehouse coordinates — `identifier` is the
/// `identifier:` override that surfaces as `relation_name` on the detail.
fn source_at(
    name: &str,
    database: Option<&str>,
    schema: Option<&str>,
    identifier: Option<&str>,
) -> (String, RawSource) {
    (
        format!("source.p.{name}"),
        RawSource {
            name: name.into(),
            resource_type: "source".into(),
            database: database.map(str::to_string),
            schema: schema.map(str::to_string),
            identifier: identifier.map(str::to_string),
            ..Default::default()
        },
    )
}

/// A model literal carrying an explicit `config.materialized` (chained_views
/// / exposure_parents_materializations read it).
fn mat_model(name: &str, materialized: &str) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("staging/{name}.sql")),
            config: crate::manifest::RawConfig {
                materialized: Some(materialized.into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

/// A generic-test node attached to `target_uid` — `test_on`'s model-uid
/// sibling, for seeds/snapshots.
fn test_attached(target_uid: &str, kind: &str, column: &str) -> (String, RawNode) {
    (
        format!("test.p.{kind}_{column}_on_{}", target_uid.replace('.', "_")),
        RawNode {
            name: format!("{kind}_{column}"),
            resource_type: "test".into(),
            attached_node: Some(target_uid.into()),
            column_name: Some(column.into()),
            test_metadata: Some(RawTestMetadata {
                name: Some(kind.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
}

/// Declared-column JSON — `[(name, description, data_type)]` triples.
fn columns(
    specs: &[(&str, Option<&str>, Option<&str>)],
) -> serde_json::Map<String, serde_json::Value> {
    specs
        .iter()
        .map(|(name, desc, dtype)| {
            let mut col = serde_json::Map::new();
            if let Some(d) = desc {
                col.insert("description".into(), serde_json::json!(d));
            }
            if let Some(t) = dtype {
                col.insert("data_type".into(), serde_json::json!(t));
            }
            (name.to_string(), serde_json::Value::Object(col))
        })
        .collect()
}

/// An `exposures` entry — extend `RawManifest::exposures` with it and wire
/// its parents through `edges` as usual.
fn exposure(name: &str) -> (String, crate::manifest::RawExposure) {
    (
        format!("exposure.p.{name}"),
        crate::manifest::RawExposure {
            name: name.into(),
            ..Default::default()
        },
    )
}

// ---- unused_sources --------------------------------------------------------

#[test]
fn unused_sources_flags_sources_nothing_reads() {
    // s.used feeds a model; s.exposed feeds an exposure — an exposure child
    // counts as a use; only s.dead has no children at all.
    let mut m = manifest(
        vec![model("stg_a", "staging/stg_a.sql")],
        vec![source("s.used"), source("s.exposed"), source("s.dead")],
        &[
            ("source.p.s.used", "model.p.stg_a"),
            ("source.p.s.exposed", "exposure.p.exp"),
        ],
    );
    m.exposures.extend([exposure("exp")]);
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::UnusedSources),
        vec!["source.p.s.dead"],
        "exposure children count as a use — only the childless source flags"
    );
}

// ---- duplicate_sources -------------------------------------------------------

#[test]
fn duplicate_sources_compares_relations_case_insensitively() {
    // a and b resolve to the same relation modulo case; c stands alone.
    let m = manifest(
        vec![],
        vec![
            source_at("a.users", Some("DB"), Some("app"), Some("users")),
            source_at("b.users", Some("db"), Some("APP"), Some("USERS")),
            source_at("c.orders", Some("db"), Some("app"), Some("orders")),
        ],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::DuplicateSources)
        .collect();
    assert_eq!(
        flagged(&f, LintRule::DuplicateSources),
        vec!["source.p.a.users", "source.p.b.users"],
        "one finding per duplicated source"
    );
    assert_eq!(hits.len(), 2);
    // related names the OTHER sources at the same relation.
    assert_eq!(hits[0].related, vec!["source.p.b.users".to_string()]);
    assert_eq!(hits[1].related, vec!["source.p.a.users".to_string()]);
}

#[test]
fn duplicate_sources_prefers_the_identifier_over_the_source_name() {
    // `x` declares identifier "users" — its relation collides with the
    // source NAMED users. `alias` declares "other_users" — the identifier
    // wins over the name, so it doesn't collide with either.
    let m = manifest(
        vec![],
        vec![
            source_at("x", Some("db"), Some("app"), Some("users")),
            source_at("users", Some("db"), Some("app"), None),
            source_at("alias", Some("db"), Some("app"), Some("other_users")),
        ],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::DuplicateSources),
        vec!["source.p.users", "source.p.x"],
        "the identifier, not the declared name, is the relation"
    );
}

#[test]
fn duplicate_sources_missing_schema_never_flags_but_missing_database_compares_without_it() {
    let m = manifest(
        vec![],
        vec![
            // schema missing on one side → "unknown", never "same relation".
            source_at("no_schema", Some("db"), None, Some("users")),
            source_at("has_schema", Some("db"), Some("app"), Some("users")),
            // database missing on one side → compared on schema.identifier.
            source_at("no_db", None, Some("app"), Some("orders")),
            source_at("has_db", Some("db"), Some("app"), Some("orders")),
        ],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::DuplicateSources),
        vec!["source.p.has_db", "source.p.no_db"],
        "a schema-less pair never flags; a database-less pair still compares"
    );
}

#[test]
fn duplicate_sources_three_or_more_each_list_the_others() {
    let m = manifest(
        vec![],
        vec![
            source_at("a", Some("db"), Some("app"), Some("users")),
            source_at("b", Some("db"), Some("app"), Some("users")),
            source_at("c", Some("db"), Some("app"), Some("users")),
        ],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::DuplicateSources),
        vec!["source.p.a", "source.p.b", "source.p.c"]
    );
    for f in f.iter().filter(|f| f.rule == LintRule::DuplicateSources) {
        let others: Vec<String> = ["source.p.a", "source.p.b", "source.p.c"]
            .iter()
            .filter(|u| **u != f.node_uid)
            .map(|u| u.to_string())
            .collect();
        assert_eq!(f.related, others, "{} lists the other two", f.node_uid);
    }
}

// ---- staging_depends_on_staging ----------------------------------------------

#[test]
fn staging_depends_on_staging_flags_staging_parents() {
    // stg_b reads stg_a (staging→staging). int_c also reads stg_a but isn't
    // staging itself; stg_c reads int_c — its parent isn't staging.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("stg_b", "staging/stg_b.sql"),
            model("int_c", "intermediate/int_c.sql"),
            model("stg_c", "staging/stg_c.sql"),
        ],
        vec![],
        &[
            ("model.p.stg_a", "model.p.stg_b"),
            ("model.p.stg_a", "model.p.int_c"),
            ("model.p.int_c", "model.p.stg_c"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::StagingDependsOnStaging)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.p.stg_b");
    assert_eq!(hits[0].related, vec!["model.p.stg_a".to_string()]);
}

// ---- too_many_joins ------------------------------------------------------------

#[test]
fn too_many_joins_flags_at_the_configured_threshold() {
    // fct_hub reads 7 data-resource parents (6 models + 1 source); fct_light
    // reads 2. The default threshold is 7.
    let m = manifest(
        vec![
            model("fct_hub", "marts/fct_hub.sql"),
            model("fct_light", "marts/fct_light.sql"),
            model("int_a", "intermediate/int_a.sql"),
            model("int_b", "intermediate/int_b.sql"),
            model("int_c", "intermediate/int_c.sql"),
            model("int_d", "intermediate/int_d.sql"),
            model("int_e", "intermediate/int_e.sql"),
            model("int_f", "intermediate/int_f.sql"),
        ],
        vec![source("s.raw")],
        &[
            ("model.p.int_a", "model.p.fct_hub"),
            ("model.p.int_b", "model.p.fct_hub"),
            ("model.p.int_c", "model.p.fct_hub"),
            ("model.p.int_d", "model.p.fct_hub"),
            ("model.p.int_e", "model.p.fct_hub"),
            ("model.p.int_f", "model.p.fct_hub"),
            ("source.p.s.raw", "model.p.fct_hub"),
            ("model.p.int_a", "model.p.fct_light"),
            ("model.p.int_b", "model.p.fct_light"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::TooManyJoins),
        vec!["model.p.fct_hub"],
        "7 data-resource parents hit the default threshold"
    );
    // A higher threshold un-flags the same graph.
    let cfg = config_from("lint:\n  rules:\n    too_many_joins: {threshold: 8}\n");
    assert!(flagged(&findings_of(&m, &cfg), LintRule::TooManyJoins).is_empty());
}

// ---- chained_views -------------------------------------------------------------

#[test]
fn chained_views_flags_at_exactly_the_threshold_and_only_the_end() {
    // v1→v2→v3→v4→v5 is a 5-chain — the default threshold. w1→…→w4 is one
    // short. Only the chain END flags — v1..v4 run on into v5.
    let m = manifest(
        vec![
            mat_model("v1", "view"),
            mat_model("v2", "view"),
            mat_model("v3", "view"),
            mat_model("v4", "view"),
            mat_model("v5", "view"),
            mat_model("w1", "view"),
            mat_model("w2", "view"),
            mat_model("w3", "view"),
            mat_model("w4", "view"),
        ],
        vec![],
        &[
            ("model.p.v1", "model.p.v2"),
            ("model.p.v2", "model.p.v3"),
            ("model.p.v3", "model.p.v4"),
            ("model.p.v4", "model.p.v5"),
            ("model.p.w1", "model.p.w2"),
            ("model.p.w2", "model.p.w3"),
            ("model.p.w3", "model.p.w4"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::ChainedViews)
        .collect();
    assert_eq!(hits.len(), 1, "one finding per chain — at the end only");
    assert_eq!(hits[0].node_uid, "model.p.v5");
    // related is the whole chain upstream→downstream (the flagged node last).
    assert_eq!(
        hits[0].related,
        vec![
            "model.p.v1".to_string(),
            "model.p.v2".to_string(),
            "model.p.v3".to_string(),
            "model.p.v4".to_string(),
            "model.p.v5".to_string(),
        ]
    );
    // A lower threshold flags the shorter chain's end too — middles never.
    let cfg = config_from("lint:\n  rules:\n    chained_views: {threshold: 4}\n");
    assert_eq!(
        flagged(&findings_of(&m, &cfg), LintRule::ChainedViews),
        vec!["model.p.v5", "model.p.w4"]
    );
}

#[test]
fn chained_views_flags_each_end_of_a_branching_chain() {
    // v1→v2→v3→v4 forks into v5a and v5b — two ends of a 5-chain.
    let m = manifest(
        vec![
            mat_model("v1", "view"),
            mat_model("v2", "view"),
            mat_model("v3", "view"),
            mat_model("v4", "view"),
            mat_model("v5a", "view"),
            mat_model("v5b", "view"),
        ],
        vec![],
        &[
            ("model.p.v1", "model.p.v2"),
            ("model.p.v2", "model.p.v3"),
            ("model.p.v3", "model.p.v4"),
            ("model.p.v4", "model.p.v5a"),
            ("model.p.v4", "model.p.v5b"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::ChainedViews),
        vec!["model.p.v5a", "model.p.v5b"],
        "a branching chain reports one finding per end"
    );
}

#[test]
fn chained_views_breaks_on_durable_materializations_and_counts_ephemeral() {
    // A table breaks a chain (v3..v4 = 2); an ephemeral counts like a view
    // (the e-chain reaches 5); a model with NO materialized doesn't count.
    let m = manifest(
        vec![
            mat_model("v1", "view"),
            mat_model("v2", "view"),
            mat_model("t_mid", "table"),
            mat_model("v3", "view"),
            mat_model("v4", "view"),
            mat_model("e1", "view"),
            mat_model("e2", "ephemeral"),
            mat_model("e3", "view"),
            mat_model("e4", "view"),
            mat_model("e5", "view"),
            // materialized: None — an unknown storage shape, not a view.
            model("plain", "staging/plain.sql"),
            mat_model("v6", "view"),
            mat_model("v7", "view"),
        ],
        vec![],
        &[
            ("model.p.v1", "model.p.v2"),
            ("model.p.v2", "model.p.t_mid"),
            ("model.p.t_mid", "model.p.v3"),
            ("model.p.v3", "model.p.v4"),
            ("model.p.e1", "model.p.e2"),
            ("model.p.e2", "model.p.e3"),
            ("model.p.e3", "model.p.e4"),
            ("model.p.e4", "model.p.e5"),
            ("model.p.plain", "model.p.v6"),
            ("model.p.v6", "model.p.v7"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::ChainedViews),
        vec!["model.p.e5"],
        "the table breaks its chain; ephemeral counts; no-materialization doesn't"
    );
}

#[test]
fn chained_views_reports_the_longest_chain_then_the_smallest_parent() {
    // v5 has two view parents: y (a 5-chain) and x (a 2-chain) — the related
    // chain walks the LONGER side. v6 has two equal 2-chain parents — the
    // tie resolves to the smaller unique_id.
    let m = manifest(
        vec![
            mat_model("q1", "view"),
            mat_model("q2", "view"),
            mat_model("q3", "view"),
            mat_model("q4", "view"),
            mat_model("y", "view"),
            mat_model("p1", "view"),
            mat_model("x", "view"),
            mat_model("v5", "view"),
            mat_model("m0", "view"),
            mat_model("m1", "view"),
            mat_model("n0", "view"),
            mat_model("n1", "view"),
            mat_model("v6", "view"),
        ],
        vec![],
        &[
            ("model.p.q1", "model.p.q2"),
            ("model.p.q2", "model.p.q3"),
            ("model.p.q3", "model.p.q4"),
            ("model.p.q4", "model.p.y"),
            ("model.p.y", "model.p.v5"),
            ("model.p.p1", "model.p.x"),
            ("model.p.x", "model.p.v5"),
            ("model.p.m0", "model.p.m1"),
            ("model.p.m1", "model.p.v6"),
            ("model.p.n0", "model.p.n1"),
            ("model.p.n1", "model.p.v6"),
        ],
    );
    // threshold 6: only v5 qualifies (q1→…→y→v5 = 6; v6 = 3).
    let cfg = config_from("lint:\n  rules:\n    chained_views: {threshold: 6}\n");
    let f = findings_of(&m, &cfg);
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::ChainedViews)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.p.v5");
    assert_eq!(
        hits[0].related,
        vec![
            "model.p.q1".to_string(),
            "model.p.q2".to_string(),
            "model.p.q3".to_string(),
            "model.p.q4".to_string(),
            "model.p.y".to_string(),
            "model.p.v5".to_string(),
        ],
        "the longest chain upstream→downstream"
    );
    // threshold 3: v6's equal-length parents tie-break on the uid.
    let cfg = config_from("lint:\n  rules:\n    chained_views: {threshold: 3}\n");
    let f = findings_of(&m, &cfg);
    let v6 = f
        .iter()
        .find(|f| f.rule == LintRule::ChainedViews && f.node_uid == "model.p.v6")
        .unwrap();
    assert_eq!(
        v6.related,
        vec![
            "model.p.m0".to_string(),
            "model.p.m1".to_string(),
            "model.p.v6".to_string(),
        ],
        "equal-length parents resolve to the smaller unique_id"
    );
}

// ---- exposure_parents_materializations ----------------------------------------

#[test]
fn exposure_parents_materializations_flags_source_and_view_parents() {
    // bad reads a view and a source (plus a durable table — that one
    // doesn't count); ok reads only the table.
    let mut m = manifest(
        vec![mat_model("v_in", "view"), mat_model("t_ok", "table")],
        vec![source("s.raw")],
        &[
            ("model.p.v_in", "exposure.p.bad"),
            ("source.p.s.raw", "exposure.p.bad"),
            ("model.p.t_ok", "exposure.p.bad"),
            ("model.p.t_ok", "exposure.p.ok"),
        ],
    );
    m.exposures.extend([exposure("bad"), exposure("ok")]);
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::ExposureParentsMaterializations)
        .collect();
    assert_eq!(hits.len(), 1, "one finding per exposure");
    assert_eq!(hits[0].node_uid, "exposure.p.bad");
    assert_eq!(
        hits[0].related,
        vec!["model.p.v_in".to_string(), "source.p.s.raw".to_string()],
        "the offending parents, sorted"
    );
}

// ---- exposures_dependent_on_private_models ------------------------------------

#[test]
fn exposures_dependent_on_private_models_flags_non_public_parents() {
    // private + protected parents flag; public doesn't; a model with NO
    // access value is "unknown" (source mode doesn't read it) — never
    // judged.
    let mut priv_m = model("dim_priv", "marts/dim_priv.sql");
    priv_m.1.access = Some("private".into());
    let mut prot_m = model("dim_prot", "marts/dim_prot.sql");
    prot_m.1.access = Some("protected".into());
    let mut pub_m = model("dim_pub", "marts/dim_pub.sql");
    pub_m.1.access = Some("public".into());
    let mut m = manifest(
        vec![
            priv_m,
            prot_m,
            pub_m,
            model("dim_none", "marts/dim_none.sql"),
        ],
        vec![],
        &[
            ("model.p.dim_priv", "exposure.p.exp"),
            ("model.p.dim_prot", "exposure.p.exp"),
            ("model.p.dim_pub", "exposure.p.exp"),
            ("model.p.dim_none", "exposure.p.exp"),
        ],
    );
    m.exposures.extend([exposure("exp")]);
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::ExposuresDependentOnPrivateModels)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "exposure.p.exp");
    assert_eq!(
        hits[0].related,
        vec![
            "model.p.dim_priv".to_string(),
            "model.p.dim_prot".to_string()
        ],
        "private+protected flag; public and access-less don't"
    );
}

// ---- undocumented_sources -------------------------------------------------------

#[test]
fn undocumented_sources_flags_missing_descriptions() {
    let mut doc = source("s.doc");
    doc.1.description = Some("documented".into());
    let mut blank = source("s.blank");
    blank.1.description = Some("  \n".into()); // cleaned to None at build
    let m = manifest(vec![], vec![doc, source("s.bare"), blank], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::UndocumentedSources);
    got.sort_unstable();
    assert_eq!(got, vec!["source.p.s.bare", "source.p.s.blank"]);
}

// ---- undocumented_columns --------------------------------------------------------

#[test]
fn undocumented_columns_names_the_bare_columns() {
    // fct_x declares a documented column plus a bare one → one finding
    // naming the bare column. fct_clean is fully documented; fct_none
    // declares NO columns — not "missing column descriptions".
    let mut x = model("fct_x", "marts/fct_x.sql");
    x.1.columns = columns(&[("id", Some("the key"), None), ("amt", None, None)]);
    let mut clean = model("fct_clean", "marts/fct_clean.sql");
    clean.1.columns = columns(&[("id", Some("the key"), None)]);
    let m = manifest(
        vec![x, clean, model("fct_none", "marts/fct_none.sql")],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::UndocumentedColumns)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.p.fct_x");
    assert!(
        hits[0].message.contains("amt"),
        "the bare column is named: {}",
        hits[0].message
    );
}

// ---- undocumented_public_models ---------------------------------------------------

#[test]
fn undocumented_public_models_requires_the_model_and_its_columns_documented() {
    // rpt_no_desc: public, no model description, columns documented → flag.
    // rpt_bare_col: public, described, one column bare → flag.
    // rpt_clean: public, described, no declared columns → clean.
    // rpt_full: public, described, documented column → clean.
    // rpt_priv: undocumented but NOT public → out of scope.
    let mut no_desc = model("rpt_no_desc", "marts/rpt_no_desc.sql");
    no_desc.1.access = Some("public".into());
    no_desc.1.columns = columns(&[("id", Some("the key"), None)]);
    let mut bare_col = model("rpt_bare_col", "marts/rpt_bare_col.sql");
    bare_col.1.access = Some("public".into());
    bare_col.1.description = Some("described".into());
    bare_col.1.columns = columns(&[("id", Some("the key"), None), ("amt", None, None)]);
    let mut clean = model("rpt_clean", "marts/rpt_clean.sql");
    clean.1.access = Some("public".into());
    clean.1.description = Some("described".into());
    let mut full = model("rpt_full", "marts/rpt_full.sql");
    full.1.access = Some("public".into());
    full.1.description = Some("described".into());
    full.1.columns = columns(&[("id", Some("the key"), None)]);
    let mut priv_m = model("rpt_priv", "marts/rpt_priv.sql");
    priv_m.1.access = Some("private".into());
    let m = manifest(vec![no_desc, bare_col, clean, full, priv_m], vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::UndocumentedPublicModels);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.rpt_bare_col", "model.p.rpt_no_desc"],
        "missing model OR column descriptions flag; non-public and columnless don't"
    );
}

// ---- model_directories --------------------------------------------------------------

#[test]
fn model_directories_flags_a_prefix_folder_disagreement_only() {
    // staging/fct_x: prefix resolves marts, dir resolves staging → flag —
    // the mismatch naming_conventions can't see (prefix-first says fct_x
    // is a WELL-NAMED marts model). staging/stg_y agrees → clean.
    // reports/rpt_z: prefix resolves marts but `reports` isn't a configured
    // layer → the dir stage is unresolved → clean. marts/plain: no prefix
    // match → the prefix stage is unresolved → clean.
    let m = manifest(
        vec![
            model("fct_x", "staging/fct_x.sql"),
            model("stg_y", "staging/stg_y.sql"),
            model("rpt_z", "reports/rpt_z.sql"),
            model("plain", "marts/plain.sql"),
        ],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::ModelDirectories),
        vec!["model.p.fct_x"],
        "only a disagreement between BOTH resolved layers flags"
    );
    assert!(
        !flagged(&f, LintRule::NamingConventions).contains(&"model.p.fct_x"),
        "prefix-first naming_conventions passes staging/fct_x — this rule is the one that catches it"
    );
}

#[test]
fn lint_model_type_stages_compose_into_the_same_resolution() {
    // The two stages model_directories calls must compose into the same
    // resolution lint_model_type always had: prefix first, dir second,
    // `other` last — so the split didn't change any rule's input.
    let m = manifest(
        vec![
            model("fct_x", "staging/fct_x.sql"),
            model("plain", "staging/plain.sql"),
            model("rpt_x", "reports/rpt_x.sql"),
            model("bare", "reports/bare.sql"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();
    for (uid, prefix_layer, dir_layer, combined) in [
        // the prefix stage wins over the dir.
        ("model.p.fct_x", Some("marts"), Some("staging"), "marts"),
        // dir alone resolves the layer.
        ("model.p.plain", None, Some("staging"), "staging"),
        // an unmapped dir isn't a layer at all.
        ("model.p.rpt_x", Some("marts"), None, "marts"),
        // nothing resolves → other.
        ("model.p.bare", None, None, "other"),
    ] {
        let n = dag.get(uid).unwrap();
        assert_eq!(
            rules::lint_model_type_by_prefix(n, &cfg),
            prefix_layer,
            "{uid} prefix stage"
        );
        assert_eq!(
            rules::lint_model_type_by_dir(n, &cfg),
            dir_layer,
            "{uid} dir stage"
        );
        assert_eq!(
            rules::lint_model_type(n, &cfg),
            combined,
            "{uid} combined resolution"
        );
    }
}

// ---- required_meta_keys --------------------------------------------------------------

#[test]
fn required_meta_keys_flags_only_missing_keys() {
    // keys: [owner, domain] — stg_a has both, stg_b misses domain, stg_c
    // misses both, stg_d's owner is an EMPTY string (present is present).
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
            mk("stg_a", serde_json::json!({"owner": "t", "domain": "d"})),
            mk("stg_b", serde_json::json!({"owner": "t"})),
            mk("stg_c", serde_json::json!({})),
            mk("stg_d", serde_json::json!({"owner": "", "domain": "d"})),
        ],
        vec![],
        &[],
    );
    // Empty keys → the rule is a no-op.
    assert!(
        flagged(
            &findings_of(&m, &LintConfig::default()),
            LintRule::RequiredMetaKeys
        )
        .is_empty(),
        "no configured keys means no findings"
    );
    let cfg = config_from("lint:\n  rules:\n    required_meta_keys: {keys: [owner, domain]}\n");
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::RequiredMetaKeys);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.stg_b", "model.p.stg_c"],
        "an empty-string value still counts as present"
    );
    // The message names the missing keys.
    let stg_c = f
        .iter()
        .find(|f| f.rule == LintRule::RequiredMetaKeys && f.node_uid == "model.p.stg_c")
        .unwrap();
    assert!(
        stg_c.message.contains("owner") && stg_c.message.contains("domain"),
        "the message names the missing keys: {}",
        stg_c.message
    );
}

// ---- contract_columns_without_type ----------------------------------------------------

#[test]
fn contract_columns_without_type_flags_untyped_declared_columns() {
    // Enforced with a typeless column → flag (the column rides the message);
    // enforced with all-typed columns → clean; a typeless column on a
    // NON-enforced model isn't this rule's business.
    let mut bad = model("rpt_bad", "marts/rpt_bad.sql");
    bad.1.contract = Some(RawContract { enforced: true });
    bad.1.columns = columns(&[("id", None, Some("int")), ("amt", None, None)]);
    let mut good = model("rpt_good", "marts/rpt_good.sql");
    good.1.contract = Some(RawContract { enforced: true });
    good.1.columns = columns(&[("id", None, Some("int"))]);
    let mut plain = model("rpt_plain", "marts/rpt_plain.sql");
    plain.1.columns = columns(&[("amt", None, None)]);
    let m = manifest(vec![bad, good, plain], vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::ContractColumnsWithoutType)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.p.rpt_bad");
    assert!(
        hits[0].message.contains("amt"),
        "the untyped column is named: {}",
        hits[0].message
    );
}

// ---- missing_primary_key_tests.resource_types -----------------------------------------

#[test]
fn missing_primary_key_tests_resource_types_widens_the_target_set() {
    // A seed, a snapshot and a model — untested — plus a PK-tested seed.
    let seed = |name: &str| {
        (
            format!("seed.p.{name}"),
            RawNode {
                name: name.into(),
                resource_type: "seed".into(),
                path: Some(format!("seeds/{name}.csv")),
                ..Default::default()
            },
        )
    };
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            seed("raw_seed"),
            seed("pk_seed"),
            (
                "snapshot.p.snap".to_string(),
                RawNode {
                    name: "snap".into(),
                    resource_type: "snapshot".into(),
                    path: Some("snapshots/snap.sql".into()),
                    ..Default::default()
                },
            ),
            test_attached("seed.p.pk_seed", "unique", "id"),
            test_attached("seed.p.pk_seed", "not_null", "id"),
        ],
        vec![],
        &[],
    );
    // Default [model]: only the model flags — seeds/snapshots are out.
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::MissingPrimaryKeyTests),
        vec!["model.p.stg_a"],
        "the default targets models only"
    );
    // [model, seed]: the untested seed joins; the tested one stays clean.
    let cfg = config_from(
        "lint:\n  rules:\n    missing_primary_key_tests: {resource_types: [model, seed]}\n",
    );
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::MissingPrimaryKeyTests);
    got.sort_unstable();
    assert_eq!(got, vec!["model.p.stg_a", "seed.p.raw_seed"]);
    // An invalid value is a config error naming the valid set.
    let err = ProjectConfig::from_yaml(
        "lint:\n  rules:\n    missing_primary_key_tests: {resource_types: [model, bogus]}\n",
    )
    .unwrap_err();
    assert!(
        err.contains("bogus") && err.contains("model") && err.contains("seed"),
        "{err} names the bad value and the valid set"
    );
}

// ---- suppression on the new rules ------------------------------------------------------

#[test]
fn exclude_and_meta_suppression_reach_the_new_rules() {
    // One `exclude`, one `meta.dbtl.ignore`, spanning two different new
    // rules — both ride run_rules' shared suppression path.
    let mut stg_ignored = model("stg_ignored", "staging/stg_ignored.sql");
    stg_ignored.1.meta = serde_json::json!({"dbtl": {"ignore": ["staging_depends_on_staging"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            stg_ignored,
            model("stg_b", "staging/stg_b.sql"),
        ],
        vec![source("dead_src"), source("kept_src")],
        &[
            ("model.p.stg_a", "model.p.stg_ignored"),
            ("model.p.stg_a", "model.p.stg_b"),
        ],
    );
    let cfg = config_from("lint:\n  rules:\n    unused_sources: {exclude: [\"dead_src\"]}\n");
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::UnusedSources),
        vec!["source.p.kept_src"],
        "the exclude drops dead_src's finding; kept_src still flags"
    );
    assert_eq!(
        flagged(&f, LintRule::StagingDependsOnStaging),
        vec!["model.p.stg_b"],
        "meta.dbtl.ignore suppresses stg_ignored's finding; stg_b still flags"
    );
}

// ---- incremental_without_unique_key ----------------------------------------------

/// A model literal with a materialization and (optional) `unique_key` —
/// `unique_key` rides the wire `Value` shape (bare string or list).
fn keyed_model(
    name: &str,
    materialized: &str,
    unique_key: Option<serde_json::Value>,
) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("marts/{name}.sql")),
            original_file_path: Some(format!("models/marts/{name}.sql")),
            config: crate::manifest::RawConfig {
                materialized: Some(materialized.into()),
                unique_key,
            },
            ..Default::default()
        },
    )
}

// ---- SQL-backed & catalog-backed rules ------------------------------------
//
// The fixtures here are tiny `raw_code`/`compiled_code` RawManifest literals —
// the `SqlParseResolver` parses them exactly like a real manifest's SQL, and
// the shared lazy parse is asserted through its memo probes.

/// A model carrying `raw_code` (the SQL the rules read).
fn model_sql(name: &str, raw: &str) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("models/{name}.sql")),
            raw_code: Some(raw.into()),
            ..Default::default()
        },
    )
}

/// A model carrying declared `columns:` — `enforced` pins
/// `contract: {enforced: true}` (the declared list becomes authoritative).
fn model_sql_cols(
    name: &str,
    raw: &str,
    cols: &[(&str, Option<&str>)],
    enforced: bool,
) -> (String, RawNode) {
    let mut columns = serde_json::Map::new();
    for (c, ty) in cols {
        let mut v = serde_json::Map::new();
        v.insert("name".into(), (*c).into());
        if let Some(ty) = ty {
            v.insert("data_type".into(), (*ty).into());
        }
        columns.insert((*c).to_string(), serde_json::Value::Object(v));
    }
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("models/{name}.sql")),
            raw_code: Some(raw.into()),
            columns,
            contract: enforced.then_some(RawContract { enforced: true }),
            ..Default::default()
        },
    )
}

#[test]
fn incremental_without_unique_key_flags_unkeyed_incrementals() {
    let m = manifest(
        vec![
            keyed_model("no_key", "incremental", None),
            keyed_model("scalar", "incremental", Some(serde_json::json!("id"))),
            keyed_model("listed", "incremental", Some(serde_json::json!(["a", "b"]))),
            keyed_model("tbl", "table", None),
        ],
        vec![],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::IncrementalWithoutUniqueKey),
        vec!["model.p.no_key"],
        "only the unkeyed incremental flags (a table never does)"
    );
    let hit = f
        .iter()
        .find(|f| f.rule == LintRule::IncrementalWithoutUniqueKey)
        .unwrap();
    assert_eq!(
        hit.severity,
        Severity::Warn,
        "the new rule defaults to warn"
    );
    assert_eq!(
        hit.location.as_ref().unwrap().file,
        "models/marts/no_key.sql"
    );
}

#[test]
fn incremental_without_unique_key_honours_exclude_and_meta_ignore() {
    // The shared suppression machinery reaches the new rules like any
    // other — exclude by selector, or `meta.dbtl.ignore` on the node.
    let mut ignored = keyed_model("ignored", "incremental", None);
    ignored.1.meta = serde_json::json!({"dbtl": {"ignore": ["incremental_without_unique_key"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let m = manifest(
        vec![
            keyed_model("excluded", "incremental", None),
            ignored,
            keyed_model("still_bad", "incremental", None),
        ],
        vec![],
        &[],
    );
    let cfg = config_from(
        "lint:\n  rules:\n    incremental_without_unique_key: {exclude: [\"excluded\"]}\n",
    );
    assert_eq!(
        flagged(
            &findings_of(&m, &cfg),
            LintRule::IncrementalWithoutUniqueKey
        ),
        vec!["model.p.still_bad"]
    );
}

// ---- sources_without_freshness ----------------------------------------------------

/// A source literal with (optional) freshness — `threshold` builds the
/// wire `{count, period}` shape real dbt emits (nulls included).
fn threshold(
    count: Option<i64>,
    period: Option<&str>,
) -> Option<crate::manifest::RawFreshnessThreshold> {
    Some(crate::manifest::RawFreshnessThreshold {
        count,
        period: period.map(str::to_string),
    })
}

fn fresh_source(
    name: &str,
    freshness: Option<crate::manifest::RawFreshness>,
) -> (String, RawSource) {
    (
        format!("source.p.{name}"),
        RawSource {
            name: name.split('.').next_back().unwrap().into(),
            resource_type: "source".into(),
            original_file_path: Some("models/schema.yml".into()),
            freshness,
            ..Default::default()
        },
    )
}

#[test]
fn sources_without_freshness_flags_unconfigured_sources() {
    let m = manifest(
        vec![],
        vec![
            fresh_source("s.bare", None),
            // `freshness: null` on the wire (a table's explicit clear) —
            // just as unconfigured as a missing block.
            fresh_source("s.cleared", None),
            // An all-null threshold carries no signal.
            fresh_source(
                "s.null_threshold",
                Some(crate::manifest::RawFreshness {
                    warn_after: None,
                    error_after: threshold(None, None),
                }),
            ),
            fresh_source(
                "s.warned",
                Some(crate::manifest::RawFreshness {
                    warn_after: threshold(Some(12), Some("hour")),
                    error_after: None,
                }),
            ),
            // error_after alone counts — warn_after is not required.
            fresh_source(
                "s.errored",
                Some(crate::manifest::RawFreshness {
                    warn_after: None,
                    error_after: threshold(Some(6), Some("hour")),
                }),
            ),
        ],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    let mut got = flagged(&f, LintRule::SourcesWithoutFreshness);
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            "source.p.s.bare",
            "source.p.s.cleared",
            "source.p.s.null_threshold"
        ],
        "a configured threshold on either side is effective freshness"
    );
    // The finding lands on the declaring yml — the source's own
    // original_file_path (not a patch_path; dbt doesn't emit one).
    let hit = f
        .iter()
        .find(|f| f.rule == LintRule::SourcesWithoutFreshness)
        .unwrap();
    assert_eq!(hit.location.as_ref().unwrap().file, "models/schema.yml");
}

#[test]
fn sources_without_freshness_honours_exclude_and_meta_ignore() {
    let mut ignored = fresh_source("s.ignored", None);
    ignored.1.meta = serde_json::json!({"dbtl": {"ignore": ["sources_without_freshness"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let m = manifest(
        vec![],
        vec![
            fresh_source("s.excluded", None),
            ignored,
            fresh_source("s.still_bad", None),
        ],
        &[],
    );
    let cfg =
        config_from("lint:\n  rules:\n    sources_without_freshness: {exclude: [\"excluded\"]}\n");
    assert_eq!(
        flagged(&findings_of(&m, &cfg), LintRule::SourcesWithoutFreshness),
        vec!["source.p.s.still_bad"]
    );
}

// ---- the location envelope: which file a finding points at -------------------------

/// A model literal carrying `original_file_path` and (optional)
/// `patch_path` — the two files the envelope chooses between.
fn filed_model(name: &str, ofp: &str, patch: Option<&str>) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("marts/{name}.sql")),
            original_file_path: Some(ofp.into()),
            patch_path: patch.map(|p| format!("sample://{p}")),
            ..Default::default()
        },
    )
}

/// A model with only `compiled_code` (no `raw_code`) — deliberately NOT
/// judged by `hard_coded_references` (a compiled `ref()` is a written-
/// looking name).
fn model_compiled_only(name: &str, compiled: &str) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(format!("models/{name}.sql")),
            compiled_code: Some(compiled.into()),
            ..Default::default()
        },
    )
}

fn file_of(findings: &[Finding], rule: LintRule, uid: &str) -> Option<String> {
    findings
        .iter()
        .find(|f| f.rule == rule && f.node_uid == uid)
        .and_then(|f| f.location.as_ref().map(|l| l.file.clone()))
}

#[test]
fn location_file_follows_the_rule_kind() {
    // undocumented_models fires on a model with no description; root_models
    // fires on the SAME model (no parents) — same node, different file:
    // the yaml-declared-attribute rules take patch_path, the graph rules
    // keep original_file_path.
    let m = manifest(
        vec![
            filed_model(
                "declared",
                "models/marts/declared.sql",
                Some("models/marts/schema.yml"),
            ),
            filed_model("unpatched", "models/marts/unpatched.sql", None),
        ],
        vec![source("s.bare")],
        &[],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        file_of(&f, LintRule::UndocumentedModels, "model.p.declared"),
        Some("models/marts/schema.yml".to_string()),
        "a yml-declared-attribute finding prefers the declaring yml"
    );
    assert_eq!(
        file_of(&f, LintRule::UndocumentedModels, "model.p.unpatched"),
        Some("models/marts/unpatched.sql".to_string()),
        "no patch → the model's own file"
    );
    assert_eq!(
        file_of(&f, LintRule::RootModels, "model.p.declared"),
        Some("models/marts/declared.sql".to_string()),
        "a graph finding keeps the .sql even when a patch exists"
    );
    assert_eq!(
        file_of(&f, LintRule::MissingPrimaryKeyTests, "model.p.declared"),
        Some("models/marts/schema.yml".to_string()),
        "test declarations live in the yml too"
    );
    // A source finding points at its declaring yml — the source's own
    // original_file_path (dbt emits no patch_path on sources).
    let mut src = source("s.bare");
    src.1.original_file_path = Some("models/staging/schema.yml".into());
    let m2 = manifest(vec![], vec![src], &[]);
    let f2 = findings_of(&m2, &LintConfig::default());
    assert_eq!(
        file_of(&f2, LintRule::UnusedSources, "source.p.s.bare"),
        Some("models/staging/schema.yml".to_string()),
    );
}

// ---- yaml_name_line -----------------------------------------------------------------

fn names(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn yaml_name_line_finds_source_model_and_column_entries() {
    let yml = "version: 2\n\nsources:\n  - name: raw\n    tables:\n      - name: orders\n      - name: customers\n\nmodels:\n  - name: stg_a\n    columns:\n      - name: id\n        description: x\n      - name: status\n";
    // [source, table] / [model] / [model, column] — 1-based lines.
    assert_eq!(yaml_name_line(yml, &names(&["raw", "orders"])), Some(6));
    assert_eq!(yaml_name_line(yml, &names(&["raw", "customers"])), Some(7));
    assert_eq!(yaml_name_line(yml, &names(&["stg_a"])), Some(10));
    assert_eq!(yaml_name_line(yml, &names(&["stg_a", "id"])), Some(12));
    assert_eq!(yaml_name_line(yml, &names(&["stg_a", "status"])), Some(14));
    // Unknown names → None; a name outside the chain's block → None.
    assert_eq!(yaml_name_line(yml, &names(&["ghost"])), None);
    assert_eq!(yaml_name_line(yml, &names(&["raw", "status"])), None);
}

#[test]
fn yaml_name_line_respects_block_boundaries() {
    // `status` exists under BOTH models — the chain descends into the
    // matching parent's block, so each column resolves in its own model.
    let yml = "models:\n  - name: a\n    columns:\n      - name: status\n  - name: b\n    columns:\n      - name: status\n";
    assert_eq!(yaml_name_line(yml, &names(&["a", "status"])), Some(4));
    assert_eq!(yaml_name_line(yml, &names(&["b", "status"])), Some(7));
    // A `raw` model and a `raw` SOURCE in one file: the chain retries the
    // next same-name entry when the first block can't complete it.
    let both = "models:\n  - name: raw\n    description: a model\n\nsources:\n  - name: raw\n    tables:\n      - name: orders\n";
    assert_eq!(
        yaml_name_line(both, &names(&["raw", "orders"])),
        Some(8),
        "the first `raw` (a model) can't complete the chain — the source's wins"
    );
    // A name deeper than the block's end never matches: the search stops
    // at the first line back at the parent's indent.
    let yml = "sources:\n  - name: raw\n    tables:\n      - name: orders\n  - name: other\n    tables: []\n";
    assert_eq!(yaml_name_line(yml, &names(&["other", "orders"])), None);
}

#[test]
fn yaml_name_line_handles_quotes_and_comments() {
    // Quoted `- name:` values match the bare name; comment and blank lines
    // don't end a block.
    let yml = "sources:\n  - name: \"raw\"\n    # a comment inside the block\n\n    tables:\n      - name: 'orders'\n";
    assert_eq!(yaml_name_line(yml, &names(&["raw", "orders"])), Some(6));
    assert_eq!(yaml_name_line(yml, &names(&["raw"])), Some(2));
    // A `- name:` with no value can't match.
    let yml = "models:\n  - name:\n    description: x\n  - name: real\n";
    assert_eq!(yaml_name_line(yml, &names(&["real"])), Some(4));
}

// ---- fill_finding_lines --------------------------------------------------------------

#[test]
fn fill_finding_lines_resolves_lines_and_survives_missing_files() {
    // One model declared in schema.yml with two undescribed columns (the
    // finding lands on the FIRST offending column's line), one source in
    // the same file, and one model whose yml is absent from disk — its
    // line stays unset, never an error.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nsources:\n  - name: s\n    tables:\n      - name: bare\n\nmodels:\n  - name: m\n    columns:\n      - name: id\n      - name: amt\n",
    )
    .unwrap();
    let cols = |name: &str| (name.to_string(), serde_json::json!({"name": name}));
    let mut m_cols = serde_json::Map::new();
    m_cols.extend([cols("id"), cols("amt")]);
    let mut declared = filed_model("m", "models/m.sql", Some("models/schema.yml"));
    declared.1.description = Some("documented".into());
    declared.1.columns = m_cols;
    let mut ghost = filed_model("ghost", "models/ghost.sql", Some("models/ghost.yml"));
    ghost.1.description = Some("documented".into());
    ghost.1.columns = serde_json::Map::from_iter([cols("x")]);
    let mut src = source("s.bare");
    src.1.original_file_path = Some("models/schema.yml".into());
    let m = manifest(vec![declared, ghost], vec![src], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line_of = |rule, uid| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == uid)
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(
        line_of(LintRule::UndocumentedColumns, "model.p.m"),
        Some(11),
        "the first undescribed column's `- name:` line"
    );
    assert_eq!(
        line_of(LintRule::UnusedSources, "source.p.s.bare"),
        Some(6),
        "a source finding descends [source, table]"
    );
    assert_eq!(
        line_of(LintRule::SourcesWithoutFreshness, "source.p.s.bare"),
        Some(6)
    );
    // The missing file just leaves the line unset.
    assert_eq!(
        line_of(LintRule::UndocumentedColumns, "model.p.ghost"),
        None
    );
    // A graph finding on a .sql file is never line-filled.
    assert_eq!(line_of(LintRule::RootModels, "model.p.m"), None);
}

/// A catalog literal the `Rc`-carrying resolver and the lint input share.
fn catalog_json(json: &str) -> Rc<Catalog> {
    Rc::new(Catalog::from_json(json).unwrap())
}

/// The resolver the SQL rules read (catalog-less unless a test pins one).
fn sql_resolver(catalog: Option<Rc<Catalog>>) -> SqlParseResolver {
    SqlParseResolver::new(catalog, SqlDialect::Generic)
}

/// A `LintConfig` with ONLY `rules` enabled — the finding set a test sees
/// is exactly the rules it names.
fn cfg_only(rules: &[LintRule]) -> LintConfig {
    let mut cfg = LintConfig::default();
    for r in LintRule::ALL {
        if !rules.contains(&r) {
            cfg.severities.insert(r, None);
        }
    }
    cfg
}

/// `run_rules` over `dag` with every input the SQL/catalog rules could
/// read, returning the full outcome (findings AND the skipped list).
fn lint_outcome(
    dag: &Dag,
    cfg: &LintConfig,
    sql: Option<&SqlParseResolver>,
    catalog: Option<&Catalog>,
) -> LintOutcome {
    run_rules(LintInput::new(dag, cfg).with_sql(sql).with_catalog(catalog))
}

#[test]
fn select_star_flags_output_producing_wildcards_only() {
    let m = manifest(
        vec![
            model_sql("star", "select * from {{ ref('src') }}"),
            model_sql("qualified_star", "select t.* from tbl t"),
            model_sql(
                "cte_star",
                "with c as (select * from src) select c.a, c.b from c",
            ),
            model_sql("final_star", "with final as (select 1) select * from final"),
            model_sql("union_arm", "select a from x union all select * from y"),
            model_sql("agg", "select count(*) as n from t"),
        ],
        vec![source("src.raw")],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::SelectStar),
        vec![
            "model.p.final_star",
            "model.p.qualified_star",
            "model.p.star",
            "model.p.union_arm",
        ],
        "CTE-internal * and count(*) must not flag; final/UNION/output * must"
    );
    assert!(out.skipped.is_empty());
}

#[test]
fn select_star_skips_models_without_analyzable_sql() {
    let m = manifest(
        vec![
            model_sql("unparsable", "select from where or"),
            model_sql("empty", "   "),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.findings.is_empty(),
        "unparsable/absent SQL is 'not judged', never flagged"
    );
}

#[test]
fn hard_coded_references_flags_only_written_relation_names() {
    let m = manifest(
        vec![
            model_sql("plain", "select * from analytics.orders"),
            model_sql("via_ref", "select * from {{ ref('orders') }}"),
            model_sql("via_source", "select * from {{ source('a','b') }}"),
            model_sql("cte_name", "with cte_x as (select 1) select * from cte_x"),
            model_sql(
                "recursive_cte",
                "with n(i) as (select 1 union all select i + 1 from n) select * from n",
            ),
            model_sql("table_fn", "select * from generate_series(3)"),
            model_sql("nested_written", "select * from (select * from real.rel) d"),
            model_sql(
                "join_side",
                "select * from {{ ref('a') }} t join b.t2 on t.id = b.t2.id",
            ),
            model_sql("jinja_position", "select * from {{ target.schema }}.t"),
            model_sql("unparsable", "select from where or"),
            model_compiled_only("compiled_only", "select * from prod.orders"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::HardCodedReferences]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::HardCodedReferences),
        vec![
            "model.p.join_side",
            "model.p.nested_written",
            "model.p.plain"
        ],
        "ref()/source()-rendered, CTE, table-function, and Jinja-position names must not flag"
    );
    let msg_of = |uid| {
        out.findings
            .iter()
            .find(|f| f.node_uid == uid)
            .unwrap()
            .message
            .clone()
    };
    assert!(msg_of("model.p.plain").contains("analytics.orders"));
    assert!(msg_of("model.p.nested_written").contains("real.rel"));
    assert!(msg_of("model.p.join_side").contains("b.t2"));
}

#[test]
fn cartesian_join_flags_conditionless_joins() {
    let m = manifest(
        vec![
            model_sql("cross", "select * from a.t cross join b.u"),
            model_sql("bare_join", "select * from a.t join b.u"),
            model_sql("with_on", "select * from a.t join b.u on a.t.id = b.u.id"),
            model_sql("with_using", "select * from a.t join b.u using (id)"),
            model_sql("natural", "select * from a.t natural join b.u"),
            model_sql(
                "unnest",
                "select * from a.t cross join unnest(a.t.arr) as x",
            ),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::CartesianJoin),
        vec!["model.p.bare_join", "model.p.cross"],
        "ON/USING/NATURAL conditions and UNNEST joins must not flag"
    );
}

#[test]
fn unknown_column_reference_trusts_only_catalog_or_contract_parents() {
    let m = manifest(
        vec![
            // Contract-enforced parent: its declared columns are the schema.
            model_sql_cols(
                "stg_contract",
                "select 1",
                &[("id", None), ("amount", None)],
                true,
            ),
            // Declared-but-unenforced parent: advisory, never evidence.
            model_sql_cols(
                "stg_advisory",
                "select 1",
                &[("id", None), ("amount", None)],
                false,
            ),
            model_sql("fct_contract", "select id, nope from stg_contract"),
            model_sql("fct_advisory", "select id, nope from stg_advisory"),
            model_sql("fct_source", "select id, nope from src_tbl"),
        ],
        vec![source("src.src_tbl")],
        &[
            ("model.p.stg_contract", "model.p.fct_contract"),
            ("model.p.stg_advisory", "model.p.fct_advisory"),
            ("source.p.src.src_tbl", "model.p.fct_source"),
        ],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnknownColumnReference]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnknownColumnReference),
        vec!["model.p.fct_contract"],
        "a contract-enforced parent's missing column flags; advisory declared and source lists never do"
    );
    let f = out
        .findings
        .iter()
        .find(|f| f.node_uid == "model.p.fct_contract")
        .unwrap();
    assert_eq!(f.related, vec!["model.p.stg_contract".to_string()]);
    assert!(f.message.contains("nope"), "the missing column is named");

    // The same child against a CATALOG-backed parent (no contract needed —
    // the warehouse record is authoritative).
    let cat = catalog_json(
        r#"{"nodes": {"model.p.stg_advisory": {"columns": {"id": {"type": "INT"}, "amount": {"type": "INT"}}}}}"#,
    );
    let res = sql_resolver(Some(cat));
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnknownColumnReference),
        vec!["model.p.fct_advisory", "model.p.fct_contract"],
        "the catalog's column list makes the parent trusted — the contract parent still flags"
    );
}

#[test]
fn forbidden_patterns_matches_raw_sql_verbatim() {
    let m = manifest(
        vec![
            model_sql("uses_udf", "select unsafe_udf(x) from t"),
            model_sql(
                "commented_udf",
                "select x from t -- unsafe_udf stays readable in raw",
            ),
            model_sql("clean", "select x from t"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    // Empty pattern list: nothing to check — the rule is a no-op.
    let cfg = cfg_only(&[LintRule::ForbiddenPatterns]);
    let res = sql_resolver(None);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.findings.is_empty(),
        "an empty patterns list flags nothing"
    );

    let cfg = config_from(
        "lint:\n  rules:\n    forbidden_patterns:\n      patterns: ['unsafe_udf', 'drop table']\n",
    );
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::ForbiddenPatterns),
        vec!["model.p.commented_udf", "model.p.uses_udf"],
        "raw text is matched verbatim — a comment still counts"
    );
    let msg = &out
        .findings
        .iter()
        .find(|f| f.node_uid == "model.p.uses_udf" && f.rule == LintRule::ForbiddenPatterns)
        .unwrap()
        .message;
    assert!(msg.contains("unsafe_udf"), "the hit pattern is named");

    // An invalid regex is a CONFIG error at load — the rule never sees it.
    let err = ProjectConfig::from_yaml(
        "lint:\n  rules:\n    forbidden_patterns:\n      patterns: ['([']\n",
    )
    .unwrap_err();
    assert!(
        err.contains("not a valid regex") && err.contains("'(['"),
        "the error names the pattern: {err}"
    );
}

// ---- catalog-backed rules ---------------------------------------------------

/// A model with declared columns and no SQL — the warehouse rules only
/// read `detail.columns` + the catalog.
fn model_cols(name: &str, cols: &[(&str, Option<&str>)]) -> (String, RawNode) {
    model_sql_cols(name, "", cols, false)
}

#[test]
fn warehouse_type_drift_flags_only_declared_type_disagreements() {
    let m = manifest(
        vec![
            model_cols(
                "mismatch",
                &[("amount", Some("int")), ("id", Some("STRING"))],
            ),
            model_cols("agrees", &[("amount", Some("NUMERIC(10,2)"))]),
            model_cols("no_declared_type", &[("amount", None)]),
            model_cols("not_in_catalog", &[("ghost", Some("INT"))]),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cat = catalog_json(
        r#"{"nodes": {
            "model.p.mismatch": {"columns": {"amount": {"type": "NUMERIC(10,2)"}, "id": {"type": "STRING"}}},
            "model.p.agrees": {"columns": {"amount": {"type": "NUMERIC(10,2)"}}},
            "model.p.no_declared_type": {"columns": {"amount": {"type": "NUMERIC(10,2)"}}}
        }}"#,
    );
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::WarehouseTypeDrift]);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    assert_eq!(
        flagged(&out.findings, LintRule::WarehouseTypeDrift),
        vec!["model.p.mismatch"],
        "matching types, undeclared types, and catalog-missing nodes never flag"
    );
    let msg = &out
        .findings
        .iter()
        .find(|f| f.node_uid == "model.p.mismatch")
        .unwrap()
        .message;
    assert!(
        msg.contains("amount") && msg.contains("int") && msg.contains("NUMERIC(10,2)"),
        "the message names column + both types: {msg}"
    );
    assert!(
        !msg.contains("id"),
        "the agreeing column isn't listed: {msg}"
    );
}

#[test]
fn columns_missing_in_warehouse_flags_documented_but_absent_columns() {
    let m = manifest(
        vec![
            model_cols("gap", &[("real", None), ("ghost", None)]),
            model_cols("whole", &[("real", None)]),
            model_cols("absent_node", &[("anything", None)]),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cat = catalog_json(
        r#"{"nodes": {
            "model.p.gap": {"columns": {"real": {"type": "INT"}}},
            "model.p.whole": {"columns": {"real": {"type": "INT"}, "extra": {"type": "INT"}}}
        }}"#,
    );
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::ColumnsMissingInWarehouse]);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    assert_eq!(
        flagged(&out.findings, LintRule::ColumnsMissingInWarehouse),
        vec!["model.p.gap"],
        "only declared columns absent from the warehouse flag"
    );
    let msg = &out
        .findings
        .iter()
        .find(|f| f.node_uid == "model.p.gap")
        .unwrap()
        .message;
    assert!(msg.contains("ghost") && !msg.contains("real"));
}

// ---- requirement-aware skipping & laziness ---------------------------------

#[test]
fn missing_inputs_report_skipped_rules_not_findings() {
    let m = manifest(vec![model_sql("m", "select 1")], vec![], &[]);
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();

    // No aux inputs at all: the five Sql rules + two Catalog rules skip.
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(out.skipped.len(), 7);
    let sql_skips: Vec<_> = out
        .skipped
        .iter()
        .filter(|s| s.missing == Requires::Sql)
        .map(|s| s.rule)
        .collect();
    assert_eq!(
        sql_skips,
        vec![
            LintRule::SelectStar,
            LintRule::HardCodedReferences,
            LintRule::CartesianJoin,
            LintRule::UnknownColumnReference,
            LintRule::ForbiddenPatterns,
        ],
        "skipped entries stay in LintRule::ALL order"
    );
    let cat_skips: Vec<_> = out
        .skipped
        .iter()
        .filter(|s| s.missing == Requires::Catalog)
        .map(|s| s.rule)
        .collect();
    assert_eq!(
        cat_skips,
        vec![
            LintRule::WarehouseTypeDrift,
            LintRule::ColumnsMissingInWarehouse,
        ]
    );

    // A resolver alone still leaves the catalog rules skipped.
    let res = sql_resolver(None);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(out.skipped.len(), 2);
    assert!(out.skipped.iter().all(|s| s.missing == Requires::Catalog));

    // Disabled rules never skip — off is not "skipped".
    let mut cfg = LintConfig::default();
    cfg.severities.insert(LintRule::SelectStar, None);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert!(
        !out.skipped.iter().any(|s| s.rule == LintRule::SelectStar),
        "a disabled rule produces neither findings nor a skip"
    );
}

#[test]
fn disabled_sql_rules_never_parse() {
    let m = manifest(
        vec![
            model_sql("a", "select * from t"),
            model_sql("b", "select * from u"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    // Every SQL-requiring rule off — the resolver must stay cold.
    let mut cfg = LintConfig::default();
    for r in LintRule::ALL {
        if r.info().requires == Requires::Sql {
            cfg.severities.insert(r, None);
        }
    }
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.skipped.iter().all(|s| s.missing == Requires::Catalog),
        "disabled rules never skip — only the still-enabled catalog rules do"
    );
    assert_eq!(
        res.analyzed_count(),
        0,
        "no SQL rule enabled → not one parse happened"
    );
    assert_eq!(res.hard_coded_count(), 0);
}

#[test]
fn enabled_sql_rules_share_one_parse_per_model() {
    let m = manifest(
        vec![
            model_sql("a", "select * from t"),
            model_sql("b", "select * from u"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[
        LintRule::SelectStar,
        LintRule::CartesianJoin,
        LintRule::UnknownColumnReference,
        LintRule::HardCodedReferences,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        res.analyzed_count(),
        2,
        "four SQL rules × two models = one shared analysis each"
    );
    assert_eq!(
        res.hard_coded_count(),
        2,
        "the marked re-parse is one per raw-carrying model"
    );
    assert_eq!(out.findings.len(), 4); // a + b flag select_star AND hard_coded
}
