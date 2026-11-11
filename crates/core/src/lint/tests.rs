use super::*;
use crate::manifest::{RawContract, RawManifest, RawNode, RawSource, RawTestMetadata};
use std::collections::HashMap;

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
    run_rules(&Dag::build(m), config)
}

#[test]
fn default_config_enables_every_rule() {
    let cfg = LintConfig::default();
    for rule in LintRule::ALL {
        assert!(cfg.enabled(rule), "{} must default to enabled", rule.name());
    }
    assert_eq!(cfg.model_fanout_threshold, crate::PR_HUB_MIN_DOWNSTREAM);
    assert_eq!(cfg.source_fanout_threshold, 2);
}

#[test]
fn yaml_disables_rules_and_overrides_thresholds() {
    let cfg = LintConfig::from_yaml(
        r#"lint:
  rules:
    undocumented_models: false
    source_fanout: false
  model_fanout_threshold: 3
  naming_prefixes:
    staging: [stg_, base_]
"#,
    )
    .unwrap();
    assert!(!cfg.enabled(LintRule::UndocumentedModels));
    assert!(!cfg.enabled(LintRule::SourceFanout));
    assert!(cfg.enabled(LintRule::RootModels));
    assert_eq!(cfg.model_fanout_threshold, 3);
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
fn yaml_rejects_unknown_rule_key_and_threshold_zero() {
    assert!(
        LintConfig::from_yaml("lint:\n  rules:\n    typo_rule: false\n")
            .unwrap_err()
            .contains("unknown lint rule 'typo_rule'")
    );
    assert!(LintConfig::from_yaml("lint:\n  nope: 1\n").is_err()); // deny_unknown_fields
    assert!(LintConfig::from_yaml("lint:\n  model_fanout_threshold: 0\n").is_err());
    assert!(LintConfig::from_yaml("lint: [not, a, map]\n").is_err()); // malformed
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
    let default = run_rules(&base, &LintConfig::default());
    assert!(flagged(&default, LintRule::ModelFanout).is_empty());
    let cfg = LintConfig::from_yaml("lint:\n  model_fanout_threshold: 2\n").unwrap();
    let tightened = run_rules(&base, &cfg);
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
    let cfg = LintConfig::from_yaml("lint:\n  naming_prefixes:\n    other: [rpt_]\n").unwrap();
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
    let cfg = LintConfig::from_yaml("lint:\n  rules:\n    root_models: false\n").unwrap();
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
    let a = run_rules(&dag, &LintConfig::default());
    let b = run_rules(&dag, &LintConfig::default());
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
    let f = run_rules(&dag, &LintConfig::default());
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
    let node_finding = Finding {
        rule: LintRule::DirectJoinToSource,
        node_uid: "model.p.x".into(),
        related: vec!["source.p.s.t".into()],
        message: String::new(),
    };
    assert_eq!(
        node_finding.key(),
        (LintRule::DirectJoinToSource, "model.p.x", &[][..])
    );
    let edge_finding = Finding {
        rule: LintRule::LayerViolation,
        node_uid: "model.p.c".into(),
        related: vec!["model.p.p".into()],
        message: String::new(),
    };
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
fn lint_config_path_prefers_the_project_root_then_target() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("target").join("manifest.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    // No .dbtl.yml anywhere → None.
    assert_eq!(lint_config_path(Some(&manifest), None), None);
    // Standard layout `<root>/target/manifest.json`: the PROJECT ROOT's
    // file wins (it is the conventional config home)…
    let root_cfg = dir.path().join(LINT_CONFIG_NAME);
    std::fs::write(&root_cfg, "lint: {}\n").unwrap();
    let target_cfg = dir.path().join("target").join(LINT_CONFIG_NAME);
    std::fs::write(&target_cfg, "lint: {}\n").unwrap();
    assert_eq!(
        lint_config_path(Some(&manifest), None),
        Some(root_cfg.clone()),
        "project root outranks target/ under the standard layout"
    );
    // …and target/ is only an escape hatch when the root has none.
    std::fs::remove_file(&root_cfg).unwrap();
    assert_eq!(lint_config_path(Some(&manifest), None), Some(target_cfg));
    // Source mode looks in the project dir itself (it IS the root).
    let proj = tempfile::tempdir().unwrap();
    let cfg = proj.path().join(LINT_CONFIG_NAME);
    std::fs::write(&cfg, "lint: {}\n").unwrap();
    assert_eq!(lint_config_path(None, Some(proj.path())), Some(cfg));
}

#[test]
fn lint_config_path_never_climbs_past_a_nonstandard_manifest_dir() {
    // `--manifest /tmp/x/manifest.json` must NOT see /tmp/.dbtl.yml —
    // the parent hop exists only for the `<root>/target/` layout.
    let parent = tempfile::tempdir().unwrap();
    let x = parent.path().join("x");
    std::fs::create_dir_all(&x).unwrap();
    let manifest = x.join("manifest.json");
    let foreign = parent.path().join(LINT_CONFIG_NAME);
    std::fs::write(&foreign, "lint: {}\n").unwrap();
    assert_eq!(
        lint_config_path(Some(&manifest), None),
        None,
        "a parent's .dbtl.yml must not leak into a non-target manifest dir"
    );
    // Its own dir still works.
    let own = x.join(LINT_CONFIG_NAME);
    std::fs::write(&own, "lint: {}\n").unwrap();
    assert_eq!(lint_config_path(Some(&manifest), None), Some(own));
}
