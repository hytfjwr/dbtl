//! Tests for the source loader: parse a tiny synthetic dbt project (no compiled
//! manifest) and assert the synthesized manifest / DAG. Mirrors the
//! `manifest_fixture` strategy: assert structure, not rendering.

use dbtl::{build_model_list, load_dag_from_source, manifest_from_source, SortMode};

const PROJECT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_project");
const MISSING: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/does_not_exist");

#[test]
fn synthesizes_nodes_and_sources() {
    let m = manifest_from_source(PROJECT).expect("sample project parses");
    for uid in [
        "model.sample.stg_orders",
        "model.sample.fct_orders",
        "model.sample.dim_customers",
        "model.sample.agg_country_orders",
        "seed.sample.country_codes",
        "snapshot.sample.orders_snapshot",
    ] {
        assert!(m.nodes.contains_key(uid), "missing node {uid}");
    }
    assert!(m.sources.contains_key("source.sample.raw.orders"));
    assert!(m.sources.contains_key("source.sample.raw.customers"));
}

#[test]
fn disabled_models_are_dropped_like_dbt_parks_them() {
    // All three disable channels park the node under dbt's `disabled` (not
    // `nodes`), so the source loader must drop them too:
    //   stg_disabled        in-file `config(enabled=false)`
    //   stg_schema_disabled schema.yml `config: enabled: false`
    //   old_model           dbt_project.yml tree `legacy: +enabled: false`
    let m = manifest_from_source(PROJECT).unwrap();
    for uid in [
        "model.sample.stg_disabled",
        "model.sample.stg_schema_disabled",
        "model.sample.old_model",
    ] {
        assert!(!m.nodes.contains_key(uid), "{uid} must not be synthesized");
    }
    // Edges are asserted at the DAG level: the schema.yml-disabled node is
    // dropped AFTER ref resolution, so the synthesized child_map may carry a
    // dangling entry — which `Dag::build`'s prune-first step removes (the
    // documented contract; same as an unresolved ref).
    let dag = load_dag_from_source(PROJECT).unwrap();
    assert!(
        dag.downstream("model.sample.stg_orders")
            .iter()
            .all(|x| !x.contains("disabled") && !x.contains("old_model")),
        "no DAG edge into a disabled model"
    );
}

#[test]
fn config_keys_outside_config_calls_are_inert() {
    // fct_orders carries `enabled=false` and `materialized='ephemeral'` in a
    // SQL comment and a `disabled_flag = false` WHERE clause — none inside a
    // config() call, so (like dbt) they must configure NOTHING: the node stays
    // enabled and keeps its real in-file materialization.
    let dag = load_dag_from_source(PROJECT).unwrap();
    let fct = dag
        .get("model.sample.fct_orders")
        .expect("fct stays enabled");
    assert_eq!(fct.materialized.as_deref(), Some("table"));
}

#[test]
fn ref_and_source_become_edges() {
    let m = manifest_from_source(PROJECT).unwrap();
    // stg_orders depends only on the raw.orders source.
    assert_eq!(
        m.parent_map.get("model.sample.stg_orders").cloned(),
        Some(vec!["source.sample.raw.orders".to_string()]),
    );
    // fct_orders refs a model AND a seed.
    let fct = m
        .parent_map
        .get("model.sample.fct_orders")
        .cloned()
        .unwrap_or_default();
    assert!(fct.contains(&"model.sample.stg_orders".to_string()));
    assert!(fct.contains(&"seed.sample.country_codes".to_string()));
    // The snapshot's body ref is an edge too.
    assert_eq!(
        m.parent_map.get("snapshot.sample.orders_snapshot").cloned(),
        Some(vec!["model.sample.stg_orders".to_string()]),
    );
}

#[test]
fn identifier_embedding_ref_or_source_is_not_an_edge() {
    // `my_ref('stg_orders')` / `other_source('raw','orders')` embed the macro names
    // inside a longer identifier — the word boundary must reject them: stg_orders
    // must NOT be a parent of dim_customers.
    let m = manifest_from_source(PROJECT).unwrap();
    let parents = m
        .parent_map
        .get("model.sample.dim_customers")
        .cloned()
        .unwrap_or_default();
    assert!(
        !parents.iter().any(|p| p.contains("stg_orders")),
        "identifier-embedded ref( must not be an edge, got {parents:?}"
    );
    assert!(
        parents.contains(&"source.sample.raw.customers".to_string()),
        "the real source() is an edge, got {parents:?}"
    );
}

