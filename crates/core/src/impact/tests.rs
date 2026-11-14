use super::*;
use crate::manifest::{RawManifest, RawNode};
use crate::{compute_diff, Dag, PackageScope};
use std::collections::HashMap;

/// A two-model manifest `stg_x -> fct_x` (staging feeding a mart — the
/// legal direction), with `x`'s SQL and tests parameterized.
fn two_model_manifest() -> RawManifest {
    let mut nodes = HashMap::from([
        (
            "model.p.stg_x".to_string(),
            RawNode {
                name: "stg_x".into(),
                resource_type: "model".into(),
                path: Some("staging/stg_x.sql".into()),
                raw_code: Some("select 1".into()),
                ..Default::default()
            },
        ),
        (
            "model.p.fct_x".to_string(),
            RawNode {
                name: "fct_x".into(),
                resource_type: "model".into(),
                path: Some("marts/fct_x.sql".into()),
                raw_code: Some("select 2".into()),
                ..Default::default()
            },
        ),
    ]);
    nodes.get_mut("model.p.fct_x").unwrap().depends_on.nodes = vec!["model.p.stg_x".to_string()];
    RawManifest {
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),

        parent_map: HashMap::from([(
            "model.p.fct_x".to_string(),
            vec!["model.p.stg_x".to_string()],
        )]),
        child_map: HashMap::from([(
            "model.p.stg_x".to_string(),
            vec!["model.p.fct_x".to_string()],
        )]),
        docs: Default::default(),
        disabled: Default::default(),
        ref_scope: None,
    }
}

/// `stg_x` (owner=carol, CHANGED) feeding two downstream models owned by
/// alice and bob — the affected-owners population.
fn owner_manifest() -> RawManifest {
    let model = |name: &str, path: &str, owner: Option<&str>| {
        (
            format!("model.p.{name}"),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(path.into()),
                raw_code: Some("select 1".into()),
                meta: owner
                    .map(|o| {
                        serde_json::Map::from_iter([(
                            "owner".to_string(),
                            serde_json::Value::from(o),
                        )])
                    })
                    .unwrap_or_default(),
                ..Default::default()
            },
        )
    };
    let mut nodes = HashMap::from([
        model("stg_x", "staging/stg_x.sql", Some("carol")),
        model("fct_x", "marts/fct_x.sql", Some("alice")),
        model("mart_b", "marts/mart_b.sql", Some("bob")),
        model("orphan", "marts/orphan.sql", Some("mallory")),
    ]);
    // stg_x → fct_x → mart_b; orphan is downstream of nothing.
    nodes.get_mut("model.p.fct_x").unwrap().depends_on.nodes = vec!["model.p.stg_x".to_string()];
    nodes.get_mut("model.p.mart_b").unwrap().depends_on.nodes = vec!["model.p.fct_x".to_string()];
    RawManifest {
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::from([
            (
                "model.p.fct_x".to_string(),
                vec!["model.p.stg_x".to_string()],
            ),
            (
                "model.p.mart_b".to_string(),
                vec!["model.p.fct_x".to_string()],
            ),
        ]),
        child_map: HashMap::from([
            (
                "model.p.stg_x".to_string(),
                vec!["model.p.fct_x".to_string()],
            ),
            (
                "model.p.fct_x".to_string(),
                vec!["model.p.mart_b".to_string()],
            ),
        ]),
        docs: Default::default(),
        disabled: Default::default(),
        ref_scope: None,
    }
}

#[test]
fn affected_owners_collects_distinct_downstream_owners() {
    let base = Dag::build(&owner_manifest());
    // Change stg_x's SQL → modified; fct_x + mart_b are its downstream.
    let mut m = owner_manifest();
    m.nodes.get_mut("model.p.stg_x").unwrap().raw_code = Some("select 2".into());
    let current = Dag::build(&m);
    let diff = compute_diff(&base, &current);
    let pr = pr_impact(&current, Some(&base), &diff, &ImpactOpts::default());
    assert_eq!(
        pr.affected_owners,
        vec!["alice".to_string(), "bob".to_string()],
        "distinct downstream owners, sorted — the changed node's own owner \
             (carol) and the untouched orphan's (mallory) are both excluded"
    );
    // A different `owner_key` reads a different meta key (here: nothing).
    let pr = pr_impact(
        &current,
        Some(&base),
        &diff,
        &ImpactOpts {
            owner_key: "team",
            ..ImpactOpts::default()
        },
    );
    assert!(pr.affected_owners.is_empty());
    // The markdown section lists them.
    let report = diff_report(
        &current,
        Some(&base),
        &diff,
        "b",
        PackageScope::Root,
        &ImpactOpts::default(),
    );
    let md = pr_impact_markdown("p", &report);
    assert!(md.contains("## Affected owners (2)\n- alice\n- bob\n"));
}

