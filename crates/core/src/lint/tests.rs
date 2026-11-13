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
        unresolved_adjacent: Default::default(),
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
        docs: Default::default(),
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

/// A versioned-model literal — `model.p.<name>.<vseg>`, the uid shape a
/// real `versions:`-ed model compiles to (several nodes sharing one
/// declared `name`). `version`/`latest_version` carry the declared values
/// like the manifest does.
fn versioned_model(name: &str, vseg: &str, path: &str) -> (String, RawNode) {
    (
        format!("model.p.{name}.{vseg}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(path.into()),
            version: Some(vseg.trim_start_matches(['v', 'V']).into()),
            latest_version: Some("2".into()),
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
fn default_config_enables_the_registry_default_set() {
    // The `standard` preset resolves the registry's `default_enabled` set —
    // opt-in rules ship `default_enabled: false` and stay off until a
    // config (or the `strict` preset) reaches for them.
    let cfg = LintConfig::default();
    for rule in LintRule::ALL {
        assert_eq!(
            cfg.enabled(rule),
            rule.info().default_enabled,
            "{}'s default enablement",
            rule.name()
        );
    }
    // The opt-in set the registry ships today — a pinned list so adding a
    // fourth default-off rule is a deliberate edit, not drift.
    assert_eq!(
        LintRule::ALL
            .iter()
            .filter(|r| !r.info().default_enabled)
            .map(|r| r.name())
            .collect::<Vec<_>>(),
        vec![
            "undocumented_exposures",
            "model_too_long",
            "layer_materializations"
        ]
    );
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
fn multiple_sources_joined_merges_mutually_exclusive_branch_sources() {
    // `{% if %}`/`{% elif %}`/`{% else %}` arms never execute in one
    // render — sources the graph reaches only through pairwise-exclusive
    // arms count once (the edges still exist; only the "simultaneous"
    // claim goes away). The same calls unconditionally still flag.
    let m = manifest(
        vec![
            sited_model(
                "branch",
                "models/m/branch.sql",
                "select *\nfrom {% if target.name == 'prod' %}{{ source('a', 't') }}{% else %}{{ source('b', 't') }}{% endif %}",
            ),
            sited_model(
                "joined",
                "models/m/joined.sql",
                "select *\nfrom {{ source('a', 't') }} a\njoin {{ source('b', 't') }} b on a.id = b.id",
            ),
            // Nesting composes — a source one arm deeper still disagrees
            // on the outer chain.
            sited_model(
                "nested",
                "models/m/nested.sql",
                "select *\nfrom {% if x %}{{ source('a', 't') }}{% else %}{% if y %}{{ source('b', 't') }}{% endif %}{% endif %}",
            ),
        ],
        vec![source("a.t"), source("b.t")],
        &[
            ("source.p.a.t", "model.p.branch"),
            ("source.p.b.t", "model.p.branch"),
            ("source.p.a.t", "model.p.joined"),
            ("source.p.b.t", "model.p.joined"),
            ("source.p.a.t", "model.p.nested"),
            ("source.p.b.t", "model.p.nested"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::MultipleSourcesJoined),
        vec!["model.p.joined"],
        "sources reachable only through exclusive if/else arms count once — nested too"
    );
    // The edges still exist — only the simultaneous-read claim was dropped.
    let dag = Dag::build(&m);
    for uid in ["model.p.branch", "model.p.nested"] {
        assert_eq!(dag.direct_parents(uid).len(), 2, "{uid} keeps both edges");
    }
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
fn source_fanout_folds_model_versions_into_one_consumer() {
    // raw feeds fct_orders.v1+v2+vbeta — three DAG children but ONE model
    // family: a naive count trips the default threshold (2) on versions
    // alone, which is exactly the version-inflation the fold prevents.
    let versioned = || {
        vec![
            versioned_model("fct_orders", "v1", "marts/fct_orders_v1.sql"),
            versioned_model("fct_orders", "v2", "marts/fct_orders_v2.sql"),
            versioned_model("fct_orders", "vbeta", "marts/fct_orders_vbeta.sql"),
        ]
    };
    let m = manifest(
        versioned(),
        vec![source("raw.orders")],
        &[
            ("source.p.raw.orders", "model.p.fct_orders.v1"),
            ("source.p.raw.orders", "model.p.fct_orders.v2"),
            ("source.p.raw.orders", "model.p.fct_orders.vbeta"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert!(
        flagged(&f, LintRule::SourceFanout).is_empty(),
        "3 versions of one model are ONE consumer — no fanout finding"
    );

    // Control: a second DISTINCT consumer crosses the threshold — and the
    // message counts families (2, not 4), naming the multi-version family
    // by its bare model name while keeping every member uid in `related`.
    let mut nodes = versioned();
    nodes.push(model("stg_other", "staging/stg_other.sql"));
    let m2 = manifest(
        nodes,
        vec![source("raw.orders")],
        &[
            ("source.p.raw.orders", "model.p.fct_orders.v1"),
            ("source.p.raw.orders", "model.p.fct_orders.v2"),
            ("source.p.raw.orders", "model.p.fct_orders.vbeta"),
            ("source.p.raw.orders", "model.p.stg_other"),
        ],
    );
    let f2 = findings_of(&m2, &LintConfig::default());
    let hits: Vec<&Finding> = f2
        .iter()
        .filter(|f| f.rule == LintRule::SourceFanout)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "findings: {:?}",
        flagged(&f2, LintRule::SourceFanout)
    );
    let hit = hits[0];
    assert!(
        hit.message.contains("directly feeds 2 models"),
        "family count, not node count: {}",
        hit.message
    );
    assert!(
        hit.message.contains("fct_orders,") || hit.message.contains("fct_orders "),
        "the folded family names bare: {}",
        hit.message
    );
    assert!(!hit.message.contains("fct_orders.v"), "{:?}", hit.message);
    assert!(hit.message.contains("stg_other"), "{}", hit.message);
    assert_eq!(
        hit.related.len(),
        4,
        "related keeps the version-specific member uids"
    );
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
fn model_fanout_skips_leaves_only_packages_consume() {
    // `fct_pkg` reads leaf-shaped after the Root prune, but an
    // installed-package model consumes it — leaf-ness can't be
    // established, so int_hub counts only fct_leaf and stays under the
    // threshold. `int_two` feeds two proven leaves: the control flags.
    let mut pkg = model("pkg_use", "dep/pkg_use.sql");
    pkg.0 = "model.dep.pkg_use".into();
    let mut m = manifest(
        vec![
            pkg,
            model("int_hub", "intermediate/int_hub.sql"),
            model("fct_leaf", "marts/fct_leaf.sql"),
            model("fct_pkg", "marts/fct_pkg.sql"),
            model("int_two", "intermediate/int_two.sql"),
            model("fct_one", "marts/fct_one.sql"),
            model("fct_two", "marts/fct_two.sql"),
        ],
        vec![],
        &[
            ("model.p.int_hub", "model.p.fct_leaf"),
            ("model.p.int_hub", "model.p.fct_pkg"),
            ("model.p.fct_pkg", "model.dep.pkg_use"),
            ("model.p.int_two", "model.p.fct_one"),
            ("model.p.int_two", "model.p.fct_two"),
        ],
    );
    multi_package(&mut m);
    let (dag, _) = crate::PackageScope::Root.apply_with_warnings(Dag::build(&m));
    let cfg = config_from("lint:\n  model_fanout_threshold: 2\n");
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::ModelFanout),
        vec!["model.p.int_two"],
        "a pruned package consumer keeps fct_pkg from proving leaf-ness"
    );
    assert_eq!(
        dag.pruned_adjacent("model.p.fct_pkg"),
        Some(&crate::PrunedAdjacent {
            parents: 0,
            children: 1
        })
    );
}

#[test]
fn model_fanout_folds_versioned_leaves_into_one_consumer() {
    // int_hub's only leaf children are fct_x.v1 + fct_x.v2 — one leaf
    // FAMILY feeding one logical consumer. At threshold 2 the per-node
    // count flags, the fold must not; a second distinct leaf does flag.
    let m = manifest(
        vec![
            model("int_hub", "intermediate/int_hub.sql"),
            versioned_model("fct_x", "v1", "marts/fct_x_v1.sql"),
            versioned_model("fct_x", "v2", "marts/fct_x_v2.sql"),
        ],
        vec![],
        &[
            ("model.p.int_hub", "model.p.fct_x.v1"),
            ("model.p.int_hub", "model.p.fct_x.v2"),
        ],
    );
    let cfg = config_from("lint:\n  model_fanout_threshold: 2\n");
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::ModelFanout).is_empty(),
        "two versions of one leaf = one consumer — no fanout finding"
    );

    let m2 = manifest(
        vec![
            model("int_hub", "intermediate/int_hub.sql"),
            versioned_model("fct_x", "v1", "marts/fct_x_v1.sql"),
            versioned_model("fct_x", "v2", "marts/fct_x_v2.sql"),
            model("fct_other", "marts/fct_other.sql"),
        ],
        vec![],
        &[
            ("model.p.int_hub", "model.p.fct_x.v1"),
            ("model.p.int_hub", "model.p.fct_x.v2"),
            ("model.p.int_hub", "model.p.fct_other"),
        ],
    );
    let f2 = findings_of(&m2, &cfg);
    let hits: Vec<&Finding> = f2
        .iter()
        .filter(|f| f.rule == LintRule::ModelFanout)
        .collect();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].message.contains("directly feeds 2 leaf models"),
        "family count, not node count: {}",
        hits[0].message
    );
    assert!(
        hits[0].message.contains("fct_x") && !hits[0].message.contains("fct_x.v"),
        "folded family named bare once: {}",
        hits[0].message
    );
}

#[test]
fn model_fanout_endpoint_children_dont_disqualify_a_leaf() {
    // The issue's repro: hub feeds three reporting models and each reads
    // leaf-shaped — an exposure on rpt_a and a `semantic_models:` consumer
    // on rpt_b are ENDPOINTS, not data-resource children, so they don't
    // take away leaf-ness. Threshold 3 still flags hub. The control
    // int_ctrl's only child feeds a real model downstream — not a leaf —
    // so it stays quiet.
    let mut m = manifest(
        vec![
            model("int_hub", "intermediate/int_hub.sql"),
            model("rpt_a", "marts/rpt_a.sql"),
            model("rpt_b", "marts/rpt_b.sql"),
            model("rpt_c", "marts/rpt_c.sql"),
            model("int_ctrl", "intermediate/int_ctrl.sql"),
            model("rpt_mid", "marts/rpt_mid.sql"),
            model("fct_deep", "marts/fct_deep.sql"),
        ],
        vec![],
        &[
            ("model.p.int_hub", "model.p.rpt_a"),
            ("model.p.int_hub", "model.p.rpt_b"),
            ("model.p.int_hub", "model.p.rpt_c"),
            ("model.p.rpt_a", "exposure.p.dash"),
            ("model.p.int_ctrl", "model.p.rpt_mid"),
            ("model.p.rpt_mid", "model.p.fct_deep"),
        ],
    );
    m.exposures.extend([exposure("dash")]);
    m.semantic_models
        .extend([semantic_model("sm_rpt", "model.p.rpt_b")]);
    let cfg = config_from("lint:\n  model_fanout_threshold: 3\n");
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::ModelFanout),
        vec!["model.p.int_hub"],
        "exposure/semantic children keep rpt_a/rpt_b leaves — int_hub flags"
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
fn rejoining_upstream_ignore_shared_parents_exempts_matching_as() {
    // dim_users -> fct_orders -> rpt_daily AND dim_users -> rpt_daily: the
    // dimensional shape — a fact and its downstream both join the same
    // dim. The selector matches A (the shared parent), never B (the
    // flagged hop) — `exclude:` can't express that (it filters B).
    let m = manifest(
        vec![
            model("dim_users", "marts/dim_users.sql"),
            model("fct_orders", "marts/fct_orders.sql"),
            model("rpt_daily", "marts/rpt_daily.sql"),
            model("stg_extra", "staging/stg_extra.sql"),
        ],
        vec![],
        &[
            ("model.p.dim_users", "model.p.fct_orders"),
            ("model.p.fct_orders", "model.p.rpt_daily"),
            ("model.p.dim_users", "model.p.rpt_daily"),
        ],
    );
    // Default (empty list): the redundant hop still flags.
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        flagged(&f, LintRule::RejoiningUpstream),
        vec!["model.p.fct_orders"]
    );
    // dim_* exempts every shared parent → nothing left to flag.
    let cfg = config_from(
        "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"dim_*\"]}\n",
    );
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::RejoiningUpstream).is_empty(),
        "every shared parent matched → no bypass remains"
    );
    // A selector that doesn't match A exempts nothing.
    let cfg = config_from(
        "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"fct_*\"]}\n",
    );
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::RejoiningUpstream),
        vec!["model.p.fct_orders"],
        "the selector matches shared parents (A), never the flagged hop (B)"
    );
    // A second, non-matching shared parent keeps the finding alive — the
    // parameter drops A's from the list, not the whole check.
    let m = manifest(
        vec![
            model("dim_users", "marts/dim_users.sql"),
            model("stg_extra", "staging/stg_extra.sql"),
            model("fct_orders", "marts/fct_orders.sql"),
            model("rpt_daily", "marts/rpt_daily.sql"),
        ],
        vec![],
        &[
            ("model.p.dim_users", "model.p.fct_orders"),
            ("model.p.stg_extra", "model.p.fct_orders"),
            ("model.p.fct_orders", "model.p.rpt_daily"),
            ("model.p.dim_users", "model.p.rpt_daily"),
            ("model.p.stg_extra", "model.p.rpt_daily"),
        ],
    );
    let cfg = config_from(
        "lint:\n  rules:\n    rejoining_upstream: {ignore_shared_parents: [\"dim_*\"]}\n",
    );
    let f = findings_of(&m, &cfg);
    let hit = f
        .iter()
        .find(|f| f.rule == LintRule::RejoiningUpstream)
        .expect("stg_extra is still an unexempted shared parent");
    assert_eq!(hit.node_uid, "model.p.fct_orders");
    assert!(
        hit.message.contains("stg_extra"),
        "the remaining A is the one the message names: {}",
        hit.message
    );
}

#[test]
fn rejoining_upstream_counts_only_data_node_children_and_endpoints() {
    // the single-use semantics:
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
    // A `semantic_models:` consumer lands on `semantic_dependents` — the
    // note reads both channels (manifest mode also carries the edge).
    m.semantic_models
        .extend([semantic_model("sm_b", "model.p.int_b")]);
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
    // The endpoints still read B — the message names them (the graph
    // child exposure AND the `semantic_dependents` entry, deduped) so
    // "redundant hop" can't be read as "nothing consumes int_b".
    assert!(
        hits[0].message.contains(
            "(also read by exposure exp, semantic_model sm_b; endpoint consumers do not count as uses)"
        ),
        "endpoint consumers named in the note: {}",
        hits[0].message
    );
}

#[test]
fn rejoining_upstream_flags_a_snapshot_relay() {
    // A→B→C + A→C where the redundant hop B is a snapshot (the
    // single-use check isn't model-only).
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
fn rejoining_upstream_skips_bs_with_pruned_package_children() {
    // The A→B→C + A→C shape is complete, but B's other consumer lives in
    // an installed package — after the Root prune B reads single-child,
    // yet "otherwise unused" can't be established. `int_y` is the
    // all-in-project control: it flags.
    let mut pkg = model("pkg_use", "dep/pkg_use.sql");
    pkg.0 = "model.dep.pkg_use".into();
    let mut m = manifest(
        vec![
            pkg,
            model("stg_a", "staging/stg_a.sql"),
            model("int_b", "intermediate/int_b.sql"),
            model("fct_c", "marts/fct_c.sql"),
            model("stg_x", "staging/stg_x.sql"),
            model("int_y", "intermediate/int_y.sql"),
            model("fct_z", "marts/fct_z.sql"),
        ],
        vec![],
        &[
            ("model.p.stg_a", "model.p.int_b"),
            ("model.p.int_b", "model.p.fct_c"),
            ("model.p.stg_a", "model.p.fct_c"),
            ("model.p.int_b", "model.dep.pkg_use"),
            ("model.p.stg_x", "model.p.int_y"),
            ("model.p.int_y", "model.p.fct_z"),
            ("model.p.stg_x", "model.p.fct_z"),
        ],
    );
    multi_package(&mut m);
    let (dag, _) = crate::PackageScope::Root.apply_with_warnings(Dag::build(&m));
    let cfg = cfg_only(&[LintRule::RejoiningUpstream]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::RejoiningUpstream),
        vec!["model.p.int_y"],
        "a severed package consumer means 'single use' can't be established"
    );
    assert_eq!(
        dag.pruned_adjacent("model.p.int_b"),
        Some(&crate::PrunedAdjacent {
            parents: 0,
            children: 1
        })
    );
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
    // layer resolution is prefix-first: staging/fct_x IS a marts model
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
    // type resolution coalesces (prefix match, folder layer, 'other'):
    //  - staging/fct_x → marts type, fct_ is appropriate → NOT flagged
    //    (over-flag fix; the prefix-vs-folder mismatch is a different
    //    rule's concern — `model_directories`).
    //  - marts/plain → folder type, no prefix → flagged.
    //  - reports/x → 'other', which is prefix-free by default: unclassified
    //    models only get prefixes when the config defines an `other:` layer.
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
    // need a prefix — `rpt_` enforcement for the `other` layer.
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
    // The 43 ids are a fixed contract — config keys, --rules args and the
    // baseline format all rely on them. The original structure rules lead;
    // rules added later append in the registry's order so the emitted
    // sequence stays stable.
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
            "unused_models",
            "references_deprecated_model",
            "undeclared_warehouse_columns",
            "trailing_semicolon",
            "duplicate_output_columns",
            "unused_cte",
            "order_by_without_limit",
            "undocumented_exposures",
            "model_too_long",
            "layer_materializations",
        ]
    );
    // Only rules that represent a broken guarantee or a build failure
    // default to error — everything else reports at warn so it never
    // silently gates a project that didn't opt in.
    let error_by_default = [
        "missing_primary_key_tests",
        "public_models_without_contract",
        "unknown_column_reference",
        "trailing_semicolon",
        "duplicate_output_columns",
    ];
    // `requires` is a fixed contract too — only the SQL-scanning and
    // catalog-reading rules ask for more than the graph (`trailing_semicolon`
    // reads `raw_code`, which the graph already carries).
    let sql_rules = [
        "select_star",
        "hard_coded_references",
        "cartesian_join",
        "unknown_column_reference",
        "forbidden_patterns",
        "duplicate_output_columns",
        "unused_cte",
        "order_by_without_limit",
    ];
    let catalog_rules = [
        "warehouse_type_drift",
        "columns_missing_in_warehouse",
        "undeclared_warehouse_columns",
    ];
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
                ParamKind::SelectorList { default } => {
                    assert_eq!(
                        LintConfig::default().str_list_param(row.rule, p.name),
                        Some(default.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
                        "{}.{}",
                        row.id,
                        p.name
                    );
                    for d in default {
                        assert!(
                            crate::select::validate_selector(d).is_ok(),
                            "{}.{}'s default '{d}' must parse as a selector",
                            row.id,
                            p.name
                        );
                    }
                }
                ParamKind::LayerList { default, builtins } => {
                    assert_eq!(
                        LintConfig::default().str_list_param(row.rule, p.name),
                        Some(default.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
                        "{}.{}",
                        row.id,
                        p.name
                    );
                    assert!(
                        default.iter().all(|d| builtins.contains(d)),
                        "{}.{}'s default sits outside its built-in layer set",
                        row.id,
                        p.name
                    );
                }
                ParamKind::LayerMap { default, builtins } => {
                    assert_eq!(
                        LintConfig::default().map_param(row.rule, p.name),
                        Some(
                            default
                                .iter()
                                .map(|(l, vs)| {
                                    (
                                        l.to_string(),
                                        vs.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                    )
                                })
                                .collect()
                        ),
                        "{}.{}",
                        row.id,
                        p.name
                    );
                    assert!(
                        default.iter().all(|(l, _)| builtins.contains(l)),
                        "{}.{}'s default names a layer outside its built-in set",
                        row.id,
                        p.name
                    );
                }
                ParamKind::Bool { default } => {
                    assert_eq!(
                        LintConfig::default().bool_param(row.rule, p.name),
                        Some(default),
                        "{}.{}",
                        row.id,
                        p.name
                    );
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
    assert_eq!(g.severity, Severity::Warn);
    assert_eq!(
        g.location,
        Some(Location::new("models/staging/stg_root.sql")),
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
        LintRule::MissingPrimaryKeyTests,
        "model.p.x",
        vec![],
        "x has no primary key test",
    );
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["rule"], "missing_primary_key_tests");
    assert_eq!(v["severity"], "error");
    assert_eq!(v["baselined"], false);
    assert!(
        v.get("location").is_none(),
        "location is omitted from JSON when absent"
    );
    let mut f2 = f.clone();
    f2.location = Some(Location::new("models/x.sql"));
    let v2 = serde_json::to_value(&f2).unwrap();
    assert_eq!(v2["location"]["file"], "models/x.sql");
    assert!(v2["location"].get("line").is_none());
}

#[test]
fn finding_json_carries_position_labels_help_and_docs() {
    // A fully positioned finding: the location's span fields, the primary
    // label, secondary labels, the EFFECTIVE help (the rule's own here)
    // and the per-rule docs anchor all serialize.
    let f = Finding::new(
        LintRule::SelectStar,
        "model.p.x",
        vec![],
        "x's final SELECT uses select * / t.*",
    )
    .with_span("models/x.sql", 3, 8, 3, 17)
    .with_label("select *")
    .with_labels(vec![
        Label {
            line: 9,
            column: None,
            end_line: None,
            end_column: None,
            message: "another * here".into(),
        },
        Label {
            line: 5,
            column: Some(12),
            end_line: Some(5),
            end_column: Some(15),
            message: "t.* here".into(),
        },
    ])
    .with_help("project the columns explicitly");
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(
        v["location"],
        serde_json::json!({
            "file": "models/x.sql",
            "line": 3,
            "column": 8,
            "end_line": 3,
            "end_column": 17,
        }),
        "the full span serializes"
    );
    assert_eq!(v["label"], "select *");
    // with_labels sorted the secondary positions by (line, column).
    assert_eq!(
        v["labels"],
        serde_json::json!([
            {"line": 5, "column": 12, "end_line": 5, "end_column": 15, "message": "t.* here"},
            {"line": 9, "message": "another * here"},
        ])
    );
    assert_eq!(v["help"], "project the columns explicitly");
    assert_eq!(
        v["docs_url"],
        "https://hytfjwr.github.io/dbtl/reference/lint-rules/#select_star"
    );
    // Existing fields keep their slots; absent metadata stays absent.
    let bare = serde_json::to_value(Finding::new(LintRule::RootModels, "m", vec![], "x")).unwrap();
    assert!(bare.get("label").is_none() && bare.get("labels").is_none());
    assert_eq!(
        bare["help"],
        LintRule::RootModels.info().help,
        "no finding help → the registry default serializes"
    );
}

#[test]
fn effective_help_prefers_the_finding_then_the_registry() {
    let bare = Finding::new(LintRule::RootModels, "m", vec![], "x");
    assert_eq!(bare.effective_help(), LintRule::RootModels.info().help);
    let f = bare.with_help("do this instead");
    assert_eq!(f.effective_help(), "do this instead");
}

#[test]
fn every_rule_docs_url_anchors_at_its_own_id() {
    // The reference doc's headings are `## `<id>`` — github-slugger (what
    // the site builds anchors with) strips the backticks and keeps
    // word characters, so a snake_case id slugs to itself. Any id that
    // wouldn't (uppercase, spaces, punctuation) breaks the #<id> links.
    for rule in LintRule::ALL {
        let id = rule.name();
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{id} must slug to itself for the docs anchor"
        );
        assert_eq!(
            rule.docs_url(),
            format!("{}#{id}", LINT_RULE_DOCS_BASE),
            "{id}'s anchor is the id itself"
        );
    }
}

// ---- per-rule excludes -------------------------------------------------------