#[test]
fn comment_semantics_match_dbt_jinja_before_sql() {
    let m = manifest_from_source(PROJECT).unwrap();
    // A `{# #}`-commented ref is invisible to dbt → no edge.
    let stg = m
        .parent_map
        .get("model.sample.stg_orders")
        .cloned()
        .unwrap_or_default();
    assert!(
        !stg.iter().any(|p| p.contains("ghost_model")),
        "a {{# #}}-commented ref must not become an edge, got {stg:?}"
    );
    // But dbt renders Jinja BEFORE SQL comments exist, so a `--`-commented
    // `{{ ref('country_codes') }}` IS a real dependency in the manifest — and
    // must be one here too (the consistency contract).
    let dim = m
        .parent_map
        .get("model.sample.dim_customers")
        .cloned()
        .unwrap_or_default();
    assert!(
        dim.contains(&"seed.sample.country_codes".to_string()),
        "a --commented ref IS an edge (Jinja renders first), got {dim:?}"
    );
}

#[test]
fn materialization_precedence_matches_dbt() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let mat = |uid: &str| {
        dag.detail(uid)
            .and_then(|d| d.materialized.clone())
            .unwrap_or_default()
    };
    assert_eq!(mat("model.sample.stg_orders"), "view");
    assert_eq!(mat("model.sample.fct_orders"), "table");
    // dim_customers: .sql says incremental, schema.yml says table -> .sql wins.
    assert_eq!(mat("model.sample.dim_customers"), "incremental");
    // agg_country_orders has NO in-file config and NO schema entry: the
    // dbt_project.yml tree (models: sample: marts: +materialized: table) wins
    // over the global `view` default.
    assert_eq!(mat("model.sample.agg_country_orders"), "table");
}

#[test]
fn schema_tests_are_captured_but_never_dag_nodes() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    // Tests are captured pre-prune as data only; they never become DAG nodes.
    assert_eq!(dag.count_by_resource_type("test"), 0);

    let stg = dag.tests("model.sample.stg_orders");
    assert!(
        stg.iter()
            .any(|t| t.kind == "unique" && t.column_name.as_deref() == Some("id")),
        "stg_orders.id unique test captured, got {stg:?}"
    );
    assert!(stg
        .iter()
        .any(|t| t.kind == "not_null" && t.column_name.as_deref() == Some("id")));
    assert!(
        stg.iter()
            .any(|t| t.kind == "accepted_values" && t.column_name.as_deref() == Some("status")),
        "accepted_values map-form test captured, got {stg:?}"
    );
    // `data_tests:` alias is honoured.
    assert!(dag
        .tests("model.sample.fct_orders")
        .iter()
        .any(|t| t.kind == "not_null" && t.column_name.as_deref() == Some("id")));
}

#[test]
fn builds_dag_with_expected_counts_and_lineage() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    assert_eq!(dag.count_by_resource_type("model"), 4, "model count");
    assert_eq!(dag.count_by_resource_type("source"), 2, "source count");
    assert_eq!(dag.count_by_resource_type("seed"), 1, "seed count");
    assert_eq!(dag.count_by_resource_type("snapshot"), 1, "snapshot count");

    // fct_orders upstream: stg_orders + its source (transitive) + the seed.
    let up = dag.upstream("model.sample.fct_orders");
    assert!(up.contains("model.sample.stg_orders"));
    assert!(up.contains("source.sample.raw.orders"), "transitive source");
    assert!(up.contains("seed.sample.country_codes"));
    // agg_country_orders extends the chain one hop downstream of fct.
    assert!(dag
        .downstream("model.sample.fct_orders")
        .contains("model.sample.agg_country_orders"));
}

#[test]
fn source_table_test_is_captured_on_the_source() {
    // raw.orders.id declares `not_null` in schema.yml: captured on the SOURCE
    // uid (dbt attaches source tests via depends_on; same Dag-level result).
    let dag = load_dag_from_source(PROJECT).unwrap();
    let tests = dag.tests("source.sample.raw.orders");
    assert!(
        tests
            .iter()
            .any(|t| t.kind == "not_null" && t.column_name.as_deref() == Some("id")),
        "source column test captured, got {tests:?}"
    );
}

#[test]
fn columns_preserve_schema_definition_order() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let cols = &dag
        .detail("model.sample.stg_orders")
        .expect("stg_orders detail")
        .columns;
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["id", "status"], "columns kept in schema order");
}

#[test]
fn source_mode_groups_models_by_logical_layer() {
    // `path` must be model-paths-root-relative (`staging/x.sql`), NOT project-root
    // relative (`models/staging/x.sql`), or the left pane collapses into one
    // unrecognized "models" group instead of grouping by layer.
    let dag = load_dag_from_source(PROJECT).unwrap();
    let list = build_model_list(&dag, SortMode::Layer);
    let layers: Vec<&str> = list.groups.iter().map(|g| g.layer.as_str()).collect();
    assert_eq!(
        layers,
        vec!["staging", "marts"],
        "layer grouping, got {layers:?}"
    );
}

