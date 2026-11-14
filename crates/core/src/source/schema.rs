use super::*;

// ---- dbt_project.yml / schema.yml deserialization (only the fields we use) ----

#[derive(Debug, Deserialize, Default)]
pub(super) struct DbtProject {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(
        rename = "model-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    pub(super) model_paths: Option<Vec<String>>,
    #[serde(rename = "seed-paths", default, deserialize_with = "de_opt_string_vec")]
    pub(super) seed_paths: Option<Vec<String>>,
    /// `data-paths` — the pre-1.0 alias for `seed-paths` (dbt still accepts
    /// it with a deprecation warning). Declaring both is a dbt error;
    /// `seed-paths` wins here.
    #[serde(rename = "data-paths", default, deserialize_with = "de_opt_string_vec")]
    pub(super) data_paths: Option<Vec<String>>,
    #[serde(
        rename = "snapshot-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    pub(super) snapshot_paths: Option<Vec<String>>,
    #[serde(
        rename = "source-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    pub(super) source_paths: Option<Vec<String>>,
    /// `docs-paths` scopes the `.md` docs-block scan; unset -> every resource
    /// path. The next three are read ONLY for that default union (verified:
    /// a block in `macros/` resolves with no `docs-paths` set).
    #[serde(rename = "docs-paths", default, deserialize_with = "de_opt_string_vec")]
    pub(super) docs_paths: Option<Vec<String>>,
    #[serde(
        rename = "macro-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    pub(super) macro_paths: Option<Vec<String>>,
    #[serde(
        rename = "analysis-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    pub(super) analysis_paths: Option<Vec<String>>,
    #[serde(rename = "test-paths", default, deserialize_with = "de_opt_string_vec")]
    pub(super) test_paths: Option<Vec<String>>,
    /// The directory-hierarchical config trees (`+materialized` / `+enabled`
    /// under nested path keys). Kept as raw YAML and walked by
    /// [`resolve_tree_config`]; `Value::default()` is `Null`, which resolves to
    /// no config.
    #[serde(default)]
    pub(super) models: serde_json::Value,
    #[serde(default)]
    pub(super) seeds: serde_json::Value,
    #[serde(default)]
    pub(super) snapshots: serde_json::Value,
    /// The `sources:` config tree — keyed by the source's fqn (`<pkg>`:
    /// `<declaring-yml dirs>`: `<source>`: `<table>` — verified against real
    /// dbt's recorded fqn). `+enabled` parks the table; `+meta` and
    /// `+freshness` merge at the bottom of their precedence chains.
    #[serde(default)]
    pub(super) sources: serde_json::Value,
    /// The `exposures:` config tree — keyed by the exposure's fqn (`<pkg>`:
    /// `<declaring-yml dirs>`: `<name>` — the same fqn a compiled manifest
    /// records). `+enabled` parks the exposure; `+meta` merges below the
    /// entry's `meta:` (other `+`-configs like `+tags`/`+group` are not
    /// implemented for exposures).
    #[serde(default)]
    pub(super) exposures: serde_json::Value,
    /// The `vars:` block, raw — `var('name')` calls in `config(enabled=)`
    /// resolve against it (scoped `<name>:` entries shadowing the top
    /// level). `+enabled` does NOT see it: `dbt_project.yml` renders
    /// before `vars:` loads (see vars.rs for the two scopes).
    #[serde(default)]
    pub(super) vars: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaFile {
    #[serde(default)]
    pub(super) models: Vec<SchemaModel>,
    #[serde(default)]
    pub(super) sources: Vec<SchemaSource>,
    #[serde(default)]
    pub(super) seeds: Vec<SchemaModel>,
    #[serde(default)]
    pub(super) snapshots: Vec<SchemaModel>,
    #[serde(default)]
    pub(super) exposures: Vec<SchemaExposure>,
    /// `semantic_models:` — read only for each entry's `model:` ref, the
    /// semantic-layer consumption signal the synthesized manifest can't
    /// carry as nodes (no semantic-layer resources are synthesized).
    #[serde(default)]
    pub(super) semantic_models: Vec<SchemaSemanticModel>,
    /// `unit_tests:` — dbt ≥1.8 unit test declarations. Synthesized into
    /// `RawManifest::unit_tests` so `Dag::build` attaches them to their
    /// `model:` targets through the same intake a real manifest feeds
    /// (the `tests` side map — never a graph node).
    #[serde(default)]
    pub(super) unit_tests: Vec<SchemaUnitTest>,
}

/// Shared shape for `models:` / `seeds:` / `snapshots:` entries (only what we
/// read).
///
/// `tests` and `data_tests` are kept as SEPARATE fields (not one aliased
/// field) so a file declaring both keys on the same entry still deserializes
/// — serde treats an `alias` as just another name for the SAME field, so two
/// keys mapping to it is a duplicate-field error that silently drops the
/// whole file; the two lists are unioned by the caller instead.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaModel {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) config: SchemaConfig,
    #[serde(default, deserialize_with = "de_string_vec")]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) columns: Vec<SchemaColumn>,
    #[serde(default)]
    pub(super) tests: Vec<serde_json::Value>,
    #[serde(default)]
    pub(super) data_tests: Vec<serde_json::Value>,
    /// The `relation:` Jinja expression of a YAML-defined snapshot (dbt
    /// 1.9+), e.g. `ref('stg_orders')`. Always `None` for models/seeds and
    /// for a `{% snapshot %}`-block snapshot (its relation is implicit).
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) relation: Option<String>,
    /// Top-level `access:` / `group:` — dbt accepts these as bare model
    /// properties AND under `config:` (declaring both is a dbt parse error;
    /// `config:` wins here, the same convention `meta:` follows).
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) access: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) group: Option<String>,
    /// `deprecation_date:` — a model property (never a `config:` key).
    /// dbt validates it's a date; kept as the declared string.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) deprecation_date: Option<String>,
    /// `versions:` — dbt's versioned-model declaration. Read for NAMES
    /// only: a `versions:`-declared model and each entry's `defined_in:`
    /// are names `ref()` can legitimately reach — versioned nodes are
    /// never synthesized, so `unresolved_references` counts them
    /// resolved rather than judging an impossible target. Raw `Value`
    /// entries: each is `{v: …, defined_in: …}`-shaped (a `v:` value may
    /// itself be a number or string).
    #[serde(default)]
    pub(super) versions: Vec<serde_json::Value>,
    /// The model-level `constraints:` list — the manifest wire shape
    /// (`{type, columns, …}`); column-level constraints live on
    /// [`SchemaColumn::constraints`]. A property, not a `config:` key.
    #[serde(default)]
    pub(super) constraints: Vec<RawConstraint>,
    /// Top-level `meta:` dict — dbt treats it as equivalent to
    /// `config.meta` (both together are a dbt parse error; here it merges
    /// with `config.meta` winning per key). Kept as raw `Value` so a
    /// non-mapping `meta:` can't fail the whole file's deserialization.
    #[serde(default)]
    pub(super) meta: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaConfig {
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) materialized: Option<String>,
    /// `config.enabled:` — raw (a `{{ … }}` string must reach the
    /// enabled-expression evaluator; see [`de_enabled`]).
    #[serde(default, deserialize_with = "de_enabled")]
    pub(super) enabled: Option<serde_json::Value>,
    /// `config: meta:` — merges over a top-level `meta:` per key.
    #[serde(default)]
    pub(super) meta: serde_json::Value,
    /// `config: alias:` — the relation-name override (dbt's `alias` config).
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) alias: Option<String>,
    /// `config: unique_key:` — kept raw: dbt accepts a bare string or a
    /// string list and records the declared shape; `Dag::build` normalizes
    /// both to `Vec<String>`.
    #[serde(default)]
    pub(super) unique_key: serde_json::Value,
    /// `config: access:` / `config: group:` — the schema.yml spelling of the
    /// model governance configs (`+access`/`+group` and in-file
    /// `config(access=…)` are the other two intake paths).
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) access: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) group: Option<String>,
    /// `config: contract:` — dbt's canonical `{enforced: bool}` object (kept
    /// raw; a bare bool is tolerated as `enforced: <bool>` downstream).
    #[serde(default)]
    pub(super) contract: serde_json::Value,
    /// `config: incremental_strategy:` — `append` / `merge` /
    /// `delete+insert` / `insert_overwrite` / `microbatch`.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) incremental_strategy: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaColumn {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) data_type: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) description: Option<String>,
    /// The column-level `constraints:` list — dbt records it inside the
    /// column object (no `columns` key — the constraint covers the column).
    #[serde(default)]
    pub(super) constraints: Vec<RawConstraint>,
    #[serde(default)]
    pub(super) tests: Vec<serde_json::Value>,
    #[serde(default)]
    pub(super) data_tests: Vec<serde_json::Value>,
}