#[test]
fn exclude_unions_glob_path_tag_and_exact_names() {
    // root_models flags every parentless model — five of them here.
    let mut tagged = model("stg_tagged", "staging/stg_tagged.sql");
    tagged.1.tags = vec!["wip".to_string()];
    let mut kept = model("plain_kept", "marts/plain_kept.sql");
    kept.1.original_file_path = Some("models/marts/plain_kept.sql".into());
    let m = manifest(
        vec![
            model("stg_legacy_a", "staging/stg_legacy_a.sql"),
            tagged,
            model("stg_exact", "staging/stg_exact.sql"),
            kept,
            model("fct_kept", "marts/fct_kept.sql"),
        ],
        vec![],
        &[],
    );
    let cfg = config_from(
        "lint:\n  rules:\n    root_models:\n      exclude: [\"stg_legacy_*\", \"stg_exact\", \"path:models/marts/plain*\", \"tag:wip\"]\n",
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
fn exclude_with_a_legacy_path_spec_surfaces_a_deprecation_warning() {
    // The resource-relative `path:` form still suppresses (compat) but the
    // outcome carries the one-line deprecation note for the caller's
    // warning channel — once, however many findings it suppressed.
    let mut a = model("stg_a", "staging/stg_a.sql");
    a.1.original_file_path = Some("models/staging/stg_a.sql".into());
    let mut b = model("stg_b", "staging/stg_b.sql");
    b.1.original_file_path = Some("models/staging/stg_b.sql".into());
    let m = manifest(vec![a, b], vec![], &[]);
    let cfg = config_from("lint:\n  rules:\n    root_models:\n      exclude: [\"path:staging\"]\n");
    let dag = Dag::build(&m);
    let out = run_rules(LintInput::new(&dag, &cfg));
    assert!(
        flagged(&out.findings, LintRule::RootModels).is_empty(),
        "the legacy form still excludes"
    );
    assert_eq!(
        out.warnings,
        vec![
            "selector 'path:staging' uses the deprecated resource-relative \
             path form — write it project-relative instead: 'path:models/staging'"
                .to_string()
        ],
        "one deduped warning for the legacy spec"
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

#[test]
fn malformed_meta_dbtl_ignore_warns_once_per_node() {
    // Every malformed `meta.dbtl.ignore` form — a scalar that isn't "all",
    // a non-list value, a list with non-string items, unknown rule ids —
    // lands ONE load warning per node (not per finding, not per id), while
    // the value still suppresses nothing. The warnings ride the same
    // loader channel every surface drains.
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
            mk("stg_a", serde_json::json!({"dbtl": {"ignore": "not_all"}})),
            mk(
                "stg_b",
                serde_json::json!({"dbtl": {"ignore": {"root_models": true}}}),
            ),
            mk(
                "stg_c",
                serde_json::json!({"dbtl": {"ignore": [42, "root_models"]}}),
            ),
            mk(
                "stg_d",
                serde_json::json!({"dbtl": {"ignore": ["bogus_rule", "root_models"]}}),
            ),
            // Well-formed: "all", a known-id list, and a `dbtl` block with
            // no `ignore` key — none of these warn.
            mk("stg_e", serde_json::json!({"dbtl": {"ignore": "all"}})),
            mk(
                "stg_f",
                serde_json::json!({"dbtl": {"ignore": ["root_models"]}}),
            ),
            mk("stg_g", serde_json::json!({"dbtl": {"docs": "hi"}})),
            // `meta.dbtl` written as a JSON-encoded string: a malformed
            // `ignore` inside still warns (the suppression reader
            // re-parses the same stringified value) — and a plain
            // non-JSON string carries no `ignore` at all.
            mk(
                "stg_h",
                serde_json::json!({"dbtl": "{\"ignore\": {\"x\": 1}}"}),
            ),
            mk("stg_i", serde_json::json!({"dbtl": "just a note"})),
        ],
        vec![(
            "source.p.raw.t".into(),
            RawSource {
                name: "raw".into(),
                resource_type: "source".into(),
                meta: serde_json::json!({"dbtl": {"ignore": 7}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                ..Default::default()
            },
        )],
        &[],
    );
    // Manifest mode: the load warnings `load_dag_with_warnings` emits.
    let (_dag, warnings) = crate::dag_from_manifest(&m);
    let meta_warnings: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("meta.dbtl.ignore"))
        .collect();
    assert_eq!(
        meta_warnings.len(),
        6,
        "one warning per malformed node+source, none for the valid ones: {meta_warnings:?}"
    );
    for (uid, what) in [
        ("model.p.stg_a", "must be"),
        ("model.p.stg_b", "must be"),
        ("model.p.stg_c", "must be"),
        ("model.p.stg_d", "unknown lint rule id(s): bogus_rule"),
        ("model.p.stg_h", "must be"),
        ("source.p.raw.t", "must be"),
    ] {
        assert_eq!(
            meta_warnings
                .iter()
                .filter(|w| w.starts_with(&format!("{uid}:")) && w.contains(what))
                .count(),
            1,
            "{uid} warns exactly once ({what}): {meta_warnings:?}"
        );
    }
    // …and the malformed values still suppress NOTHING — the warnings
    // aren't a substitute for enforcement.
    let dag = Dag::build(&m);
    let f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let roots = flagged(&f, LintRule::RootModels);
    for uid in ["model.p.stg_a", "model.p.stg_b", "model.p.stg_c"] {
        assert!(
            roots.contains(&uid),
            "{uid}'s malformed ignore suppresses nothing"
        );
    }
    // stg_d's VALID entries still apply — `root_models` was a known id in
    // a list that also named an unknown one.
    assert!(
        !roots.contains(&"model.p.stg_d"),
        "known ids in a partly-unknown list still suppress"
    );
}

#[test]
fn malformed_meta_dbtl_ignore_warns_in_source_mode() {
    // The same warning through the source loader — `+meta:` on a model
    // folder lands on the synthesized manifest and reports identically.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models/stg")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nmodels:\n  p:\n    stg:\n      +meta:\n        dbtl:\n          ignore: [bogus_rule]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("models/stg/stg_a.sql"), "select 1").unwrap();
    let (_manifest, warnings) =
        crate::manifest_from_source_with_warnings(dir.path(), None).unwrap();
    let meta_warnings: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("meta.dbtl.ignore"))
        .collect();
    assert_eq!(
        meta_warnings.len(),
        1,
        "one warning for the malformed node: {meta_warnings:?}"
    );
    assert!(
        meta_warnings[0].contains("model.p.stg_a") && meta_warnings[0].contains("bogus_rule"),
        "{}",
        meta_warnings[0]
    );
}

// ---- suppression reasons -------------------------------------------------------

/// The suppressed findings of a run, keyed `(node_uid, rule)` → the
/// recorded `(by, reason)` — the shape the report's rollup reads.
fn suppressed_of(
    m: &RawManifest,
    config: &LintConfig,
) -> std::collections::BTreeMap<(String, LintRule), (SuppressionSource, Option<String>)> {
    run_rules(LintInput::new(&Dag::build(m), config))
        .suppressed
        .into_iter()
        .map(|f| {
            let s = f
                .suppression
                .expect("a suppressed finding carries its record");
            ((f.node_uid, f.rule), (s.by, s.reason))
        })
        .collect()
}

#[test]
fn meta_dbtl_ignore_map_form_suppresses_with_reasons() {
    // The `{<rule id>|all: <reason>}` map form: a rule-named key suppresses
    // that rule with its recorded reason, `all` covers every rule, and a
    // `~` value records no reason of its own.
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
                serde_json::json!({"dbtl": {"ignore": {"root_models": "no parent on purpose"}}}),
            ),
            mk(
                "stg_b",
                serde_json::json!({"dbtl": {"ignore": {"all": "legacy staging"}}}),
            ),
            mk(
                "stg_c",
                serde_json::json!({"dbtl": {"ignore": {"root_models": null}}}),
            ),
        ],
        vec![],
        &[],
    );
    let outcome = run_rules(LintInput::new(&Dag::build(&m), &LintConfig::default()));
    let suppressed = suppressed_of(&m, &LintConfig::default());
    // stg_a: root_models suppressed with its reason; the other rules on it
    // still report.
    assert_eq!(
        suppressed.get(&("model.p.stg_a".to_string(), LintRule::RootModels)),
        Some(&(
            SuppressionSource::Meta,
            Some("no parent on purpose".to_string())
        ))
    );
    assert!(flagged(&outcome.findings, LintRule::UndocumentedModels).contains(&"model.p.stg_a"));
    // stg_b: `all` covers every rule with the one reason.
    assert_eq!(
        suppressed.get(&("model.p.stg_b".to_string(), LintRule::RootModels)),
        Some(&(SuppressionSource::Meta, Some("legacy staging".to_string())))
    );
    assert_eq!(
        suppressed.get(&("model.p.stg_b".to_string(), LintRule::UndocumentedModels)),
        Some(&(SuppressionSource::Meta, Some("legacy staging".to_string())))
    );
    // stg_c: `~` records no reason — the finding still suppressed.
    assert_eq!(
        suppressed.get(&("model.p.stg_c".to_string(), LintRule::RootModels)),
        Some(&(SuppressionSource::Meta, None))
    );
    // …and the suppressed set is OUT of `findings` — it never gates.
    assert!(
        !flagged(&outcome.findings, LintRule::RootModels)
            .iter()
            .any(|u| u.starts_with("model.p.stg_")),
        "suppressed findings never join the counted set"
    );
}

#[test]
fn meta_dbtl_reason_is_the_fallback_for_every_shape() {
    // `meta.dbtl.reason` fills in where the `ignore` value records no
    // reason of its own: the list form, `"all"`, and `~` map values all
    // inherit it; a map value's own reason wins.
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
                "stg_list",
                serde_json::json!({"dbtl": {"ignore": ["root_models"], "reason": "shared list reason"}}),
            ),
            mk(
                "stg_all",
                serde_json::json!({"dbtl": {"ignore": "all", "reason": "shared all reason"}}),
            ),
            mk(
                "stg_null",
                serde_json::json!({"dbtl": {"ignore": {"root_models": null}, "reason": "map fallback"}}),
            ),
            mk(
                "stg_own",
                serde_json::json!({"dbtl": {"ignore": {"root_models": "own"}, "reason": "map fallback"}}),
            ),
            // A rule-named key beats `all` for that rule's reason.
            mk(
                "stg_both",
                serde_json::json!({"dbtl": {"ignore": {"root_models": "specific", "all": "generic"}}}),
            ),
        ],
        vec![],
        &[],
    );
    let suppressed = suppressed_of(&m, &LintConfig::default());
    let at = |uid: &str, rule: LintRule| suppressed.get(&(uid.to_string(), rule)).cloned();
    assert_eq!(
        at("model.p.stg_list", LintRule::RootModels),
        Some((
            SuppressionSource::Meta,
            Some("shared list reason".to_string())
        ))
    );
    assert_eq!(
        at("model.p.stg_all", LintRule::UndocumentedModels),
        Some((
            SuppressionSource::Meta,
            Some("shared all reason".to_string())
        ))
    );
    assert_eq!(
        at("model.p.stg_null", LintRule::RootModels),
        Some((SuppressionSource::Meta, Some("map fallback".to_string())))
    );
    assert_eq!(
        at("model.p.stg_own", LintRule::RootModels),
        Some((SuppressionSource::Meta, Some("own".to_string())))
    );
    assert_eq!(
        at("model.p.stg_both", LintRule::RootModels),
        Some((SuppressionSource::Meta, Some("specific".to_string())))
    );
    assert_eq!(
        at("model.p.stg_both", LintRule::UndocumentedModels),
        Some((SuppressionSource::Meta, Some("generic".to_string())))
    );
}

#[test]
fn exclude_object_form_suppresses_with_reasons() {
    // `.dbtl.yml`'s `exclude: [{select, reason}]`: bare strings suppress
    // reasonless, objects carry theirs, and the FIRST matching spec's
    // reason records. A `meta.dbtl.ignore` on the node wins the `by`
    // attribution over any exclude spec.
    let cfg = config_from(
        "lint:\n  rules:\n    root_models:\n      exclude:\n        - stg_a\n        - {select: stg_*, reason: staging roots}\n        - {select: \"*\", reason: everything}\n    undocumented_models:\n      exclude:\n        - {select: stg_*, reason: staging undocumented}\n",
    );
    let mut meta_node = model("stg_meta", "staging/stg_meta.sql");
    meta_node.1.meta =
        serde_json::json!({"dbtl": {"ignore": {"undocumented_models": "node reason"}}})
            .as_object()
            .cloned()
            .unwrap_or_default();
    let m = manifest(
        vec![model("stg_a", "staging/stg_a.sql"), meta_node],
        vec![],
        &[],
    );
    let outcome = run_rules(LintInput::new(&Dag::build(&m), &cfg));
    let suppressed = suppressed_of(&m, &cfg);
    // `stg_a` matches the bare string first — reasonless despite a later
    // object form also matching (`stg_*`).
    assert_eq!(
        suppressed.get(&("model.p.stg_a".to_string(), LintRule::RootModels)),
        Some(&(SuppressionSource::Exclude, None))
    );
    // `stg_meta` matches `stg_*` (the bare `stg_a` doesn't cover it).
    assert_eq!(
        suppressed.get(&("model.p.stg_meta".to_string(), LintRule::RootModels)),
        Some(&(
            SuppressionSource::Exclude,
            Some("staging roots".to_string())
        ))
    );
    // meta.dbtl.ignore wins over an exclude spec on the same finding.
    assert_eq!(
        suppressed.get(&("model.p.stg_meta".to_string(), LintRule::UndocumentedModels)),
        Some(&(SuppressionSource::Meta, Some("node reason".to_string())))
    );
    // …and the suppressed findings never joined the counted set.
    assert!(
        flagged(&outcome.findings, LintRule::RootModels).is_empty(),
        "every root_models finding suppressed"
    );
    assert!(
        !flagged(&outcome.findings, LintRule::UndocumentedModels)
            .iter()
            .any(|u| u.starts_with("model.p.stg_")),
        "every undocumented_models finding suppressed"
    );
}

#[test]
fn meta_dbtl_ignore_map_warns_on_unknown_ids_and_bad_values() {
    // The map form is VALID — `{root_models: …}` suppresses and never
    // warns. An unknown map key warns like an unknown list id (valid keys
    // still apply); a non-string non-`~` map value is malformed and
    // suppresses nothing.
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
                "stg_ok",
                serde_json::json!({"dbtl": {"ignore": {"root_models": "why"}}}),
            ),
            mk(
                "stg_unknown",
                serde_json::json!({"dbtl": {"ignore": {"bogus_rule": "x", "root_models": "y"}}}),
            ),
            mk(
                "stg_bad",
                serde_json::json!({"dbtl": {"ignore": {"root_models": 7}}}),
            ),
        ],
        vec![],
        &[],
    );
    let (_dag, warnings) = crate::dag_from_manifest(&m);
    let meta_warnings: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("meta.dbtl.ignore"))
        .collect();
    assert_eq!(
        meta_warnings.len(),
        2,
        "the valid map never warns; the unknown key and bad value do: {meta_warnings:?}"
    );
    assert!(
        meta_warnings
            .iter()
            .any(|w| w.starts_with("model.p.stg_unknown:") && w.contains("bogus_rule")),
        "unknown map key names the id: {meta_warnings:?}"
    );
    assert!(
        meta_warnings
            .iter()
            .any(|w| w.starts_with("model.p.stg_bad:") && w.contains("must be")),
        "a non-string map value is malformed: {meta_warnings:?}"
    );
    let suppressed = suppressed_of(&m, &LintConfig::default());
    assert_eq!(
        suppressed.get(&("model.p.stg_ok".to_string(), LintRule::RootModels)),
        Some(&(SuppressionSource::Meta, Some("why".to_string())))
    );
    // The valid key inside a partly-unknown map still applies.
    assert_eq!(
        suppressed.get(&("model.p.stg_unknown".to_string(), LintRule::RootModels)),
        Some(&(SuppressionSource::Meta, Some("y".to_string())))
    );
    // The malformed value suppresses nothing.
    let dag = Dag::build(&m);
    assert!(
        flagged(
            &run_rules(LintInput::new(&dag, &LintConfig::default())).findings,
            LintRule::RootModels
        )
        .contains(&"model.p.stg_bad"),
        "a malformed map value suppresses nothing"
    );
    // An empty `ignore: {}` map is malformed — it suppresses nothing and
    // warns like every other unrecognized shape.
    let m = manifest(
        vec![mk("stg_empty", serde_json::json!({"dbtl": {"ignore": {}}}))],
        vec![],
        &[],
    );
    let (_dag, warnings) = crate::dag_from_manifest(&m);
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("model.p.stg_empty:") && w.contains("meta.dbtl.ignore")),
        "empty ignore map warns: {warnings:?}"
    );
}

#[test]
fn meta_dbtl_reason_non_scalar_warns() {
    // A map/list `meta.dbtl.reason` drops silently in `meta_dbtl` — warn
    // at load like a malformed `ignore`, and an empty string reads as no
    // reason at all (never a dangling `suppressed:` marker).
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
                "stg_badreason",
                serde_json::json!({"dbtl": {"ignore": "all", "reason": {"ticket": 1}}}),
            ),
            mk(
                "stg_emptyreason",
                serde_json::json!({"dbtl": {"ignore": "all", "reason": ""}}),
            ),
        ],
        vec![],
        &[],
    );
    let (_dag, warnings) = crate::dag_from_manifest(&m);
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("model.p.stg_badreason:") && w.contains("meta.dbtl.reason")),
        "non-scalar reason warns: {warnings:?}"
    );
    let suppressed = suppressed_of(&m, &LintConfig::default());
    assert_eq!(
        suppressed.get(&("model.p.stg_emptyreason".to_string(), LintRule::RootModels)),
        Some(&(SuppressionSource::Meta, None)),
        "an empty reason reads as none"
    );
}

