//! Tests for the source loader: parse a tiny synthetic dbt project (no compiled
//! manifest) and assert the synthesized manifest / DAG. Mirrors the
//! `manifest_fixture` strategy: assert structure, not rendering.

mod common;
use common::SAMPLE_PROJECT as PROJECT;
use std::collections::HashSet;

use dbtl::{
    build_model_list, load_dag_from_source, load_dag_from_source_with_warnings,
    manifest_from_source, run_rules, LintConfig, LintInput, LintRule, SortMode,
};

const MISSING: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/does_not_exist");
const YAML_SNAPSHOT_PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/yaml_snapshot_project"
);
const COLLISION_PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/collision_project"
);

#[test]
fn synthesizes_nodes_and_sources() {
    let m = manifest_from_source(PROJECT).expect("sample project parses");
    for uid in [
        "model.sample.stg_orders",
        "model.sample.fct_orders",
        "model.sample.dim_customers",
        "model.sample.agg_country_orders",
        "model.sample.fct_customer_history",
        "model.sample.fct_line_items",
        "model.sample.fct_daily_orders",
        "seed.sample.country_codes",
        "snapshot.sample.orders_snapshot",
        "snapshot.sample.customers_snapshot",
    ] {
        assert!(m.nodes.contains_key(uid), "missing node {uid}");
    }
    assert!(m.sources.contains_key("source.sample.raw.orders"));
    assert!(m.sources.contains_key("source.sample.raw.customers"));
    assert!(m.sources.contains_key("source.sample.raw.payments"));
    assert!(m.sources.contains_key("source.sample.archive.audit_log"));
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
    assert_eq!(dag.count_by_resource_type("model"), 17, "model count");
    assert_eq!(dag.count_by_resource_type("source"), 6, "source count");
    assert_eq!(dag.count_by_resource_type("seed"), 2, "seed count");
    assert_eq!(dag.count_by_resource_type("snapshot"), 3, "snapshot count");

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
    assert_eq!(
        names,
        vec!["id", "status", "country_code", "disabled_flag"],
        "columns kept in schema order"
    );
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
fn unique_key_follows_dbt_precedence() {
    // The full chain — in-file config() > schema.yml `config:` > the
    // `dbt_project.yml` tree — resolves into one normalized Vec<String>
    // (consistency.rs pins byte-parity with real dbt for every node).
    let dag = load_dag_from_source(PROJECT).unwrap();
    let uk = |uid: &str| {
        dag.detail(uid)
            .unwrap_or_else(|| panic!("{uid} detail"))
            .unique_key
            .clone()
    };
    // dim_customers: in-file `unique_key="id"` beats the schema's
    // `customer_id`.
    assert_eq!(uk("model.sample.dim_customers"), vec!["id".to_string()]);
    // fct_line_items: schema.yml's list form beats the `tree_shadowed`
    // project-tree leaf.
    assert_eq!(
        uk("model.sample.fct_line_items"),
        vec!["order_id".to_string(), "line_no".to_string()]
    );
    // stg_two_configs: nothing declares above the tree — `+unique_key` lands.
    assert_eq!(
        uk("model.sample.stg_two_configs"),
        vec!["tree_uk".to_string()]
    );
    // fct_daily_orders: incremental with no unique_key anywhere — empty.
    assert!(uk("model.sample.fct_daily_orders").is_empty());
    // Snapshots pick it up too (yaml `config:` and the {% config() %} block).
    assert_eq!(
        uk("snapshot.sample.customers_snapshot"),
        vec!["id".to_string()]
    );
    assert_eq!(
        uk("snapshot.sample.orders_snapshot"),
        vec!["id".to_string()]
    );
}

#[test]
fn patch_path_records_the_declaring_yml() {
    // `patch_path` = the schema file the node was declared in (project-root
    // relative — the same normalization manifest mode applies to
    // `pkg://path`). Nodes with no schema entry keep `None`.
    let dag = load_dag_from_source(PROJECT).unwrap();
    let pp = |uid: &str| {
        dag.detail(uid)
            .unwrap_or_else(|| panic!("{uid} detail"))
            .patch_path
            .clone()
    };
    assert_eq!(
        pp("model.sample.stg_orders").as_deref(),
        Some("models/staging/schema.yml")
    );
    assert_eq!(
        pp("model.sample.fct_line_items").as_deref(),
        Some("models/marts/schema.yml")
    );
    // A `seeds:` entry in staging/schema.yml patches to THAT file.
    assert_eq!(
        pp("seed.sample.country_codes").as_deref(),
        Some("models/staging/schema.yml")
    );
    assert_eq!(
        pp("snapshot.sample.customers_snapshot").as_deref(),
        Some("snapshots/snapshots.yml")
    );
    // No schema entry → None (sql-file-only declarations and
    // `{% snapshot %}` blocks aren't yml-patched resources).
    assert_eq!(pp("model.sample.stg_two_configs"), None);
    assert_eq!(pp("model.sample.fct_customer_history"), None);
    assert_eq!(pp("snapshot.sample.orders_snapshot"), None);
}

#[test]
fn source_ingestion_fields_follow_dbt_semantics() {
    // loader: source-level only, cascades to every table (dbt ignores
    // table-level and `+loader` declarations).
    // loaded_at_field: table overrides the source, an explicit table `null`
    // clears it, `+loaded_at_field` never lands.
    // freshness: per-threshold merge table > source > `+freshness`; an
    // explicit `freshness: null` at a level stops the walk below it.
    let dag = load_dag_from_source(PROJECT).unwrap();
    let d = |uid: &str| dag.detail(uid).unwrap_or_else(|| panic!("{uid} detail"));

    // raw.orders: table-level overrides over the source-level values.
    let orders = d("source.sample.raw.orders");
    assert_eq!(orders.loader.as_deref(), Some("fivetran"));
    assert_eq!(orders.loaded_at_field.as_deref(), Some("loaded_at"));
    let warn = orders.freshness_warn_after.as_ref().expect("warn_after");
    assert_eq!(warn.count, 12);
    let err = orders.freshness_error_after.as_ref().expect("error_after");
    assert_eq!(err.count, 6);

    // raw.payments: pure source-level inheritance.
    let payments = d("source.sample.raw.payments");
    assert_eq!(payments.loader.as_deref(), Some("fivetran"));
    assert_eq!(payments.loaded_at_field.as_deref(), Some("updated_at"));
    assert_eq!(
        payments.freshness_error_after.as_ref().map(|t| t.count),
        Some(24)
    );

    // raw.customers: `freshness: null` and `loaded_at_field: null` clear
    // every inherited value — loader still cascades.
    let customers = d("source.sample.raw.customers");
    assert_eq!(customers.loader.as_deref(), Some("fivetran"));
    assert_eq!(customers.loaded_at_field, None);
    assert!(!customers.has_freshness());

    // archive.audit_log: no schema-level ingestion keys — only the
    // `sources:` tree `+freshness` lands; `+loader`/`+loaded_at_field` are
    // ignored by real dbt and so never synthesized.
    let audit = d("source.sample.archive.audit_log");
    assert_eq!(audit.loader, None);
    assert_eq!(audit.loaded_at_field, None);
    assert_eq!(
        audit.freshness_warn_after.as_ref().map(|t| t.count),
        Some(48)
    );
    assert_eq!(audit.freshness_error_after, None);
    assert!(audit.has_freshness());
}

#[test]
fn meta_merges_per_dbt_precedence() {
    // The fixture exercises every `meta` intake; the merges must match real
    // dbt (the consistency suite pins them byte-for-byte):
    //   in-file config(meta=…) > schema.yml meta/config.meta > project tree
    //   (+meta), shallowly per top-level key; source meta cascades to tables.
    let dag = load_dag_from_source(PROJECT).unwrap();
    let meta = |uid: &str| {
        dag.detail(uid)
            .unwrap_or_else(|| panic!("{uid} detail"))
            .meta
            .clone()
    };
    // fct_orders: in-file `owner` beats the schema `config.meta` value, the
    // schema `zone` beats the tree, the tree fills `team`/`domain`, and the
    // nested dict stringifies to compact JSON.
    let fct = meta("model.sample.fct_orders");
    assert_eq!(fct["owner"], "alice");
    assert_eq!(fct["zone"], "z1");
    assert_eq!(fct["deep"], r#"{"x":1}"#);
    assert_eq!(fct["domain"], "finance");
    assert_eq!(fct["team"], "analytics-eng");
    // stg_orders: top-level `meta:` on the schema entry + the project tree.
    let stg = meta("model.sample.stg_orders");
    assert_eq!(stg["pii"], "true");
    assert_eq!(stg["team"], "analytics-eng");
    assert!(
        !stg.contains_key("domain"),
        "staging has no marts tree meta"
    );
    // dim_customers: schema `config.meta` + tree.
    assert_eq!(meta("model.sample.dim_customers")["owner"], "bob");
    // A marts model with no schema/in-file meta inherits the whole tree.
    let agg = meta("model.sample.agg_country_orders");
    assert_eq!(agg.len(), 2);
    // Seed: the `seeds:` properties `config.meta` + the `seeds:` tree.
    let seed = meta("seed.sample.country_codes");
    assert_eq!(seed["owner"], "data-eng");
    assert_eq!(seed["domain"], "reference");
    // Snapshots: `{% config(meta=) %}` in-file and the snapshots.yml
    // `config.meta`, each over the `snapshots:` tree.
    let block_snap = meta("snapshot.sample.orders_snapshot");
    assert_eq!(block_snap["owner"], "snap-file");
    assert_eq!(block_snap["domain"], "snapshots");
    let yml_snap = meta("snapshot.sample.customers_snapshot");
    assert_eq!(yml_snap["owner"], "snapshot-eng");
    assert_eq!(yml_snap["domain"], "snapshots");
    // Sources: source-level meta cascades to every table; a table-level meta
    // overrides per key. The `sources:` project tree is the LOWEST level:
    // `tree_src` (source level) reaches every table, `tree_tbl` only its
    // table, and schema-level `meta:` beats the tree on shared keys.
    let orders = meta("source.sample.raw.orders");
    assert_eq!(orders["owner"], "payments-eng");
    assert_eq!(orders["shared"], "yes");
    assert_eq!(orders["tree_src"], "proj-tree-src");
    assert_eq!(orders["tree_tbl"], "proj-tree-tbl");
    let customers = meta("source.sample.raw.customers");
    assert_eq!(customers["owner"], "data-eng");
    assert_eq!(customers["shared"], "yes");
    assert_eq!(customers["tree_src"], "proj-tree-src");
    assert!(
        !customers.contains_key("tree_tbl") && !customers.contains_key("flat_key"),
        "table-level tree meta and the package-less `sources: raw:` key must not land: {customers:?}"
    );
    // Bare top-level `meta:` on a seed — dbt accepts it like `config.meta`,
    // over the `seeds:` tree. The values also pin PyYAML scalar resolution:
    // bare `yes`/`on`/`off` are bools ("true"/"false" stringified) while
    // `y`/`n` stay strings.
    let extra_seed = meta("seed.sample.extra_seed");
    assert_eq!(extra_seed["bare_seed_meta"], "LANDED");
    assert_eq!(extra_seed["domain"], "reference");
    assert_eq!(extra_seed["seed_flag"], "true");
    assert_eq!(extra_seed["on_key"], "true");
    assert_eq!(extra_seed["off_key"], "false");
    assert_eq!(extra_seed["y_key"], "y");
    assert_eq!(extra_seed["n_key"], "n");
    // Bare top-level `meta:` on a yaml-defined snapshot works identically.
    let extra_snap = meta("snapshot.sample.extra_snapshot");
    assert_eq!(extra_snap["bare_snap_meta"], "LANDED");
    assert_eq!(extra_snap["domain"], "snapshots");
    // Two in-file config() calls: dbt applies each in order — `materialized`
    // resolves last-wins and `meta` merges per top-level key ({a:1,b:'first'}
    // then {b:'second',c:3}).
    let two = meta("model.sample.stg_two_configs");
    assert_eq!(two["a"], "1");
    assert_eq!(two["b"], "second");
    assert_eq!(two["c"], "3");
    assert_eq!(two["team"], "analytics-eng");
    assert_eq!(
        dag.detail("model.sample.stg_two_configs")
            .and_then(|d| d.materialized.clone())
            .as_deref(),
        Some("table"),
        "the second config() call's materialized wins"
    );
}

#[test]
fn doc_blocks_render_in_descriptions() {
    // `{{ doc() }}` refs in schema.yml descriptions expand to the docs block
    // bodies in models/marts/docs.md, the way real dbt renders them (the
    // consistency suite pins byte-parity against a real-dbt manifest).
    let dag = load_dag_from_source(PROJECT).unwrap();
    // Model description: multiline block, body trimmed like dbt's
    // `block_contents.strip()`.
    assert_eq!(
        dag.detail("model.sample.fct_orders")
            .expect("fct_orders detail")
            .description
            .as_deref(),
        Some("One row per order, enriched with the customer's country.\n\nGrain: `order_id`."),
    );
    // Model description: `{{-` whitespace control eats the newline before
    // the tag, exactly like dbt's Jinja render.
    let stg = dag.detail("model.sample.stg_orders").unwrap();
    assert_eq!(
        stg.description.as_deref(),
        Some(
            "Staged orders.Orders exactly as exported by the operational app.\nRefreshed nightly."
        ),
    );
    // Model column description: an inline (single-line) block.
    assert_eq!(
        stg.columns
            .iter()
            .find(|c| c.name == "id")
            .and_then(|c| c.description.as_deref()),
        Some("Surrogate primary key (unique, not null)."),
    );
    // Source-table description: whitespace-controlled `{%- docs -%}` block.
    let src = dag
        .detail("source.sample.raw.orders")
        .expect("raw.orders detail");
    assert_eq!(
        src.description.as_deref(),
        Some("Orders exactly as exported by the operational app."),
    );
    // Source-table description: a `{# #}` Jinja comment disappears entirely
    // (dbt renders the description as Jinja first) — the doc() inside it is
    // never evaluated, so even an undefined block warns/errors nothing.
    let cust = dag
        .detail("source.sample.raw.customers")
        .expect("raw.customers detail");
    assert_eq!(cust.description.as_deref(), Some("Raw customers"));
    // Source-table COLUMN description: 2-arg `doc('sample', 'x')` naming the
    // project itself resolves locally (verified against real dbt).
    assert_eq!(
        src.columns
            .iter()
            .find(|c| c.name == "id")
            .and_then(|c| c.description.as_deref()),
        Some("Surrogate primary key (unique, not null)."),
    );
    // Exposure description: text mixed around the doc() ref.
    assert_eq!(
        dag.detail("exposure.sample.orders_dashboard")
            .expect("orders_dashboard detail")
            .description
            .as_deref(),
        Some("Weekly orders overview for finance. Sourced from order facts."),
    );
}

#[test]
fn undefined_doc_ref_warns_and_keeps_raw_text() {
    // Real dbt hard-errors on a missing docs block; dbtl degrades: one
    // warning, the raw `{{ doc() }}` text stays, the load succeeds.
    use std::fs;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("dbt_project.yml"),
        "name: d\nversion: \"1.0.0\"\nconfig-version: 2\n",
    )
    .unwrap();
    fs::create_dir(root.join("models")).unwrap();
    fs::write(root.join("models/m.sql"), "select 1 as id\n").unwrap();
    fs::write(
        root.join("models/schema.yml"),
        "version: 2\nmodels:\n  - name: m\n    description: \"{{ doc('ghost') }}\"\n",
    )
    .unwrap();

    let (dag, warnings) = load_dag_from_source_with_warnings(root).expect("project loads");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("ghost"), "{warnings:?}");
    assert_eq!(
        dag.detail("model.d.m")
            .expect("m detail")
            .description
            .as_deref(),
        Some("{{ doc('ghost') }}"),
        "unresolved doc() keeps the raw text"
    );
}