#[test]
fn seed_has_seed_materialization() {
    // dbt records seeds with materialization "seed" (so the detail modal does not
    // mislabel them); the source loader must match.
    let dag = load_dag_from_source(PROJECT).unwrap();
    assert_eq!(
        dag.detail("seed.sample.country_codes")
            .and_then(|d| d.materialized.clone())
            .as_deref(),
        Some("seed"),
    );
}

#[test]
fn source_table_metadata_is_carried() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let src = dag
        .detail("source.sample.raw.orders")
        .expect("raw.orders detail");
    assert_eq!(src.materialized, None, "sources have no materialization");
    assert_eq!(src.schema.as_deref(), Some("raw_data"));
    assert!(src.columns.iter().any(|c| c.name == "id"));
    // dbt records the declaring schema file as the source's path: needed for
    // the `$EDITOR` jump and detail modal to behave like manifest mode.
    assert_eq!(
        src.original_file_path.as_deref(),
        Some("models/staging/schema.yml"),
        "source path is the declaring schema file"
    );
}

#[test]
fn missing_project_is_err_not_panic() {
    assert!(manifest_from_source(MISSING).is_err(), "missing dir is Err");
    let msg = format!("{:#}", manifest_from_source(MISSING).unwrap_err());
    assert!(
        msg.contains("dbt_project.yml"),
        "error should name the missing project file, got: {msg}"
    );
}

/// Regression: a directory symlink loop (`models/loop` -> `models`) must not
/// recurse forever. The collector decides recursion via `entry.file_type()`
/// (which does not follow symlinks), so loading terminates and still picks up
/// the regular files next to the link.
#[cfg(unix)]
#[test]
fn symlinked_dir_loop_terminates() {
    use std::fs;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("dbt_project.yml"),
        "name: looped\nversion: \"1.0.0\"\nconfig-version: 2\n",
    )
    .unwrap();
    let models = root.join("models");
    fs::create_dir(&models).unwrap();
    fs::write(models.join("a.sql"), "select 1 as id\n").unwrap();
    std::os::unix::fs::symlink(&models, models.join("loop")).unwrap();

    let m = manifest_from_source(root).expect("loop project parses");
    assert!(
        m.nodes.contains_key("model.looped.a"),
        "regular file still collected"
    );
    assert_eq!(m.nodes.len(), 1, "the symlinked dir is never entered");
}

/// Regression: `model-paths`/`seed-paths`/`snapshot-paths` come from an
/// untrusted `dbt_project.yml`; a traversal value (`../outside`, an absolute
/// path, or a `..` mid-path) must be skipped so the loader never reads
/// `.sql`/`.csv`/`.yml` files outside the project root.
#[test]
fn traversal_resource_paths_are_skipped() {
    use std::fs;

    let tmp = tempfile::tempdir().expect("tempdir");
    // `<tmp>/outside` holds files that must never be ingested; the project
    // lives one level down so `../outside` points straight at them.
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("evil.sql"), "select 1 as id\n").unwrap();
    fs::write(outside.join("evil.csv"), "id\n1\n").unwrap();
    fs::write(
        outside.join("schema.yml"),
        "sources:\n  - name: ext\n    tables:\n      - name: leaked\n",
    )
    .unwrap();

    let root = tmp.path().join("project");
    fs::create_dir_all(root.join("models")).unwrap();
    fs::write(root.join("models/safe.sql"), "select 1 as id\n").unwrap();
    fs::write(
        root.join("dbt_project.yml"),
        format!(
            "name: contained\nversion: \"1.0.0\"\nconfig-version: 2\n\
             model-paths: [\"../outside\", \"models\"]\n\
             seed-paths: ['{}']\n\
             snapshot-paths: [\"models/../../outside\"]\n",
            // Single-quoted YAML: a Windows temp path's backslashes (`C:\U…`)
            // would be escape sequences in a double-quoted scalar.
            outside.display()
        ),
    )
    .unwrap();

    let m = manifest_from_source(&root).expect("project parses");
    assert!(
        m.nodes.contains_key("model.contained.safe"),
        "the in-root resource path is still scanned"
    );
    assert_eq!(
        m.nodes.len(),
        1,
        "no nodes ingested from outside the root, got {:?}",
        m.nodes.keys().collect::<Vec<_>>()
    );
    assert!(
        m.sources.is_empty(),
        "no sources ingested from an outside schema.yml, got {:?}",
        m.sources.keys().collect::<Vec<_>>()
    );
}