/// An `exposures:` entry (only what we surface). `depends_on` holds Jinja
/// strings (`ref('x')` / `source('a','b')`) per the dbt spec.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaExposure {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(rename = "type", default, deserialize_with = "de_opt_string")]
    pub(super) exposure_type: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) description: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) url: Option<String>,
    #[serde(default)]
    pub(super) owner: SchemaExposureOwner,
    #[serde(default)]
    pub(super) config: SchemaConfig,
    /// Top-level `meta:` dict — merged under `config.meta` per key, the
    /// same precedence a model entry's `meta:` follows (verified against
    /// real dbt: exposures accept both spellings together and record the
    /// merged map, config winning, on the wire).
    #[serde(default)]
    pub(super) meta: serde_json::Value,
    #[serde(default, deserialize_with = "de_string_vec")]
    pub(super) depends_on: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaExposureOwner {
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) name: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) email: Option<String>,
}

/// A `semantic_models:` entry (only what marks consumption). `model:` holds
/// a Jinja string (`ref('x')`) per the dbt spec; `enabled` is honored at
/// both spellings — top-level and `config.enabled` (config wins when both
/// are declared, the precedence the model `config:` block follows).
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaSemanticModel {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    /// The `model:` Jinja expression — `ref('x')` in every real project.
    /// Kept raw so a `disabled` entry still deserializes; the caller drops
    /// disabled entries before parsing the ref.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) model: Option<String>,
    /// Top-level `enabled:` — a semantic-model property (unlike models,
    /// where `enabled` only lives under `config:`). Raw — see [`de_enabled`].
    #[serde(default, deserialize_with = "de_enabled")]
    pub(super) enabled: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) config: SchemaSemanticModelConfig,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaSemanticModelConfig {
    #[serde(default, deserialize_with = "de_enabled")]
    pub(super) enabled: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaSource {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) database: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) schema: Option<String>,
    /// `meta:` at the source level — cascades to every table (a table-level
    /// `meta:` overrides it per key, verified against real dbt).
    #[serde(default)]
    pub(super) meta: serde_json::Value,
    /// `tags:` at the source level — accumulates with `config.tags` and the
    /// table-level tags (dbt unions them; it does not override).
    #[serde(default, deserialize_with = "de_string_vec")]
    pub(super) tags: Vec<String>,
    /// `loader:` — the tool loading these tables; cascades to every table
    /// (dbt ignores it on the table level and in `dbt_project.yml`).
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) loader: Option<String>,
    /// `loaded_at_field:` at the source level — inherited by every table
    /// unless the table overrides or explicitly nulls it.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) loaded_at_field: Option<String>,
    /// `freshness:` at the source level — per-threshold defaults for the
    /// tables. Three-state so `freshness: null` clears a tree-provided
    /// `+freshness` instead of passing it through.
    #[serde(default, deserialize_with = "de_opt_some")]
    pub(super) freshness: Option<Option<RawFreshness>>,
    /// The source-level `config:` block — its keys sit ABOVE the source's
    /// top-level properties and below the table's (source props < source
    /// `config:` < table props < table `config:`, verified).
    #[serde(default)]
    pub(super) config: SchemaSourceConfig,
    #[serde(default)]
    pub(super) tables: Vec<SchemaSourceTable>,
}