#[test]
fn unclosed_and_stray_docs_tags_warn() {
    // Real dbt hard-errors on a `{% docs %}` with no `{% enddocs %}` (and on
    // a stray `{% enddocs %}`); dbtl degrades to warnings and keeps loading.
    use std::fs;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("dbt_project.yml"),
        "name: d\nversion: \"1.0.0\"\nconfig-version: 2\n",
    )
    .unwrap();
    fs::create_dir(root.join("models")).unwrap();
    fs::write(root.join("models/m.sql"), "select 1 as id\n").unwrap();
    fs::write(
        root.join("models/broken.md"),
        "{% docs good %}GOOD{% enddocs %}\n{% docs orphan %}no close\n",
    )
    .unwrap();
    fs::write(root.join("models/stray.md"), "text\n{% enddocs %}\n").unwrap();
    fs::write(
        root.join("models/schema.yml"),
        "version: 2\nmodels:\n  - name: m\n    description: \"{{ doc('good') }}\"\n",
    )
    .unwrap();

    let (dag, warnings) = load_dag_from_source_with_warnings(root).expect("project loads");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("unclosed") && w.contains("orphan")),
        "{warnings:?}"
    );
    assert!(warnings.iter().any(|w| w.contains("stray")), "{warnings:?}");
    // Well-formed blocks in the same files still resolve.
    assert_eq!(
        dag.detail("model.d.m")
            .expect("m detail")
            .description
            .as_deref(),
        Some("GOOD")
    );
}