#[test]
fn meta_dbtl_reason_is_a_stringifyable_scalar_in_both_modes() {
    // `meta.dbtl.reason` survives the meta normalization — the same
    // `{ignore, reason}` JSON object lands on NodeDetail whether the
    // manifest or the source loader produced it.
    let mut node = model("stg_x", "staging/stg_x.sql");
    node.1.meta =
        serde_json::json!({"dbtl": {"ignore": {"root_models": "per-rule"}, "reason": "shared"}})
            .as_object()
            .cloned()
            .unwrap_or_default();
    let dag = Dag::build(&manifest(vec![node], vec![], &[]));
    assert_eq!(
        dag.detail("model.p.stg_x").unwrap().meta["dbtl"],
        r#"{"ignore":{"root_models":"per-rule"},"reason":"shared"}"#
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nmodels:\n  p:\n    +meta:\n      dbtl:\n        ignore: {root_models: per-rule}\n        reason: shared\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("models/stg_y.sql"), "select 1").unwrap();
    let sm = crate::manifest_from_source(dir.path()).unwrap();
    let sdag = Dag::build(&sm);
    assert_eq!(
        sdag.detail("model.p.stg_y").unwrap().meta["dbtl"],
        r#"{"ignore":{"root_models":"per-rule"},"reason":"shared"}"#
    );
    // …and suppression + reason fire identically under source mode.
    let suppressed = run_rules(LintInput::new(&sdag, &LintConfig::default()))
        .suppressed
        .into_iter()
        .find(|f| f.node_uid == "model.p.stg_y" && f.rule == LintRule::RootModels)
        .expect("the suppressed root_models finding");
    let s = suppressed.suppression.expect("the record");
    assert_eq!(s.by, SuppressionSource::Meta);
    assert_eq!(s.reason.as_deref(), Some("per-rule"));
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
    // `layer_violation` findings key on (rule, child, related): same node,
    // different related set → NOT baselined — the violation IS the edge.
    let edge = Finding::new(
        LintRule::LayerViolation,
        "model.p.b",
        vec!["model.p.a".into()],
        "",
    );
    let baseline = Baseline::from_findings(std::slice::from_ref(&edge));
    let mut got = vec![
        Finding::new(
            LintRule::LayerViolation,
            "model.p.b",
            vec!["model.p.x".into()],
            "",
        ),
        edge.clone(),
    ];
    baseline.apply(&mut got);
    assert!(!got[0].baselined, "a different edge is a different finding");
    assert!(got[1].baselined);
    // `rejoining_upstream` — and every non-edge rule — keys on
    // (rule, node) only: `related` is context, so a rejoin that gained
    // another bypassing parent stays the same frozen finding.
    let rejoin = Finding::new(
        LintRule::RejoiningUpstream,
        "model.p.b",
        vec!["model.p.c".into()],
        "",
    );
    let nb = Baseline::from_findings(&[rejoin]);
    let mut nf = vec![Finding::new(
        LintRule::RejoiningUpstream,
        "model.p.b",
        vec!["model.p.a".into(), "model.p.c".into()],
        "",
    )];
    nb.apply(&mut nf);
    assert!(
        nf[0].baselined,
        "a changed related set is still the same rejoin finding"
    );
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
    {"rule": "layer_violation", "node": "model.p.b",
     "related": ["model.p.c", "model.p.a", "model.p.c"]}
  ]
}"#;
    let baseline = Baseline::from_json(text).unwrap();
    assert_eq!(baseline.len(), 1, "the duplicate collapses");
    let canonical = Baseline::from_findings(&[Finding::new(
        LintRule::LayerViolation,
        "model.p.b",
        vec!["model.p.a".into(), "model.p.c".into()],
        "",
    )]);
    assert_eq!(baseline.to_json(), canonical.to_json());
    let mut got = vec![Finding::new(
        LintRule::LayerViolation,
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
fn baseline_accepts_the_old_rejoining_upstream_key() {
    // A `rejoining_upstream` entry written before the rule went node-keyed
    // carried the edge's related set — the finding it froze is the same
    // finding, so the old row must still match (its `related` normalizes
    // to `[]` at parse) while a rewrite emits the new shape.
    let text = r#"{
  "schema": "dbtl.lint-baseline/v1",
  "findings": [
    {"rule": "rejoining_upstream", "node": "model.p.b",
     "related": ["model.p.c", "model.p.a"]}
  ]
}"#;
    let baseline = Baseline::from_json(text).unwrap();
    let mut got = vec![
        Finding::new(
            LintRule::RejoiningUpstream,
            "model.p.b",
            // A DIFFERENT related set — under the old key this finding
            // would have read as new; under the node key it still matches.
            vec!["model.p.a".into(), "model.p.c".into(), "model.p.x".into()],
            "",
        ),
        Finding::new(
            LintRule::RejoiningUpstream,
            "model.p.other",
            vec!["model.p.c".into()],
            "",
        ),
    ];
    baseline.apply(&mut got);
    assert!(
        got[0].baselined,
        "the old related-bearing row still matches"
    );
    assert!(!got[1].baselined, "a different node is a different finding");
    // Serializing the accepted old row emits the new node-keyed shape.
    let canonical = Baseline::from_findings(std::slice::from_ref(&got[0]));
    assert_eq!(baseline.to_json(), canonical.to_json());
    assert!(canonical.to_json().contains(r#""related": []"#));
}

#[test]
fn baseline_subset_rewrite_keeps_the_rules_that_did_not_run() {
    // `--write-baseline --rules X` rewrites X's rows but must carry the
    // rest: dropping them would silently un-freeze violations the run
    // never evaluated.
    let ran: std::collections::BTreeSet<LintRule> =
        [LintRule::RejoiningUpstream].into_iter().collect();
    let old = r#"{
  "schema": "dbtl.lint-baseline/v1",
  "findings": [
    {"rule": "rejoining_upstream", "node": "model.p.b", "related": ["model.p.c"]},
    {"rule": "root_models", "node": "model.p.lonely", "related": []},
    {"rule": "cartesian_join", "node": "model.p.x", "related": []},
    {"rule": "hand_written_rule", "node": "model.p.y", "related": []}
  ]
}"#;
    let mut baseline = Baseline::from_json(old).unwrap();
    // The subset run froze just the one rejoin finding it evaluated
    // (rewritten under the new node key).
    let fresh = Baseline::from_findings(&[Finding::new(
        LintRule::RejoiningUpstream,
        "model.p.b",
        vec!["model.p.a".into(), "model.p.c".into()],
        "",
    )]);
    baseline.replace_entries_for(&ran, fresh);
    let written = baseline.to_json();
    assert!(
        written.contains(r#""rule": "rejoining_upstream""#)
            && written.contains(r#""node": "model.p.b""#),
        "the ran rule's row is rewritten: {written}"
    );
    assert!(
        written.contains(r#""related": []"#),
        "the rewrite emits the node key: {written}"
    );
    for kept in ["root_models", "cartesian_join", "hand_written_rule"] {
        assert!(
            written.contains(&format!(r#""rule": "{kept}""#)),
            "{kept} survives the subset rewrite: {written}"
        );
    }
    assert_eq!(baseline.len(), 4, "no row is lost or duplicated");
    // A rule that ran freezes nothing → its old row goes away (the
    // violation is fixed — rewriting it back would re-freeze a ghost).
    let ran: std::collections::BTreeSet<LintRule> = [LintRule::RootModels].into_iter().collect();
    baseline.replace_entries_for(&ran, Baseline::default());
    assert!(
        !baseline.to_json().contains("root_models"),
        "a rule that ran clean drops its frozen rows"
    );
    assert_eq!(baseline.len(), 3);
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

// ---- unused_models -----------------------------------------------------------

/// A `semantic_models` record — extend `RawManifest::semantic_models` with
/// it; `depends_on` names the model uid it consumes (the wire shape).
fn semantic_model(name: &str, on: &str) -> (String, crate::manifest::RawSemanticResource) {
    (
        format!("semantic_model.p.{name}"),
        crate::manifest::RawSemanticResource {
            name: name.into(),
            depends_on: crate::manifest::RawDependsOn {
                nodes: vec![on.into()],
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

#[test]
fn unused_models_flags_layered_models_nothing_reads() {
    // Every kind of downstream counts as a use: a model child, an
    // exposure, a `semantic_models` consumer. marts and unlayered models
    // aren't checked by default (marts are the DAG's endpoints by
    // design), and non-model resources never apply.
    let mut m = manifest(
        vec![
            model("stg_dead", "staging/stg_dead.sql"),
            model("int_dead", "intermediate/int_dead.sql"),
            model("fct_dead", "marts/fct_dead.sql"),
            model("other_dead", "ops/other_dead.sql"),
            model("stg_live", "staging/stg_live.sql"),
            model("stg_exposed", "staging/stg_exposed.sql"),
            model("stg_semantic", "staging/stg_semantic.sql"),
            model("fct_a", "marts/fct_a.sql"),
            (
                "seed.p.raw_seed".to_string(),
                RawNode {
                    name: "raw_seed".into(),
                    resource_type: "seed".into(),
                    path: Some("seeds/raw_seed.csv".into()),
                    ..Default::default()
                },
            ),
        ],
        vec![],
        &[
            ("model.p.stg_live", "model.p.fct_a"),
            ("model.p.stg_exposed", "exposure.p.exp"),
        ],
    );
    m.exposures.extend([exposure("exp")]);
    m.semantic_models
        .extend([semantic_model("sm_orders", "model.p.stg_semantic")]);
    let dag = Dag::build(&m);
    // The semantic-model consumer records BOTH the edge and the detail
    // mark — the mark is the channel source mode fills without the node.
    assert_eq!(
        dag.detail("model.p.stg_semantic")
            .unwrap()
            .semantic_dependents,
        vec!["sm_orders".to_string()]
    );
    let cfg = cfg_only(&[LintRule::UnusedModels]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedModels),
        vec!["model.p.int_dead", "model.p.stg_dead"],
        "every downstream kind is a use; marts/other layers aren't checked"
    );
    assert_eq!(
        finding_on(&out.findings, LintRule::UnusedModels, "model.p.stg_dead").message,
        "staging model stg_dead has no downstream"
    );
}

#[test]
fn unused_models_layers_param_retargets_the_check() {
    // `layers: [marts]` drops the default staging/intermediate check and
    // sweeps marts instead — fct_dead flags, stg_dead stops.
    let m = manifest(
        vec![
            model("stg_dead", "staging/stg_dead.sql"),
            model("fct_dead", "marts/fct_dead.sql"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let mut cfg = config_from("lint:\n  rules:\n    unused_models:\n      layers: [marts]\n");
    for r in LintRule::ALL {
        if r != LintRule::UnusedModels {
            cfg.rules.insert(r, RuleEnabled::Off);
        }
    }
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedModels),
        vec!["model.p.fct_dead"]
    );
}

#[test]
fn unused_models_counts_a_package_only_child_as_a_use() {
    // `stg_pkg_read`'s only consumer is an installed-package model: after
    // the Root prune it reads childless, but the PrunedAdjacent record
    // says otherwise — it is used, just not visibly. `stg_dead` isn't.
    let mut pkg = model("pkg_model", "dep/pkg_model.sql");
    pkg.0 = "model.dep.pkg_model".into();
    let mut m = manifest(
        vec![
            pkg,
            model("stg_pkg_read", "staging/stg_pkg_read.sql"),
            model("stg_dead", "staging/stg_dead.sql"),
        ],
        vec![],
        &[("model.p.stg_pkg_read", "model.dep.pkg_model")],
    );
    multi_package(&mut m);
    let (dag, _) = crate::PackageScope::Root.apply_with_warnings(Dag::build(&m));
    let cfg = cfg_only(&[LintRule::UnusedModels]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedModels),
        vec!["model.p.stg_dead"],
        "a pruned package consumer is a use — just not a visible one"
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

#[test]
fn too_many_joins_folds_versioned_parents_into_one_resource() {
    // fct_join reads dim.v1 + dim.v2 + int_a: three parent nodes, two
    // data-resource FAMILIES — joining two spellings of `dim` is still
    // reading `dim`. At threshold 3 the naive count flags, the fold must not.
    let m = manifest(
        vec![
            model("fct_join", "marts/fct_join.sql"),
            versioned_model("dim", "v1", "marts/dim_v1.sql"),
            versioned_model("dim", "v2", "marts/dim_v2.sql"),
            model("int_a", "intermediate/int_a.sql"),
        ],
        vec![],
        &[
            ("model.p.dim.v1", "model.p.fct_join"),
            ("model.p.dim.v2", "model.p.fct_join"),
            ("model.p.int_a", "model.p.fct_join"),
        ],
    );
    let cfg = config_from("lint:\n  rules:\n    too_many_joins: {threshold: 3}\n");
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::TooManyJoins).is_empty(),
        "v1+v2 of one model count once — 2 families < 3"
    );

    // Control: a third DISTINCT parent crosses it — the message counts 3
    // families and names the folded `dim` family bare once.
    let m2 = manifest(
        vec![
            model("fct_join", "marts/fct_join.sql"),
            versioned_model("dim", "v1", "marts/dim_v1.sql"),
            versioned_model("dim", "v2", "marts/dim_v2.sql"),
            model("int_a", "intermediate/int_a.sql"),
            model("int_b", "intermediate/int_b.sql"),
        ],
        vec![],
        &[
            ("model.p.dim.v1", "model.p.fct_join"),
            ("model.p.dim.v2", "model.p.fct_join"),
            ("model.p.int_a", "model.p.fct_join"),
            ("model.p.int_b", "model.p.fct_join"),
        ],
    );
    let f2 = findings_of(&m2, &cfg);
    let hits: Vec<&Finding> = f2
        .iter()
        .filter(|f| f.rule == LintRule::TooManyJoins)
        .collect();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0]
            .message
            .contains("directly depends on 3 data resources"),
        "family count, not node count: {}",
        hits[0].message
    );
    assert!(
        hits[0].message.contains("dim,") && !hits[0].message.contains("dim.v"),
        "folded family named bare once: {}",
        hits[0].message
    );
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

#[test]
fn chained_views_skips_ends_only_packages_extend() {
    // v1→…→v5 is a 5-view chain, but v5's only downstream is an
    // installed-package model — after the Root prune v5 reads as a chain
    // end, yet the pruned consumer's materialization is invisible, so the
    // end can't be established. w1→…→w5 is the same chain all
    // in-project: the control flags.
    let mut pkg = mat_model("pkg_v", "view");
    pkg.0 = "model.dep.pkg_v".into();
    let mut m = manifest(
        vec![
            pkg,
            mat_model("v1", "view"),
            mat_model("v2", "view"),
            mat_model("v3", "view"),
            mat_model("v4", "view"),
            mat_model("v5", "view"),
            mat_model("w1", "view"),
            mat_model("w2", "view"),
            mat_model("w3", "view"),
            mat_model("w4", "view"),
            mat_model("w5", "view"),
        ],
        vec![],
        &[
            ("model.p.v1", "model.p.v2"),
            ("model.p.v2", "model.p.v3"),
            ("model.p.v3", "model.p.v4"),
            ("model.p.v4", "model.p.v5"),
            ("model.p.v5", "model.dep.pkg_v"),
            ("model.p.w1", "model.p.w2"),
            ("model.p.w2", "model.p.w3"),
            ("model.p.w3", "model.p.w4"),
            ("model.p.w4", "model.p.w5"),
        ],
    );
    multi_package(&mut m);
    let (dag, _) = crate::PackageScope::Root.apply_with_warnings(Dag::build(&m));
    let cfg = cfg_only(&[LintRule::ChainedViews]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::ChainedViews),
        vec!["model.p.w5"],
        "a pruned package child could continue the chain — the end is unprovable"
    );
    assert_eq!(
        dag.pruned_adjacent("model.p.v5"),
        Some(&crate::PrunedAdjacent {
            parents: 0,
            children: 1
        })
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
    // access value (a manifest whose wire omits it — source mode fills
    // dbt's `protected` default) is "unknown" — never judged.
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
    // Each parent's declared access lands in the message.
    assert!(
        hits[0]
            .message
            .contains("dim_priv (access: private), dim_prot (access: protected)"),
        "access values named: {}",
        hits[0].message
    );
    assert!(
        hits[0].message.contains("[public]"),
        "the configured allow-list is named: {}",
        hits[0].message
    );
}

#[test]
fn exposures_dependent_on_private_models_allowed_access_retargets_the_check() {
    // `allowed_access: [public, protected]` — a project treating dbt's
    // `protected` default as intended flags only `private` parents.
    let mut priv_m = model("dim_priv", "marts/dim_priv.sql");
    priv_m.1.access = Some("private".into());
    let mut prot_m = model("dim_prot", "marts/dim_prot.sql");
    prot_m.1.access = Some("protected".into());
    let mut m = manifest(
        vec![priv_m, prot_m],
        vec![],
        &[
            ("model.p.dim_priv", "exposure.p.exp"),
            ("model.p.dim_prot", "exposure.p.exp"),
        ],
    );
    m.exposures.extend([exposure("exp")]);
    let cfg = config_from(
        "lint:\n  rules:\n    exposures_dependent_on_private_models: {allowed_access: [public, protected]}\n",
    );
    let f = findings_of(&m, &cfg);
    let hits: Vec<&Finding> = f
        .iter()
        .filter(|f| f.rule == LintRule::ExposuresDependentOnPrivateModels)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].related,
        vec!["model.p.dim_priv".to_string()],
        "protected parents pass under the widened allow-list"
    );
    assert!(
        hits[0].message.contains("[public, protected]"),
        "the configured allow-list is named: {}",
        hits[0].message
    );

    // An explicitly empty allow-list opts the judgment out entirely.
    let cfg = config_from(
        "lint:\n  rules:\n    exposures_dependent_on_private_models: {allowed_access: []}\n",
    );
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::ExposuresDependentOnPrivateModels).is_empty(),
        "allowed_access: [] disables the check"
    );
}

// ---- references_deprecated_model ----------------------------------------------

/// A model carrying `deprecation_date` (`None` = no date on the wire).
fn deprecated(name: &str, path: &str, date: Option<&str>) -> (String, RawNode) {
    let mut m = model(name, path);
    m.1.deprecation_date = date.map(Into::into);
    m
}

#[test]
fn references_deprecated_model_flags_every_dependent_kind() {
    // Model, snapshot, exposure and semantic_model dependents of dim_old
    // all flag; dim_old itself doesn't (the finding names the edge, not
    // the sunset), and dependents of the undated dim_ok or the
    // blank-dated dim_blank don't (no date declared = nothing to judge).
    let mut m = manifest(
        vec![
            deprecated("dim_old", "marts/dim_old.sql", Some("2026-12-01")),
            deprecated("dim_blank", "marts/dim_blank.sql", Some("  ")),
            model("dim_ok", "marts/dim_ok.sql"),
            model("fct_a", "marts/fct_a.sql"),
            model("fct_ok", "marts/fct_ok.sql"),
            model("fct_blank", "marts/fct_blank.sql"),
            (
                "snapshot.p.snap".to_string(),
                RawNode {
                    name: "snap".into(),
                    resource_type: "snapshot".into(),
                    path: Some("snapshots/snap.sql".into()),
                    ..Default::default()
                },
            ),
        ],
        vec![],
        &[
            ("model.p.dim_old", "model.p.fct_a"),
            ("model.p.dim_old", "snapshot.p.snap"),
            ("model.p.dim_old", "exposure.p.exp"),
            ("model.p.dim_old", "semantic_model.p.sm"),
            ("model.p.dim_ok", "model.p.fct_ok"),
            ("model.p.dim_blank", "model.p.fct_blank"),
        ],
    );
    m.exposures.extend([exposure("exp")]);
    m.semantic_models
        .extend([semantic_model("sm", "model.p.dim_old")]);
    let cfg = cfg_only(&[LintRule::ReferencesDeprecatedModel]);
    let out = lint_outcome(&Dag::build(&m), &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::ReferencesDeprecatedModel),
        vec![
            "exposure.p.exp",
            "model.p.fct_a",
            "semantic_model.p.sm",
            "snapshot.p.snap"
        ],
        "every dependent kind flags; the deprecated model and un-deprecated edges never do"
    );
    let f = finding_on(
        &out.findings,
        LintRule::ReferencesDeprecatedModel,
        "model.p.fct_a",
    );
    assert_eq!(
        f.message, "fct_a depends on dim_old (deprecated on 2026-12-01)",
        "the message names the dependent's own name, the parent's display name and the date"
    );
    assert_eq!(f.related, vec!["model.p.dim_old".to_string()]);
}

#[test]
fn references_deprecated_model_names_each_deprecated_parent() {
    // Two deprecated parents → one finding, both named with their dates
    // in sorted uid order.
    let m = manifest(
        vec![
            deprecated("dim_a", "marts/dim_a.sql", Some("2026-01-01")),
            deprecated("dim_b", "marts/dim_b.sql", Some("2026-12-01")),
            model("dim_ok", "marts/dim_ok.sql"),
            model("fct_a", "marts/fct_a.sql"),
        ],
        vec![],
        &[
            ("model.p.dim_b", "model.p.fct_a"),
            ("model.p.dim_a", "model.p.fct_a"),
            ("model.p.dim_ok", "model.p.fct_a"),
        ],
    );
    let cfg = cfg_only(&[LintRule::ReferencesDeprecatedModel]);
    let out = lint_outcome(&Dag::build(&m), &cfg, None, None);
    let f = finding_on(
        &out.findings,
        LintRule::ReferencesDeprecatedModel,
        "model.p.fct_a",
    );
    assert_eq!(
        f.message,
        "fct_a depends on dim_a (deprecated on 2026-01-01), dim_b (deprecated on 2026-12-01)"
    );
    assert_eq!(
        f.related,
        vec!["model.p.dim_a".to_string(), "model.p.dim_b".to_string()]
    );
}

#[test]
fn references_deprecated_model_respects_ref_versions() {
    // A versioned ref lands on the deprecated version; a bare ref lands
    // on the latest (v2) — same name, different node, different answer.
    let mut v1 = deprecated("dim_cust", "marts/dim_cust.sql", Some("2026-12-01"));
    v1.0 = "model.p.dim_cust.v1".into();
    v1.1.version = Some("v1".into());
    v1.1.latest_version = Some("v2".into());
    let mut v2 = model("dim_cust", "marts/dim_cust.sql");
    v2.0 = "model.p.dim_cust.v2".into();
    v2.1.version = Some("v2".into());
    v2.1.latest_version = Some("v2".into());
    let m = manifest(
        vec![
            v1,
            v2,
            model_sql_pos("fct_old", "select * from {{ ref('dim_cust', v='v1') }}"),
            model_sql_pos("fct_new", "select * from {{ ref('dim_cust') }}"),
        ],
        vec![],
        &[
            ("model.p.dim_cust.v1", "model.p.fct_old"),
            ("model.p.dim_cust.v2", "model.p.fct_new"),
        ],
    );
    let cfg = cfg_only(&[LintRule::ReferencesDeprecatedModel]);
    let out = lint_outcome(&Dag::build(&m), &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::ReferencesDeprecatedModel),
        vec!["model.p.fct_old"],
        "only the version-pinned dependent reads the deprecated node"
    );
    let f = finding_on(
        &out.findings,
        LintRule::ReferencesDeprecatedModel,
        "model.p.fct_old",
    );
    assert_eq!(
        f.message,
        "fct_old depends on dim_cust.v1 (deprecated on 2026-12-01)"
    );
}

#[test]
fn references_deprecated_model_positions_land_on_each_dep_call() {
    // Two calls to the same deprecated parent: the first is the
    // location, the rest labels — the position is the `ref()` that must
    // be repointed.
    let raw = "select *\nfrom {{ ref('dim_old') }} d\njoin {{ ref('dim_old') }} d2 on d.id = d2.id";
    let m = manifest(
        vec![
            deprecated("dim_old", "marts/dim_old.sql", Some("2026-12-01")),
            model_sql_pos("fct_a", raw),
        ],
        vec![],
        &[("model.p.dim_old", "model.p.fct_a")],
    );
    let cfg = cfg_only(&[LintRule::ReferencesDeprecatedModel]);
    let out = lint_outcome(&Dag::build(&m), &cfg, None, None);
    let f = finding_on(
        &out.findings,
        LintRule::ReferencesDeprecatedModel,
        "model.p.fct_a",
    );
    let (line, column, end_line, end_column) = needle_pos(raw, "ref('dim_old')", 0);
    assert_eq!(
        pos_of(f),
        (
            "models/fct_a.sql".to_string(),
            line,
            column,
            end_line,
            end_column
        )
    );
    assert_eq!(f.label.as_deref(), Some("dim_old is deprecated"));
    assert_eq!(f.labels.len(), 1, "the second call is a label");
    assert_eq!(f.labels[0].message, "dim_old is deprecated");
    let (l2, _, _, _) = needle_pos(raw, "ref('dim_old')", 1);
    assert_eq!(f.labels[0].line, l2);
}

#[test]
fn references_deprecated_model_scopes_the_dependent_not_the_parent() {
    // Under --include-packages the deprecated parent is dependency
    // context wherever it lives: the ROOT model refing the package's
    // deprecated model flags, the PACKAGE model refing the root's
    // deprecated model doesn't (only root-project nodes are flagged).
    let mut dep = deprecated("dep_old", "dep/dep_old.sql", Some("2026-12-01"));
    dep.0 = "model.dep.dep_old".into();
    let mut pkg_user = model("pkg_user", "dep/pkg_user.sql");
    pkg_user.0 = "model.dep.pkg_user".into();
    let mut m = manifest(
        vec![
            dep,
            pkg_user,
            deprecated("dim_old", "marts/dim_old.sql", Some("2026-12-01")),
            model("fct_a", "marts/fct_a.sql"),
        ],
        vec![],
        &[
            ("model.dep.dep_old", "model.p.fct_a"),
            ("model.p.dim_old", "model.dep.pkg_user"),
        ],
    );
    multi_package(&mut m);
    // `Dag::build` keeps every package — the include-packages graph; the
    // in_scope check alone decides who can be flagged.
    let cfg = cfg_only(&[LintRule::ReferencesDeprecatedModel]);
    let out = lint_outcome(&Dag::build(&m), &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::ReferencesDeprecatedModel),
        vec!["model.p.fct_a"],
        "the package parent is context for the root finding; the package dependent is out of scope"
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

// ---- removed rules ---------------------------------------------------------

#[test]
fn removed_rule_ids_are_reserved_and_never_resolve() {
    // A dropped id stays out of the registry and `from_name`, but is still
    // recognised by `removed_rule` so entry points can explain the removal.
    for removed in REMOVED_RULES {
        assert!(
            LintRule::from_name(removed.id).is_none(),
            "{} must not resolve to a live rule",
            removed.id
        );
        assert!(
            REGISTRY.iter().all(|row| row.id != removed.id),
            "{} must not be a registry row",
            removed.id
        );
        assert!(
            !removed.replacements.is_empty(),
            "{} needs replacement guidance",
            removed.id
        );
        for replacement in removed.replacements {
            assert!(
                LintRule::from_name(replacement).is_some(),
                "{}'s replacement {replacement} must be a live rule",
                removed.id
            );
        }
        let msg = removed_rule_error(removed.id).expect("a removed id errors");
        assert!(
            msg.contains(removed.id) && removed.replacements.iter().all(|r| msg.contains(r)),
            "the guidance names the id and every replacement: {msg}"
        );
    }
    assert!(removed_rule_error("not_a_rule").is_none());
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
                ..Default::default()
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

#[test]
fn envelope_preserves_a_rule_supplied_position() {
    // A rule that pinned the SAME file the envelope resolves keeps its
    // whole position — span, primary label, secondary labels and help.
    let m = manifest(
        vec![filed_model(
            "declared",
            "models/marts/declared.sql",
            Some("models/marts/schema.yml"),
        )],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();
    let mut f = Finding::new(
        LintRule::UndocumentedModels,
        "model.p.declared",
        vec![],
        "declared has no description",
    )
    .with_span("models/marts/schema.yml", 4, 11, 4, 19)
    .with_label("declared")
    .with_labels(vec![Label {
        line: 9,
        column: Some(15),
        end_line: Some(9),
        end_column: Some(17),
        message: "this column too".into(),
    }])
    .with_help("write the description under this entry");
    super::rules::envelope(&dag, &cfg, &mut f);
    assert_eq!(
        f.location,
        Some(Location {
            file: "models/marts/schema.yml".into(),
            line: Some(4),
            column: Some(11),
            end_line: Some(4),
            end_column: Some(19),
        }),
        "a position inside the resolved file survives the envelope"
    );
    assert_eq!(f.label.as_deref(), Some("declared"));
    assert_eq!(f.labels.len(), 1);
    assert_eq!(
        f.help.as_deref(),
        Some("write the description under this entry")
    );
}

#[test]
fn envelope_drops_a_position_named_for_the_wrong_file() {
    // The yml-attribute rules re-target `location.file` at patch_path —
    // a span a rule recorded inside the model's .sql would point at text
    // that doesn't exist in the .yml, so the position AND its labels are
    // dropped (the file-only location replaces them). `help` isn't
    // positional and survives.
    let m = manifest(
        vec![filed_model(
            "declared",
            "models/marts/declared.sql",
            Some("models/marts/schema.yml"),
        )],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();
    let mut f = Finding::new(
        LintRule::UndocumentedModels,
        "model.p.declared",
        vec![],
        "declared has no description",
    )
    .with_span("models/marts/declared.sql", 12, 1, 12, 20)
    .with_label("the sql")
    .with_labels(vec![Label {
        line: 3,
        column: None,
        end_line: None,
        end_column: None,
        message: "sql position".into(),
    }])
    .with_help("still valid advice");
    super::rules::envelope(&dag, &cfg, &mut f);
    assert_eq!(
        f.location,
        Some(Location::new("models/marts/schema.yml")),
        "the envelope's file wins; the mismatched span is discarded"
    );
    assert_eq!(f.label, None, "the primary label pointed at the .sql");
    assert!(f.labels.is_empty(), "secondary positions too");
    assert_eq!(f.help.as_deref(), Some("still valid advice"));
}

#[test]
fn envelope_clears_labels_on_a_locationless_finding() {
    // A rule that produced labels but no location can't have meant the
    // file the envelope resolves: the position that location gains later
    // (a `.yml` span from `fill_finding_lines`) is not the one the labels
    // were written for. Same reasoning as the mismatched-file arm — the
    // file fills, the labels drop, `help` survives.
    let m = manifest(
        vec![filed_model(
            "declared",
            "models/marts/declared.sql",
            Some("models/marts/schema.yml"),
        )],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();
    let mut f = Finding::new(
        LintRule::UndocumentedModels,
        "model.p.declared",
        vec![],
        "declared has no description",
    )
    .with_label("in the sql")
    .with_labels(vec![Label {
        line: 3,
        column: None,
        end_line: None,
        end_column: None,
        message: "sql position".into(),
    }])
    .with_help("still valid advice");
    super::rules::envelope(&dag, &cfg, &mut f);
    assert_eq!(
        f.location,
        Some(Location::new("models/marts/schema.yml")),
        "the file still fills"
    );
    assert_eq!(f.label, None, "the label annotated a position we lack");
    assert!(f.labels.is_empty(), "secondary positions too");
    assert_eq!(
        f.help.as_deref(),
        Some("still valid advice"),
        "help isn't positional — it survives"
    );
}

#[test]
fn envelope_keeps_a_position_it_cannot_check() {
    // A finding on a node the dag doesn't know (no detail → no resolved
    // file) keeps the rule's location claim as-is — there is nothing to
    // check it against.
    let m = manifest(vec![model("a", "staging/a.sql")], vec![], &[]);
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();
    let mut f = Finding::new(LintRule::RootModels, "model.p.gone", vec![], "x").with_span(
        "models/gone.sql",
        1,
        1,
        1,
        5,
    );
    super::rules::envelope(&dag, &cfg, &mut f);
    assert_eq!(
        f.location,
        Some(Location {
            file: "models/gone.sql".into(),
            line: Some(1),
            column: Some(1),
            end_line: Some(1),
            end_column: Some(5),
        })
    );
}

// ---- call-site positions ----------------------------------------------------
// Graph findings point at the `ref()`/`source()`/`config()` call that made
// the edge — `raw_code` + `original_file_path` are what `CallSites` resolves
// against (the same file the envelope resolves).

/// A model carrying `raw_code` AND `original_file_path` — the two fields a
/// call-site position indexes and survives the envelope on. `ofp` follows
/// the `models/<dir>/<name>.sql` convention; `path` (the layer dir the
/// layer-rank rules read) is the same path minus the `models/` root.
fn sited_model(name: &str, ofp: &str, raw: &str) -> (String, RawNode) {
    let mut m = model_sql(name, raw);
    m.1.path = Some(ofp.trim_start_matches("models/").into());
    m.1.original_file_path = Some(ofp.into());
    m
}

fn finding_of<'a>(findings: &'a [Finding], rule: LintRule, uid: &str) -> &'a Finding {
    findings
        .iter()
        .find(|f| f.rule == rule && f.node_uid == uid)
        .unwrap_or_else(|| panic!("{rule:?} must fire on {uid}"))
}

#[test]
fn direct_join_positions_the_source_call_and_labels_the_model_calls() {
    // bad.sql's `source('s', 'raw')` sits at line 2 col 10; its two
    // `ref('stg_a')` calls are the secondary labels — the primary is the
    // FIRST source() call, every model read a labeled position.
    let raw = "select *\nfrom {{ source('s', 'raw') }} a\njoin {{ ref('stg_a') }} b on a.id = b.id\njoin {{ ref('stg_a') }} c on a.id = c.id\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("bad", "models/marts/bad.sql", raw),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.stg_a"),
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad");
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/marts/bad.sql".into(),
            line: Some(2),
            column: Some(9),
            end_line: Some(2),
            end_column: Some(27),
        }),
        "the primary position is the source() call, `{{ }}` excluded"
    );
    assert_eq!(hit.label.as_deref(), Some("source"));
    assert_eq!(
        hit.labels
            .iter()
            .map(|l| (l.line, l.column, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(3, Some(9), "model"), (4, Some(9), "model"),],
        "each model read gets its own labeled position"
    );
    assert_eq!(hit.message, "bad reads source s.raw and model stg_a");
    assert_eq!(hit.related, ["source.p.s.raw", "model.p.stg_a"]);
    // stg_a is the ONLY other staging reader of s.raw → a concrete fix.
    assert_eq!(hit.help.as_deref(), Some("select from stg_a instead"));
}

#[test]
fn direct_join_suggests_nothing_without_a_single_staging_reader() {
    // No staging model reads s.raw → the finding can't name the fix; and
    // with TWO staging readers the choice is ambiguous — both leave help
    // at the registry default.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("stg_b", "staging/stg_b.sql"),
            sited_model(
                "bad",
                "models/marts/bad.sql",
                "select * from {{ source('s', 'raw') }}",
            ),
            sited_model(
                "worse",
                "models/marts/worse.sql",
                "select * from {{ source('s', 'shared') }}",
            ),
        ],
        vec![source("s.raw"), source("s.shared")],
        &[
            ("source.p.s.shared", "model.p.stg_a"),
            ("source.p.s.shared", "model.p.stg_b"),
            ("source.p.s.shared", "model.p.worse"),
            ("model.p.stg_a", "model.p.worse"),
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    assert_eq!(
        finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad").help,
        None,
        "zero staging readers — nothing to suggest"
    );
    assert_eq!(
        finding_of(&f, LintRule::DirectJoinToSource, "model.p.worse").help,
        None,
        "two staging readers — the fix is ambiguous"
    );
}

#[test]
fn multiple_sources_joined_positions_each_source_call() {
    let raw = "select 1\nunion all\nselect 2 from {{ source('a', 't') }}\nunion all\nselect 3 from {{ source('b', 't') }}\n";
    let m = manifest(
        vec![sited_model("stg_dup", "models/staging/stg_dup.sql", raw)],
        vec![source("a.t"), source("b.t")],
        &[
            ("source.p.a.t", "model.p.stg_dup"),
            ("source.p.b.t", "model.p.stg_dup"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::MultipleSourcesJoined, "model.p.stg_dup");
    assert_eq!(hit.message, "stg_dup reads sources a.t, b.t");
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/staging/stg_dup.sql".into(),
            line: Some(3),
            column: Some(18),
            end_line: Some(3),
            end_column: Some(34),
        }),
        "the first source() call in file order is primary"
    );
    assert_eq!(hit.label.as_deref(), Some("source"));
    assert_eq!(
        hit.labels
            .iter()
            .map(|l| (l.line, l.column, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(5, Some(18), "source")]
    );
}

#[test]
fn jinja_commented_calls_contribute_no_position() {
    // `{# {{ ref('stg_a') }} #}` is not a call — the only real call is the
    // source() on line 2, which becomes the primary; stg_a's edge exists
    // (it's a parent) but gets no label.
    let raw = "{# {{ ref('stg_a') }} #}\nselect * from {{ source('s', 'raw') }}\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("bad", "models/marts/bad.sql", raw),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad");
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/marts/bad.sql".into(),
            line: Some(2),
            column: Some(18),
            end_line: Some(2),
            end_column: Some(36),
        })
    );
    assert!(
        hit.labels.is_empty(),
        "the commented ref is invisible — no label for stg_a"
    );
}

#[test]
fn sql_commented_calls_still_produce_positions() {
    // `-- {{ ref('stg_a') }}` IS a dependency in dbt's semantics (Jinja
    // renders before SQL comments exist) — the edge AND its position land.
    let raw = "-- {{ ref('stg_a') }}\nselect * from {{ source('s', 'raw') }}\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("bad", "models/marts/bad.sql", raw),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad");
    assert_eq!(
        hit.labels
            .iter()
            .map(|l| (l.line, l.column, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, Some(7), "model")],
        "the SQL-commented ref is a real call — labeled on line 1"
    );
}

#[test]
fn marts_depend_on_source_positions_the_source_call() {
    // dim_x reads s.raw directly; stg_a is the single staging reader → the
    // help names it and the position underlines the source() call.
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model(
                "dim_x",
                "models/marts/dim_x.sql",
                "select * from {{ source('s', 'raw') }}",
            ),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.stg_a"),
            ("source.p.s.raw", "model.p.dim_x"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::MartsDependOnSource, "model.p.dim_x");
    assert_eq!(hit.message, "marts model dim_x reads source s.raw");
    assert_eq!(hit.help.as_deref(), Some("select from stg_a instead"));
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/marts/dim_x.sql".into(),
            line: Some(1),
            column: Some(18),
            end_line: Some(1),
            end_column: Some(36),
        })
    );
    assert_eq!(hit.label.as_deref(), Some("source used here"));
}

#[test]
fn layer_violation_positions_the_later_layer_ref() {
    // stg_b reads fct_y — an edge staging→marts (parent rank > child rank).
    // The position is stg_b's own `ref('fct_y')` call.
    let m = manifest(
        vec![
            model("fct_y", "marts/fct_y.sql"),
            sited_model(
                "stg_b",
                "models/staging/stg_b.sql",
                "select * from {{ ref('fct_y') }}",
            ),
        ],
        vec![],
        &[("model.p.fct_y", "model.p.stg_b")],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::LayerViolation, "model.p.stg_b");
    assert_eq!(hit.related, ["model.p.fct_y"]);
    assert_eq!(
        hit.message,
        "stg_b directly depends on fct_y (a later layer)"
    );
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/staging/stg_b.sql".into(),
            line: Some(1),
            column: Some(18),
            end_line: Some(1),
            end_column: Some(30),
        })
    );
    assert_eq!(hit.label.as_deref(), Some("fct_y is in a later layer"));
}