/// The `config:` block of a source or a source table — where dbt moved
/// `freshness`/`loaded_at_field`/`meta`/`enabled`/`tags` (the top-level
/// spellings still work). `enabled` lives ONLY here: a table's top-level
/// `enabled:` is ignored by dbt and a source's is a parse error — neither
/// is read.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaSourceConfig {
    /// `config.enabled:` — the ONLY disable channel for a source table.
    /// Raw — see [`de_enabled`].
    #[serde(default, deserialize_with = "de_enabled")]
    pub(super) enabled: Option<serde_json::Value>,
    /// `config.meta` — merges per key at its precedence level (a source
    /// `config.meta` sits below the table's top-level `meta:` — verified).
    #[serde(default)]
    pub(super) meta: serde_json::Value,
    /// `config.loaded_at_field` — three-state like the top-level spelling:
    /// absent inherits, `null` clears the inherited value.
    #[serde(default, deserialize_with = "de_opt_opt_string")]
    pub(super) loaded_at_field: Option<Option<String>>,
    /// `config.freshness` — merges per threshold at its precedence level;
    /// `null` stops the inheritance walk like the top-level spelling.
    #[serde(default, deserialize_with = "de_opt_some")]
    pub(super) freshness: Option<Option<RawFreshness>>,
    /// `config.tags` — unions with the other tag channels, not a replace.
    #[serde(default, deserialize_with = "de_string_vec")]
    pub(super) tags: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaSourceTable {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) description: Option<String>,
    /// schema.yml's `identifier:` — the table's real warehouse name when it
    /// differs from `name` (dbt renders `source()` to it in compiled SQL).
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) identifier: Option<String>,
    /// `meta:` at the table level — overrides the source's `meta:` per key.
    #[serde(default)]
    pub(super) meta: serde_json::Value,
    /// `tags:` at the table level — accumulates with `config.tags` and the
    /// source-level tags (dbt unions them; it does not override).
    #[serde(default, deserialize_with = "de_string_vec")]
    pub(super) tags: Vec<String>,
    /// `loaded_at_field:` at the table level — three-state: absent inherits
    /// the source's, explicit `null` clears it, a value overrides it.
    #[serde(default, deserialize_with = "de_opt_opt_string")]
    pub(super) loaded_at_field: Option<Option<String>>,
    /// `freshness:` at the table level — merges with the source's PER
    /// THRESHOLD (verified: a table's `error_after` keeps the inherited
    /// `warn_after`); an explicit `null` clears both.
    #[serde(default, deserialize_with = "de_opt_some")]
    pub(super) freshness: Option<Option<RawFreshness>>,
    /// The table-level `config:` block — the highest precedence level of
    /// the source-table config chain (and the ONLY place `enabled` is
    /// honored for a source table).
    #[serde(default)]
    pub(super) config: SchemaSourceConfig,
    #[serde(default)]
    pub(super) columns: Vec<SchemaColumn>,
    #[serde(default)]
    pub(super) tests: Vec<serde_json::Value>,
    #[serde(default)]
    pub(super) data_tests: Vec<serde_json::Value>,
}