#[test]
fn docs_paths_config_scans_only_listed_dirs() {
    // dbt's `docs-paths` restricts the .md scan ("will only look in the
    // specified directory"); unset it scans every resource path (the sample
    // project exercises the default via models/marts/docs.md).
    use std::fs;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("dbt_project.yml"),
        "name: d\nversion: \"1.0.0\"\nconfig-version: 2\ndocs-paths: [\"docs\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("models")).unwrap();
    fs::write(
        root.join("docs/d.md"),
        "{% docs shared %}SHARED{% enddocs %}\n",
    )
    .unwrap();
    // A block OUTSIDE docs-paths is invisible to dbt — left unresolved here.
    fs::write(
        root.join("models/hidden.md"),
        "{% docs hidden %}HIDDEN{% enddocs %}\n",
    )
    .unwrap();
    fs::write(root.join("models/m.sql"), "select 1 as id\n").unwrap();
    fs::write(
        root.join("models/schema.yml"),
        "version: 2\nmodels:\n  - name: m\n    description: \"{{ doc('shared') }}\"\n    columns:\n      - name: id\n        description: \"{{ doc('hidden') }}\"\n",
    )
    .unwrap();

    let (dag, warnings) = load_dag_from_source_with_warnings(root).expect("project loads");
    let detail = dag.detail("model.d.m").expect("m detail");
    assert_eq!(detail.description.as_deref(), Some("SHARED"));
    assert_eq!(
        detail.columns[0].description.as_deref(),
        Some("{{ doc('hidden') }}"),
        "a block outside docs-paths stays unresolved"
    );
    assert!(
        warnings.iter().any(|w| w.contains("hidden")),
        "{warnings:?}"
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

#[test]
fn yaml_defined_snapshot_becomes_a_node() {
    // dbt 1.9+ lets a snapshot be declared entirely in schema.yml (a
    // `relation:` key instead of a `{% snapshot %}` SQL block); it must be
    // synthesized as a real node, not silently dropped.
    let m = manifest_from_source(YAML_SNAPSHOT_PROJECT).expect("project parses");
    let node = m
        .nodes
        .get("snapshot.ysnap.s1")
        .expect("s1 is synthesized as a node");
    assert_eq!(node.resource_type, "snapshot");
    assert_eq!(node.config.materialized.as_deref(), Some("snapshot"));
    assert_eq!(
        node.raw_code.as_deref(),
        Some("select * from {{ ref('base') }}"),
        "raw_code wraps the bare relation like dbt V2 does"
    );
    assert_eq!(node.description.as_deref(), Some("yaml snapshot"));
}

#[test]
fn yaml_snapshot_edges_resolve_in_both_directions() {
    let dag = load_dag_from_source(YAML_SNAPSHOT_PROJECT).expect("project loads");
    let up = dag.upstream("snapshot.ysnap.s1");
    assert!(
        up.contains("model.ysnap.base"),
        "s1's relation ref is an upstream edge, got {up:?}"
    );
    let down = dag.downstream("snapshot.ysnap.s1");
    assert!(
        down.contains("model.ysnap.uses_snap"),
        "a model ref'ing the yaml snapshot is a downstream edge, got {down:?}"
    );
}

#[test]
fn versioned_ref_resolves_to_unversioned_model() {
    let dag = load_dag_from_source(YAML_SNAPSHOT_PROJECT).expect("project loads");
    let up = dag.upstream("model.ysnap.uses_versioned");
    assert!(
        up.contains("model.ysnap.base"),
        "ref('base', v=2) resolves to the unversioned model, got {up:?}"
    );
}

#[test]
fn duplicate_stem_keeps_first_and_warns() {
    // `models/a/dup.sql` and `models/b/dup.sql` both produce the uid
    // `model.coll.dup`; path-sorted collection makes `a/dup.sql` the winner.
    let (dag, warnings) =
        load_dag_from_source_with_warnings(COLLISION_PROJECT).expect("project loads");
    assert_eq!(
        dag.upstream("model.coll.dup"),
        HashSet::from(["model.coll.parent_a".to_string()]),
        "the first (path-sorted) file wins"
    );
    assert!(
        dag.downstream("model.coll.parent_b").is_empty(),
        "the losing file's ref never became an edge"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("dup") && w.contains("models/b/dup.sql")),
        "the losing file is named in a warning, got {warnings:?}"
    );
}

#[test]
fn tests_and_data_tests_merge_instead_of_dropping_the_file() {
    // `models/schema.yml` declares BOTH `tests:` and `data_tests:` on the same
    // column — real dbt hard-errors on this, but dbtl unions instead of
    // dropping the whole file (and its `sources:` block) silently.
    let dag = load_dag_from_source(COLLISION_PROJECT).expect("project loads");
    assert!(
        dag.contains("source.coll.rawsrc.t1"),
        "the file survived parsing"
    );
    let kinds: HashSet<&str> = dag
        .tests("model.coll.parent_a")
        .iter()
        .map(|t| t.kind.as_str())
        .collect();
    assert_eq!(kinds, HashSet::from(["unique", "not_null"]));
}

#[test]
fn schema_relationships_test_resolves_ref_and_source_targets() {
    // `synth_tests` resolves a `relationships` test's `to:` through
    // the same ref()/source() extractors the .sql scan uses, so `Dag::build`
    // fills `ref_uid`/`ref_name`/`ref_column` identically in both load modes.
    use std::fs;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("dbt_project.yml"),
        "name: relp\nversion: \"1.0.0\"\nconfig-version: 2\nmodel-paths: [\"models\"]\n",
    )
    .unwrap();
    let models = root.join("models");
    fs::create_dir(&models).unwrap();
    fs::write(
        models.join("orders.sql"),
        "select 1 as id, 'a' as customer_key, 2 as pay_key\n",
    )
    .unwrap();
    fs::write(models.join("customers.sql"), "select 'a' as key\n").unwrap();
    fs::write(
        models.join("schema.yml"),
        "version: 2\n\
         sources:\n  - name: raw\n    tables:\n      - name: payments\n\
         models:\n  - name: orders\n    columns:\n      - name: customer_key\n        \
         tests:\n          - relationships:\n              to: ref('customers')\n              \
         field: key\n      - name: pay_key\n        tests:\n          - relationships:\n              \
         to: source('raw','payments')\n              field: id\n",
    )
    .unwrap();

    let dag = load_dag_from_source(root).expect("project loads");
    let tests = dag.tests("model.relp.orders");
    let rel_ref = tests
        .iter()
        .find(|t| t.kind == "relationships" && t.column_name.as_deref() == Some("customer_key"))
        .expect("ref relationships test captured");
    assert_eq!(rel_ref.ref_uid.as_deref(), Some("model.relp.customers"));
    assert_eq!(rel_ref.ref_name.as_deref(), Some("customers"));
    assert_eq!(rel_ref.ref_column.as_deref(), Some("key"));

    let rel_src = tests
        .iter()
        .find(|t| t.kind == "relationships" && t.column_name.as_deref() == Some("pay_key"))
        .expect("source relationships test captured");
    assert_eq!(rel_src.ref_uid.as_deref(), Some("source.relp.raw.payments"));
    // ref_name is the target NODE's name — a source table's display name.
    assert_eq!(rel_src.ref_name.as_deref(), Some("payments"));
    assert_eq!(rel_src.ref_column.as_deref(), Some("id"));

    // An unresolvable `to:` keeps the test registered, just target-less —
    // matching the manifest path where a missing ref leaves ref_uid empty.
    fs::write(
        models.join("schema2.yml"),
        "version: 2\nmodels:\n  - name: orders\n    columns:\n      - name: id\n        \
         tests:\n          - relationships:\n              to: ref('ghost')\n              field: id\n",
    )
    .unwrap();
    let dag = load_dag_from_source(root).expect("project reloads");
    let ghost = dag
        .tests("model.relp.orders")
        .iter()
        .find(|t| t.kind == "relationships" && t.column_name.as_deref() == Some("id"))
        .expect("unresolved relationships test still captured");
    assert_eq!(ghost.ref_uid, None, "unresolvable to: has no target");
}

