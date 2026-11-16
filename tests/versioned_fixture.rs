//! The versioned-model fixture: `tests/fixtures/versioned_manifest.json` is
//! real-`dbt compile` output over `tests/fixtures/versioned_project/` —
//! regenerate with:
//!
//! ```sh
//! cargo xtask gen-fixture-manifest \
//!     --project tests/fixtures/versioned_project --dbt-version 1.12.6
//! ```
//!
//! It is the manifest-mode contract for dbt `versions:`-ed models:
//!
//! - `fct_orders` declares three versions — `v: 1` (a JSON NUMBER on the
//!   wire), `v: 2`, and `v: beta` (a JSON string) — plus three consumers
//!   resolving `ref()` with no version, `v=1`, and `v='beta'`. Every version
//!   is its own DAG node (`model.versioned.fct_orders.vN`) but one logical
//!   model for counting.
//! - A `versions: include: [2]` unit test must attach to v2 exactly.
//!
//! Source mode synthesizes the same `model.<pkg>.<name>.vN` members —
//! `ref('fct_orders')` lands on the declared latest, `v=`/`version=`
//! pins land on their member, and `versions: include`-scoped unit
//! tests expand per member through the synthesized family — the
//! manifest↔source assertions at the bottom pin the equivalence.

mod common;

use common::consistency as cmp;
use common::{lines, run, stderr, stdout};
use dbtl::{
    build_model_list, load_dag, load_dag_from_source, run_rules, Dag, LintConfig, LintInput,
    LintRule, PackageScope, SortMode,
};

const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/versioned_manifest.json"
);

const PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/versioned_project"
);

fn dag() -> Dag {
    load_dag(MANIFEST).expect("versioned fixture must load")
}

/// `dbtl ls <specs> --manifest <VERSIONED> <extra...>`.
fn ls(specs: &[&str], extra: &[&str]) -> std::process::Output {
    let mut args: Vec<String> = vec!["ls".to_string()];
    args.extend(specs.iter().map(|s| s.to_string()));
    args.push("--manifest".to_string());
    args.push(MANIFEST.to_string());
    args.extend(extra.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs)
}

// ---- loading: the wire shape that used to fail the parse --------------------

#[test]
fn numeric_and_string_versions_load_as_distinct_nodes() {
    // The original failure: `invalid type: integer '1', expected a string`.
    // All three spellings now land — `v: 1` (JSON number), `v: 2`, and
    // `v: beta` (JSON string) — as separately addressable nodes sharing the
    // declared name `fct_orders`.
    let dag = dag();
    for uid in [
        "model.versioned.fct_orders.v1",
        "model.versioned.fct_orders.v2",
        "model.versioned.fct_orders.vbeta",
    ] {
        let n = dag.get(uid).unwrap_or_else(|| panic!("{uid} is a node"));
        assert_eq!(n.name, "fct_orders", "{uid} shares the declared name");
        assert_eq!(n.resource_type, "model");
    }
    // `version`/`latest_version` carry the declared values verbatim —
    // `"2"`, not `"v2"`.
    let detail = |uid: &str| {
        let d = dag.detail(uid).expect("detail");
        (d.version.clone(), d.latest_version.clone())
    };
    assert_eq!(
        detail("model.versioned.fct_orders.v1"),
        (Some("1".into()), Some("2".into()))
    );
    assert_eq!(
        detail("model.versioned.fct_orders.v2"),
        (Some("2".into()), Some("2".into()))
    );
    assert_eq!(
        detail("model.versioned.fct_orders.vbeta"),
        (Some("beta".into()), Some("2".into()))
    );
}

#[test]
fn versioned_edges_match_dbt_resolution() {
    // `ref('fct_orders', v=1)` → v1; bare `ref('fct_orders')` → the latest
    // (v2); `ref('fct_orders', v='beta')` → vbeta.
    let dag = dag();
    let parents = |uid: &str| -> Vec<String> {
        let mut p: Vec<String> = dag.direct_parents(uid).to_vec();
        p.sort();
        p
    };
    assert_eq!(
        parents("model.versioned.use_v1"),
        vec!["model.versioned.fct_orders.v1".to_string()]
    );
    assert_eq!(
        parents("model.versioned.use_latest"),
        vec!["model.versioned.fct_orders.v2".to_string()]
    );
    assert_eq!(
        parents("model.versioned.use_beta"),
        vec!["model.versioned.fct_orders.vbeta".to_string()]
    );
}

// ---- `dbtl ls`: one version vs the family -----------------------------------