#[test]
fn layer_violation_picks_the_versioned_call_for_the_right_parent() {
    // stg_c reads BOTH versions: `ref('x')` resolves to the family's latest
    // (v2), `ref('x', version='v1')` names v1 explicitly — each violation's
    // position underlines the call that made ITS edge.
    let v1 = (
        "model.p.x.v1".to_string(),
        RawNode {
            name: "x".into(),
            resource_type: "model".into(),
            path: Some("marts/x_v1.sql".into()),
            version: Some("1".into()),
            latest_version: Some("2".into()),
            ..Default::default()
        },
    );
    let v2 = (
        "model.p.x.v2".to_string(),
        RawNode {
            name: "x".into(),
            resource_type: "model".into(),
            path: Some("marts/x_v2.sql".into()),
            version: Some("2".into()),
            latest_version: Some("2".into()),
            ..Default::default()
        },
    );
    let raw = "{{ ref('x') }}\n{{ ref('x', version='v1') }}\n";
    let m = manifest(
        vec![
            v1,
            v2,
            sited_model("stg_c", "models/staging/stg_c.sql", raw),
        ],
        vec![],
        &[
            ("model.p.x.v1", "model.p.stg_c"),
            ("model.p.x.v2", "model.p.stg_c"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit_v1 = f
        .iter()
        .find(|f| f.rule == LintRule::LayerViolation && f.related == ["model.p.x.v1"])
        .expect("v1 violation");
    let hit_v2 = f
        .iter()
        .find(|f| f.rule == LintRule::LayerViolation && f.related == ["model.p.x.v2"])
        .expect("v2 violation");
    assert_eq!(
        hit_v1.location.as_ref().and_then(|l| l.line),
        Some(2),
        "v1's edge comes from the explicit version call"
    );
    assert_eq!(
        hit_v2.location.as_ref().and_then(|l| l.line),
        Some(1),
        "v2's edge comes from the bare ref — the family's latest"
    );
}

#[test]
fn rejoining_upstream_positions_the_redundant_ref() {
    // int_a → stg_b → fct_c plus int_a → fct_c: stg_b's ONLY data child is
    // fct_c, and int_a feeds fct_c directly — stg_b is the redundant hop.
    // The position is stg_b's `ref('int_a')` (the edge fct_c bypasses).
    let m = manifest(
        vec![
            model("int_a", "intermediate/int_a.sql"),
            sited_model(
                "stg_b",
                "models/staging/stg_b.sql",
                "select * from {{ ref('int_a') }}",
            ),
            model("fct_c", "marts/fct_c.sql"),
        ],
        vec![],
        &[
            ("model.p.int_a", "model.p.stg_b"),
            ("model.p.stg_b", "model.p.fct_c"),
            ("model.p.int_a", "model.p.fct_c"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::RejoiningUpstream, "model.p.stg_b");
    assert_eq!(hit.related, ["model.p.fct_c", "model.p.int_a"]);
    assert_eq!(
        hit.message,
        "stg_b is a redundant hop: int_a already feeds fct_c directly"
    );
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/staging/stg_b.sql".into(),
            line: Some(1),
            column: Some(18),
            end_line: Some(1),
            end_column: Some(30),
        })
    );
    assert_eq!(hit.label.as_deref(), Some("also read directly by fct_c"));
}

#[test]
fn staging_depends_on_staging_positions_the_staging_ref() {
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model(
                "stg_b",
                "models/staging/stg_b.sql",
                "select * from {{ ref('stg_a') }}",
            ),
        ],
        vec![],
        &[("model.p.stg_a", "model.p.stg_b")],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::StagingDependsOnStaging, "model.p.stg_b");
    assert_eq!(
        hit.message,
        "staging model stg_b directly depends on staging model stg_a"
    );
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/staging/stg_b.sql".into(),
            line: Some(1),
            column: Some(18),
            end_line: Some(1),
            end_column: Some(30),
        })
    );
    assert_eq!(hit.label.as_deref(), Some("staging model"));
}

#[test]
fn too_many_joins_labels_every_parent_call() {
    // Threshold 2 via config: `fct_x` reads two parents; the first call in
    // file order is primary, the second the labeled secondary naming its
    // target.
    let raw = "select *\nfrom {{ ref('stg_a') }}\njoin {{ ref('stg_b') }} on true\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("stg_b", "staging/stg_b.sql"),
            sited_model("fct_x", "models/marts/fct_x.sql", raw),
        ],
        vec![],
        &[
            ("model.p.stg_a", "model.p.fct_x"),
            ("model.p.stg_b", "model.p.fct_x"),
        ],
    );
    let cfg = config_from("lint:\n  rules:\n    too_many_joins: {threshold: 2}\n");
    let f = findings_of(&m, &cfg);
    let hit = finding_of(&f, LintRule::TooManyJoins, "model.p.fct_x");
    assert_eq!(
        hit.message,
        "fct_x directly depends on 2 data resources: stg_a, stg_b"
    );
    assert_eq!(hit.label.as_deref(), Some("reads stg_a"));
    assert_eq!(
        hit.labels
            .iter()
            .map(|l| (l.line, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(3, "reads stg_b")]
    );
}

#[test]
fn incremental_without_unique_key_positions_the_materialized_arg() {
    // The position underlines the `materialized='incremental'` VALUE — the
    // argument the missing unique_key would sit beside.
    let mut node = keyed_model("no_key", "incremental", None);
    node.1.raw_code = Some("{{ config(materialized='incremental') }}\nselect 1\n".into());
    let m = manifest(vec![node], vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::IncrementalWithoutUniqueKey, "model.p.no_key");
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/marts/no_key.sql".into(),
            line: Some(1),
            column: Some(24),
            end_line: Some(1),
            end_column: Some(37),
        }),
        "the 'incremental' literal — where the fix's unique_key goes"
    );
    assert_eq!(hit.label.as_deref(), Some("materialized 'incremental'"));
}

#[test]
fn incremental_without_unique_key_positions_through_a_config_comment() {
    // A `{# #}` INSIDE the config() args: the loader resolves
    // `materialized='incremental'` on the masked text, and the position
    // lookup must parse that same masked text — the literal still lands.
    let mut node = keyed_model("no_key", "incremental", None);
    node.1.raw_code = Some(
        "{{ config(\n    materialized = 'incremental', {# trailing note #}\n) }}\nselect 1\n"
            .into(),
    );
    let m = manifest(vec![node], vec![], &[]);
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::IncrementalWithoutUniqueKey, "model.p.no_key");
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/marts/no_key.sql".into(),
            line: Some(2),
            column: Some(20),
            end_line: Some(2),
            end_column: Some(33),
        }),
        "the 'incremental' literal on line 2 — the comment doesn't derail it"
    );
}

#[test]
fn duplicate_calls_all_get_labeled() {
    // The same source() called twice in one file plus a ref() between
    // them: the first site is primary, the rest labeled — and `with_labels`
    // re-sorts by position, not by insertion order.
    let raw = "select * from {{ source('s', 'raw') }}\njoin {{ ref('stg_a') }} on true\nunion all\nselect * from {{ source('s', 'raw') }}\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("dim_x", "models/marts/dim_x.sql", raw),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.stg_a"),
            ("source.p.s.raw", "model.p.dim_x"),
            ("model.p.stg_a", "model.p.dim_x"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.dim_x");
    assert_eq!(
        hit.location.as_ref().and_then(|l| l.line),
        Some(1),
        "the first source() call is primary"
    );
    assert_eq!(hit.label.as_deref(), Some("source"));
    assert_eq!(
        hit.labels
            .iter()
            .map(|l| (l.line, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "model"), (4, "source")],
        "the ref and the repeated source() label at their own positions"
    );
}

#[test]
fn call_positions_survive_jinja_crlf_tabs_and_multibyte() {
    // Positions index the ORIGINAL bytes; columns count chars — `{% if %}`
    // blocks, a multiline `{{ config(…) }}`, CRLF newlines, a leading tab
    // and non-ASCII text all leave the call where it actually is.
    let raw = "{% if true %}\r\n\t{{ config(\r\n    materialized='view',\r\n) }}\r\n{% endif %}\r\n-- あいう {{ ref('stg_a') }}\r\nselect * from {{ source('s', 'raw') }}\r\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("bad", "models/marts/bad.sql", raw),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad");
    assert_eq!(
        hit.location,
        Some(Location {
            file: "models/marts/bad.sql".into(),
            line: Some(7),
            column: Some(18),
            end_line: Some(7),
            end_column: Some(36),
        }),
        "CRLF counts once per line; the config() block's lines still count"
    );
    assert_eq!(
        hit.labels
            .iter()
            .map(|l| (l.line, l.column, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(6, Some(11), "model")],
        "`-- あいう {{ ` is ten chars — the ref lands at column 11"
    );
}

#[test]
fn a_variable_ref_gets_no_label() {
    // `ref(var('stg_a'))` is not a literal call — extraction never saw it,
    // so the stg_a edge exists but gets no position.
    let raw = "select * from {{ ref(var('stg_a')) }} join {{ source('s', 'raw') }} on true\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("bad", "models/marts/bad.sql", raw),
        ],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad");
    assert_eq!(
        hit.location.as_ref().and_then(|l| l.line),
        Some(1),
        "the source() call still positions the finding"
    );
    assert!(
        hit.labels.is_empty(),
        "no label for a call lint can't resolve"
    );
}

#[test]
fn call_site_positions_are_deterministic_across_runs() {
    // The whole finding vec — positions, labels, help — is stable across
    // two evaluations of the same dag.
    let raw = "select *\nfrom {{ source('a', 't') }}\njoin {{ source('b', 't') }} on true\njoin {{ ref('stg_a') }} on true\n";
    let m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            sited_model("bad", "models/marts/bad.sql", raw),
        ],
        vec![source("a.t"), source("b.t")],
        &[
            ("source.p.a.t", "model.p.stg_a"),
            ("source.p.a.t", "model.p.bad"),
            ("source.p.b.t", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let cfg = LintConfig::default();
    let dag = Dag::build(&m);
    let a = run_rules(LintInput::new(&dag, &cfg)).findings;
    let b = run_rules(LintInput::new(&dag, &cfg)).findings;
    assert_eq!(a, b);
}

#[test]
fn a_node_without_sql_keeps_the_file_only_location() {
    // `raw_code` absent (a manifest that records none) or a .py model:
    // `CallSites::for_node` returns None and the finding keeps just the
    // file — no position is guessed.
    let mut py = model("bad", "marts/bad.py");
    py.1.language = Some("python".into());
    py.1.original_file_path = Some("models/marts/bad.py".into());
    let m = manifest(
        vec![model("stg_a", "staging/stg_a.sql"), py],
        vec![source("s.raw")],
        &[
            ("source.p.s.raw", "model.p.bad"),
            ("model.p.stg_a", "model.p.bad"),
        ],
    );
    let f = findings_of(&m, &LintConfig::default());
    let hit = finding_of(&f, LintRule::DirectJoinToSource, "model.p.bad");
    assert_eq!(
        hit.location,
        Some(Location::new("models/marts/bad.py")),
        "the envelope's file with no position — Python call sites aren't read"
    );
    assert!(hit.labels.is_empty());
    assert_eq!(hit.label, None);
}

// ---- byte_span_position --------------------------------------------------------------

#[test]
fn byte_span_position_maps_ascii_offsets() {
    // "one\ntwo\nthree" — the span of "two" is bytes 4..7.
    assert_eq!(
        byte_span_position("one\ntwo\nthree", 4, 7),
        Some((2, 1, 2, 4)),
        "a mid-line word: (line, col) to the exclusive end on the same line"
    );
    // A whole first line INCLUDING its newline ends at the next line's 1.
    assert_eq!(
        byte_span_position("one\ntwo\nthree", 0, 4),
        Some((1, 1, 2, 1))
    );
    // An empty span reports start == end.
    assert_eq!(byte_span_position("abc", 1, 1), Some((1, 2, 1, 2)));
}

#[test]
fn byte_span_position_counts_chars_not_bytes() {
    // `é` is two bytes but one column — offsets and columns agree in
    // chars, never bytes.
    let text = "aé\nz";
    let é = text.char_indices().nth(1).unwrap().0;
    let z = text.char_indices().nth(3).unwrap().0;
    assert_eq!(byte_span_position(text, é, é + 2), Some((1, 2, 1, 3)));
    assert_eq!(byte_span_position(text, z, z + 1), Some((2, 1, 2, 2)));
    // A tab is one column too — not a tab-stop expansion.
    assert_eq!(byte_span_position("a\tb", 1, 3), Some((1, 2, 1, 4)));
}

#[test]
fn byte_span_position_treats_crlf_as_one_newline() {
    // The position after "\r\n" is the next line's column 1; a span
    // ending at the `\r` reports the line's end, not the next line.
    let text = "ab\r\ncd";
    assert_eq!(byte_span_position(text, 4, 6), Some((2, 1, 2, 3)));
    assert_eq!(byte_span_position(text, 0, 2), Some((1, 1, 1, 3)));
}

#[test]
fn byte_span_position_handles_the_final_line_either_way() {
    // With or without a trailing newline the last line's offsets resolve.
    assert_eq!(byte_span_position("a\nb", 2, 3), Some((2, 1, 2, 2)));
    assert_eq!(byte_span_position("a\nb\n", 2, 3), Some((2, 1, 2, 2)));
    // An offset AT the end of the text is legal — the position after the
    // last character.
    assert_eq!(byte_span_position("a\nb", 3, 3), Some((2, 2, 2, 2)));
    assert_eq!(byte_span_position("a\nb\n", 4, 4), Some((3, 1, 3, 1)));
}

#[test]
fn byte_span_position_rejects_what_it_cannot_pin() {
    // end < start, end past the text, and offsets that cut a char in
    // half all return None — no guessed positions.
    assert_eq!(byte_span_position("abc", 2, 1), None);
    assert_eq!(byte_span_position("abc", 0, 4), None);
    let text = "aé";
    let inside_é = 2; // `é` spans bytes 1..3
    assert_eq!(byte_span_position(text, inside_é, 3), None);
    assert_eq!(byte_span_position(text, 0, inside_é), None);
}

// ---- yaml_name_line / yaml_name_span --------------------------------------------------

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

#[test]
fn yaml_name_span_points_at_the_value_not_the_key() {
    // `      - name: orders` — the value starts at column 15 (6 spaces +
    // `- name: `) and `orders` is 6 chars wide, so the exclusive end is 21.
    let yml = "version: 2\n\nsources:\n  - name: raw\n    tables:\n      - name: orders\n";
    assert_eq!(
        yaml_name_span(yml, &names(&["raw", "orders"])),
        Some((6, 15, 21)),
        "(line, column, exclusive end_column) of the name value"
    );
    assert_eq!(yaml_name_span(yml, &names(&["raw"])), Some((4, 11, 14)));
}

#[test]
fn yaml_name_span_points_inside_the_quotes() {
    // Quoted names underline the NAME, not the quotes: `  - name: "raw"`
    // has `raw` at columns 12..15.
    let yml = "sources:\n  - name: \"raw\"\n    tables:\n      - name: 'orders'\n";
    assert_eq!(yaml_name_span(yml, &names(&["raw"])), Some((2, 12, 15)));
    assert_eq!(
        yaml_name_span(yml, &names(&["raw", "orders"])),
        Some((4, 16, 22)),
        "single-quoted names resolve the same way"
    );
}

#[test]
fn yaml_name_span_returns_none_where_the_line_would() {
    // No chain match → no span either (columns are never guessed).
    let yml = "models:\n  - name: a\n";
    assert_eq!(yaml_name_span(yml, &names(&["ghost"])), None);
    assert_eq!(yaml_name_span(yml, &[]), None);
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

#[test]
fn fill_finding_lines_fills_the_value_span_and_a_label() {
    // The same fixture, now asserting the full span: `- name:` VALUE
    // columns, the exclusive end on the same line, and the `declared
    // here` primary label — for a source, a model-level rule, and a
    // column-scoped one.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    //      1 version: 2          7 (blank)
    //      2                     8 models:
    //      3 sources:            9   - name: m
    //      4   - name: s        10     columns:
    //      5     tables:        11       - name: id
    //      6       - name: bare 12       - name: amt
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nsources:\n  - name: s\n    tables:\n      - name: bare\n\nmodels:\n  - name: m\n    columns:\n      - name: id\n      - name: amt\n",
    )
    .unwrap();
    let cols = |name: &str| (name.to_string(), serde_json::json!({"name": name}));
    let mut declared = filed_model("m", "models/m.sql", Some("models/schema.yml"));
    declared.1.description = Some("documented".into());
    declared.1.columns = serde_json::Map::from_iter([cols("id"), cols("amt")]);
    let mut src = source("s.bare");
    src.1.original_file_path = Some("models/schema.yml".into());
    let m = manifest(vec![declared], vec![src], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let loc = |rule, uid| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == uid)
            .and_then(|x| x.location.clone())
            .unwrap_or_else(|| panic!("{rule:?} on {uid} has a location"))
    };
    // `- name: id` at line 11: the value `id` spans columns 15..17.
    assert_eq!(
        loc(LintRule::UndocumentedColumns, "model.p.m"),
        Location {
            file: "models/schema.yml".into(),
            line: Some(11),
            column: Some(15),
            end_line: Some(11),
            end_column: Some(17),
        },
        "a column finding spans the column's `- name:` value"
    );
    // `      - name: bare` at line 6: `bare` spans columns 15..19.
    for rule in [LintRule::UnusedSources, LintRule::SourcesWithoutFreshness] {
        assert_eq!(
            loc(rule, "source.p.s.bare"),
            Location {
                file: "models/schema.yml".into(),
                line: Some(6),
                column: Some(15),
                end_line: Some(6),
                end_column: Some(19),
            },
            "{rule:?} spans the table's `- name:` value"
        );
    }
    // `  - name: m` at line 9: `m` spans columns 11..12 — the model
    // finding points at the model's own entry (its PK test would
    // live under it).
    assert_eq!(
        loc(LintRule::MissingPrimaryKeyTests, "model.p.m"),
        Location {
            file: "models/schema.yml".into(),
            line: Some(9),
            column: Some(11),
            end_line: Some(9),
            end_column: Some(12),
        }
    );
    // Every resolved finding carries the primary label.
    for x in &f {
        if x.location.as_ref().is_some_and(|l| l.line.is_some()) {
            assert_eq!(x.label.as_deref(), Some("declared here"), "{x:?}");
        }
    }
}

#[test]
fn fill_finding_lines_spans_quoted_names() {
    // A quoted `- name:` underlines inside the quotes.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "sources:\n  - name: \"s\"\n    tables:\n      - name: 'bare'\n",
    )
    .unwrap();
    let mut src = source("s.bare");
    src.1.original_file_path = Some("models/schema.yml".into());
    let m = manifest(vec![], vec![src], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let loc = f
        .iter()
        .find(|x| x.rule == LintRule::UnusedSources)
        .and_then(|x| x.location.clone())
        .unwrap();
    // `      - name: 'bare'` — the value inside the quotes: 16..20.
    assert_eq!(
        (loc.line, loc.column, loc.end_line, loc.end_column),
        (Some(4), Some(16), Some(4), Some(20))
    );
}

#[test]
fn fill_finding_lines_respects_what_a_rule_pinned() {
    // A position a rule pinned is its claim — the fill only completes the
    // missing parts, never overwrites. Using one finding's `.yml` location
    // (the `missing_primary_key_tests` one on model m, whose chain is the
    // `- name: m` entry at line 4) cloned into every pin shape:
    //
    //    3 models:               — not a `- name:` line
    //    4   - name: m          — `m` at columns 11..12
    //    6       - name: id     — `id` at columns 15..17
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nmodels:\n  - name: m\n    columns:\n      - name: id\n      - name: amt\n",
    )
    .unwrap();
    let cols = |name: &str| (name.to_string(), serde_json::json!({"name": name}));
    let mut declared = filed_model("m", "models/m.sql", Some("models/schema.yml"));
    declared.1.description = Some("documented".into());
    declared.1.columns = serde_json::Map::from_iter([cols("id"), cols("amt")]);
    let m = manifest(vec![declared], vec![], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let base = f
        .iter()
        .find(|x| x.rule == LintRule::MissingPrimaryKeyTests && x.node_uid == "model.p.m")
        .expect("the model finding")
        .clone();
    // Sanity: the unfilled baseline resolved to the `- name: m` span.
    assert_eq!(base.label.as_deref(), Some("declared here"));

    let pinned =
        |line: Option<u32>, column: Option<u32>, end_line: Option<u32>, end_column: Option<u32>| {
            let mut c = base.clone();
            c.label = None;
            let loc = c.location.as_mut().unwrap();
            loc.line = line;
            loc.column = column;
            loc.end_line = end_line;
            loc.end_column = end_column;
            c
        };
    let mut cases = vec![
        // Line pinned on a `- name:` entry — the columns lift from THAT
        // line's own value, not the chain's.
        pinned(Some(6), None, None, None),
        // Line pinned on a non-`-name:` line — nothing to lift; the
        // line stays alone (no columns guessed).
        pinned(Some(3), None, None, None),
        // Column pinned only — the line resolves via the chain; the
        // pinned column keeps its claim and no end_column is invented
        // for a start the span doesn't own.
        pinned(None, Some(7), None, None),
        // Line AND column pinned — the start is claimed whole; the
        // finding is left untouched.
        pinned(Some(4), Some(3), Some(4), Some(4)),
        // A pinned end_line the chain disagrees with caps the span —
        // end_column can't be supplied for it.
        pinned(None, None, Some(20), None),
    ];
    fill_finding_lines(&mut cases, &dag, dir.path());
    let pos = |c: &Finding| {
        let l = c.location.as_ref().unwrap();
        (l.line, l.column, l.end_line, l.end_column)
    };
    assert_eq!(
        pos(&cases[0]),
        (Some(6), Some(15), Some(6), Some(17)),
        "line 6 is `- name: id` — its own value supplies the columns"
    );
    assert_eq!(cases[0].label.as_deref(), Some("declared here"));
    assert_eq!(
        pos(&cases[1]),
        (Some(3), None, None, None),
        "`models:` holds no `- name:` value — the line stays column-less"
    );
    assert_eq!(cases[1].label, None, "no resolved span, no label");
    assert_eq!(
        pos(&cases[2]),
        (Some(4), Some(7), Some(4), None),
        "the chain's line fills; the pinned column survives; end_column stays unknown"
    );
    assert_eq!(
        pos(&cases[3]),
        (Some(4), Some(3), Some(4), Some(4)),
        "a fully-pinned start is never touched"
    );
    assert_eq!(cases[3].label, None, "untouched means the label too");
    assert_eq!(
        pos(&cases[4]),
        (Some(4), Some(11), Some(20), None),
        "a foreign end_line isn't given the span's end_column"
    );
}

#[test]
fn fill_finding_lines_with_matches_the_disk_wrapper() {
    // The text-resolving variant and the disk-reading `fill_finding_lines`
    // must fill identical positions — a caller that holds the sources in
    // memory gets the same locations a disk run produces. Same fixture as
    // the disk-path tests, both fills over the same finding set.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    let schema = "version: 2\n\nsources:\n  - name: s\n    tables:\n      - name: bare\n\nmodels:\n  - name: m\n    columns:\n      - name: id\n      - name: amt\n";
    std::fs::write(dir.path().join("models/schema.yml"), schema).unwrap();
    let cols = |name: &str| (name.to_string(), serde_json::json!({"name": name}));
    let mut m_cols = serde_json::Map::new();
    m_cols.extend([cols("id"), cols("amt")]);
    let mut declared = filed_model("m", "models/m.sql", Some("models/schema.yml"));
    declared.1.description = Some("documented".into());
    declared.1.columns = m_cols;
    let mut src = source("s.bare");
    src.1.original_file_path = Some("models/schema.yml".into());
    let m = manifest(vec![declared], vec![src], &[]);
    let dag = Dag::build(&m);
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let mut on_disk = findings.clone();
    fill_finding_lines(&mut on_disk, &dag, dir.path());
    let mut from_text = findings;
    fill_finding_lines_with(&mut from_text, &dag, &|file| {
        (file == "models/schema.yml").then(|| schema.to_string())
    });
    assert_eq!(
        on_disk, from_text,
        "the text-resolving fill lands the same locations the disk fill does"
    );
}

#[test]
fn fill_finding_lines_scopes_same_names_to_their_sections() {
    // A source table and a model both named `orders`: the model finding
    // resolves inside `models:` — its own `- name:` — never the source
    // table's same-named entry, and the source finding descends
    // `sources:` → `tables:` to the table's line.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    //      4   - name: raw           8
    //      5     tables:            9 models:
    //      6       - name: orders  10   - name: orders
    //      7       - name: customers
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nsources:\n  - name: raw\n    tables:\n      - name: orders\n      - name: customers\n\nmodels:\n  - name: orders\n",
    )
    .unwrap();
    let model = filed_model("orders", "models/orders.sql", Some("models/schema.yml"));
    let mut src = source("raw.orders");
    src.1.original_file_path = Some("models/schema.yml".into());
    let m = manifest(vec![model], vec![src], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line = |rule, uid| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == uid)
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(
        line(LintRule::UndocumentedModels, "model.p.orders"),
        Some(10),
        "the model's own `- name:` under `models:` — not the table's"
    );
    assert_eq!(
        line(LintRule::MissingPrimaryKeyTests, "model.p.orders"),
        Some(10)
    );
    assert_eq!(
        line(LintRule::UnusedSources, "source.p.raw.orders"),
        Some(6),
        "the source finding descends sources → tables → the table"
    );
}

#[test]
fn fill_finding_lines_a_column_named_like_a_model_stays_a_column() {
    // `stg_x` is a COLUMN of `orders` and a MODEL in its own right; a
    // source table shares the model's name too. The column finding
    // descends orders → columns → stg_x, and the `stg_x` model finding
    // lands on its own `models:` entry — the column above it can't
    // answer for it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    //      4   - name: raw            9   - name: orders
    //      5     tables:             10     columns:
    //      6       - name: orders    11       - name: stg_x
    //      7                         12   - name: stg_x
    //      8 models:
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nsources:\n  - name: raw\n    tables:\n      - name: orders\n\nmodels:\n  - name: orders\n    columns:\n      - name: stg_x\n  - name: stg_x\n",
    )
    .unwrap();
    let mut orders = filed_model("orders", "models/orders.sql", Some("models/schema.yml"));
    orders.1.columns =
        serde_json::Map::from_iter([("stg_x".to_string(), serde_json::json!({"name": "stg_x"}))]);
    let stg_x = filed_model("stg_x", "models/stg_x.sql", Some("models/schema.yml"));
    let m = manifest(vec![orders, stg_x], vec![], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line = |rule, uid| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == uid)
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(
        line(LintRule::UndocumentedModels, "model.p.orders"),
        Some(9),
        "the model entry — not the source table's same-named one"
    );
    assert_eq!(
        line(LintRule::UndocumentedColumns, "model.p.orders"),
        Some(11),
        "the column's `- name:` inside orders' `columns:`"
    );
    assert_eq!(
        line(LintRule::UndocumentedModels, "model.p.stg_x"),
        Some(12),
        "the `stg_x` MODEL — the column above it is another list's entry"
    );
}