#[test]
fn invalid_yaml_emits_warning_but_project_loads() {
    let (dag, warnings) =
        load_dag_from_source_with_warnings(COLLISION_PROJECT).expect("project loads");
    assert!(
        warnings.iter().any(|w| w.contains("broken.yml")),
        "warnings: {warnings:?}"
    );
    assert!(
        dag.contains("model.coll.parent_a"),
        "the rest of the project still loads"
    );
}

// ---- governance / config fields --------------------------------------------

/// Every intake rung of the `in-file config() > schema.yml > project tree >
/// default` chain is exercised by the fixture's governance models — these
/// are the same values `tests/consistency.rs` proves equal to real dbt.
#[test]
fn governance_fields_resolve_through_all_intake_rungs() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let d = |uid: &str| dag.detail(uid).expect(uid).clone();

    // fct_public_api: in-file `config(access='public')` beats the schema
    // top-level `access: protected`. `deprecation_date` is a model
    // PROPERTY — schema.yml is its only intake, kept as the declared
    // string (real dbt stamps it as a local-midnight ISO timestamp).
    let pub_api = d("model.sample.fct_public_api");
    assert_eq!(pub_api.access.as_deref(), Some("public"));
    assert_eq!(pub_api.deprecation_date.as_deref(), Some("2026-12-31"));

    // fct_contracted: schema `config:` spellings — `access: private` needs
    // the group membership `config: group:` supplies; the enforced
    // contract plus model- and column-level `constraints:` ride the same
    // wire shapes the manifest records.
    let contracted = d("model.sample.fct_contracted");
    assert_eq!(contracted.access.as_deref(), Some("private"));
    assert_eq!(contracted.group.as_deref(), Some("finance"));
    assert!(contracted.contract_enforced);
    let types: Vec<&str> = contracted
        .constraints
        .iter()
        .map(|c| c.constraint_type.as_str())
        .collect();
    assert_eq!(types, ["primary_key", "check"]);
    assert_eq!(contracted.constraints[0].columns, vec!["id".to_string()]);
    assert_eq!(
        contracted.constraints[1].expression.as_deref(),
        Some("id > 0")
    );
    let id_col = contracted
        .columns
        .iter()
        .find(|c| c.name == "id")
        .expect("id column");
    assert_eq!(
        id_col
            .constraints
            .iter()
            .map(|c| c.constraint_type.as_str())
            .collect::<Vec<_>>(),
        ["not_null"]
    );

    // fct_incr_file: the in-file rung — the dict-literal `contract` and
    // `incremental_strategy` come out of `config(…)` itself.
    let incr_file = d("model.sample.fct_incr_file");
    assert!(incr_file.contract_enforced);
    assert_eq!(
        incr_file.incremental_strategy.as_deref(),
        Some("delete+insert")
    );

    // fct_incr_tree: the project-tree leaf is the ONLY declaration site —
    // `+access: private` (legal because `+group` lands too), `+group`, and
    // `+incremental_strategy` all arrive through the lowest rung.
    let incr_tree = d("model.sample.fct_incr_tree");
    assert_eq!(incr_tree.access.as_deref(), Some("private"));
    assert_eq!(incr_tree.group.as_deref(), Some("finance"));
    assert_eq!(
        incr_tree.incremental_strategy.as_deref(),
        Some("insert_overwrite")
    );

    // fct_incr_schema: `config: incremental_strategy: append` (schema.yml)
    // beats the leaf's `+incremental_strategy: insert_overwrite`.
    assert_eq!(
        d("model.sample.fct_incr_schema")
            .incremental_strategy
            .as_deref(),
        Some("append")
    );

    // The defaults an undeclared model lands on: `access: protected`
    // (dbt's model default, NOT null) and `language: "sql"`; `group` and
    // `contract` simply stay absent.
    let stg = d("model.sample.stg_orders");
    assert_eq!(stg.access.as_deref(), Some("protected"));
    assert_eq!(stg.language.as_deref(), Some("sql"));
    assert!(stg.group.is_none() && !stg.contract_enforced);
    // Seeds carry neither access nor language; snapshots record sql.
    let seed = d("seed.sample.country_codes");
    assert!(seed.access.is_none() && seed.language.is_none());
    assert_eq!(
        d("snapshot.sample.orders_snapshot").language.as_deref(),
        Some("sql")
    );
}