#[test]
fn pr_impact_reports_blast_radius_ci_and_untested_changes() {
    let base = Dag::build(&two_model_manifest());
    // Current: same graph, but fct_x's SQL changed → modified, buildable,
    // and (no tests on it) an untested change.
    let mut m = two_model_manifest();
    m.nodes.get_mut("model.p.fct_x").unwrap().raw_code = Some("select 22".into());
    let current = Dag::build(&m);
    let diff = compute_diff(&base, &current);
    let pr = pr_impact(&current, Some(&base), &diff, &ImpactOpts::default());

    assert_eq!(pr.untested_changes, vec!["fct_x".to_string()]);
    assert_eq!(pr.ci_command.as_deref(), Some("dbt build --select fct_x+"));
    assert_eq!(pr.affected_models, 0, "fct_x has no downstream");
    assert!(pr.new_layer_violations.is_empty());
}

#[test]
fn new_layer_violation_is_diffed_against_the_baseline() {
    let base = Dag::build(&two_model_manifest());
    // Flip the edge: fct_x (marts) now feeds stg_x (staging) — a violation.
    let mut m = two_model_manifest();
    m.parent_map = HashMap::from([(
        "model.p.stg_x".to_string(),
        vec!["model.p.fct_x".to_string()],
    )]);
    m.child_map = HashMap::from([(
        "model.p.fct_x".to_string(),
        vec!["model.p.stg_x".to_string()],
    )]);
    m.nodes.get_mut("model.p.stg_x").unwrap().depends_on.nodes = vec!["model.p.fct_x".to_string()];
    m.nodes.get_mut("model.p.fct_x").unwrap().depends_on.nodes = vec![];
    let current = Dag::build(&m);
    let diff = compute_diff(&base, &current);
    let pr = pr_impact(&current, Some(&base), &diff, &ImpactOpts::default());
    assert_eq!(
        pr.new_layer_violations,
        vec![("fct_x".to_string(), "stg_x".to_string())],
        "the flipped edge is a NEW violation vs the legal baseline"
    );
    // And the same violation present in BOTH sides is not "new".
    let pr = pr_impact(
        &current,
        Some(&current),
        &compute_diff(&current, &current),
        &ImpactOpts::default(),
    );
    assert!(pr.new_layer_violations.is_empty());
}

#[test]
fn coverage_counts_matches_the_buildable_base() {
    let dag = Dag::build(&two_model_manifest());
    assert_eq!(coverage_counts(&dag, None), (0, 2), "two models, no tests");
}