#[test]
fn fill_finding_lines_a_versioned_model_resolves_past_versions() {
    // `versions:` items are `- v:` lines — no `- name:` to land on — so
    // the model finding answers on the model's own entry, and a column
    // declared under a version's `columns:` resolves on its line.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    //      4   - name: fct_orders    6       - v: 1
    //      5     versions:           7       - v: 2
    //                                 8         columns:
    //                                 9           - name: id
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nmodels:\n  - name: fct_orders\n    versions:\n      - v: 1\n      - v: 2\n        columns:\n          - name: id\n",
    )
    .unwrap();
    let mut m = filed_model(
        "fct_orders",
        "models/fct_orders.sql",
        Some("models/schema.yml"),
    );
    m.1.columns =
        serde_json::Map::from_iter([("id".to_string(), serde_json::json!({"name": "id"}))]);
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line = |rule| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == "model.p.fct_orders")
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(
        line(LintRule::MissingPrimaryKeyTests),
        Some(4),
        "the model's own `- name:` — `versions:` is navigated through"
    );
    assert_eq!(
        line(LintRule::UndocumentedColumns),
        Some(9),
        "the column under a version's `columns:` still resolves"
    );
}

#[test]
fn yaml_name_line_skips_block_scalar_bodies() {
    // `description: |` bodies are text, not structure: a `- name:`
    // inside one is no declaration — the real `ghost` model wins the
    // lookup, and `a`'s block holds only `id`.
    //     1 models:                  5     columns:
    //     2   - name: a              6       - name: id
    //     3     description: |       7   - name: ghost
    //     4       - name: ghost
    let yml = "models:\n  - name: a\n    description: |\n      - name: ghost\n    columns:\n      - name: id\n  - name: ghost\n";
    assert_eq!(yaml_name_line(yml, &names(&["ghost"])), Some(7));
    assert_eq!(
        yaml_name_line(yml, &names(&["a", "ghost"])),
        None,
        "the scalar-body `- name:` isn't a declaration"
    );
    assert_eq!(yaml_name_line(yml, &names(&["a", "id"])), Some(6));
}

#[test]
fn fill_finding_lines_quoted_section_keys_still_resolve() {
    // `"models":` declares `models` like a bare `models:` — the
    // section lookup answers inside it, and a `"columns"` list key
    // resolves the same way.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    //      3 "models":            6   'columns':
    //      4   - name: orders     7     - name: id
    //      5     description: ok
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\n\"models\":\n  - name: orders\n    description: ok\n    'columns':\n      - name: id\n",
    )
    .unwrap();
    let mut m = filed_model("orders", "models/orders.sql", Some("models/schema.yml"));
    m.1.columns =
        serde_json::Map::from_iter([("id".to_string(), serde_json::json!({"name": "id"}))]);
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line = |rule| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == "model.p.orders")
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(line(LintRule::MissingPrimaryKeyTests), Some(4));
    assert_eq!(line(LintRule::UndocumentedColumns), Some(7));
}

#[test]
fn fill_finding_lines_descends_into_anchored_list_keys() {
    // `columns: &c` anchors the block the deeper lines carry — the
    // column entry still belongs to the model's `columns:` list and
    // resolves on its own `- name:` line.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    //      3 models:              6     columns: &c
    //      4   - name: orders     7       - name: id
    //      5     description: ok
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\n\nmodels:\n  - name: orders\n    description: ok\n    columns: &c\n      - name: id\n",
    )
    .unwrap();
    let mut m = filed_model("orders", "models/orders.sql", Some("models/schema.yml"));
    m.1.columns =
        serde_json::Map::from_iter([("id".to_string(), serde_json::json!({"name": "id"}))]);
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line = |rule| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == "model.p.orders")
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(
        line(LintRule::UndocumentedColumns),
        Some(7),
        "the column under an anchored `columns:` list still resolves"
    );
}

#[test]
fn fill_finding_lines_crlf_files_index_the_same() {
    // CRLF endings: the index counts lines, not bytes — the same model
    // resolves on the same line with `\r\n` separators.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("models/schema.yml"),
        "version: 2\r\n\r\nmodels:\r\n  - name: orders\r\n    columns:\r\n      - name: id\r\n",
    )
    .unwrap();
    let mut m = filed_model("orders", "models/orders.sql", Some("models/schema.yml"));
    m.1.columns =
        serde_json::Map::from_iter([("id".to_string(), serde_json::json!({"name": "id"}))]);
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    fill_finding_lines(&mut f, &dag, dir.path());
    let line = |rule| {
        f.iter()
            .find(|x| x.rule == rule && x.node_uid == "model.p.orders")
            .and_then(|x| x.location.as_ref().and_then(|l| l.line))
    };
    assert_eq!(line(LintRule::MissingPrimaryKeyTests), Some(4));
    assert_eq!(line(LintRule::UndocumentedColumns), Some(6));
}

#[test]
fn yaml_name_line_skips_folded_scalar_bodies() {
    // `description: >` folds like `|` — a `- name:` line inside its
    // body is text, not a declaration.
    //     1 models:                  5     columns:
    //     2   - name: a              6       - name: id
    //     3     description: >       7   - name: ghost
    //     4       - name: ghost
    let yml = "models:\n  - name: a\n    description: >\n      - name: ghost\n    columns:\n      - name: id\n  - name: ghost\n";
    assert_eq!(yaml_name_line(yml, &names(&["ghost"])), Some(7));
    assert_eq!(
        yaml_name_line(yml, &names(&["a", "ghost"])),
        None,
        "the folded-scalar `- name:` isn't a declaration"
    );
    assert_eq!(yaml_name_line(yml, &names(&["a", "id"])), Some(6));
}