/// The governance lint rules must fire in source mode exactly like they do
/// on the compiled manifest — the point of loading the fields.
#[test]
fn source_mode_governance_lint_rules_detect_violations() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let hits = |rule: LintRule| -> Vec<&str> {
        findings
            .iter()
            .filter(|f| f.rule == rule)
            .map(|f| f.node_uid.as_str())
            .collect()
    };
    assert_eq!(
        hits(LintRule::PublicModelsWithoutContract),
        ["model.sample.fct_public_api"]
    );
    // `access != "public"` counts private AND protected — the exposures'
    // model parents land on protected by dbt's default.
    let mut exposures = hits(LintRule::ExposuresDependentOnPrivateModels);
    exposures.sort();
    assert_eq!(
        exposures,
        [
            "exposure.sample.churn_notebook",
            "exposure.sample.orders_dashboard"
        ]
    );
    assert_eq!(
        hits(LintRule::ContractColumnsWithoutType),
        ["model.sample.fct_contracted"]
    );
}

/// A source that omits `schema:` resolves it to the source NAME (dbt's
/// default) — `ledger.payments` and `shared.payments` are then the same
/// `shared.payments` relation: the `duplicate_sources` pair.
#[test]
fn schema_less_source_defaults_schema_to_source_name() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    for uid in [
        "source.sample.ledger.payments",
        "source.sample.shared.payments",
    ] {
        assert_eq!(
            dag.detail(uid).and_then(|d| d.schema.clone()).as_deref(),
            Some("shared"),
            "{uid} resolves to schema 'shared'"
        );
    }
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let dups: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == LintRule::DuplicateSources)
        .map(|f| f.node_uid.as_str())
        .collect();
    assert_eq!(
        dups,
        [
            "source.sample.ledger.payments",
            "source.sample.shared.payments"
        ],
        "both sides of the schema-defaulted duplicate flag"
    );
}