#[test]
fn ls_qualifies_single_versions_and_the_bare_family() {
    // `name.vN` names exactly one spelling — the same selector real dbt
    // accepts (`dbt ls -s fct_orders.v1` on this project prints only v1).
    let out = ls(&["fct_orders.v1"], &[]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(lines(&out), ["fct_orders.v1"]);

    // The `ref(…, v=…)` spellings resolve the same way — dbt's
    // prefix-free version compare.
    let out = ls(&["fct_orders.1"], &[]);
    assert_eq!(lines(&out), ["fct_orders.v1"]);
    let out = ls(&["fct_orders.vbeta"], &[]);
    assert_eq!(lines(&out), ["fct_orders.vbeta"]);

    // The bare family name still hits every version — dbt's own
    // name-selector semantics.
    let out = ls(&["fct_orders"], &[]);
    assert_eq!(
        lines(&out),
        ["fct_orders.v1", "fct_orders.v2", "fct_orders.vbeta"]
    );

    // A version no node carries matches nothing.
    let out = ls(&["fct_orders.v9"], &[]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
}

#[test]
fn ls_uids_and_selector_outputs_carry_the_version() {
    // `--output uids` — the exact versioned unique_ids.
    let out = ls(&["fct_orders"], &["--output", "uids"]);
    assert_eq!(
        lines(&out),
        [
            "model.versioned.fct_orders.v1",
            "model.versioned.fct_orders.v2",
            "model.versioned.fct_orders.vbeta",
        ]
    );

    // `--output selector` emits the `name.vseg` spelling — each version
    // re-selectable on its own (the bare name would re-select all three).
    let out = ls(&["fct_orders"], &["--output", "selector"]);
    assert_eq!(
        stdout(&out),
        "fct_orders.v1 fct_orders.v2 fct_orders.vbeta\n"
    );

    // `--output json` keeps the manifest's declared `name` field — the
    // `unique_id` is what distinguishes the rows programmatically.
    let out = ls(&["fct_orders.v1"], &["--output", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["unique_id"], "model.versioned.fct_orders.v1");
    assert_eq!(rows[0]["name"], "fct_orders");
}

// ---- lint: versions count once, names stay distinguishable -----------------

#[test]
fn lint_folds_versions_into_one_consumer_and_names_them() {
    let dag = dag();
    let outcome = run_rules(LintInput::new(&dag, &LintConfig::default()));

    // `source_fanout`: raw.orders feeds seven MEMBER nodes across three
    // logical models — the fold reports `dim_items, fct_orders,
    // stg_users` once, never the member count or spellings.
    let fanout: Vec<&dbtl::Finding> = outcome
        .findings
        .iter()
        .filter(|f| f.rule == dbtl::LintRule::SourceFanout)
        .collect();
    assert_eq!(fanout.len(), 1, "one folded finding: {fanout:?}");
    assert!(
        fanout[0].message.contains("3 models"),
        "7 member nodes fold to 3 logical models: {}",
        fanout[0].message
    );
    for name in ["dim_items", "fct_orders", "stg_users"] {
        assert!(
            fanout[0].message.contains(name),
            "the family names: {}",
            fanout[0].message
        );
    }

    // `marts_depend_on_source` DOES fire — once per version (each is a
    // real model with the defect) — and the messages name the exact
    // versioned spelling, never a bare ambiguous `fct_orders`.
    let mut marts: Vec<&dbtl::Finding> = outcome
        .findings
        .iter()
        .filter(|f| f.rule == dbtl::LintRule::MartsDependOnSource)
        .collect();
    marts.sort_by_key(|f| f.node_uid.clone());
    let want: Vec<(&str, &str)> = [
        ("dim_items", "v1"),
        ("dim_items", "v2"),
        ("fct_orders", "v1"),
        ("fct_orders", "v2"),
        ("fct_orders", "vbeta"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        marts.len(),
        want.len(),
        "one finding per version: {marts:?}"
    );
    for (f, (name, v)) in marts.iter().zip(want) {
        assert_eq!(f.node_uid, format!("model.versioned.{name}.{v}"));
        assert!(
            f.message.contains(&format!("{name}.{v}")),
            "the message names the version: {}",
            f.message
        );
    }
}

// ---- unit tests: the exact versioned target ----------------------------------

#[test]
fn unit_test_attaches_to_its_declared_version_only() {
    // `versions: include: [2]` records `model.versioned.fct_orders.v2` in
    // `depends_on.nodes` — the test must land there, never on a sibling
    // spelling the name lookup could have picked at random.
    let dag = dag();
    let unit_uids = |uid: &str| -> Vec<String> {
        dag.tests(uid)
            .iter()
            .filter(|t| t.kind == "unit")
            .map(|t| t.name.clone())
            .collect()
    };
    assert_eq!(
        unit_uids("model.versioned.fct_orders.v2"),
        vec!["fct_orders_v2_keeps_rows".to_string()]
    );
    for uid in [
        "model.versioned.fct_orders.v1",
        "model.versioned.fct_orders.vbeta",
    ] {
        assert!(
            unit_uids(uid).is_empty(),
            "{uid} must not borrow the v2-scoped test"
        );
    }
}

#[test]
fn source_mode_attaches_the_versioned_unit_test_to_the_same_member() {
    // The source-mode counterpart: `versions: {include: [2]}` expands
    // over the synthesized `model.versioned.fct_orders.vN` members —
    // `.v2` carries the test with the same (inputs, given_rows,
    // expect_rows) shape the manifest records.
    let dag = load_dag_from_source(PROJECT).expect("versioned project parses");
    let tests = dag.tests("model.versioned.fct_orders.v2");
    let unit = tests
        .iter()
        .find(|t| t.kind == "unit")
        .expect("source mode attaches the include-scoped unit test");
    assert_eq!(unit.name, "fct_orders_v2_keeps_rows");
    let shape = unit.unit.as_ref().expect("kind unit carries its shape");
    assert_eq!(
        (shape.inputs, shape.given_rows, shape.expect_rows),
        (1, Some(1), Some(1))
    );
    for uid in [
        "model.versioned.fct_orders.v1",
        "model.versioned.fct_orders.vbeta",
    ] {
        assert!(
            dag.tests(uid).iter().all(|t| t.kind != "unit"),
            "{uid} must not borrow the v2-scoped test"
        );
    }
}

#[test]
fn source_mode_flags_no_unresolved_references() {
    // The 0-findings pin: `ref('fct_orders')` lands on the declared
    // latest member and both `v=` pins land on their synthesized member
    // — the `RefScope` the loader emits carries the same `(pkg, name,
    // version)` tuples a real manifest's does, so the pins judge the
    // same way and resolve.
    let dag = load_dag_from_source(PROJECT).expect("versioned project parses");
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    assert!(
        findings
            .iter()
            .all(|f| f.rule != LintRule::UnresolvedReferences),
        "every ref in the fixture resolves under the source scope: {findings:?}"
    );
}

// ---- display: list rows, docs pages, column addressing ----------------------

#[test]
fn model_list_names_each_version() {
    // The TUI left pane renders `DisplayRow::Model.name` — the `list_name`
    // spelling that distinguishes all three versions (the underlying
    // `NodeInfo.name` stays the declared `fct_orders`, like `ls --output
    // json`'s `name` field).
    let list = build_model_list(&dag(), SortMode::Layer);
    let names: Vec<&str> = list
        .rows
        .iter()
        .filter_map(|r| match r {
            dbtl::model_list::DisplayRow::Model { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for want in ["fct_orders.v1", "fct_orders.v2", "fct_orders.vbeta"] {
        assert!(
            names.contains(&want),
            "model list must row {want}, got: {names:?}"
        );
    }
    assert_eq!(
        names.iter().filter(|n| **n == "fct_orders").count(),
        0,
        "no bare ambiguous row survives: {names:?}"
    );
}

#[test]
fn docs_generate_one_distinguishable_page_per_version() {
    let out = dbtl::generate_docs(&dag(), "versioned", MANIFEST, PackageScope::Root);
    let page = |rel: &str| {
        out.iter()
            .find(|(p, _)| p == rel)
            .map(|(_, c)| c.as_str())
            .unwrap_or_else(|| panic!("missing generated page {rel}"))
    };
    // Three distinct pages — uid-derived filenames never collided — but the
    // headings and labels now distinguish them too.
    for v in ["v1", "v2", "vbeta"] {
        let md = page(&format!("models/model.versioned.fct_orders.{v}.md"));
        assert!(
            md.contains(&format!("# fct_orders.{v}")),
            "page title for {v}"
        );
    }
    // The v2 page records its declared `version`/`latest_version`, carries
    // the version-scoped unit test only, and names links in the qualified
    // spelling.
    let v2 = page("models/model.versioned.fct_orders.v2.md");
    assert!(v2.contains("- version: `2`"), "version field: {v2}");
    assert!(v2.contains("- latest: `2`"), "latest field: {v2}");
    assert!(
        v2.contains("fct_orders_v2_keeps_rows"),
        "the v2 unit test: {v2}"
    );
    assert!(v2.contains("[use_latest]"), "downstream link: {v2}");
    let v1 = page("models/model.versioned.fct_orders.v1.md");
    assert!(
        !v1.contains("fct_orders_v2_keeps_rows"),
        "v1 must not borrow the v2 test"
    );
    // The README/index listings qualify the family instead of repeating
    // one ambiguous `fct_orders`.
    let readme = page("README.md");
    assert!(
        readme.contains("fct_orders.v1"),
        "index names the versions: {readme}"
    );
}

#[test]
fn columns_address_a_single_version_or_bail_ambiguous() {
    // `fct_orders.v2.id` resolves to that one version's column.
    let out = run(&["columns", "--manifest", MANIFEST, "fct_orders.v2.id"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("fct_orders.v2.id"),
        "the report names the versioned column: {}",
        stdout(&out)
    );

    // The bare family name is ambiguous — the error lists the versioned
    // uids to pick from, matching the same CLI/engine wording.
    let out = run(&["columns", "--manifest", MANIFEST, "fct_orders.id"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("is ambiguous"),
        "stderr: {}",
        stderr(&out)
    );
}

// ---- manifest↔source: the same project both ways ----------------------------

/// (manifest-mode Dag, source-mode Dag) of `versioned_project`.
fn both() -> (Dag, Dag) {
    cmp::dags(
        MANIFEST,
        PROJECT,
        "cargo xtask gen-fixture-manifest --project tests/fixtures/versioned_project --dbt-version 1.12.6",
    )
}

#[test]
fn source_mode_matches_the_manifest_node_set_and_edges() {
    // The synthesized `.vN` members land exactly where real dbt put them:
    // same uid set, same adjacency (`ref('fct_orders')` → `.v2`, the
    // `v=`/`version=` pins → their member).
    let (m, s) = both();
    cmp::assert_node_infos_identical(&m, &s, cmp::NO_DROPPED_EDGES, cmp::NO_SOURCE_ONLY_EDGES);
    cmp::assert_closures_identical(&m, &s, cmp::NO_DROPPED_EDGES, cmp::NO_SOURCE_ONLY_EDGES);
}

#[test]
fn source_mode_matches_the_manifest_details() {
    // `version`/`latest_version`/`deprecation_date` ride NodeDetail — the
    // shared comparison covers them through the description/meta surface
    // plus the per-member patch_path (`<pkg>://` stripped on both sides).
    let (m, s) = both();
    cmp::assert_details_identical(&m, &s);
    for (uid, want) in [
        ("model.versioned.fct_orders.v1", ("1", "2")),
        ("model.versioned.fct_orders.v2", ("2", "2")),
        ("model.versioned.fct_orders.vbeta", ("beta", "2")),
    ] {
        let d = s.detail(uid).expect("source detail");
        assert_eq!(
            (d.version.as_deref(), d.latest_version.as_deref()),
            (Some(want.0), Some(want.1)),
            "{uid} version fields"
        );
    }
}

#[test]
fn source_mode_matches_the_manifest_tests() {
    let (m, s) = both();
    cmp::assert_tests_identical(&m, &s);
}

#[test]
fn source_mode_matches_the_manifest_lint_findings() {
    // Same rules, nodes, messages — the one documented difference is the
    // finding LOCATION: the manifest records only `patch_path` (the file),
    // while source mode also resolves the `name:` line/column inside it.
    let (m, s) = both();
    let cfg = LintConfig::default();
    let key = |f: &dbtl::Finding| {
        (
            f.rule,
            f.node_uid.clone(),
            f.related.clone(),
            f.message.clone(),
            f.severity,
            f.help.clone(),
        )
    };
    let mf: Vec<_> = run_rules(LintInput::new(&m, &cfg))
        .findings
        .iter()
        .map(key)
        .collect();
    let sf: Vec<_> = run_rules(LintInput::new(&s, &cfg))
        .findings
        .iter()
        .map(key)
        .collect();
    assert_eq!(mf, sf, "lint findings diverge between modes");
}

// ---- scope: nothing versioned leaks through the prune ------------------------

#[test]
fn versioned_nodes_survive_the_default_scope() {
    // All three versions plus the consumers live in the root package — the
    // default prune keeps them (the uid's fourth segment must not confuse
    // package attribution).
    let dag = dag();
    for uid in [
        "model.versioned.fct_orders.v1",
        "model.versioned.fct_orders.v2",
        "model.versioned.fct_orders.vbeta",
        "model.versioned.use_v1",
        "model.versioned.use_latest",
        "model.versioned.use_beta",
    ] {
        assert!(dag.contains(uid), "{uid} dropped out of the root scope");
    }
    assert_eq!(dag.project_name(), Some("versioned"));
}