#[test]
fn fill_finding_lines_with_reads_each_file_once() {
    // However many findings land in one yml, `read_text` resolves it —
    // and its `- name:` index is built — exactly once.
    let schema = "models:\n  - name: a\n  - name: b\n  - name: c\n  - name: d\n";
    let m = manifest(
        vec![
            filed_model("a", "models/a.sql", Some("models/schema.yml")),
            filed_model("b", "models/b.sql", Some("models/schema.yml")),
            filed_model("c", "models/c.sql", Some("models/schema.yml")),
            filed_model("d", "models/d.sql", Some("models/schema.yml")),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let mut f = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let calls = std::cell::Cell::new(0);
    fill_finding_lines_with(&mut f, &dag, &|file| {
        calls.set(calls.get() + 1);
        (file == "models/schema.yml").then(|| schema.to_string())
    });
    assert_eq!(
        calls.get(),
        1,
        "four findings in one file — one read, one index"
    );
    assert_eq!(
        f.iter()
            .filter(|x| x.rule == LintRule::UndocumentedModels
                && x.location.as_ref().and_then(|l| l.line).is_some())
            .count(),
        4,
        "every finding still resolved its line"
    );
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
            cfg.rules.insert(r, RuleEnabled::Off);
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
fn select_star_param_exempts_only_the_provable_single_cte_close() {
    let m = manifest(
        vec![
            // Exempt under the parameter: the final select reads exactly
            // one CTE and that CTE writes its columns out — the wildcard
            // is a close-the-model idiom, not a loose grab.
            model_sql(
                "close",
                "with final as (select a, b from t) select * from final",
            ),
            model_sql(
                "close_alias",
                "with final as (select a from t) select f.* from final f",
            ),
            model_sql(
                "close_extra",
                "with final as (select a from t) select *, 1 as x from final",
            ),
            // Still flagged: no shape provably names every column the
            // wildcard emits.
            model_sql(
                "star_in_cte",
                "with final as (select * from t) select * from final",
            ),
            model_sql(
                "joined",
                "with final as (select a from t) select * from final join u on true",
            ),
            model_sql("subq", "select * from (select a from t) s"),
            model_sql(
                "unioned",
                "with final as (select a from t) select * from final union all select a from u",
            ),
            model_sql("plain_table", "select * from final"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);

    // Off by default — every output wildcard flags, exempting nothing.
    let out = lint_outcome(&dag, &cfg_only(&[LintRule::SelectStar]), Some(&res), None);
    assert_eq!(flagged(&out.findings, LintRule::SelectStar).len(), 8);

    // On: only the provable `select * from <one all-explicit CTE>` close
    // drops out. Aliased/augmented wildcards stay exempt; joins,
    // subqueries, real tables, a `*` inside the CTE, and UNION shapes
    // keep flagging.
    let cfg =
        config_from("lint:\n  rules:\n    select_star: {allow_final_star_over_single_cte: true}\n");
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::SelectStar),
        vec![
            "model.p.joined",
            "model.p.plain_table",
            "model.p.star_in_cte",
            "model.p.subq",
            "model.p.unioned",
        ],
        "only the provable single-explicit-CTE close is exempt"
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
            model_sql(
                "asof",
                "select * from a.t asof join b.u match_condition(a.t.ts >= b.u.ts)",
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
        vec!["model.p.bare_join"],
        "an explicit CROSS JOIN never counts; ON/USING/NATURAL/ASOF \
         MATCH_CONDITION conditions and UNNEST joins must not flag either"
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

// ---- SQL-rule positions: raw-file spans the findings carry ----------------
//
// A finding's position comes from the node's `raw_code` through the Jinja
// source map — never from compiled text (a macro can move everything).
// Expected positions are COMPUTED from the raw string (needle → byte span
// → line/column via `byte_span_position`), never hardcoded.

/// `model_sql` with `original_file_path` — the `.sql` path the positions
/// anchor into (no path → the finding reports unlocated).
fn model_sql_pos(name: &str, raw: &str) -> (String, RawNode) {
    let mut m = model_sql(name, raw);
    m.1.original_file_path = Some(format!("models/{name}.sql"));
    m
}

/// `(file, line, column, end_line, end_column)` of `f`'s location —
/// panics when unlocated (a positionless finding is a test bug here).
fn pos_of(f: &Finding) -> (String, u32, u32, u32, u32) {
    let l = f
        .location
        .as_ref()
        .unwrap_or_else(|| panic!("{f:?} has no location"));
    (
        l.file.clone(),
        l.line.unwrap(),
        l.column.unwrap(),
        l.end_line.unwrap(),
        l.end_column.unwrap(),
    )
}

/// The `Finding` `rule` produced on `uid` — panics when absent.
fn finding_on<'a>(findings: &'a [Finding], rule: LintRule, uid: &str) -> &'a Finding {
    findings
        .iter()
        .find(|f| f.rule == rule && f.node_uid == uid)
        .unwrap_or_else(|| panic!("{rule:?} on {uid} missing"))
}

/// `(line, column, end_line, end_column)` `needle`'s `nth` occurrence in
/// `raw` resolves to — the dynamic expected position.
fn needle_pos(raw: &str, needle: &str, nth: usize) -> (u32, u32, u32, u32) {
    let start = raw
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle:?} occurrence {nth} missing"))
        .0;
    byte_span_position(raw, start, start + needle.len())
        .unwrap_or_else(|| panic!("{needle:?} position resolves"))
}

#[test]
fn select_star_positions_point_at_each_wildcard() {
    let m = manifest(
        vec![
            model_sql_pos("star", "select id,\n       o.*\nfrom orders o"),
            model_sql_pos("qual", "select t.* from t"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let star_raw = dag.raw_code("model.p.star").unwrap();
    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.star");
    let (l, c, el, ec) = needle_pos(star_raw, "o.*", 0);
    assert_eq!(
        pos_of(f),
        ("models/star.sql".to_string(), l, c, el, ec),
        "the qualified wildcard underlines `o.*` itself"
    );
    assert_eq!(f.label.as_deref(), Some("wildcard"));

    let qual_raw = dag.raw_code("model.p.qual").unwrap();
    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.qual");
    let (l, c, el, ec) = needle_pos(qual_raw, "t.*", 0);
    assert_eq!(
        pos_of(f),
        ("models/qual.sql".to_string(), l, c, el, ec),
        "`t.*` underlines the qualifier plus the star"
    );
}

#[test]
fn select_star_positions_label_every_union_arm() {
    // Two wildcard UNION arms: the first is the primary position, the
    // second a secondary label — one finding for the whole model.
    let raw = "select *\nfrom a\nunion all\nselect *\nfrom b";
    let m = manifest(vec![model_sql_pos("u", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.u");
    let (l, c, el, ec) = needle_pos(raw, "*", 0);
    assert_eq!(pos_of(f), ("models/u.sql".to_string(), l, c, el, ec));
    assert_eq!(f.labels.len(), 1, "the second arm rides as a label");
    let (l2, c2, el2, ec2) = needle_pos(raw, "*", 1);
    let lbl = &f.labels[0];
    assert_eq!(
        (lbl.line, lbl.column, lbl.end_line, lbl.end_column),
        (l2, Some(c2), Some(el2), Some(ec2)),
        "the label underlines the second arm's `*`"
    );
    assert_eq!(lbl.message, "wildcard");
}

#[test]
fn select_star_positions_survive_jinja_and_encoding() {
    // `{{ config }}`, a multi-line `{% if %}`, `{{ ref() }}` in FROM, CRLF
    // endings, a tab indent, and a non-ASCII comment — the rendered SQL
    // shifts, the raw positions must not.
    let raw = "-- 日本語コメント\r\n{{ config(materialized='table') }}\r\nselect\r\n{% if True %}\r\n  a,\r\n{% endif %}\r\n\t*\r\nfrom {{ ref('src') }}";
    let m = manifest(vec![model_sql_pos("j", raw)], vec![source("src.raw")], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.j");
    let (l, c, el, ec) = needle_pos(raw, "*", 0);
    assert_eq!(
        pos_of(f),
        ("models/j.sql".to_string(), l, c, el, ec),
        "the wildcard after every Jinja block, CRLF, tab, and multibyte comment"
    );
    assert_eq!(l, 7, "the `*` really is line 7 (1-based)");
    assert_eq!(c, 2, "the tab indents it (a tab is one column)");
}

#[test]
fn select_star_positions_never_point_into_compiled_sql() {
    // A wildcard that exists ONLY after macro expansion: the fact flags
    // (compiled text is judged), but raw has no `*` to underline.
    let mut m = model_sql_pos("macro_star", "select a\nfrom {{ star_macro() }}");
    m.1.compiled_code = Some("select *\nfrom t".into());
    // A truly compiled-only node: no raw at all — file-only location.
    let mut c = model_compiled_only("compiled_only", "select * from t");
    c.1.original_file_path = Some("models/compiled_only.sql".into());
    let m = manifest(vec![m, c], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.macro_star");
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "a macro-rendered `*` has no raw position to point at: {f:?}"
    );
    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.compiled_only");
    let l = f.location.as_ref().expect("the .sql file is still named");
    assert_eq!(l.file, "models/compiled_only.sql");
    assert!(
        l.line.is_none(),
        "no raw text — file-only, never a compiled line"
    );
}

#[test]
fn cartesian_join_positions_underline_the_relation_not_the_keyword() {
    let raw = "select *\nfrom a.t\njoin b.u";
    let m = manifest(vec![model_sql_pos("j", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::CartesianJoin, "model.p.j");
    let (l, c, el, ec) = needle_pos(raw, "b.u", 0);
    assert_eq!(
        pos_of(f),
        ("models/j.sql".to_string(), l, c, el, ec),
        "the joined relation `b.u` is underlined — `join` stays outside"
    );
    assert_eq!(f.label.as_deref(), Some("no join condition"));
}

#[test]
fn cartesian_join_positions_skip_a_macro_changed_count() {
    // Raw shows one conditionless join, compiled gains a second from a
    // macro — counts disagree, so no positional pairing is trusted.
    let mut m = model_sql_pos("j", "select * from a.t join b.u");
    m.1.compiled_code = Some("select * from a.t join b.u join c.v".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::CartesianJoin, "model.p.j");
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "a count mismatch leaves the finding unlocated: {f:?}"
    );
}

#[test]
fn hard_coded_references_positions_underline_the_relation_name() {
    let raw = "select *\nfrom analytics.orders\njoin mart.users u on 1 = 1";
    let m = manifest(vec![model_sql_pos("h", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::HardCodedReferences]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::HardCodedReferences, "model.p.h");
    let (l, c, el, ec) = needle_pos(raw, "analytics.orders", 0);
    assert_eq!(
        pos_of(f),
        ("models/h.sql".to_string(), l, c, el, ec),
        "the first written relation name is the primary position"
    );
    assert_eq!(f.label.as_deref(), Some("not a ref() or source()"));
    let (l2, c2, el2, ec2) = needle_pos(raw, "mart.users", 0);
    let lbl = &f.labels[0];
    assert_eq!(
        (lbl.line, lbl.column, lbl.end_line, lbl.end_column),
        (l2, Some(c2), Some(el2), Some(ec2)),
        "the second literal rides as a label"
    );
    assert_eq!(lbl.message, "not a ref() or source()");
}

#[test]
fn unknown_column_reference_positions_the_unique_reference() {
    let m = manifest(
        vec![
            model_sql_cols("stg", "select 1", &[("id", None), ("amount", None)], true),
            model_sql_pos("uni", "select id, nope from stg"),
            // `nope` twice — can't tell WHICH occurrence is wrong.
            model_sql_pos("dup", "select nope, nope from stg"),
        ],
        vec![],
        &[
            ("model.p.stg", "model.p.uni"),
            ("model.p.stg", "model.p.dup"),
        ],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnknownColumnReference]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let raw = dag.raw_code("model.p.uni").unwrap();
    let f = finding_on(
        &out.findings,
        LintRule::UnknownColumnReference,
        "model.p.uni",
    );
    let (l, c, el, ec) = needle_pos(raw, "nope", 0);
    assert_eq!(
        pos_of(f),
        ("models/uni.sql".to_string(), l, c, el, ec),
        "the one `nope` occurrence underlines the column reference"
    );
    assert_eq!(
        f.label.as_deref(),
        Some("model.p.stg does not output this column")
    );

    let f = finding_on(
        &out.findings,
        LintRule::UnknownColumnReference,
        "model.p.dup",
    );
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "two `nope` occurrences — no single position is honest: {f:?}"
    );
}

#[test]
fn forbidden_patterns_positions_mark_every_match() {
    let raw = "select unsafe_udf(x),\n       unsafe_udf(y)\nfrom t -- drop table stays readable";
    let m = manifest(vec![model_sql_pos("bad", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = config_from(
        "lint:\n  rules:\n    forbidden_patterns:\n      patterns: ['unsafe_udf', 'drop table']\n",
    );
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::ForbiddenPatterns, "model.p.bad");
    let (l, c, el, ec) = needle_pos(raw, "unsafe_udf", 0);
    assert_eq!(
        pos_of(f),
        ("models/bad.sql".to_string(), l, c, el, ec),
        "the first match is the primary position"
    );
    assert_eq!(f.label.as_deref(), Some("matches unsafe_udf"));
    assert_eq!(f.labels.len(), 2, "the other two matches ride as labels");
    let (l2, c2, el2, ec2) = needle_pos(raw, "unsafe_udf", 1);
    assert_eq!(
        (
            f.labels[0].line,
            f.labels[0].column,
            f.labels[0].end_line,
            f.labels[0].end_column
        ),
        (l2, Some(c2), Some(el2), Some(ec2)),
        "second `unsafe_udf` call"
    );
    let (l3, c3, el3, ec3) = needle_pos(raw, "drop table", 0);
    assert_eq!(
        (
            f.labels[1].line,
            f.labels[1].column,
            f.labels[1].end_line,
            f.labels[1].end_column
        ),
        (l3, Some(c3), Some(el3), Some(ec3)),
        "the comment's `drop table` — raw matches comments verbatim"
    );
    assert_eq!(f.labels[1].message, "matches drop table");
}

#[test]
fn select_star_positions_skip_a_jinja_dropped_union_arm() {
    // Raw carries two UNION arms, the judged (compiled) text only one —
    // a Jinja `{% if %}` dropped the other. Wildcard counts disagree, so
    // no arm's `*` may be underlined: the first raw wildcard belongs to
    // the arm the compiled text doesn't have.
    let mut m = model_sql_pos("arms", "select *\nfrom a\nunion all\nselect *\nfrom b");
    m.1.compiled_code = Some("select *\nfrom b".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.arms");
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "arm counts disagree — underlining either `*` picks an arm: {f:?}"
    );
    assert!(f.labels.is_empty());
}

#[test]
fn cartesian_join_positions_underline_a_jinja_tag_wholly() {
    // The conditionless join's relation is `{{ ref('x') }}`: the rendered
    // `"x"` maps back to the whole tag, so the underline covers the tag —
    // not the rendered name inside it.
    let raw = "select *\nfrom a\njoin {{ ref('x') }}";
    let m = manifest(vec![model_sql_pos("j", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::CartesianJoin, "model.p.j");
    let (l, c, el, ec) = needle_pos(raw, "{{ ref('x') }}", 0);
    assert_eq!(
        pos_of(f),
        ("models/j.sql".to_string(), l, c, el, ec),
        "the position covers the complete `{{ ref('x') }}` tag"
    );
}

#[test]
fn cartesian_join_positions_label_every_conditionless_arm() {
    let raw = "select *\nfrom a\njoin b.u\njoin c.v";
    let m = manifest(vec![model_sql_pos("j", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::CartesianJoin, "model.p.j");
    let (l, c, el, ec) = needle_pos(raw, "b.u", 0);
    assert_eq!(
        pos_of(f),
        ("models/j.sql".to_string(), l, c, el, ec),
        "the first conditionless relation is primary"
    );
    assert_eq!(f.labels.len(), 1, "the second arm rides as a label");
    let (l2, c2, el2, ec2) = needle_pos(raw, "c.v", 0);
    assert_eq!(
        (
            f.labels[0].line,
            f.labels[0].column,
            f.labels[0].end_line,
            f.labels[0].end_column
        ),
        (l2, Some(c2), Some(el2), Some(ec2)),
        "the label underlines `c.v`"
    );
    assert_eq!(f.labels[0].message, "no join condition");
}

#[test]
fn hard_coded_references_positions_underline_quoted_parts() {
    // `FROM "sch"."t"` — the position covers the whole qualified name,
    // quotes included.
    let raw = "select *\nfrom \"sch\".\"t\"";
    let m = manifest(vec![model_sql_pos("h", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::HardCodedReferences]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::HardCodedReferences, "model.p.h");
    let (l, c, el, ec) = needle_pos(raw, "\"sch\".\"t\"", 0);
    assert_eq!(
        pos_of(f),
        ("models/h.sql".to_string(), l, c, el, ec),
        "the qualified name underlines itself, quotes included"
    );
}

#[test]
fn unknown_column_reference_positions_skip_a_match_against_shadow() {
    // `nope` appears once in the projection and once inside MATCH(…)
    // AGAINST — two real occurrences, so "unique" can't be claimed.
    let m = manifest(
        vec![
            model_sql_cols("stg", "select 1", &[("id", None)], true),
            model_sql_pos(
                "ma",
                "select id, nope from stg\nwhere match (nope) against ('term')",
            ),
        ],
        vec![],
        &[("model.p.stg", "model.p.ma")],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnknownColumnReference]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(
        &out.findings,
        LintRule::UnknownColumnReference,
        "model.p.ma",
    );
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "two `nope` occurrences (projection + MATCH AGAINST) — no single position: {f:?}"
    );
}

#[test]
fn sql_rule_positions_never_point_into_a_python_model() {
    // A `.py` model's `raw_code` is Python text: a regex hit or a parsed
    // token there would underline Python, not SQL — no rule may attach a
    // position. The findings still report file-scoped.
    let mut py = model_sql_pos(
        "pymodel",
        "def model(dbt, session):\n    return dbt.ref('up')\n",
    );
    py.1.original_file_path = Some("models/pymodel.py".into());
    py.1.compiled_code = Some("select *\nfrom t".into()); // fires select_star
    let m = manifest(vec![py], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);

    let cfg = cfg_only(&[LintRule::SelectStar]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    let f = finding_on(&out.findings, LintRule::SelectStar, "model.p.pymodel");
    let l = f.location.as_ref().expect("the .py file is still named");
    assert_eq!(l.file, "models/pymodel.py");
    assert!(l.line.is_none(), "a .py file never gets a line: {f:?}");

    // forbidden_patterns matches the Python raw verbatim — position or
    // not, the file stays line-free.
    let cfg =
        config_from("lint:\n  rules:\n    forbidden_patterns:\n      patterns: ['dbt\\.ref']\n");
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    let f = finding_on(
        &out.findings,
        LintRule::ForbiddenPatterns,
        "model.p.pymodel",
    );
    let l = f.location.as_ref().expect("the .py file is still named");
    assert_eq!(l.file, "models/pymodel.py");
    assert!(l.line.is_none(), "a .py file never gets a line: {f:?}");
}

#[test]
fn sql_positions_are_identical_across_runs() {
    // Same dag + resolver evaluated twice — positions, labels and all.
    let raw = "select *\nfrom a\nunion all\nselect *\nfrom b";
    let m = manifest(vec![model_sql_pos("u", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::SelectStar]);
    let first = lint_outcome(&dag, &cfg, Some(&res), None);
    let second = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        first.findings, second.findings,
        "a position is a fact — memoized or recomputed it must not move"
    );
}

// ---- trailing_semicolon -------------------------------------------------------

#[test]
fn trailing_semicolon_flags_the_effective_tail_only() {
    let m = manifest(
        vec![
            model_sql("bare", "select 1;"),
            model_sql("ws", "select 1;  \n\t"),
            model_sql("line_comment", "select 1; -- done"),
            model_sql("block_comment", "select 1; /* tail */"),
            model_sql("jinja_tag", "select 1;{% endif %}"),
            model_sql("jinja_comment", "select 1;\n{# note #}"),
            model_sql("config", "{{ config(materialized='view') }}\nselect 1;"),
            model_sql("after_tag", "select {{ ref('x') }};"),
            model_sql("clean", "select 1"),
            model_sql("semi_in_comment", "select 1 -- ;"),
            model_sql("semi_in_literal", "select ';'"),
            model_sql("semi_in_block", "select 1 /* ; */"),
            // A `;` before a `{{ }}` that renders text is mid-file, not a
            // tail.
            model_sql("mid_file", "select 1; {{ more_sql() }}"),
            // A literal or a `{{ ref() }}` (renders `"x"`) after the `;`
            // is real content too — the `;` splits two statements.
            model_sql("literal_after", "select 1; 'x'"),
            model_sql("ref_after", "select 1;{{ ref('x') }}"),
            model_sql("stray_endif", "select 1 {% endif %}"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    // No resolver: a graph-only rule still runs (Requires::Graph).
    let cfg = cfg_only(&[LintRule::TrailingSemicolon]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::TrailingSemicolon),
        vec![
            "model.p.after_tag",
            "model.p.bare",
            "model.p.block_comment",
            "model.p.config",
            "model.p.jinja_comment",
            "model.p.jinja_tag",
            "model.p.line_comment",
            "model.p.ws",
        ],
        "only an effective-tail `;` flags — comments, literals and mid-file don't count"
    );
    assert!(
        out.unparsed.is_empty() && out.skipped.is_empty(),
        "a raw-text rule never parses and needs no resolver"
    );
}

#[test]
fn trailing_semicolon_flags_unparsable_sql_without_counting_unparsed() {
    // The rule never parses — a broken model that still ends in `;` flags
    // like any other, and no rule asked for a parse so `unparsed` stays
    // empty.
    let m = manifest(vec![model_sql("bad", "select from where or;")], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::TrailingSemicolon]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::TrailingSemicolon),
        vec!["model.p.bad"]
    );
    assert!(
        out.unparsed.is_empty(),
        "a raw-text rule contributes no unparsed entry"
    );
    assert_eq!(res.analyzed_count(), 0, "no parse ever happened");
}

#[test]
fn trailing_semicolon_never_judges_python() {
    // Both axes: a `language: python` model's raw isn't SQL, and a `.py`
    // `original_file_path` can't carry SQL either (source-mode Python
    // models are the same shape).
    let lang = model_lang("lang_py", "python", "def model(dbt, session):\n    x = 1;");
    let mut file = model_sql_pos("file_py", "x = 1;");
    file.1.original_file_path = Some("models/file_py.py".into());
    let m = manifest(vec![lang, file, model_sql("sql", "select 1;")], vec![], &[]);
    let dag = Dag::build(&m);
    let cfg = cfg_only(&[LintRule::TrailingSemicolon]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::TrailingSemicolon),
        vec!["model.p.sql"],
        "a tail `;` in Python is valid syntax, not a broken statement"
    );
}

#[test]
fn trailing_semicolon_positions_point_at_the_semicolon() {
    let raw = "select a\nfrom t;\n-- a trailing comment";
    let m = manifest(vec![model_sql_pos("m", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let cfg = cfg_only(&[LintRule::TrailingSemicolon]);
    let out = lint_outcome(&dag, &cfg, None, None);

    let f = finding_on(&out.findings, LintRule::TrailingSemicolon, "model.p.m");
    let (l, c, el, ec) = needle_pos(raw, ";", 0);
    assert_eq!(
        pos_of(f),
        ("models/m.sql".to_string(), l, c, el, ec),
        "the underline is the `;` itself"
    );
    assert_eq!(f.label.as_deref(), Some("trailing semicolon"));
    assert!(f.labels.is_empty());
}

// ---- duplicate_output_columns -------------------------------------------------

#[test]
fn duplicate_output_columns_flags_repeated_output_names() {
    let m = manifest(
        vec![
            model_sql("bare_dup", "select a, a from t"),
            model_sql("folded", "select A, a from t"),
            model_sql("aliased", "select x as a, y as a from t"),
            model_sql("two_names", "select a as x, b as x, c as y, d as y from t"),
            model_sql("quoted_diff", "select \"A\", a from t"),
            model_sql("quoted_dup", "select \"A\", \"A\" from t"),
            model_sql("clean", "select a, b from t"),
            // A second UNION arm's duplicate isn't the output name — the
            // first arm names every column.
            model_sql("second_arm", "select a, b from t union select x, x from u"),
            model_sql("first_arm", "select a, a from t union select x, y from u"),
            // `select 1, 1` — positional outputs never claim a name.
            model_sql("literals", "select 1, 1 from t"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::DuplicateOutputColumns),
        vec![
            "model.p.aliased",
            "model.p.bare_dup",
            "model.p.first_arm",
            "model.p.folded",
            "model.p.quoted_dup",
            "model.p.two_names",
        ],
        "explicit repeats flag; quoted-case, second-arm and literals don't"
    );
    let f = finding_on(
        &out.findings,
        LintRule::DuplicateOutputColumns,
        "model.p.two_names",
    );
    assert!(
        f.message.ends_with(": x, y"),
        "one finding names every repeated output: {}",
        f.message
    );
}

#[test]
fn duplicate_output_columns_counts_what_a_wildcard_carries() {
    // `*` over a resolvable parent expands its columns: `a` arrives via
    // the wildcard AND explicitly → dup. An unexpandable `*` contributes
    // nothing, so `*, a` alone stays clean.
    let m = manifest(
        vec![
            model_sql("stg", "select id, a from t"),
            model_sql("expanded", "select *, a from stg"),
            model_sql("expanded_qualified", "select stg.*, a from stg"),
            model_sql("unknown_star", "select *, a from extern"),
            model_sql("star_only", "select * from stg"),
        ],
        vec![],
        &[
            ("model.p.stg", "model.p.expanded"),
            ("model.p.stg", "model.p.expanded_qualified"),
            ("model.p.stg", "model.p.star_only"),
        ],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::DuplicateOutputColumns),
        vec!["model.p.expanded", "model.p.expanded_qualified"],
        "a wildcard's carried columns count; an unknown wildcard contributes nothing"
    );
}

#[test]
fn duplicate_output_columns_positions_pair_second_with_first() {
    let raw = "select a, b as x, c, d as x\nfrom t";
    let m = manifest(vec![model_sql_pos("m", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::DuplicateOutputColumns, "model.p.m");
    // `x` is the first (only) duplicated name: its second occurrence is
    // the primary position, the first a `first defined here` label.
    let (l, c, el, ec) = needle_pos(raw, "x", 1);
    assert_eq!(
        pos_of(f),
        ("models/m.sql".to_string(), l, c, el, ec),
        "the second occurrence is the primary position"
    );
    assert_eq!(f.label.as_deref(), Some("duplicate output name"));
    assert_eq!(f.labels.len(), 1);
    let (l2, c2, el2, ec2) = needle_pos(raw, "x", 0);
    assert_eq!(
        (
            f.labels[0].line,
            f.labels[0].column,
            f.labels[0].end_line,
            f.labels[0].end_column
        ),
        (l2, Some(c2), Some(el2), Some(ec2)),
        "the first occurrence rides as a label"
    );
    assert_eq!(f.labels[0].message, "first defined here");
}

#[test]
fn duplicate_output_columns_positions_skip_a_macro_changed_count() {
    // Compiled selects `a` three times; raw writes it twice — the counts
    // disagree so no occurrence pairs: the finding stays file-scoped.
    let mut m = model_sql_pos("m", "select a, a from t");
    m.1.compiled_code = Some("select a, a, a from t".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::DuplicateOutputColumns, "model.p.m");
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "a count mismatch leaves no position: {f:?}"
    );
}

#[test]
fn duplicate_output_columns_positions_pair_the_single_raw_name() {
    // Compiled's macro made the second `a`; raw wrote `a` once — the lone
    // occurrence can only be the first `a`, so it pairs as a
    // `first defined here` label while `x` (counted twice on both sides)
    // supplies the primary. A lone-name finding can't anchor that way —
    // the envelope drops labels that have no position to annotate.
    let raw = "select a, b as x, c as x from t";
    let mut m = model_sql_pos("m", raw);
    m.1.compiled_code = Some("select a, a, b as x, c as x from t".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::DuplicateOutputColumns, "model.p.m");
    let (l, c, el, ec) = needle_pos(raw, "x", 1);
    assert_eq!(
        pos_of(f),
        ("models/m.sql".to_string(), l, c, el, ec),
        "the pairable name's second occurrence is primary"
    );
    // `a`'s only raw occurrence rides as a `first defined here` label,
    // and `x`'s first does too.
    let firsts = f
        .labels
        .iter()
        .filter(|l| l.message == "first defined here")
        .count();
    assert_eq!(firsts, 2, "{:?}", f.labels);
    let (al, ac, ael, aec) = needle_pos(raw, "a", 0);
    let (xl, xc, xel, xec) = needle_pos(raw, "x", 0);
    let a_label = f
        .labels
        .iter()
        .find(|l| l.line == al && l.column == Some(ac))
        .expect("a's first occurrence labels");
    assert_eq!(
        (a_label.end_line, a_label.end_column),
        (Some(ael), Some(aec))
    );
    let x_label = f
        .labels
        .iter()
        .find(|l| l.line == xl && l.column == Some(xc))
        .expect("x's first occurrence labels");
    assert_eq!(
        (x_label.end_line, x_label.end_column),
        (Some(xel), Some(xec))
    );
}

#[test]
fn duplicate_output_columns_derived_names_flag_only_on_postgres_family() {
    // Un-aliased expressions take a name from their SHAPE — `sum(a)` →
    // `sum`, `CASE…` → `case`, `a::int` → `a`. Only Postgres and
    // Redshift really assign those names, so only there do the repeats
    // collide; Generic (and every dialect naming outputs by position or
    // expression text) sees no written-name collision. Written names —
    // aliases, identifiers — flag under either dialect.
    let m = manifest(
        vec![
            model_sql("sum_dup", "select sum(a), sum(b) from t"),
            model_sql("count_dup", "select count(*), count(a) from t"),
            model_sql(
                "case_dup",
                "select case when a > 0 then 1 end, case when b > 0 then 2 end from t",
            ),
            model_sql("cast_dup", "select a::int, a::varchar from t"),
            model_sql("aliased_dup", "select sum(a) as x, sum(b) as x from t"),
            model_sql("bare_dup", "select a, a from t"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);

    // Generic: only the written-name collisions survive.
    let res = sql_resolver(None);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::DuplicateOutputColumns),
        vec!["model.p.aliased_dup", "model.p.bare_dup"],
        "expression-shape names can't collide under Generic"
    );

    // Postgres + Redshift assign the derived names — every shape collides.
    for dialect in [SqlDialect::Postgres, SqlDialect::Redshift] {
        let res = SqlParseResolver::new(None, dialect);
        let out = lint_outcome(&dag, &cfg, Some(&res), None);
        assert_eq!(
            flagged(&out.findings, LintRule::DuplicateOutputColumns),
            vec![
                "model.p.aliased_dup",
                "model.p.bare_dup",
                "model.p.case_dup",
                "model.p.cast_dup",
                "model.p.count_dup",
                "model.p.sum_dup",
            ],
            "{dialect:?}: the derived output names collide for real"
        );
    }
}

// ---- unused_cte ---------------------------------------------------------------

#[test]
fn unused_cte_flags_only_unread_definitions() {
    let m = manifest(
        vec![
            model_sql("read", "with a as (select 1) select * from a"),
            model_sql("unread", "with a as (select 1) select 2"),
            // Read only through a sibling CTE — still a read.
            model_sql(
                "via_sibling",
                "with a as (select 1), b as (select * from a) select * from b",
            ),
            model_sql(
                "one_of_two",
                "with a as (select 1), b as (select 2) select * from b",
            ),
            // A recursive CTE's own body never counts as a read — only the
            // outer query's `from n` does.
            model_sql(
                "recursive_read",
                "with n(i) as (select 1 union all select i + 1 from n) select * from n",
            ),
            model_sql(
                "recursive_unread",
                "with n(i) as (select 1 union all select i + 1 from n) select 2",
            ),
            // A qualified name can't bind the CTE — `sch.t` is the table,
            // so the CTE stays unread.
            model_sql("shadowed", "with t as (select 1) select * from sch.t"),
            // A nested query sees the outer WITH.
            model_sql(
                "nested_read",
                "with a as (select 1) select * from (select * from a) d",
            ),
            // A CTE inside a derived table's own WITH is judged too.
            model_sql(
                "inner_with",
                "select * from (with b as (select 1) select 2) d",
            ),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnusedCte]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedCte),
        vec![
            "model.p.inner_with",
            "model.p.one_of_two",
            "model.p.recursive_unread",
            "model.p.shadowed",
            "model.p.unread",
        ],
        "reads from the body, siblings, or nested queries all count"
    );
    let f = finding_on(&out.findings, LintRule::UnusedCte, "model.p.one_of_two");
    assert!(
        f.message.ends_with(": a"),
        "only the unread CTE is named: {}",
        f.message
    );
}

#[test]
fn unused_cte_counts_a_name_a_macro_argument_carries() {
    // `{{ join_customers('customers', …) }}` renders `NULL` — the parse
    // can't see the join the macro emits — but the CTE name rides the
    // string literal, so `customers` isn't provably unread. A literal
    // naming a different CTE — or no literal at all — still flags.
    let m = manifest(
        vec![
            model_sql(
                "macro_arg",
                "with orders as (select 1), customers as (select 2)\nselect * from orders {{ join_customers('customers', 'orders.customer_id') }}",
            ),
            model_sql(
                "other_name",
                "with orders as (select 1), customers as (select 2)\nselect * from orders {{ join_customers('orders', 'id') }}",
            ),
            model_sql(
                "plain_unread",
                "with orders as (select 1), customers as (select 2)\nselect * from orders",
            ),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnusedCte]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedCte),
        vec!["model.p.other_name", "model.p.plain_unread"],
        "a CTE name a macro-call string literal carries counts as read — other literals don't"
    );
}

// ---- Jinja rendering inside literals, identifiers and item lists ------
// The shapes that used to go `unparsable` (or flag artifacts of the
// `NULL` placeholder render): a `{{ }}` inside a string literal, tags
// glued into identifier text, `{{ }}` in relation positions, and
// select-list macro lines.

#[test]
fn jinja_inside_literals_and_identifiers_still_parses() {
    // Every one of these shapes used to fail the parse. They now render
    // to parseable SQL, so nothing lands on `unparsed` — and none of the
    // generated placeholders produces a finding (`rel_frag`'s
    // `__dbtl_…_raw.tbl` is no written relation, `macro_cols`'s
    // `, __dbtl_…` item is no comma product).
    let m = manifest(
        vec![
            model_sql(
                "lit",
                "select id, '{{ var('x', 'a') }}' as tag\nfrom {{ ref('base') }}",
            ),
            model_sql(
                "cast_lit",
                "select cast('{{ var('last_run') }}' as timestamp) as t\nfrom {{ ref('base') }}",
            ),
            model_sql(
                "glued",
                "select {{ c }}_sum as s, total_{{ c }} as t\nfrom {{ ref('base') }}",
            ),
            model_sql("rel_frag", "select id\nfrom {{ var('s') }}_raw.tbl"),
            model_sql(
                "macro_cols",
                "select o.id as other\n{{ select_cols('o') }}\nfrom {{ ref('base') }} o",
            ),
            // An apostrophe inside a `{# #}` comment is comment text —
            // the comment still closes at `#}` and the model parses.
            model_sql(
                "cmt",
                "select a {# don't run this #} , b\nfrom {{ ref('base') }}",
            ),
            // A subquery's `select`/`from` stay inside the parens — the
            // trailing `{{ }}` is a new select item, not a `NULL` alias.
            model_sql(
                "subq",
                "select a, (select y from {{ var('t') }}) {{ m() }}\nfrom {{ ref('base') }}",
            ),
            // `{{ ref('events') }}_2024` — a glued ref renders a marked
            // generated name: parses AND reads no same-named relation.
            model_sql("ref_glued", "select id\nfrom {{ ref('events') }}_2024"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[
        LintRule::HardCodedReferences,
        LintRule::CartesianJoin,
        LintRule::DuplicateOutputColumns,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.unparsed.is_empty(),
        "every jinja-heavy shape reaches the rules: {:?}",
        out.unparsed
    );
    assert!(
        out.findings.is_empty(),
        "no generated placeholder may produce a finding: {:?}",
        out.findings
    );
}

#[test]
fn jinja_placeholders_never_fold_into_a_duplicate_column() {
    // `select {{ a() }}, {{ b() }}` used to render `NULL, NULL` — a fake
    // `duplicate output column: null`. Each tag emits a unique generated
    // identifier, so the items stay distinct; a real duplicate still
    // flags.
    let m = manifest(
        vec![
            model_sql(
                "two_vars",
                "select {{ a() }}, {{ b() }}\nfrom {{ ref('base') }}",
            ),
            model_sql("real_dup", "select x as a, y as a\nfrom {{ ref('base') }}"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::DuplicateOutputColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.unparsed.is_empty(),
        "the placeholder items parse: {:?}",
        out.unparsed
    );
    assert_eq!(
        flagged(&out.findings, LintRule::DuplicateOutputColumns),
        vec!["model.p.real_dup"],
        "generated names never collide — only the written duplicate flags"
    );
}

#[test]
fn a_ref_named_like_a_cte_does_not_read_it() {
    // `{{ ref('stg_x') }}` renders `"stg_x"` — byte-identical to the
    // in-file CTE it isn't — but the rendered span is generated, so it
    // isn't a read and `stg_x` still flags unread. A WRITTEN `from
    // stg_x` does read the CTE.
    let m = manifest(
        vec![
            model_sql(
                "via_ref",
                "with stg_x as (select 1 as id)\nselect o.id\nfrom {{ ref('stg_x') }} o",
            ),
            model_sql(
                "written",
                "with stg_x as (select 1 as id)\nselect stg_x.id\nfrom stg_x",
            ),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnusedCte]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedCte),
        vec!["model.p.via_ref"],
        "the generated ref() name isn't a read of the same-named CTE"
    );
    let f = finding_on(&out.findings, LintRule::UnusedCte, "model.p.via_ref");
    assert!(
        f.message.ends_with(": stg_x"),
        "the unread CTE is named: {}",
        f.message
    );
}

#[test]
fn jinja_generated_columns_never_report_as_unknown() {
    // A `{{ }}` expression against a contract-known parent renders a
    // generated identifier — the unknown-column rule must not flag the
    // marker name. A real missing column still flags.
    let m = manifest(
        vec![
            model_sql_cols("stg", "select 1", &[("id", None), ("amount", None)], true),
            model_sql("gen_col", "select id, {{ metric() }} from stg"),
            model_sql("typo", "select id, nope from stg"),
        ],
        vec![],
        &[
            ("model.p.stg", "model.p.gen_col"),
            ("model.p.stg", "model.p.typo"),
        ],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnknownColumnReference]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnknownColumnReference),
        vec!["model.p.typo"],
        "the generated `__dbtl_` name is no unknown column — `nope` is"
    );
}

#[test]
fn unused_cte_lists_every_unread_name_in_one_finding() {
    let raw = "with a as (select 1),\n     b as (select 2)\nselect 3";
    let m = manifest(vec![model_sql_pos("m", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnusedCte]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let fs: Vec<_> = out
        .findings
        .iter()
        .filter(|f| f.rule == LintRule::UnusedCte)
        .collect();
    assert_eq!(fs.len(), 1, "one finding carries both names");
    let f = fs[0];
    assert!(
        f.message.contains("a") && f.message.contains("b"),
        "{}",
        f.message
    );
    let (l, c, el, ec) = needle_pos(raw, "a", 0);
    assert_eq!(
        pos_of(f),
        ("models/m.sql".to_string(), l, c, el, ec),
        "the first unread CTE's name is primary"
    );
    assert_eq!(f.label.as_deref(), Some("never read"));
    let (l2, c2, el2, ec2) = needle_pos(raw, "b", 0);
    assert_eq!(f.labels.len(), 1);
    assert_eq!(
        (
            f.labels[0].line,
            f.labels[0].column,
            f.labels[0].end_line,
            f.labels[0].end_column
        ),
        (l2, Some(c2), Some(el2), Some(ec2)),
        "the second unread name rides as a label"
    );
}

#[test]
fn unused_cte_positions_pair_the_single_raw_decl() {
    // Raw declares one unread `a`; compiled's macro adds a second — the
    // name's counts disagree, but the lone written decl can only be an
    // unread `a`: the exactly-one pairing still underlines it.
    let raw = "with a as (select 1) select 2";
    let mut m = model_sql_pos("m", raw);
    m.1.compiled_code = Some("with a as (select 1), a as (select 2) select 3".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnusedCte]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::UnusedCte, "model.p.m");
    let (l, c, el, ec) = needle_pos(raw, "a", 0);
    assert_eq!(
        pos_of(f),
        ("models/m.sql".to_string(), l, c, el, ec),
        "the one raw `a` decl is unambiguously unread"
    );
    assert_eq!(f.label.as_deref(), Some("never read"));
}

#[test]
fn unused_cte_positions_skip_a_macro_shifted_count() {
    // Raw declares two unread `a`s; compiled adds a third — two written
    // `a`s can't tell which three judged decls they are, so nothing
    // underlines.
    let mut m = model_sql_pos("m", "with a as (select 1), a as (select 2) select 3");
    m.1.compiled_code =
        Some("with a as (select 1), a as (select 2), a as (select 4) select 3".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UnusedCte]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::UnusedCte, "model.p.m");
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "a count mismatch leaves no position: {f:?}"
    );
}

// ---- order_by_without_limit ---------------------------------------------------

#[test]
fn order_by_without_limit_flags_only_the_unbounded_final_sort() {
    let m = manifest(
        vec![
            model_sql("bare", "select a from t order by a"),
            model_sql("positional", "select a from t order by 1"),
            // An OFFSET isn't a row bound — the sort still can't survive
            // downstream.
            model_sql("offset_only", "select a from t order by a offset 2"),
            model_sql("limited", "select a from t order by a limit 10"),
            model_sql(
                "offset_limited",
                "select a from t order by a limit 10 offset 2",
            ),
            model_sql(
                "fetched",
                "select a from t order by a fetch first 5 rows only",
            ),
            model_sql("topped", "select top 5 a from t order by a"),
            // A window function's ORDER BY isn't the final sort.
            model_sql(
                "windowed",
                "select row_number() over (order by a) as rn from t",
            ),
            // CTE/subquery ORDER BYs never count.
            model_sql(
                "cte_sorted",
                "with x as (select a from t order by a) select * from x",
            ),
            model_sql(
                "derived_sorted",
                "select * from (select a from t order by a) d",
            ),
            // `order by` inside a string literal isn't a clause.
            model_sql("literal", "select 'order by a' as s from t"),
            // The parenthesized final query's own tail IS the model's.
            model_sql("parens", "(select a from t order by a)"),
            model_sql("parens_sorted", "(select a from t) order by a"),
            model_sql("parens_limited", "(select a from t order by a) limit 3"),
            model_sql("no_order", "select a from t"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::OrderByWithoutLimit]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::OrderByWithoutLimit),
        vec![
            "model.p.bare",
            "model.p.offset_only",
            "model.p.parens",
            "model.p.parens_sorted",
            "model.p.positional",
        ],
        "only the final, unbounded ORDER BY flags"
    );
}

#[test]
fn order_by_without_limit_positions_underline_the_expressions() {
    // sqlparser's span starts after the `ORDER BY` keyword — the
    // expressions get the underline.
    let raw = "select a\nfrom t\norder by a desc, b";
    let m = manifest(vec![model_sql_pos("m", raw)], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::OrderByWithoutLimit]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::OrderByWithoutLimit, "model.p.m");
    let (l, c, el, ec) = needle_pos(raw, "a desc, b", 0);
    assert_eq!(
        pos_of(f),
        ("models/m.sql".to_string(), l, c, el, ec),
        "the expressions span — `ORDER BY` itself stays outside"
    );
    assert_eq!(f.label.as_deref(), Some("ORDER BY without LIMIT"));
}

#[test]
fn order_by_without_limit_positions_stay_off_when_raw_bounds_it() {
    // The compiled text lost the `limit` to a macro — judged unbounded —
    // but the raw file's own outer ORDER BY IS bounded, so underlining it
    // would lie: file-scoped, never a line.
    let mut m = model_sql_pos("m", "select a from t order by a limit 10");
    m.1.compiled_code = Some("select a from t order by a".into());
    let m = manifest(vec![m], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::OrderByWithoutLimit]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);

    let f = finding_on(&out.findings, LintRule::OrderByWithoutLimit, "model.p.m");
    assert!(
        f.location.as_ref().is_none_or(|l| l.line.is_none()),
        "the raw query is bounded — no honest underline: {f:?}"
    );
}

// ---- new-rule suppression + modes ---------------------------------------------

#[test]
fn new_sql_rules_respect_exclude_and_meta_ignore() {
    // `exclude:` drops findings by selector; `meta.dbtl.ignore` suppresses
    // the named rule — one of each across the four rules.
    let mut ignored = model_sql("ignored_cte", "with a as (select 1) select 2");
    ignored.1.meta = serde_json::json!({"dbtl": {"ignore": ["unused_cte"]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let m = manifest(
        vec![
            model_sql("semi_excluded", "select 1;"),
            model_sql("semi_kept", "select 2;"),
            ignored,
            model_sql("kept_cte", "with b as (select 1) select 2"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let mut cfg =
        config_from("lint:\n  rules:\n    trailing_semicolon: {exclude: [\"semi_excluded\"]}\n");
    for r in LintRule::ALL {
        if !matches!(
            r,
            LintRule::TrailingSemicolon | LintRule::DuplicateOutputColumns | LintRule::UnusedCte
        ) {
            cfg.rules.insert(r, RuleEnabled::Off);
        }
    }
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::TrailingSemicolon),
        vec!["model.p.semi_kept"],
        "exclude drops the selected node"
    );
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedCte),
        vec!["model.p.kept_cte"],
        "meta.dbtl.ignore suppresses the named rule"
    );
}

#[test]
fn new_sql_rules_fire_in_source_mode() {
    // The same four facts come off a source-loaded Dag — `manifest_from_source`
    // carries each model file's text as `raw_code`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(dir.path().join("dbt_project.yml"), "name: p\n").unwrap();
    std::fs::write(dir.path().join("models/semi.sql"), "select 1;\n").unwrap();
    std::fs::write(dir.path().join("models/dup.sql"), "select a, a from t\n").unwrap();
    std::fs::write(
        dir.path().join("models/cte.sql"),
        "with a as (select 1) select 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("models/sorted.sql"),
        "select a from t order by a\n",
    )
    .unwrap();
    let sm = crate::manifest_from_source(dir.path()).unwrap();
    let dag = Dag::build(&sm);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[
        LintRule::TrailingSemicolon,
        LintRule::DuplicateOutputColumns,
        LintRule::UnusedCte,
        LintRule::OrderByWithoutLimit,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    for (rule, uid) in [
        (LintRule::TrailingSemicolon, "model.p.semi"),
        (LintRule::DuplicateOutputColumns, "model.p.dup"),
        (LintRule::UnusedCte, "model.p.cte"),
        (LintRule::OrderByWithoutLimit, "model.p.sorted"),
    ] {
        assert_eq!(
            flagged(&out.findings, rule),
            vec![uid],
            "{rule:?} fires under source mode"
        );
    }
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

#[test]
fn undeclared_warehouse_columns_flags_columns_the_docs_never_declared() {
    // `gap` declares two of the warehouse's three columns — the
    // undeclared one names itself in the catalog's own spelling. `whole`
    // declares everything (a `missing_in_warehouse` gap is the sibling
    // rule's job, not this one). `absent_node` declares but the catalog
    // doesn't know it — "unknown", not "wrong". `silent` declares NO
    // columns at all: a node making no column-level claim isn't
    // contradicted by the catalog (docs coverage is another rule's job).
    let m = manifest(
        vec![
            model_cols("gap", &[("id", None), ("total", None)]),
            model_cols("whole", &[("id", None)]),
            model_cols("absent_node", &[("x", None)]),
            model_cols("silent", &[]),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cat = catalog_json(
        r#"{"nodes": {
            "model.p.gap": {"columns": {"id": {"type": "INT"}, "total": {"type": "NUMERIC"}, "currency": {"type": "STRING"}}},
            "model.p.whole": {"columns": {"id": {"type": "INT"}, "undocumented_extra": {"type": "INT"}}},
            "model.p.silent": {"columns": {"a": {"type": "INT"}, "b": {"type": "INT"}}}
        }}"#,
    );
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UndeclaredWarehouseColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    assert_eq!(
        flagged(&out.findings, LintRule::UndeclaredWarehouseColumns),
        vec!["model.p.gap", "model.p.whole"],
        "extras the docs never declared flag; catalog-less and declaration-less nodes don't"
    );
    assert_eq!(
        finding_on(
            &out.findings,
            LintRule::UndeclaredWarehouseColumns,
            "model.p.gap"
        )
        .message,
        "gap has 1 undeclared warehouse column: currency"
    );
}

#[test]
fn undeclared_warehouse_columns_judges_seeds_and_snapshots_case_insensitively() {
    // model|seed|snapshot all carry declared columns; the compare is
    // case-insensitive (`ID` declared vs `id` recorded isn't a gap).
    let seed = (
        "seed.p.raw_seed".to_string(),
        RawNode {
            name: "raw_seed".into(),
            resource_type: "seed".into(),
            path: Some("seeds/raw_seed.csv".into()),
            columns: columns(&[("ID", None, None)]),
            ..Default::default()
        },
    );
    let snap = (
        "snapshot.p.snap".to_string(),
        RawNode {
            name: "snap".into(),
            resource_type: "snapshot".into(),
            path: Some("snapshots/snap.sql".into()),
            columns: columns(&[("id", None, None)]),
            ..Default::default()
        },
    );
    let m = manifest(vec![seed, snap], vec![], &[]);
    let dag = Dag::build(&m);
    let cat = catalog_json(
        r#"{"nodes": {
            "seed.p.raw_seed": {"columns": {"id": {"type": "INT"}}},
            "snapshot.p.snap": {"columns": {"id": {"type": "INT"}, "extra": {"type": "INT"}}}
        }}"#,
    );
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::UndeclaredWarehouseColumns]);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    assert_eq!(
        flagged(&out.findings, LintRule::UndeclaredWarehouseColumns),
        vec!["snapshot.p.snap"],
        "seeds compare too — case-insensitively"
    );
}

// ---- requirement-aware skipping & laziness ---------------------------------

#[test]
fn missing_inputs_report_skipped_rules_not_findings() {
    let m = manifest(vec![model_sql("m", "select 1")], vec![], &[]);
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();

    // No aux inputs at all: the eight Sql rules + three Catalog rules skip
    // (`trailing_semicolon` needs only the graph — it still runs).
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(out.skipped.len(), 11);
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
            LintRule::DuplicateOutputColumns,
            LintRule::UnusedCte,
            LintRule::OrderByWithoutLimit,
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
            LintRule::UndeclaredWarehouseColumns,
        ]
    );

    // A resolver alone still leaves the catalog rules skipped.
    let res = sql_resolver(None);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(out.skipped.len(), 3);
    assert!(out.skipped.iter().all(|s| s.missing == Requires::Catalog));

    // Disabled rules never skip — off is not "skipped".
    let mut cfg = LintConfig::default();
    cfg.rules.insert(LintRule::SelectStar, RuleEnabled::Off);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert!(
        !out.skipped.iter().any(|s| s.rule == LintRule::SelectStar),
        "a disabled rule produces neither findings nor a skip"
    );

    // …nor does a rule off via `categories` or the preset — an opt-out by
    // any layer is never counted as a skip.
    let mut cfg = LintConfig::default();
    cfg.categories.insert(Category::Sql, None);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert!(
        out.skipped
            .iter()
            .all(|s| s.rule.info().category != Category::Sql),
        "a category-off sql rule produces neither findings nor a skip"
    );
    let cfg = LintConfig {
        preset: LintPreset::Minimal,
        ..LintConfig::default()
    };
    let out = lint_outcome(&dag, &cfg, None, None);
    assert!(
        out.skipped
            .iter()
            .all(|s| { LintPreset::Minimal.enables(s.rule.info()) }),
        "a preset-off rule produces neither findings nor a skip"
    );
}

#[test]
fn presets_define_the_base_rule_set() {
    // minimal = exactly the error-default rules; standard = the registry's
    // `default_enabled` set; strict = everything — the three opt-in rules
    // included.
    let minimal: Vec<_> = LintRule::ALL
        .iter()
        .copied()
        .filter(|r| LintPreset::Minimal.enables(r.info()))
        .collect();
    assert_eq!(
        minimal,
        LintRule::ALL
            .iter()
            .copied()
            .filter(|r| r.info().default_severity == Severity::Error)
            .collect::<Vec<_>>()
    );
    let standard: Vec<_> = LintRule::ALL
        .iter()
        .copied()
        .filter(|r| LintPreset::Standard.enables(r.info()))
        .collect();
    assert_eq!(
        standard,
        LintRule::ALL
            .iter()
            .copied()
            .filter(|r| r.info().default_enabled)
            .collect::<Vec<_>>()
    );
    // standard sits strictly below ALL while opt-in rules ship.
    assert!(standard.len() < LintRule::ALL.len());
    assert!(LintRule::ALL
        .iter()
        .all(|r| LintPreset::Strict.enables(r.info())));
    // …and an opt-in rule is off under minimal/standard, on under strict.
    for opt_in in [
        LintRule::UndocumentedExposures,
        LintRule::ModelTooLong,
        LintRule::LayerMaterializations,
    ] {
        assert!(!LintPreset::Minimal.enables(opt_in.info()));
        assert!(!LintPreset::Standard.enables(opt_in.info()));
        assert!(LintPreset::Strict.enables(opt_in.info()));
        assert!(!LintConfig::default().enabled(opt_in));
    }
    // The config layer reads the same predicates: `strict` runs them,
    // `standard` doesn't.
    let cfg = LintConfig {
        preset: LintPreset::Strict,
        ..LintConfig::default()
    };
    assert!(cfg.enabled(LintRule::SelectStar));
    assert!(cfg.enabled(LintRule::UndocumentedExposures));
    assert!(cfg.enabled(LintRule::ModelTooLong));
    assert!(cfg.enabled(LintRule::LayerMaterializations));
    // A `minimal` config still runs the error-default rules at their
    // registry severity.
    let cfg = LintConfig {
        preset: LintPreset::Minimal,
        ..LintConfig::default()
    };
    assert_eq!(
        cfg.severity(LintRule::MissingPrimaryKeyTests),
        Some(Severity::Error)
    );
    assert_eq!(cfg.severity(LintRule::SelectStar), None);
}

#[test]
fn category_settings_resolve_above_the_preset() {
    // A category `error`/`warn` repins the whole category — preset-off
    // rules included; `off` disables it.
    let mut cfg = LintConfig {
        preset: LintPreset::Minimal,
        categories: [
            (Category::Sql, Some(Severity::Warn)),
            (Category::Testing, None),
        ]
        .into_iter()
        .collect(),
        ..LintConfig::default()
    };
    assert_eq!(
        cfg.severity(LintRule::SelectStar),
        Some(Severity::Warn),
        "category severity lifts a preset-off rule"
    );
    assert_eq!(
        cfg.severity(LintRule::MissingPrimaryKeyTests),
        None,
        "category off disables even the preset's rules"
    );
    // An `On(None)` per-rule entry defers its severity to the category —
    // `true` only enables, it doesn't pin.
    cfg.rules
        .insert(LintRule::SelectStar, RuleEnabled::On(None));
    assert_eq!(cfg.severity(LintRule::SelectStar), Some(Severity::Warn));
    // …but a pinned per-rule severity ignores the category's repin.
    cfg.rules
        .insert(LintRule::SelectStar, RuleEnabled::On(Some(Severity::Error)));
    assert_eq!(cfg.severity(LintRule::SelectStar), Some(Severity::Error));
    // …and `Off` stays off.
    cfg.rules.insert(LintRule::SelectStar, RuleEnabled::Off);
    assert_eq!(cfg.severity(LintRule::SelectStar), None);
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
            cfg.rules.insert(r, RuleEnabled::Off);
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
        LintRule::DuplicateOutputColumns,
        LintRule::UnusedCte,
        LintRule::OrderByWithoutLimit,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        res.analyzed_count(),
        2,
        "seven SQL rules × two models = one shared analysis each"
    );
    assert_eq!(
        res.hard_coded_count(),
        2,
        "the marked re-parse is one per raw-carrying model"
    );
    assert_eq!(out.findings.len(), 4); // a + b flag select_star AND hard_coded
}

// ---- rule accuracy boundaries (parse coverage, python, pruning, dialects) ----

/// A model literal carrying `language` — `python` is the only value the
/// SQL guard distinguishes.
fn model_lang(name: &str, language: &str, raw: &str) -> (String, RawNode) {
    let mut m = model_sql(name, raw);
    m.1.language = Some(language.into());
    m
}

#[test]
fn sql_rules_never_judge_a_python_model() {
    // The `.py` model's raw_code is Python — a coincidental `select *`-like
    // string and a `dbt.ref(` call must not produce findings NOR an
    // `unparsed` entry: the model is out of the rules' scope, not a parse
    // failure. `forbidden_patterns` (raw-text) is bound by the same guard.
    let mut py = model_lang(
        "pymodel",
        "python",
        "def model(dbt, session):\n    return dbt.ref('up')\n",
    );
    // The compiled artifact WOULD trip every SQL rule — a wildcard, a
    // written relation, a conditionless join, a repeated output, an
    // unread CTE, an unbounded sort — so a missing language guard can't
    // hide behind a clean model.
    py.1.compiled_code =
        Some("with u as (select 1) select *, a, a from t cross join v order by a".into());
    let m = manifest(vec![py, model_sql("clean", "select 1")], vec![], &[]);
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[
        LintRule::SelectStar,
        LintRule::HardCodedReferences,
        LintRule::CartesianJoin,
        LintRule::UnknownColumnReference,
        LintRule::ForbiddenPatterns,
        LintRule::DuplicateOutputColumns,
        LintRule::UnusedCte,
        LintRule::OrderByWithoutLimit,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.findings.is_empty(),
        "a python model produces no SQL finding: {:?}",
        out.findings
    );
    assert!(
        out.unparsed.is_empty(),
        "a python model is out of scope, not unparsed"
    );
    assert_eq!(
        res.analyzed_count(),
        1,
        "only the SQL model was ever parsed"
    );
}

#[test]
fn unparsed_counts_models_whose_sql_wont_parse_once() {
    // `unparsable` has raw SQL that fails to parse (its uid lands on
    // `unparsed` once though seven SQL rules each asked for its facts);
    // `no_sql` carries none at all (never judged, never counted);
    // `compiled_only` has no raw — `hard_coded_relations` declines it but
    // that's a deliberate non-judgment, not a parse failure.
    let m = manifest(
        vec![
            model_sql("unparsable", "select from where or"),
            model("no_sql", "staging/no_sql.sql"),
            model_compiled_only("compiled_only", "select 1"),
            model_sql("clean", "select 1"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[
        LintRule::SelectStar,
        LintRule::HardCodedReferences,
        LintRule::CartesianJoin,
        LintRule::UnknownColumnReference,
        LintRule::DuplicateOutputColumns,
        LintRule::UnusedCte,
        LintRule::OrderByWithoutLimit,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        out.unparsed.iter().collect::<Vec<_>>(),
        vec!["model.p.unparsable"],
        "each unparsable model lands once, however many rules asked"
    );
}

// ---- pathological SQL depth ------------------------------------------------
//
// A flat infix/set-op chain (`x and …`, `x || …`, `union all …`) builds
// a left-deep AST spine the parser writes iteratively but no resolver,
// scan, or drop walk can recurse on the smallest stacks the binary
// runs on — the model must degrade to `unparsed` instead of aborting.
// Each test lints it on a 1 MiB thread: the run returning at all is
// what the bound buys.

/// Lint `sql` (one model named `deep`) with the seven parse-backed SQL
/// rules on a 1 MiB-stack thread, returning the outcome.
fn deep_sql_linted(sql: &str) -> LintOutcome {
    let m = manifest(
        vec![model_sql("deep", sql), model_sql("ok", "select a from t")],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn_scoped(s, || {
                let res = sql_resolver(None);
                let cfg = cfg_only(&[
                    LintRule::SelectStar,
                    LintRule::HardCodedReferences,
                    LintRule::CartesianJoin,
                    LintRule::UnknownColumnReference,
                    LintRule::DuplicateOutputColumns,
                    LintRule::UnusedCte,
                    LintRule::OrderByWithoutLimit,
                ]);
                lint_outcome(&dag, &cfg, Some(&res), None)
            })
            .expect("spawn the lint thread")
            .join()
            .expect("lint on a 1 MiB stack must finish, not abort")
    })
}

/// The deep model lands on `unparsed`; the plain one beside it still
/// analyzes — the bound degrades per model.
fn assert_deep_unparsed(out: &LintOutcome) {
    assert!(
        out.unparsed.iter().any(|u| u == "model.p.deep"),
        "the over-depth model must read as unparsable"
    );
    assert!(
        !out.unparsed.iter().any(|u| u == "model.p.ok"),
        "an ordinary model still parses on the same run"
    );
}

#[test]
fn deep_and_chain_degrades_to_unparsed_on_a_small_stack() {
    // ~20k `and` terms — the AND spine from the original report.
    let sql = format!(
        "select a from t where {}",
        (0..20_000)
            .map(|i| format!("a = {i}"))
            .collect::<Vec<_>>()
            .join(" and ")
    );
    assert_deep_unparsed(&deep_sql_linted(&sql));
}

#[test]
fn deep_concat_chain_degrades_to_unparsed_on_a_small_stack() {
    // ~20k `||` terms — the concat spine.
    let sql = format!("select {}", vec!["a"; 20_000].join(" || 'x' || "));
    assert_deep_unparsed(&deep_sql_linted(&sql));
}

#[test]
fn deep_union_chain_degrades_to_unparsed_on_a_small_stack() {
    // ~12k `union all` arms — the set-operation spine.
    let sql = (0..12_000)
        .map(|i| format!("select {i} as a"))
        .collect::<Vec<_>>()
        .join(" union all ");
    assert_deep_unparsed(&deep_sql_linted(&sql));
}

#[test]
fn statementless_rendered_sql_is_judged_not_unparsed() {
    // A render producing no STATEMENTS — whitespace, comments, an
    // output-less `{{ config() }}` — parses to zero statements: the
    // analysis succeeds with empty facts, the model is judged (nothing
    // to flag), and it never lands on `unparsed`. Only text that IS SQL
    // but fails to parse counts.
    let m = manifest(
        vec![
            model_sql("blank", "  \n\t\n"),
            model_sql("comments", "-- just a comment\n/* a block */\n"),
            model_sql("config_only", "{{ config(materialized='view') }}\n"),
            model_sql("broken", "select from where or"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[
        LintRule::SelectStar,
        LintRule::HardCodedReferences,
        LintRule::CartesianJoin,
        LintRule::DuplicateOutputColumns,
        LintRule::UnusedCte,
        LintRule::OrderByWithoutLimit,
    ]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert!(
        out.findings.is_empty(),
        "statementless models produce no findings: {:?}",
        out.findings
    );
    assert_eq!(
        out.unparsed.iter().collect::<Vec<_>>(),
        vec!["model.p.broken"],
        "whitespace/comments/config-only are judged empty — only real unparsable SQL counts"
    );
}

#[test]
fn forbidden_patterns_never_adds_to_unparsed() {
    // A raw-text rule: even against unparsable SQL it judges the text it
    // can see — it asks for no parse, so it records no unparsed entry.
    let m = manifest(
        vec![model_sql("bad", "select from where or -- unsafe_udf")],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg =
        config_from("lint:\n  rules:\n    forbidden_patterns:\n      patterns: ['unsafe_udf']\n");
    let mut cfg = cfg;
    for r in LintRule::ALL {
        if r != LintRule::ForbiddenPatterns {
            cfg.rules.insert(r, RuleEnabled::Off);
        }
    }
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::ForbiddenPatterns),
        vec!["model.p.bad"]
    );
    assert!(out.unparsed.is_empty(), "a raw-text rule never parses");
}

/// A `metadata.project_name`-pinned manifest so `PackageScope::Root`
/// prunes the `dep` package without an ambiguity warning.
fn multi_package(m: &mut RawManifest) {
    m.metadata.project_name = Some("p".into());
}

#[test]
fn root_models_skips_pruned_package_parents_and_unresolved_refs() {
    // `child`'s only parent lives in an installed package — after the
    // Root-scope prune it reads parentless, but the PrunedAdjacent record
    // says otherwise. `cross_pkg` refs a model source mode never parsed:
    // the unresolved `ref()` is still a dependency call, so the model is
    // under-connected, not a root. `bare` is the real root.
    let mut dep = model("base", "dep/base.sql");
    dep.0 = "model.dep.base".into();
    let mut child = model_sql_pos("stg_child", "select * from {{ ref('base') }}");
    child.0 = "model.p.stg_child".into();
    let mut cross = model_sql_pos(
        "stg_cross",
        "select * from {{ ref('dep', 'other_pkg_model') }}",
    );
    cross.0 = "model.p.stg_cross".into();
    let mut m = manifest(
        vec![dep, child, cross, model("stg_bare", "staging/stg_bare.sql")],
        vec![],
        &[("model.dep.base", "model.p.stg_child")],
    );
    multi_package(&mut m);
    let (dag, _) = crate::PackageScope::Root.apply_with_warnings(Dag::build(&m));
    let cfg = cfg_only(&[LintRule::RootModels]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::RootModels),
        vec!["model.p.stg_bare"],
        "pruned-package parents and unresolved refs are connectivity, not roots"
    );
    assert_eq!(
        dag.pruned_adjacent("model.p.stg_child"),
        Some(&crate::PrunedAdjacent {
            parents: 1,
            children: 0
        })
    );
    // The record survives a presentation `filtered` cut — subgraph
    // narrowing must not lose the package boundary.
    let sub = dag.filtered(|n| n.unique_id == "model.p.stg_child");
    assert_eq!(
        sub.pruned_adjacent("model.p.stg_child"),
        Some(&crate::PrunedAdjacent {
            parents: 1,
            children: 0
        })
    );
}

#[test]
fn unused_sources_skips_sources_only_packages_read() {
    // `s.pkg_read`'s only consumer is an installed-package model: after
    // the Root prune the source has no visible child — but something DID
    // read it, so it is not unused. `s.dead` is genuinely unread.
    let mut pkg = model("pkg_model", "dep/pkg_model.sql");
    pkg.0 = "model.dep.pkg_model".into();
    let mut m = manifest(
        vec![pkg],
        vec![source("s.pkg_read"), source("s.dead")],
        &[("source.p.s.pkg_read", "model.dep.pkg_model")],
    );
    multi_package(&mut m);
    let (dag, _) = crate::PackageScope::Root.apply_with_warnings(Dag::build(&m));
    let cfg = cfg_only(&[LintRule::UnusedSources]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::UnusedSources),
        vec!["source.p.s.dead"],
        "a source a package model consumes is used — just not visibly"
    );
    assert_eq!(
        dag.pruned_adjacent("source.p.s.pkg_read"),
        Some(&crate::PrunedAdjacent {
            parents: 0,
            children: 1
        })
    );
}

#[test]
fn missing_primary_key_tests_accepts_constraints_only_when_opted_in() {
    // Model-level and column-level `primary_key` constraints are NOT a
    // test guarantee under the default config — most warehouses never
    // enforce them — and substitute only under the explicit opt-in.
    let mut model_pk = model("stg_model_pk", "staging/stg_model_pk.sql");
    model_pk.1.constraints = vec![crate::manifest::RawConstraint {
        constraint_type: Some("primary_key".into()),
        ..Default::default()
    }];
    let mut col_pk = model("stg_col_pk", "staging/stg_col_pk.sql");
    col_pk.1.columns = serde_json::Map::from_iter([(
        "id".to_string(),
        serde_json::json!({
            "name": "id",
            "constraints": [{"type": "primary_key", "columns": null}]
        }),
    )]);
    let m = manifest(
        vec![
            model_pk,
            col_pk,
            model("stg_tested", "staging/stg_tested.sql"),
            model("stg_bare", "staging/stg_bare.sql"),
            test_on("stg_tested", "unique", "id"),
            test_on("stg_tested", "not_null", "id"),
        ],
        vec![],
        &[],
    );
    let cfg = cfg_only(&[LintRule::MissingPrimaryKeyTests]);
    let dag = Dag::build(&m);
    let out = lint_outcome(&dag, &cfg, None, None);
    let mut got = flagged(&out.findings, LintRule::MissingPrimaryKeyTests);
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            "model.p.stg_bare",
            "model.p.stg_col_pk",
            "model.p.stg_model_pk"
        ],
        "constraints are not tests by default"
    );
    let cfg = config_from(
        "lint:\n  rules:\n    missing_primary_key_tests:\n      accept_primary_key_constraint: true\n",
    );
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::MissingPrimaryKeyTests),
        vec!["model.p.stg_bare"],
        "opted in, either constraint level stands in for the test pair"
    );
}

#[test]
fn undocumented_rules_honour_their_resource_types() {
    // Default `[model]` checks models only; widening to
    // `resource_types: [model, seed, snapshot, source]` sweeps the rest —
    // `undocumented_columns` included (a source's bare column flags, and
    // its yml chain descends [source, table, column]).
    let mut seed = (
        "seed.p.raw_seed".to_string(),
        RawNode {
            name: "raw_seed".into(),
            resource_type: "seed".into(),
            path: Some("seeds/raw_seed.csv".into()),
            ..Default::default()
        },
    );
    seed.1.columns = columns(&[("id", None, None)]);
    let mut snap = (
        "snapshot.p.snap".to_string(),
        RawNode {
            name: "snap".into(),
            resource_type: "snapshot".into(),
            path: Some("snapshots/snap.sql".into()),
            ..Default::default()
        },
    );
    snap.1.columns = columns(&[("id", None, None)]);
    let mut src = source("s.raw");
    src.1.columns = columns(&[("id", None, None)]);
    let m = manifest(
        vec![model("stg_bare", "staging/stg_bare.sql"), seed, snap],
        vec![src],
        &[],
    );
    let dag = Dag::build(&m);
    let cfg = cfg_only(&[LintRule::UndocumentedModels, LintRule::UndocumentedColumns]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::UndocumentedModels),
        vec!["model.p.stg_bare"],
        "the default checks models only"
    );
    assert_eq!(
        flagged(&out.findings, LintRule::UndocumentedColumns),
        Vec::<&str>::new(),
        "the default model has no declared columns"
    );
    let cfg = config_from(
        "lint:\n  rules:\n    undocumented_models:\n      resource_types: [model, seed, snapshot]\n    undocumented_columns:\n      resource_types: [model, seed, snapshot, source]\n",
    );
    let out = lint_outcome(&dag, &cfg, None, None);
    let mut got = flagged(&out.findings, LintRule::UndocumentedModels);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.stg_bare", "seed.p.raw_seed", "snapshot.p.snap"]
    );
    let mut got = flagged(&out.findings, LintRule::UndocumentedColumns);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["seed.p.raw_seed", "snapshot.p.snap", "source.p.s.raw"],
        "sources sweep too — their declared columns need descriptions"
    );
}

#[test]
fn a_source_column_finding_descends_source_table_column() {
    // `undocumented_columns` on a source (opted in above) points at the
    // column's own `- name:` inside the table's `columns:` — the
    // [source, table, column] chain — never the table's entry.
    let yml = "version: 2\n\nsources:\n  - name: s\n    tables:\n      - name: raw\n        columns:\n          - name: id\n";
    assert_eq!(
        yaml_name_line(yml, &names(&["s", "raw", "id"])),
        Some(8),
        "the three-part chain lands on the column entry"
    );
}

#[test]
fn layer_violation_only_judges_data_flow_layers_on_root_nodes() {
    // `utilities/` carries no data-flow direction — an edge touching it is
    // never a violation. `unknown/` maps to no layer either. And under a
    // multi-package graph a package-side endpoint is context, never the
    // flagged edge. Only marts→staging is the real backward jump.
    let mut pkg_marts = model("pkg_marts", "marts/pkg_marts.sql");
    pkg_marts.0 = "model.dep.pkg_marts".into();
    let mut m = manifest(
        vec![
            model("stg_a", "staging/stg_a.sql"),
            model("stg_utils", "staging/stg_utils.sql"),
            model("util_helper", "utilities/util_helper.sql"),
            model("other_x", "unknown/other_x.sql"),
            model("fct_back", "marts/fct_back.sql"),
            pkg_marts,
        ],
        vec![],
        &[
            // utilities -> staging: backward by rank, but not a data layer.
            ("model.p.util_helper", "model.p.stg_utils"),
            // unmapped dir -> staging: no direction to compare.
            ("model.p.other_x", "model.p.stg_a"),
            // The genuine violation.
            ("model.p.fct_back", "model.p.stg_a"),
            // Package-side backward edge: context only.
            ("model.dep.pkg_marts", "model.p.stg_a"),
        ],
    );
    multi_package(&mut m);
    let dag = Dag::build(&m);
    let cfg = cfg_only(&[LintRule::LayerViolation]);
    let out = lint_outcome(&dag, &cfg, None, None);
    assert_eq!(
        flagged(&out.findings, LintRule::LayerViolation),
        vec!["model.p.stg_a"],
        "utilities/unmapped/package edges never count"
    );
    let f = finding_on(&out.findings, LintRule::LayerViolation, "model.p.stg_a");
    assert_eq!(f.related, vec!["model.p.fct_back".to_string()]);
}

#[test]
fn incremental_without_unique_key_ignores_strategies_that_never_merge() {
    // `append`, `insert_overwrite`, `microbatch` take no unique_key in
    // dbt — flagging them is a false positive. `merge`/`delete+insert`
    // still flag, with the strategy named.
    let strategy = |name: &str, s: &str| {
        let mut m = keyed_model(name, "incremental", None);
        m.1.config.incremental_strategy = Some(s.into());
        m
    };
    let m = manifest(
        vec![
            strategy("app", "append"),
            strategy("over", "insert_overwrite"),
            strategy("micro", "microbatch"),
            strategy("merged", "merge"),
            strategy("delins", "delete+insert"),
            keyed_model("plain", "incremental", None),
        ],
        vec![],
        &[],
    );
    let cfg = cfg_only(&[LintRule::IncrementalWithoutUniqueKey]);
    let dag = Dag::build(&m);
    let out = lint_outcome(&dag, &cfg, None, None);
    let mut got = flagged(&out.findings, LintRule::IncrementalWithoutUniqueKey);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.delins", "model.p.merged", "model.p.plain"],
        "non-merge strategies take no unique_key"
    );
    let f = finding_on(
        &out.findings,
        LintRule::IncrementalWithoutUniqueKey,
        "model.p.merged",
    );
    assert!(
        f.message.contains("incremental_strategy: merge"),
        "the configured strategy names itself: {}",
        f.message
    );
}

#[test]
fn hard_coded_references_honours_ignore_globs_and_information_schema() {
    let m = manifest(
        vec![
            model_sql("audit", "select * from staging.tmp_audit"),
            model_sql("legacy", "select * from analytics.legacy_orders"),
            model_sql("meta", "select * from db.information_schema.columns"),
            model_sql("plain", "select * from analytics.orders"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = config_from(
        "lint:\n  rules:\n    hard_coded_references:\n      ignore: ['*.tmp_*', 'legacy_orders']\n",
    );
    let mut cfg = cfg;
    for r in LintRule::ALL {
        if r != LintRule::HardCodedReferences {
            cfg.rules.insert(r, RuleEnabled::Off);
        }
    }
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::HardCodedReferences),
        vec!["model.p.plain"],
        "globs drop matching names (by suffix too) — information_schema never reaches the set"
    );
}

#[test]
fn cartesian_join_flags_comma_from_and_tautological_conditions() {
    let m = manifest(
        vec![
            model_sql("comma", "select * from a.t, b.u"),
            model_sql(
                "comma_filtered",
                "select * from a.t, b.u where a.t.id = b.u.id",
            ),
            model_sql("on_true", "select * from a.t join b.u on true"),
            model_sql("on_one", "select * from a.t join b.u on 1 = 1"),
            model_sql("on_literal", "select * from a.t join b.u on 'x' = 'x'"),
            model_sql(
                "on_columns",
                "select * from a.t join b.u on a.t.id = a.t.id",
            ),
            model_sql("comma_unnest", "select * from a.t, unnest(a.t.arr) as x"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    let mut got = flagged(&out.findings, LintRule::CartesianJoin);
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            "model.p.comma",
            "model.p.on_literal",
            "model.p.on_one",
            "model.p.on_true"
        ],
        "comma-FROM with no WHERE and never-rejecting ONs are cross products; a column self-comparison stays a condition"
    );
}

#[test]
fn cartesian_join_ignores_the_jinja_branch_union_comma() {
    // A `{% elif %}`/`{% else %}` split between FROM items emits `,` —
    // the render's branch union (the arms never reach one render), not
    // a written comma product. A literal `,` — before the branch — is
    // still one.
    let m = manifest(
        vec![
            model_sql(
                "branch",
                "select *\nfrom {% if target.name == 'prod' %}a.t{% else %}b.u{% endif %}",
            ),
            model_sql(
                "elif_branch",
                "select *\nfrom {% if x %}a.t{% elif y %}b.u{% else %}c.v{% endif %}",
            ),
            model_sql(
                "written",
                "select *\nfrom a.t, {% if x %}b.u{% else %}c.v{% endif %}",
            ),
            model_sql("plain_comma", "select *\nfrom a.t, b.u"),
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
        vec!["model.p.plain_comma", "model.p.written"],
        "the else-split ',' is a branch union, not a written product; a literal ',' still counts"
    );
}

#[test]
fn cartesian_join_never_flags_an_explicit_cross_join() {
    // `CROSS JOIN` is the intentional spelling of the product — the rule
    // never reads its constraint. (MySQL parses `CROSS JOIN t ON cond`
    // as its INNER JOIN spelling; even there the operator alone decides.)
    let m = manifest(
        vec![
            model_sql(
                "cross_on",
                "select * from a.t cross join b.u on a.t.id = b.u.id",
            ),
            model_sql("cross_bare", "select * from a.t cross join b.u"),
            model_sql("cross_true", "select * from a.t cross join b.u on true"),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = SqlParseResolver::new(None, SqlDialect::MySql);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    assert_eq!(
        flagged(&out.findings, LintRule::CartesianJoin),
        Vec::<String>::new(),
        "an explicit CROSS JOIN is the intentional product spelling — never a finding"
    );
}

#[test]
fn cartesian_join_skips_correlated_and_file_factors() {
    // `t.arr` after `t` binds reads `t`'s array column — a per-row
    // expansion of the SAME relation, not a second one — so neither the
    // comma nor a JOIN arm carrying it is a product. A file literal is
    // an explicit external read, likewise not a relation pair. A real
    // second relation still flags, comma or `JOIN`.
    let m = manifest(
        vec![
            model_sql("corr_comma", "select t.id, x from a.t t, t.arr as x"),
            model_sql("corr_join", "select t.id from a.t t join t.arr on true"),
            model_sql("corr_nested", "select * from a.t t, b.u, t.more"),
            model_sql("file_comma", "select * from a.t, 'data/x.parquet'"),
            model_sql("real_comma", "select * from a.t, b.u"),
            model_sql("real_join", "select * from a.t join b.u"),
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
        vec![
            "model.p.corr_nested",
            "model.p.real_comma",
            "model.p.real_join"
        ],
        "correlated array factors and file literals aren't products; `b.u` in corr_nested still is"
    );
}

#[test]
fn cartesian_join_message_uses_the_raw_join_text() {
    // The judged text renders `{{ ref('up') }}` to the relation name —
    // the message quotes the WRITTEN arm `{{ ref('up') }} b` when the
    // raw render's arm count matches the judged count (positions pair).
    let m = manifest(
        vec![
            model_sql("up", "select 1 as id"),
            model_sql_pos(
                "paired",
                "select * from {{ ref('up') }} a, {{ ref('up') }} b",
            ),
        ],
        vec![],
        &[("model.p.up", "model.p.paired")],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let cfg = cfg_only(&[LintRule::CartesianJoin]);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    let f = finding_on(&out.findings, LintRule::CartesianJoin, "model.p.paired");
    assert!(
        f.message.contains("{{ ref('up') }} b"),
        "the written join arm text, not the rendered name: {}",
        f.message
    );

    // Without raw SQL there's nothing to slice — the judged text (the
    // compiled relation name) is what the message falls back to.
    let m = manifest(
        vec![model_compiled_only(
            "compiled_only",
            "select * from up a, up b",
        )],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let res = sql_resolver(None);
    let out = lint_outcome(&dag, &cfg, Some(&res), None);
    let f = finding_on(
        &out.findings,
        LintRule::CartesianJoin,
        "model.p.compiled_only",
    );
    assert!(
        f.message.contains("up b"),
        "unpaired positions keep the judged text: {}",
        f.message
    );
}

#[test]
fn hard_coded_references_skips_files_metadata_and_correlated_names() {
    // `dual` is a row constructor; `information_schema` at any
    // qualification depth (including a name whole-quoted so its dots
    // live inside one identifier) is a metadata read; a `'file'`
    // literal is an explicit external read; and `t.arr`/`t.sub` headed
    // by a bound alias reads that relation's column, not a relation.
    // Genuinely written names still flag.
    let m = manifest(
        vec![
            model_sql("dual", "select 1 from dual"),
            model_sql("is_deep", "select * from db.information_schema.columns"),
            model_sql(
                "is_quoted",
                "select * from `proj.us.INFORMATION_SCHEMA.JOBS`",
            ),
            model_sql("file", "select * from 'data/x.parquet'"),
            model_sql("corr", "select t.id, x from raw.orders t, t.arr as x"),
            model_sql(
                "corr_nested",
                "select * from raw.orders t where exists (select 1 from t.sub)",
            ),
            model_sql("real", "select * from analytics.orders"),
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
        vec!["model.p.corr", "model.p.corr_nested", "model.p.real"],
        "only the written relations flag — the correlated tails never name one"
    );
    let msg_of = |uid| {
        out.findings
            .iter()
            .find(|f| f.node_uid == uid)
            .unwrap()
            .message
            .clone()
    };
    assert_eq!(
        msg_of("model.p.corr"),
        "corr reads relation without ref()/source(): raw.orders"
    );
    assert_eq!(
        msg_of("model.p.corr_nested"),
        "corr_nested reads relation without ref()/source(): raw.orders"
    );
    assert_eq!(
        msg_of("model.p.real"),
        "real reads relation without ref()/source(): analytics.orders"
    );
}

#[test]
fn warehouse_type_drift_compares_under_the_resolved_dialect() {
    // `bigint` declared vs an `INT64` catalog: a false positive under
    // Generic (no shared alias family), equivalent under BigQuery (its
    // only integer type is INT64). The same run also exercises the
    // case-insensitive catalog column lookup.
    let m = manifest(
        vec![model_cols(
            "m",
            &[("amount", Some("bigint")), ("ID", Some("STRING"))],
        )],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cat = catalog_json(
        r#"{"nodes": {"model.p.m": {"columns": {"amount": {"type": "INT64"}, "id": {"type": "STRING"}}}}}"#,
    );
    let cfg = cfg_only(&[LintRule::WarehouseTypeDrift]);

    // Generic: INT64 and bigint are different names — the drift stands.
    let res = SqlParseResolver::new(Some(cat.clone()), SqlDialect::Generic);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    assert_eq!(
        flagged(&out.findings, LintRule::WarehouseTypeDrift),
        vec!["model.p.m"],
        "generic knows no int64↔bigint alias"
    );

    // BigQuery: INT64 IS bigint — no drift, and the `ID`/`id` case gap
    // still resolves.
    let res = SqlParseResolver::new(Some(cat.clone()), SqlDialect::BigQuery);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    assert!(
        flagged(&out.findings, LintRule::WarehouseTypeDrift).is_empty(),
        "the resolver's dialect flows into the comparison"
    );
}

#[test]
fn warehouse_type_drift_honours_snowflake_number_semantics() {
    // Snowflake `NUMBER` is integer-shaped only at scale 0: declared
    // `int` vs `NUMBER`/`NUMBER(38,0)` agree, `int` vs `NUMBER(10,2)`
    // drifts — and a bare `varchar` promise accepts `VARCHAR(255)` while
    // `varchar(10)` doesn't.
    let m = manifest(
        vec![
            model_cols("exact", &[("amount", Some("int"))]),
            model_cols("scaled", &[("amount", Some("int"))]),
            model_cols("bare_width", &[("name", Some("varchar"))]),
            model_cols("pinned_width", &[("name", Some("varchar(10)"))]),
        ],
        vec![],
        &[],
    );
    let dag = Dag::build(&m);
    let cat = catalog_json(
        r#"{"nodes": {
            "model.p.exact": {"columns": {"amount": {"type": "NUMBER(38,0)"}}},
            "model.p.scaled": {"columns": {"amount": {"type": "NUMBER(10,2)"}}},
            "model.p.bare_width": {"columns": {"name": {"type": "VARCHAR(255)"}}},
            "model.p.pinned_width": {"columns": {"name": {"type": "VARCHAR(255)"}}}
        }}"#,
    );
    let res = SqlParseResolver::new(Some(cat.clone()), SqlDialect::Snowflake);
    let cfg = cfg_only(&[LintRule::WarehouseTypeDrift]);
    let out = lint_outcome(&dag, &cfg, Some(&res), Some(&cat));
    let mut got = flagged(&out.findings, LintRule::WarehouseTypeDrift);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.pinned_width", "model.p.scaled"],
        "scale-0 NUMBER is int-shaped, (10,2) is not; a declared width is a real promise"
    );
}

#[test]
fn lint_input_dialect_follows_the_resolver_then_the_override() {
    // `new` auto-resolves from the dag adapter; `with_sql` adopts the
    // resolver's; `with_dialect` pins over both.
    let m = manifest(vec![model("a", "staging/a.sql")], vec![], &[]);
    let dag = Dag::build(&m);
    let cfg = LintConfig::default();
    assert_eq!(
        LintInput::new(&dag, &cfg).dialect,
        SqlDialect::Generic,
        "no adapter metadata → generic"
    );
    let res = SqlParseResolver::new(None, SqlDialect::Snowflake);
    assert_eq!(
        LintInput::new(&dag, &cfg).with_sql(Some(&res)).dialect,
        SqlDialect::Snowflake,
        "the resolver's dialect is adopted — SQL and catalog rules agree"
    );
    assert_eq!(
        LintInput::new(&dag, &cfg)
            .with_sql(Some(&res))
            .with_dialect(Some(SqlDialect::BigQuery))
            .dialect,
        SqlDialect::BigQuery,
        "an explicit pin wins"
    );
    assert_eq!(
        LintInput::new(&dag, &cfg)
            .with_sql(Some(&res))
            .with_dialect(None)
            .dialect,
        SqlDialect::Snowflake,
        "a None pin changes nothing"
    );
}

// ---- required_meta_keys.resource_types ----------------------------------------

/// A `seed`/`snapshot` node literal — `required_meta_keys` widens to them.
fn node_of(name: &str, resource_type: &str, path: &str) -> (String, RawNode) {
    (
        format!("{resource_type}.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: resource_type.into(),
            path: Some(path.into()),
            ..Default::default()
        },
    )
}

#[test]
fn required_meta_keys_resource_types_defaults_to_models_only() {
    // A meta-less exposure / source / seed / snapshot sit next to a
    // meta-less model: with no `resource_types` the rule still answers on
    // models alone — the shipped `[model]` default.
    let mut m = manifest(
        vec![
            model("stg_bare", "staging/stg_bare.sql"),
            node_of("raw_seed", "seed", "seeds/raw_seed.csv"),
            node_of("snap", "snapshot", "snapshots/snap.sql"),
        ],
        vec![source("s.raw")],
        &[],
    );
    m.exposures.extend([exposure("exp")]);
    let cfg = config_from("lint:\n  rules:\n    required_meta_keys: {keys: [owner]}\n");
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::RequiredMetaKeys),
        vec!["model.p.stg_bare"],
        "the default resource_types is [model] — nothing else is checked"
    );
}

#[test]
fn required_meta_keys_resource_types_widens_to_each_kind() {
    // Every kind in the allowed set participates: the tagged exposure and
    // source carry `owner` (meta present is present, any value), the seed,
    // snapshot, bare source and bare exposure don't.
    let mut with_meta = source("s.tagged");
    with_meta.1.meta = serde_json::json!({"owner": "t"})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut exp_tagged = exposure("exp_tagged");
    exp_tagged.1.meta = serde_json::json!({"owner": "t"})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut stg_tagged = model("stg_tagged", "staging/stg_tagged.sql");
    stg_tagged.1.meta = serde_json::json!({"owner": "t"})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut m = manifest(
        vec![
            stg_tagged,
            node_of("raw_seed", "seed", "seeds/raw_seed.csv"),
            node_of("snap", "snapshot", "snapshots/snap.sql"),
        ],
        vec![with_meta, source("s.bare")],
        &[],
    );
    m.exposures.extend([exp_tagged, exposure("exp_bare")]);
    let cfg = config_from(
        "lint:\n  rules:\n    required_meta_keys: {keys: [owner], resource_types: [model, seed, snapshot, source, exposure]}\n",
    );
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::RequiredMetaKeys);
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            "exposure.p.exp_bare",
            "seed.p.raw_seed",
            "snapshot.p.snap",
            "source.p.s.bare",
        ],
        "every listed kind is checked against the merged meta it carries"
    );
    // Narrowing back to a subset re-scopes — exposures alone.
    let cfg = config_from(
        "lint:\n  rules:\n    required_meta_keys: {keys: [owner], resource_types: [exposure]}\n",
    );
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::RequiredMetaKeys),
        vec!["exposure.p.exp_bare"],
        "resource_types: [exposure] checks exposures alone"
    );
}

#[test]
fn exposure_meta_lands_on_node_detail_in_both_modes() {
    // The manifest wire's `exposure.meta` is dbt's merged map — source mode
    // must land the identical merge (top-level `meta:` under `config.meta`,
    // per key) so `required_meta_keys` reads the same dict either way.
    let mut exp = exposure("exp");
    exp.1.meta = serde_json::json!({"owner": "cfg_wins", "team": "t"})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut m = manifest(vec![model("stg_a", "staging/stg_a.sql")], vec![], &[]);
    m.exposures.extend([exp]);
    let dag = Dag::build(&m);
    let meta = &dag.detail("exposure.p.exp").unwrap().meta;
    assert_eq!(meta["owner"], "cfg_wins");
    assert_eq!(meta["team"], "t");

    // Source mode: an `exposures:` entry with both spellings — `config:
    // meta:` wins per key, non-conflicting top-level keys survive, and a
    // `required_meta_keys` run on it behaves identically to the manifest
    // side.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(dir.path().join("dbt_project.yml"), "name: p\nprofile: p\n").unwrap();
    std::fs::write(dir.path().join("models/stg_a.sql"), "select 1").unwrap();
    std::fs::write(
        dir.path().join("models/exposures.yml"),
        "version: 2\nexposures:\n  - name: exp\n    type: dashboard\n    depends_on:\n      - ref('stg_a')\n    owner:\n      name: t\n    meta:\n      owner: top_level\n      team: t\n    config:\n      meta:\n        owner: cfg_wins\n",
    )
    .unwrap();
    let sm = crate::manifest_from_source(dir.path()).unwrap();
    let sdag = Dag::build(&sm);
    let meta = &sdag.detail("exposure.p.exp").unwrap().meta;
    assert_eq!(meta["owner"], "cfg_wins", "config.meta wins per key");
    assert_eq!(
        meta["team"], "t",
        "a non-conflicting top-level key survives"
    );
    // The identical finding answers off both dags.
    let cfg = config_from(
        "lint:\n  rules:\n    required_meta_keys: {keys: [owner, domain], resource_types: [exposure]}\n",
    );
    let m_f = findings_of(&m, &cfg);
    let s_f = run_rules(LintInput::new(&sdag, &cfg)).findings;
    for f in [&m_f, &s_f] {
        let hit = f
            .iter()
            .find(|f| f.rule == LintRule::RequiredMetaKeys)
            .unwrap_or_else(|| panic!("required_meta_keys must fire: {f:?}"));
        assert_eq!(hit.node_uid, "exposure.p.exp");
        assert!(
            hit.message.contains("domain"),
            "only `domain` is missing from the merged meta: {}",
            hit.message
        );
    }
}

#[test]
fn exposures_tree_meta_merges_below_entry_and_enabled_parks() {
    // `exposures:` tree config keys by the exposure fqn — the project
    // name, the declaring yml's dirs, the name. `+meta` sits at the
    // BOTTOM of the merge (entry `meta:` overrides it, `config: meta:`
    // wins overall) and `+enabled: false` parks the exposure, exactly
    // like the other resource trees.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models/marts")).unwrap();
    std::fs::create_dir_all(dir.path().join("models/parked")).unwrap();
    std::fs::write(dir.path().join("models/stg_a.sql"), "select 1").unwrap();
    std::fs::write(
        dir.path().join("dbt_project.yml"),
        "name: p\nprofile: p\nexposures:\n  p:\n    marts:\n      +meta:\n        domain: from_tree\n        team: tree_loses\n    parked:\n      +enabled: false\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("models/marts/exp.yml"),
        "version: 2\nexposures:\n  - name: exp\n    type: dashboard\n    owner:\n      name: t\n    meta:\n      team: entry_wins\n    config:\n      meta:\n        owner: cfg_wins\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("models/parked/exp2.yml"),
        "version: 2\nexposures:\n  - name: exp2\n    type: dashboard\n    owner:\n      name: t\n",
    )
    .unwrap();
    let sm = crate::manifest_from_source(dir.path()).unwrap();
    let sdag = Dag::build(&sm);
    let meta = &sdag
        .detail("exposure.p.exp")
        .expect("the marts exposure survives")
        .meta;
    assert_eq!(meta["domain"], "from_tree", "tree +meta fills a gap");
    assert_eq!(meta["team"], "entry_wins", "entry meta: beats the tree");
    assert_eq!(meta["owner"], "cfg_wins", "config.meta beats both");
    assert!(
        sdag.get("exposure.p.exp2").is_none(),
        "exposures: +enabled: false parks the exposure"
    );
}

// ---- undocumented_exposures ---------------------------------------------------

#[test]
fn undocumented_exposures_is_opt_in_and_flags_blank_descriptions() {
    let mut documented = exposure("exp_documented");
    documented.1.description = Some("What the dashboard answers".into());
    let mut blank = exposure("exp_blank");
    blank.1.description = Some("   ".into());
    let mut m = manifest(vec![model("stg_a", "staging/stg_a.sql")], vec![], &[]);
    m.exposures
        .extend([documented, blank, exposure("exp_missing")]);
    // The shipped default leaves the rule off — a missing description is
    // not yet a finding.
    let f = findings_of(&m, &LintConfig::default());
    assert!(
        flagged(&f, LintRule::UndocumentedExposures).is_empty(),
        "opt-in: the default config stays silent"
    );
    let cfg = config_from("lint:\n  rules:\n    undocumented_exposures: true\n");
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::UndocumentedExposures);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["exposure.p.exp_blank", "exposure.p.exp_missing"],
        "missing and whitespace-only descriptions both flag; documented doesn't"
    );
    // …and the strict preset switches it on the same way.
    let cfg = LintConfig {
        preset: LintPreset::Strict,
        ..LintConfig::default()
    };
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::UndocumentedExposures).len(),
        2,
        "strict runs the opt-in rule"
    );
}

// ---- model_too_long -----------------------------------------------------------

/// A SQL file of `counted` non-blank lines.
fn sql_of(counted: usize) -> String {
    (0..counted)
        .map(|i| format!("select {i} as c{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn model_too_long_is_opt_in_and_counts_content_lines() {
    // Four counted lines: the raw file is padded with blanks, a one-line
    // `{# #}` comment, and a multi-line comment block — none of which
    // count.
    let raw = "\
{# a header comment #}
\n\
{# a comment
   spanning lines #}\n\
select 1 as a\n\
\n\
   \n\
select 2 as b\n\
{# trailing comment #}\n\
select 3 as c\n\
select 4 as d\n";
    let m = manifest(
        vec![
            sited_model("long_model", "models/marts/long_model.sql", raw),
            sited_model(
                "short_model",
                "models/marts/short_model.sql",
                "select 1\nselect 2\n",
            ),
        ],
        vec![],
        &[],
    );
    // Off under the shipped default.
    let f = findings_of(&m, &LintConfig::default());
    assert!(
        flagged(&f, LintRule::ModelTooLong).is_empty(),
        "opt-in: the default config stays silent"
    );
    // max_lines 4 → exactly at the budget is clean (only OVER flags).
    let cfg = config_from("lint:\n  rules:\n    model_too_long: {max_lines: 4}\n");
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::ModelTooLong).is_empty(),
        "at max_lines exactly is under the bar"
    );
    // max_lines 3 → long_model crosses on its fourth counted line.
    let cfg = config_from("lint:\n  rules:\n    model_too_long: {max_lines: 3}\n");
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::ModelTooLong),
        vec!["model.p.long_model"],
        "only the over-budget file flags"
    );
    let hit = finding_of(&f, LintRule::ModelTooLong, "model.p.long_model");
    assert!(
        hit.message.contains("4 counted lines") && hit.message.contains("max_lines budget of 3"),
        "the message reports the true count over the budget: {}",
        hit.message
    );
    // The position is the (max_lines + 1)-th COUNTED line — the raw file's
    // last line here — never a line-1 guess.
    let loc = hit.location.as_ref().expect("a position lands");
    assert_eq!(loc.file.as_str(), "models/marts/long_model.sql");
    assert_eq!(
        loc.line,
        Some(11),
        "the fourth counted line is raw line 11 (comments and blanks didn't count)"
    );
}

#[test]
fn model_too_long_skips_python_models_and_missing_sql() {
    let mut py = model("py_model", "marts/py_model.py");
    py.1.language = Some("python".into());
    py.1.original_file_path = Some("models/marts/py_model.py".into());
    let mut sql_node = sited_model("sqlless", "models/marts/sqlless.sql", "x");
    sql_node.1.raw_code = None;
    let m = manifest(vec![py, sql_node], vec![], &[]);
    let cfg = config_from("lint:\n  rules:\n    model_too_long: {max_lines: 1}\n");
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::ModelTooLong).is_empty(),
        "a python model and a model without raw SQL are never judged"
    );
    // A real 300-line default budget stays silent for ordinary files.
    let cfg = config_from("lint:\n  rules:\n    model_too_long: true\n");
    let m = manifest(
        vec![sited_model(
            "ordinary",
            "models/marts/ordinary.sql",
            &sql_of(50),
        )],
        vec![],
        &[],
    );
    let f = findings_of(&m, &cfg);
    assert!(
        flagged(&f, LintRule::ModelTooLong).is_empty(),
        "the default max_lines is 300"
    );
}

// ---- layer_materializations ---------------------------------------------------

/// A model literal under `path` carrying `config.materialized` — the layer
/// resolves off the path's first dir (or the name prefix), the shape off
/// the config.
fn mat_model_in(name: &str, path: &str, materialized: &str) -> (String, RawNode) {
    (
        format!("model.p.{name}"),
        RawNode {
            name: name.into(),
            resource_type: "model".into(),
            path: Some(path.into()),
            config: crate::manifest::RawConfig {
                materialized: Some(materialized.into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

#[test]
fn layer_materializations_is_opt_in_and_flags_disallowed_shapes() {
    // staging table + marts ephemeral violate the shipped defaults; a
    // staging view and an incremental marts model conform; `other`-layer
    // and unmaterialized models are unchecked.
    let m = manifest(
        vec![
            mat_model_in("stg_persisted", "staging/stg_persisted.sql", "table"),
            mat_model_in("stg_view", "staging/stg_view.sql", "view"),
            mat_model_in("fct_ephemeral", "marts/fct_ephemeral.sql", "ephemeral"),
            mat_model_in("fct_incr", "marts/fct_incr.sql", "incremental"),
            mat_model_in("ops_table", "ops/ops_table.sql", "table"),
            model("fct_unmat", "marts/fct_unmat.sql"),
        ],
        vec![],
        &[],
    );
    // Off under the shipped default.
    let f = findings_of(&m, &LintConfig::default());
    assert!(
        flagged(&f, LintRule::LayerMaterializations).is_empty(),
        "opt-in: the default config stays silent"
    );
    let cfg = config_from("lint:\n  rules:\n    layer_materializations: true\n");
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::LayerMaterializations);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.fct_ephemeral", "model.p.stg_persisted"],
        "the defaults allow staging view/ephemeral, marts table/incremental/view; \
         'other' and materialization-less models are unchecked"
    );
}

#[test]
fn layer_materializations_reports_layer_shape_and_allowed_set() {
    let m = manifest(
        vec![mat_model_in(
            "stg_persisted",
            "staging/stg_persisted.sql",
            "table",
        )],
        vec![],
        &[],
    );
    let cfg = config_from("lint:\n  rules:\n    layer_materializations: true\n");
    let f = findings_of(&m, &cfg);
    let hit = finding_of(&f, LintRule::LayerMaterializations, "model.p.stg_persisted");
    assert_eq!(
        hit.message,
        "staging model stg_persisted materializes as 'table' (allowed: view, ephemeral)",
        "the message names the layer, the shape, and the allowed list"
    );
}

#[test]
fn layer_materializations_allowed_overlays_one_layer_at_a_time() {
    // `allowed: {staging: [table]}` turns the staging policy on its head —
    // while marts keeps the shipped default (per-layer merge, not a
    // whole-map replace), and a layer mapped to `[]` is unchecked like a
    // `naming_prefixes` empty list.
    let m = manifest(
        vec![
            mat_model_in("stg_view", "staging/stg_view.sql", "view"),
            mat_model_in("stg_table", "staging/stg_table.sql", "table"),
            mat_model_in("fct_eph", "marts/fct_eph.sql", "ephemeral"),
        ],
        vec![],
        &[],
    );
    let cfg =
        config_from("lint:\n  rules:\n    layer_materializations: {allowed: {staging: [table]}}\n");
    let f = findings_of(&m, &cfg);
    let mut got = flagged(&f, LintRule::LayerMaterializations);
    got.sort_unstable();
    assert_eq!(
        got,
        vec!["model.p.fct_eph", "model.p.stg_view"],
        "staging flipped to table-only; marts kept its default (ephemeral still flags)"
    );
    // `{staging: []}` opts staging out of the check entirely.
    let cfg =
        config_from("lint:\n  rules:\n    layer_materializations: {allowed: {staging: []}}\n");
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::LayerMaterializations),
        vec!["model.p.fct_eph"],
        "an empty allowed list means 'no policy' for that layer"
    );
}

#[test]
fn layer_materializations_reads_custom_naming_prefix_layers() {
    // A `lint.naming_prefixes` layer is a valid `allowed` key — the layer
    // resolves the same way `lint_model_type` does (prefix first, then
    // directory).
    let m = manifest(
        vec![
            mat_model_in("sem_view", "semantic/sem_view.sql", "view"),
            mat_model_in("sem_table", "semantic/sem_table.sql", "table"),
        ],
        vec![],
        &[],
    );
    let cfg = config_from(
        "lint:\n  naming_prefixes: {semantic: [sem_]}\n  rules:\n    layer_materializations: {allowed: {semantic: [table]}}\n",
    );
    let f = findings_of(&m, &cfg);
    assert_eq!(
        flagged(&f, LintRule::LayerMaterializations),
        vec!["model.p.sem_view"],
        "the custom layer's allowed list applies to prefix-matched models"
    );
}