/// Model-level generic tests carry their column in `test_metadata.kwargs` —
/// the legacy `column_name:` and dbt >=1.10's `arguments:` spellings alike —
/// normalized into `TestInfo.column_name` so `has_pk_guarantee` sees the
/// unique+not_null pair (fct_model_tests is NOT a missing-pk finding).
#[test]
fn model_level_test_column_name_comes_from_kwargs() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let kinds: Vec<(&str, Option<&str>)> = dag
        .tests("model.sample.fct_model_tests")
        .iter()
        .map(|t| (t.kind.as_str(), t.column_name.as_deref()))
        .collect();
    assert!(kinds.contains(&("unique", Some("id"))), "{kinds:?}");
    assert!(kinds.contains(&("not_null", Some("id"))), "{kinds:?}");
}

/// `has_pk_guarantee` must see the kwargs-carried column in source mode:
/// fct_model_tests declares unique+not_null on `id` through model-level
/// `column_name:`/`arguments:` spellings — it must NOT surface in
/// `missing_primary_key_tests` (the false positive the kwargs read exists
/// to prevent).
#[test]
fn model_level_test_kwargs_satisfy_missing_pk_in_source_mode() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let pks: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == LintRule::MissingPrimaryKeyTests)
        .map(|f| f.node_uid.as_str())
        .collect();
    assert!(
        !pks.contains(&"model.sample.fct_model_tests"),
        "fct_model_tests carries unique+not_null on id via kwargs: {pks:?}"
    );
}