/// A `unit_tests:` entry — dbt's YAML-declared unit test (dbt ≥1.8).
/// `model:` is a bare model name (not a Jinja `ref()`); `versions:`
/// selects the versioned members when the model is versioned;
/// `given:`/`expect:` deserialize into the wire `RawUnitTestIo` shape
/// directly — `input:`/`rows:`/`format:` are the surfaced fields
/// (`format:` tells a parseable `csv` block from a `sql` query string)
/// and the `fixture:` key rides along ignored, exactly like a real
/// manifest read.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaUnitTest {
    #[serde(deserialize_with = "de_string")]
    pub(super) name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) config: SchemaUnitTestConfig,
    #[serde(default)]
    pub(super) versions: SchemaUnitTestVersions,
    #[serde(default)]
    pub(super) given: Vec<RawUnitTestIo>,
    #[serde(default)]
    pub(super) expect: Option<RawUnitTestIo>,
}

/// The `config:` block of a `unit_tests:` entry. `enabled` is the ONLY
/// disable channel — a top-level `enabled:` is a dbt parse error, so it
/// is never read.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaUnitTestConfig {
    /// `config.enabled:` — raw, like every other `enabled` channel (a
    /// `{{ … }}` string must reach the expression evaluator).
    #[serde(default, deserialize_with = "de_enabled")]
    pub(super) enabled: Option<serde_json::Value>,
}

/// The `versions:` filter of a `unit_tests:` entry. `include` is a
/// version list or `'*'` (every version); `exclude` a version list the
/// selection subtracts. Kept as raw `Value` — numbers, `'beta'`-style
/// strings, and the wildcard all parse under dbt's schema.
#[derive(Debug, Deserialize, Default)]
pub(super) struct SchemaUnitTestVersions {
    #[serde(default)]
    pub(super) include: serde_json::Value,
    #[serde(default)]
    pub(super) exclude: serde_json::Value,
}