#[test]
fn column_coverage_counts_counts_tested_declared_columns() {
    // stg_x declares 2 columns (id tested, extra not); fct_x declares 1
    // untested column; a third model declares none and drops out of the
    // denominator entirely.
    let mut m = two_model_manifest();
    m.nodes.get_mut("model.p.stg_x").unwrap().columns = serde_json::json!({
        "id": {"name": "id"},
        "extra": {"name": "extra"},
    })
    .as_object()
    .unwrap()
    .clone();
    m.nodes.get_mut("model.p.fct_x").unwrap().columns = serde_json::json!({
        "amt": {"name": "amt"},
    })
    .as_object()
    .unwrap()
    .clone();
    m.nodes.insert(
        "model.p.empty".to_string(),
        crate::RawNode {
            name: "empty".into(),
            resource_type: "model".into(),
            ..Default::default()
        },
    );
    m.nodes.insert(
        "test.p.nn".to_string(),
        crate::RawNode {
            name: "nn".into(),
            resource_type: "test".into(),
            attached_node: Some("model.p.stg_x".into()),
            column_name: Some("id".into()),
            test_metadata: Some(crate::RawTestMetadata {
                name: Some("not_null".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let dag = Dag::build(&m);
    // 1 of 3 declared columns is tested; `empty` contributes 0/0.
    assert_eq!(column_coverage_counts(&dag, None), (1, 3));
}

#[test]
fn doc_coverage_counts_counts_models_with_descriptions() {
    // stg_x documented, fct_x not, `blank` whitespace-only (→ None at
    // build), `seeded` a seed (not the model base — drops out of both
    // counters).
    let mut m = two_model_manifest();
    m.nodes.get_mut("model.p.stg_x").unwrap().description = Some("staged".into());
    m.nodes.insert(
        "model.p.blank".to_string(),
        crate::RawNode {
            name: "blank".into(),
            resource_type: "model".into(),
            description: Some("   \n".into()),
            ..Default::default()
        },
    );
    m.nodes.insert(
        "seed.p.codes".to_string(),
        crate::RawNode {
            name: "codes".into(),
            resource_type: "seed".into(),
            description: Some("a seed".into()),
            ..Default::default()
        },
    );
    let dag = Dag::build(&m);
    assert_eq!(
        doc_coverage_counts(&dag, None),
        (1, 3),
        "stg_x of stg_x+fct_x+blank: a whitespace-only description \
             is undocumented, and the seed never counts"
    );
    // The single predicate the gate and the lint rule share.
    let detail = dag.detail("model.p.stg_x").unwrap();
    assert!(model_documented(detail));
    let detail = dag.detail("model.p.blank").unwrap();
    assert!(!model_documented(detail));
}

#[test]
fn column_doc_coverage_counts_counts_documented_declared_columns() {
    // stg_x declares id (documented) + extra (whitespace-only — treated
    // as undocumented); fct_x declares one bare column.
    let mut m = two_model_manifest();
    m.nodes.get_mut("model.p.stg_x").unwrap().columns = serde_json::json!({
        "id": {"name": "id", "description": "the id"},
        "extra": {"name": "extra", "description": "  "},
    })
    .as_object()
    .unwrap()
    .clone();
    m.nodes.get_mut("model.p.fct_x").unwrap().columns = serde_json::json!({
        "amt": {"name": "amt"},
    })
    .as_object()
    .unwrap()
    .clone();
    let dag = Dag::build(&m);
    assert_eq!(column_doc_coverage_counts(&dag, None), (1, 3));
}

#[test]
fn changed_hubs_honours_the_passed_threshold() {
    // stg_x (changed) → fct_x: exactly one downstream model. The hub
    // list is built at the passed threshold — the same `pr_impact` the
    // check gate reads.
    let base = Dag::build(&two_model_manifest());
    let mut m = two_model_manifest();
    m.nodes.get_mut("model.p.stg_x").unwrap().raw_code = Some("select 2".into());
    let current = Dag::build(&m);
    let diff = compute_diff(&base, &current);
    let pr = pr_impact(&current, Some(&base), &diff, &ImpactOpts::default());
    assert!(
        pr.changed_hubs.is_empty(),
        "1 downstream is under the shipped default {PR_HUB_MIN_DOWNSTREAM}"
    );
    let pr = pr_impact(
        &current,
        Some(&base),
        &diff,
        &ImpactOpts {
            hub_min_downstream: 1,
            ..ImpactOpts::default()
        },
    );
    assert_eq!(pr.changed_hubs, vec![("stg_x".to_string(), 1)]);
}

#[test]
fn diff_report_and_markdown_are_deterministic() {
    let base = Dag::build(&two_model_manifest());
    let mut m = two_model_manifest();
    m.nodes.get_mut("model.p.fct_x").unwrap().raw_code = Some("select 22".into());
    let current = Dag::build(&m);
    let diff = compute_diff(&base, &current);
    let r1 = diff_report(
        &current,
        Some(&base),
        &diff,
        "base",
        PackageScope::Root,
        &ImpactOpts::default(),
    );
    let r2 = diff_report(
        &current,
        Some(&base),
        &diff,
        "base",
        PackageScope::Root,
        &ImpactOpts::default(),
    );
    assert_eq!(r1, r2);
    let md = pr_impact_markdown("p", &r1);
    assert!(md.contains("# PR Impact Report: p"));
    assert!(
        md.contains("- scope: root project only"),
        "the scope line leads the report, got: {md}"
    );
    assert!(md.contains("Modified"), "changed node listed");
    assert!(md.contains("fct_x"));
    // JSON shape used by `dbtl impact --format json`.
    let json = serde_json::to_string(&r1).expect("serializes");
    assert!(json.contains("\"affected_models\""));
}