/// A whitespace-only column `description:` counts as missing (the shared
/// blank predicate) — `undocumented_columns` names the column in source
/// mode, identical to the verbatim-whitespace manifest wire.
#[test]
fn whitespace_only_column_description_is_undocumented() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let notes = dag
        .detail("model.sample.fct_model_tests")
        .expect("detail")
        .columns
        .iter()
        .find(|c| c.name == "notes")
        .expect("notes column");
    assert!(notes.description.is_none());
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let f = findings
        .iter()
        .find(|f| {
            f.rule == LintRule::UndocumentedColumns && f.node_uid == "model.sample.fct_model_tests"
        })
        .expect("undocumented_columns fires on fct_model_tests");
    assert!(f.message.contains("notes"), "{:?}", f.message);
}

/// `dim_customers` reads `source('raw', 'customers')` directly — the
/// `marts_depend_on_source` finding's position underlines that call
/// (line 13, columns 9–35; the `{{ }}` wrapper is not part of the span).
/// The `--`-commented `ref('country_codes')` is a real edge by dbt's
/// Jinja-before-SQL semantics, and the identifier-embedded `my_ref(` /
/// `other_source(` fakes contribute no calls.
#[test]
fn graph_findings_position_the_source_call_that_made_the_edge() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let f = findings
        .iter()
        .find(|f| {
            f.rule == LintRule::MartsDependOnSource && f.node_uid == "model.sample.dim_customers"
        })
        .expect("marts_depend_on_source fires on dim_customers");
    assert_eq!(
        f.message,
        "marts model dim_customers reads source raw.customers"
    );
    let loc = f.location.as_ref().expect("a call-site position lands");
    assert_eq!(loc.file, "models/marts/dim_customers.sql");
    assert_eq!(
        (loc.line, loc.column, loc.end_line, loc.end_column),
        (Some(13), Some(9), Some(13), Some(35)),
        "the source('raw', 'customers') call on the from line"
    );
    assert_eq!(f.label.as_deref(), Some("source used here"));
    // No staging model reads raw.customers — nothing concrete to suggest,
    // so the finding keeps the registry help rather than a named fix.
    assert_eq!(f.help, None);
    // stg_dup_probe's two source() calls: first in file order is primary,
    // the second its labeled secondary.
    let f = findings
        .iter()
        .find(|f| {
            f.rule == LintRule::MultipleSourcesJoined && f.node_uid == "model.sample.stg_dup_probe"
        })
        .expect("multiple_sources_joined fires on stg_dup_probe");
    assert_eq!(
        f.message,
        "stg_dup_probe reads sources ledger.payments, shared.payments"
    );
    let loc = f.location.as_ref().expect("a call-site position lands");
    assert_eq!(loc.file, "models/staging/stg_dup_probe.sql");
    assert_eq!(
        (loc.line, loc.column, loc.end_line, loc.end_column),
        (Some(4), Some(9), Some(4), Some(37))
    );
    assert_eq!(
        f.labels
            .iter()
            .map(|l| (l.line, l.column, l.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(7, Some(9), "source")]
    );
}

/// `fct_daily_orders` declares `materialized='incremental'` with no
/// `unique_key` anywhere — the finding underlines the config argument's
/// VALUE (line 1, columns 24–37: the `'incremental'` literal).
#[test]
fn incremental_without_unique_key_positions_the_config_arg() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let f = findings
        .iter()
        .find(|f| {
            f.rule == LintRule::IncrementalWithoutUniqueKey
                && f.node_uid == "model.sample.fct_daily_orders"
        })
        .expect("incremental_without_unique_key fires on fct_daily_orders");
    let loc = f.location.as_ref().expect("the config arg positions");
    assert_eq!(loc.file, "models/marts/fct_daily_orders.sql");
    assert_eq!(
        (loc.line, loc.column, loc.end_line, loc.end_column),
        (Some(1), Some(24), Some(1), Some(37))
    );
    assert_eq!(f.label.as_deref(), Some("materialized 'incremental'"));
    // fct_line_items is incremental too but its unique_key comes from
    // schema.yml — no finding.
    assert!(
        !findings.iter().any(|f| {
            f.rule == LintRule::IncrementalWithoutUniqueKey
                && f.node_uid == "model.sample.fct_line_items"
        }),
        "the schema.yml unique_key clears the rule"
    );
}

/// The byte offset `text`'s `line`/`column` (1-based, columns count `char`s
/// like `byte_span_position`) marks.
fn offset_of(text: &str, line: u32, column: u32) -> usize {
    let mut byte = 0usize;
    for _ in 1..line {
        byte += text[byte..].find('\n').expect("line exists") + 1;
    }
    byte + text[byte..]
        .char_indices()
        .nth(column as usize - 1)
        .expect("column exists")
        .0
}

/// Every positioned finding's `line:col` (and every secondary label's)
/// underlines text that actually starts `ref(`/`source(` — or the
/// `materialized` literal for `incremental_without_unique_key`. The
/// mechanical wrong-position detector: an off-by-one in extraction or the
/// masking shows up as a span that isn't a call.
#[test]
fn every_position_underlines_a_real_call() {
    let dag = load_dag_from_source(PROJECT).unwrap();
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let positioned: Vec<_> = findings
        .iter()
        .filter(|f| {
            f.location
                .as_ref()
                .is_some_and(|l| l.line.is_some() && l.column.is_some())
        })
        .collect();
    assert!(
        !positioned.is_empty(),
        "the fixture produces call-site-positioned findings"
    );
    for f in &positioned {
        let loc = f.location.as_ref().unwrap();
        let text = std::fs::read_to_string(std::path::Path::new(PROJECT).join(&loc.file))
            .expect("the located file exists");
        let mut spans = vec![(loc.line.unwrap(), loc.column.unwrap(), "primary")];
        spans.extend(
            f.labels
                .iter()
                .map(|l| (l.line, l.column.expect("labels carry columns"), "label")),
        );
        for (line, col, kind) in spans {
            let at = &text[offset_of(&text, line, col)..];
            let is_call = at.starts_with("ref(") || at.starts_with("source(");
            if f.rule == LintRule::IncrementalWithoutUniqueKey {
                assert!(
                    at.starts_with("'incremental'") || at.starts_with("\"incremental\""),
                    "{kind} {line}:{col} of {} must be the materialized literal, found {:?}",
                    loc.file,
                    &at[..30]
                );
            } else {
                assert!(
                    is_call,
                    "{kind} {line}:{col} of {} must start a ref()/source() call, found {:?}",
                    loc.file,
                    &at[..30]
                );
            }
        }
    }
    // Two evaluations agree on everything — the deterministic contract.
    let again = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    assert_eq!(findings, again);
}

// ---- semantic_models: the source-mode consumption channel --------------------

/// A minimal project on disk: `dbt_project.yml` + `models/` + the files
/// `files` names (`(path, contents)` pairs). Returns the tempdir (kept
/// alive by the caller).
fn tiny_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    use std::fs;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join("dbt_project.yml"),
        "name: d\nversion: \"1.0.0\"\nconfig-version: 2\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("models")).unwrap();
    for (path, contents) in files {
        fs::write(root.join(path), contents).unwrap();
    }
    tmp
}

#[test]
fn semantic_models_entries_mark_the_consumed_models_dependents() {
    // `semantic_models:` is the one semantic-layer resource source mode
    // reads — not as nodes (a real manifest would carry them) but as the
    // `semantic_dependents` mark on each consumed model, so `unused_models`
    // answers "the semantic layer reads this" like manifest mode does.
    // Disabled entries (either spelling) and unresolved refs mark nothing.
    let tmp = tiny_project(&[
        ("models/stg_a.sql", "select 1 as id\n"),
        ("models/stg_b.sql", "select 1 as id\n"),
        ("models/stg_c.sql", "select 1 as id\n"),
        ("models/stg_d.sql", "select 1 as id\n"),
        (
            "models/schema.yml",
            "version: 2\n\
             models:\n  - name: stg_a\n  - name: stg_b\n  - name: stg_c\n  - name: stg_d\n\
             semantic_models:\n  - name: sm_a\n    model: \"ref('stg_a')\"\n  - name: sm_off\n    model: \"ref('stg_b')\"\n    enabled: false\n  - name: sm_cfg_off\n    model: \"ref('stg_c')\"\n    config:\n      enabled: false\n  - name: sm_ghost\n    model: \"ref('nope')\"\n",
        ),
    ]);
    let (dag, _warnings) = load_dag_from_source_with_warnings(tmp.path()).expect("project loads");
    assert_eq!(
        dag.detail("model.d.stg_a").unwrap().semantic_dependents,
        vec!["sm_a".to_string()],
        "the entry's name lands on the consumed model"
    );
    for uid in ["model.d.stg_b", "model.d.stg_c", "model.d.stg_d"] {
        assert!(
            dag.detail(uid).unwrap().semantic_dependents.is_empty(),
            "disabled/absent consumers mark nothing: {uid}"
        );
    }
    // …and the lint answer: the consumed model is used, the rest flag.
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let flagged: Vec<&str> = findings
        .iter()
        .filter(|f| f.rule == LintRule::UnusedModels)
        .map(|f| f.node_uid.as_str())
        .collect();
    assert_eq!(
        flagged,
        vec!["model.d.stg_b", "model.d.stg_c", "model.d.stg_d"],
        "a semantic_models consumer is a use in source mode too"
    );
}

#[test]
fn references_deprecated_model_evaluates_in_source_mode() {
    // `deprecation_date` rides the schema.yml entry (the schema.yml governance intake) —
    // a dependent's finding lands with the same shape manifest mode gives,
    // position included (the ref() in the dependent's own .sql).
    let tmp = tiny_project(&[
        ("models/dim_old.sql", "select 1 as id\n"),
        ("models/fct_a.sql", "select * from {{ ref('dim_old') }}\n"),
        (
            "models/schema.yml",
            "version: 2\n\
             models:\n  - name: dim_old\n    deprecation_date: \"2026-12-01\"\n  - name: fct_a\n",
        ),
    ]);
    let dag = load_dag_from_source(tmp.path()).expect("project loads");
    let findings = run_rules(LintInput::new(&dag, &LintConfig::default())).findings;
    let hits: Vec<_> = findings
        .iter()
        .filter(|f| f.rule == LintRule::ReferencesDeprecatedModel)
        .collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_uid, "model.d.fct_a");
    assert_eq!(
        hits[0].message,
        "fct_a depends on dim_old (deprecated on 2026-12-01)"
    );
    let loc = hits[0].location.as_ref().expect("a located finding");
    assert_eq!(loc.file, "models/fct_a.sql");
    assert_eq!(loc.line, Some(1));
}
