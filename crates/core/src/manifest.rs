//! The dbt `manifest.json` wire types: the minimal subset of the manifest
//! schema this tool reads, plus the disk loader. Both the real-manifest path
//! (`load_manifest`) and the no-compile source loader ([`crate::source`], which
//! SYNTHESIZES a [`RawManifest`] of this exact shape) feed these types into
//! [`Dag::build`](crate::Dag::build) — so nothing downstream knows which mode
//! produced the data.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// `config` sub-object of a node (only the fields we surface).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawConfig {
    #[serde(default)]
    pub materialized: Option<String>,
    /// The declared `unique_key` — dbt keeps the declared shape on the wire:
    /// a bare string for `unique_key: id`, an array for
    /// `unique_key: [a, b]`. Left as a `Value` here; `Dag::build` normalizes
    /// both spellings to `Vec<String>` (empty = unconfigured).
    #[serde(default)]
    pub unique_key: Option<serde_json::Value>,
    /// `config.incremental_strategy` — `append` / `merge` / `delete+insert` /
    /// `insert_overwrite` / `microbatch`. `None` when undeclared (dbt picks
    /// the adapter default at run time and records `null` on the wire).
    #[serde(default)]
    pub incremental_strategy: Option<String>,
}

/// `depends_on` sub-object of a node (the node refs it points at). `macros`
/// carries the macro unique_ids (`macro.<project>.<name>`) the node's compiled
/// SQL calls — present on real manifests, empty on synthesized source ones.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawDependsOn {
    #[serde(default)]
    pub nodes: Vec<String>,
    #[serde(default)]
    pub macros: Vec<String>,
}

/// `test_metadata` sub-object of a generic test node (`name` is the test kind:
/// `unique`, `not_null`, `relationships`, …). Absent for singular tests.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawTestMetadata {
    #[serde(default)]
    pub name: Option<String>,
    /// The test's rendered keyword args (`to`, `field`, `values`, …). Only
    /// `field` is consumed today — a `relationships` test's referenced column;
    /// the rest rides along verbatim so source mode can fill the same shape.
    /// `Option` (not a defaulted map) because dbt writes an explicit
    /// `"kwargs": null` for tests declared with no args.
    #[serde(default)]
    pub kwargs: Option<serde_json::Map<String, serde_json::Value>>,
}

/// A single node in the parsed manifest (`nodes` entry).
///
/// Only the subset of fields we surface is typed; unknown fields are ignored.
/// Every node must deserialize (exclusion happens *after* parsing), so anything
/// not guaranteed across all resource types is optional. `Default` is derived so
/// the unit-test literals can use `..Default::default()` and never churn again as
/// fields are added. `columns` keeps dbt definition order via `serde_json`'s
/// `preserve_order` (its `Map` is then an `IndexMap`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawNode {
    pub name: String,
    pub resource_type: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
    /// `patch_path` — the `.yml` properties file this node's docs/config were
    /// declared in (`<package>://<project-relative path>` on the wire, e.g.
    /// `sample://models/schema.yml`; `null` when nothing patches the node).
    /// `Dag::build` strips the scheme so it is comparable with the
    /// `original_file_path` convention.
    #[serde(default)]
    pub patch_path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub config: RawConfig,
    #[serde(default)]
    pub columns: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    /// The dbt package that DEFINED this node (`my_project`, `dbt_utils`, …).
    /// Real manifests always set it; source-synthesized nodes leave it `None`
    /// (they're all local — the uid's project segment already names them).
    #[serde(default)]
    pub package_name: Option<String>,
    // --- test-only fields (captured pre-prune for the tests side map) ---
    #[serde(default)]
    pub test_metadata: Option<RawTestMetadata>,
    #[serde(default)]
    pub attached_node: Option<String>,
    #[serde(default)]
    pub column_name: Option<String>,
    #[serde(default)]
    pub depends_on: RawDependsOn,
    /// The node's raw (uncompiled) SQL — the real dbt manifest field name (v12
    /// nodes carry it). `#[serde(default)]` + `Option` so test literals using
    /// `..Default::default()` never churn; sources/seeds carry none.
    #[serde(default)]
    pub raw_code: Option<String>,
    /// The node's compiled (Jinja-expanded) SQL — only present when the
    /// manifest was written BY `dbt compile`/`build`/`run` (a `dbt parse`
    /// manifest leaves it null). `#[serde(default)]` like `raw_code`; the
    /// `target/compiled/` files are the fallback for every parse-produced
    /// manifest.
    #[serde(default)]
    pub compiled_code: Option<String>,
    // --- governance fields (dbt model access controls) ---
    /// `private`/`protected`/`public` — who may `ref()` this model.
    #[serde(default)]
    pub access: Option<String>,
    /// The model group it belongs to (a `groups:` entry in the project).
    #[serde(default)]
    pub group: Option<String>,
    /// `contract: {enforced: true}` — the model's declared columns are the
    /// enforced warehouse contract.
    #[serde(default)]
    pub contract: Option<RawContract>,
    /// The model's `language` — `"sql"` for every node dbt builds from .sql
    /// (models AND snapshots); python models record `"python"`. `null` on
    /// seeds and non-buildable nodes.
    #[serde(default)]
    pub language: Option<String>,
    /// `deprecation_date` — when the model stops being supported. dbt writes
    /// a timezone-bearing ISO timestamp (`2026-12-01T00:00:00+09:00`) parsed
    /// from the date the schema file declared; kept as the raw string — this
    /// tool surfaces it, never interprets it.
    #[serde(default)]
    pub deprecation_date: Option<String>,
    /// The model-level `constraints:` list — `{type, name, expression,
    /// warn_unenforced, warn_unsupported, to, to_columns, columns}`. Empty on
    /// real manifests when undeclared (dbt writes `[]`, not `null`); column-
    /// level constraints live inside `columns` entries instead.
    #[serde(default)]
    pub constraints: Vec<RawConstraint>,
    /// The version this node IS (for `versions:`-ed models) — `v1`, `2`, …
    /// (the wire type is a JSON string).
    #[serde(default)]
    pub version: Option<String>,
    /// The newest version's name when this node is versioned.
    #[serde(default)]
    pub latest_version: Option<String>,
    /// The node's `meta` dictionary (owner / domain / arbitrary key-values).
    /// Real manifests mirror the merged result at the node top level (it equals
    /// `config.meta`), so only this field is read. Values are arbitrary JSON —
    /// kept raw here and stringified for display by [`crate::Dag::build`].
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
    /// The node's warehouse relation name — dbt's effective `alias` (the
    /// `config(alias=…)` override or `generate_alias_name` result; `name`
    /// when unconfigured). Real manifests always record it; CLL uses it to
    /// match `FROM "db"."schema"."<alias>"` in compiled SQL — matching `name`
    /// alone misses roughly a third of relations on alias-heavy projects.
    #[serde(default)]
    pub alias: Option<String>,
    /// The `semantic_models:` entries that consume this node — source-mode
    /// ONLY: a real manifest records those consumers as `semantic_model`
    /// nodes plus edges, but the source loader synthesizes neither and
    /// still needs the "the semantic layer reads this" signal, so it fills
    /// this synthesized channel from each entry's `model: ref()` instead.
    /// `#[serde(skip)]` keeps it off the wire entirely (a real manifest
    /// can't carry it, a serialized one never writes it); `Dag::build`
    /// merges it with the same names it derives from `semantic_models`
    /// records, so both modes answer identically.
    #[serde(skip)]
    pub semantic_dependents: Vec<String>,
}

/// `contract` sub-object: whether `enforced` is on (the declared column list
/// becomes the actual warehouse schema).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawContract {
    #[serde(default)]
    pub enforced: bool,
}

/// One `constraints:` entry (model- or column-level — the same wire shape;
/// `columns` exists only on the model level, `to`/`to_columns` only on
/// `foreign_key`). dbt fills `warn_unenforced`/`warn_unsupported` per
/// adapter; they ride along for completeness but the domain keeps only the
/// declarative fields.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawConstraint {
    /// `primary_key` / `not_null` / `foreign_key` / `unique` / `check` /
    /// `custom`. `type` is a Rust keyword, hence the rename.
    #[serde(rename = "type", default)]
    pub constraint_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub expression: Option<String>,
    /// A `foreign_key` constraint's target: the referenced model's `name` or
    /// a `ref('x')`/`source('a','b')` expression.
    #[serde(default)]
    pub to: Option<String>,
    /// The referenced columns for `foreign_key`. `Option` (not a defaulted
    /// vec): dbt writes `null` on kinds that don't carry the key.
    #[serde(default)]
    pub to_columns: Option<Vec<String>>,
    /// The model columns the constraint covers (model-level only — a
    /// column-level entry's constraint applies to its own column, so dbt
    /// writes `null` there).
    #[serde(default)]
    pub columns: Option<Vec<String>>,
}

/// A single source in the parsed manifest (`sources` entry).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawSource {
    pub name: String,
    pub resource_type: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub columns: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    /// The dbt package that declared this source (`package_name` in the
    /// manifest; `None` for synthesized source-mode nodes).
    #[serde(default)]
    pub package_name: Option<String>,
    /// The source's merged `meta` dictionary (source-level `meta:` cascades to
    /// every table; a table-level `meta:` overrides it per key).
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
    /// The source TABLE's warehouse name — schema.yml's `identifier:`
    /// override (`name` when unconfigured). CLL matches `FROM <identifier>`
    /// against it the same way [`RawNode::alias`] covers models.
    #[serde(default)]
    pub identifier: Option<String>,
    /// `loaded_at_field` — the timestamp column freshness is measured
    /// against (table-level overrides the source-level declaration; a table's
    /// explicit `null` clears the inherited value, so the wire value is the
    /// already-resolved effective one).
    #[serde(default)]
    pub loaded_at_field: Option<String>,
    /// `tags` — dbt accumulates every tag channel rather than overriding:
    /// the `config.tags` of the source then the table, then the top-level
    /// `tags:` of the TABLE then the source (verified on 1.12.6 — a table's
    /// own tags precede the inherited source tags on the wire). The wire
    /// value is the resolved union, in that order.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The tool that loads this table (`loader:` — source-level only; dbt
    /// ignores it on tables and in `dbt_project.yml`). dbt writes `""` when
    /// unconfigured.
    #[serde(default)]
    pub loader: Option<String>,
    /// The resolved `freshness` block: `warn_after`/`error_after` merged per
    /// threshold (table > source > `dbt_project.yml` `+freshness`). `null`
    /// when nothing configures it — including a table's explicit
    /// `freshness: null`, which clears inherited thresholds. `filter` is out
    /// of scope and ignored.
    #[serde(default)]
    pub freshness: Option<RawFreshness>,
}

/// `freshness` sub-object of a source: the two thresholds dbt compares a
/// table's `loaded_at_field` against. Each threshold is optional; a threshold
/// object whose `count` is null is effectively absent (dbt writes
/// `{"count": null, "period": null}` for the unset side).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawFreshness {
    #[serde(default)]
    pub warn_after: Option<RawFreshnessThreshold>,
    #[serde(default)]
    pub error_after: Option<RawFreshnessThreshold>,
}

/// One freshness threshold (`{count: 12, period: hour}`). `count` may be null
/// even when the object is present; a null count carries no signal.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawFreshnessThreshold {
    #[serde(default)]
    pub count: Option<i64>,
    #[serde(default)]
    pub period: Option<String>,
}

/// `owner` sub-object of an exposure. dbt requires at least one of name/email,
/// but both stay optional here (exclusion is never a parse failure).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawExposureOwner {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// A single exposure in the parsed manifest (`exposures` entry): a declared
/// downstream consumer (dashboard / notebook / application / …). Exposures are
/// pure leaves — they have `depends_on` parents and never children — so the
/// DAG keeps them as terminator nodes.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawExposure {
    pub name: String,
    /// The exposure kind (`dashboard`, `notebook`, `analysis`, `ml`,
    /// `application`). `type` is a Rust keyword, hence the rename.
    #[serde(rename = "type", default)]
    pub exposure_type: Option<String>,
    #[serde(default)]
    pub owner: RawExposureOwner,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
    /// The nodes this exposure consumes. Used as an adjacency source alongside
    /// the manifest's `parent_map`/`child_map` (deduplicated in `Dag::build`),
    /// so a synthesized manifest (source mode) only needs to fill this.
    #[serde(default)]
    pub depends_on: RawDependsOn,
}

/// A semantic-layer resource (`semantic_models` / `metrics` / `saved_queries`)
/// — like an exposure, a declared downstream consumer that only ever gains
/// parents, so it joins the DAG as a terminator node. `resource_type` comes
/// from which manifest map it arrived in (not a field on the wire).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawSemanticResource {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
    #[serde(default)]
    pub depends_on: RawDependsOn,
}

/// A dbt unit test (`unit_tests` entry, dbt ≥1.8): attached to ONE model via
/// its `model` field (the manifest key's last uid segment).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawUnitTest {
    /// `#[serde(default)]`: real manifests always carry `name`, but it is the
    /// only required field on a map whose shape already diverged once between
    /// engines (dbt-core #15900) — a missing name must not fail the whole
    /// manifest parse, it just yields a nameless `TestInfo`.
    #[serde(default)]
    pub name: String,
    /// The model name this unit test is declared on.
    #[serde(default)]
    pub model: Option<String>,
    /// The `given:` inputs (each a `ref`/`source` table plus its rows) — the
    /// count and inline row counts are surfaced in the structure modal.
    #[serde(default)]
    pub given: Vec<RawUnitTestIo>,
    /// The `expect:` output rows the test asserts.
    #[serde(default)]
    pub expect: Option<RawUnitTestIo>,
}

/// One `given:`/`expect:` entry of a unit test. Only `rows` is surfaced: an
/// inline `rows:` list is countable from the manifest, while a `fixture:`
/// reference or a CSV-string `rows:` payload is not (dbt stores the string,
/// not the parsed rows — counting it would invent a number).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawUnitTestIo {
    /// The `input:` reference for `given:` entries (`ref('x')` /
    /// `source('a','b')` as a string). Kept raw — display doesn't resolve it.
    #[serde(default)]
    pub input: Option<String>,
    /// The inline `rows:` payload: a JSON array for row-dict fixtures, a
    /// string for `format: csv` fixtures. Counted only when it is an array.
    #[serde(default)]
    pub rows: Option<serde_json::Value>,
}

/// A single macro in the parsed manifest (`macros` entry). Only the fields the
/// diff needs: `macro_sql` is the surface a shared-macro change moves, and
/// `depends_on.macros` is the macro→macro call graph for transitive impact.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawMacro {
    pub name: String,
    /// The macro's Jinja body — the real dbt manifest field name (v12).
    #[serde(default)]
    pub macro_sql: Option<String>,
    #[serde(default)]
    pub depends_on: RawDependsOn,
}

/// A `docs` entry (`doc.<pkg>.<name>` — a `{% docs %}` block rendered at
/// parse time). Only the fields the project-overview intake reads; the rest
/// of the wire shape is ignored like everywhere else (no
/// `deny_unknown_fields`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawDoc {
    /// The block's `{% docs <name> %}` name (`__overview__` is the project
    /// overview dbt renders atop its own docs index).
    #[serde(default)]
    pub name: String,
    /// The package that declared the block — `dbt` ships a boilerplate
    /// `doc.dbt.__overview__` in every manifest, so the package segment is
    /// what distinguishes the project's own overview from an installed
    /// package's.
    #[serde(default)]
    pub package_name: Option<String>,
    /// The rendered block body — dbt expands `doc()` refs at parse time, so
    /// this is display-ready text.
    #[serde(default)]
    pub block_contents: String,
}

/// `metadata` sub-object of the manifest. Only the fields we surface:
/// `project_name` is the ROOT project's name as dbt recorded it (dbt ≥1.6) —
/// the authoritative answer to "which package owns this graph", immune to
/// installed packages whose names happen to sort first in `unique_id`s.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawMetadata {
    #[serde(default)]
    pub project_name: Option<String>,
    /// The adapter the manifest was compiled against (`duckdb`, `redshift`,
    /// `snowflake`, …) — `metadata.adapter_type` in real manifests. CLL picks
    /// its SQL dialect from it; source mode leaves it `None` (profiles.yml is
    /// deliberately out of scope) and the `--sql-dialect` override fills in.
    #[serde(default)]
    pub adapter_type: Option<String>,
}

/// Minimal subset of the dbt manifest we care about.
///
/// `deny_unknown_fields` is intentionally NOT set: the v12 schema has many
/// fields we don't model, and they must be ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawManifest {
    /// `#[serde(default)]`: real dbt manifests always carry a `metadata`
    /// object (though `project_name` exists in it only since dbt 1.6) — this
    /// guard is for manifests that omit the object entirely (minimal or
    /// hand-built ones). Either way `Dag::build` falls back to inferring the
    /// project name from node `unique_id`s.
    #[serde(default)]
    pub metadata: RawMetadata,
    pub nodes: HashMap<String, RawNode>,
    pub sources: HashMap<String, RawSource>,
    /// `#[serde(default)]`: pre-exposure fixtures (and minimal hand-built
    /// manifests) omit the key entirely.
    #[serde(default)]
    pub exposures: HashMap<String, RawExposure>,
    /// `#[serde(default)]`: manifests before the macros key existed (and every
    /// synthesized source-mode manifest, which never parses macros) omit it —
    /// an absent map disables macro-diff detection rather than inventing one.
    #[serde(default)]
    pub macros: HashMap<String, RawMacro>,
    /// Semantic layer (dbt ≥1.6): all three resource kinds are downstream-only
    /// terminators, so one raw type serves all three maps.
    #[serde(default)]
    pub semantic_models: HashMap<String, RawSemanticResource>,
    #[serde(default)]
    pub metrics: HashMap<String, RawSemanticResource>,
    #[serde(default)]
    pub saved_queries: HashMap<String, RawSemanticResource>,
    /// dbt ≥1.8 unit tests — excluded from the DAG like schema tests; captured
    /// into the per-model `tests` side map instead.
    #[serde(default)]
    pub unit_tests: HashMap<String, RawUnitTest>,
    /// dbt Fusion UDFs (`function.<pkg>.<name>`, a top-level `functions` map
    /// distinct from `nodes`). Read only to COUNT them — a non-empty map
    /// yields a "not displayed" load warning
    /// ([`load_dag_with_warnings`](crate::load_dag_with_warnings)) so UDFs
    /// never drop silently; `Value` keeps the parse shape-tolerant.
    #[serde(default)]
    pub functions: HashMap<String, serde_json::Value>,
    /// `#[serde(default)]`: the `docs` map (`doc.<pkg>.<name>` — rendered
    /// `{% docs %}` blocks). Only `__overview__` is consumed — the root
    /// project's own entry feeds [`Dag::overview`](crate::Dag::overview);
    /// every other block stays unread (installed packages' blocks and dbt's
    /// boilerplate included). Manifests without a `docs` key simply yield no
    /// overview.
    #[serde(default)]
    pub docs: HashMap<String, RawDoc>,
    pub parent_map: HashMap<String, Vec<String>>,
    pub child_map: HashMap<String, Vec<String>>,
}

/// The JSON-parse failure wording both manifest loaders share: the file
/// loader appends the path it read, the in-memory loader stops at the bare
/// phrase (no file exists to name).
const PARSE_MANIFEST_ERR: &str = "failed to parse manifest JSON";

/// Read and parse a manifest from disk.
///
/// Returns `Err` (never panics) when the file is missing or the contents are
/// not valid JSON / do not match the expected manifest shape. The error
/// message names the cause (file read vs JSON parse).
pub fn load_manifest<P: AsRef<Path>>(path: P) -> Result<RawManifest> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read manifest file: {}", path.display()))?;
    let manifest: RawManifest = serde_json::from_str(&contents)
        .with_context(|| format!("{PARSE_MANIFEST_ERR}: {}", path.display()))?;
    Ok(manifest)
}

/// Parse a manifest from its JSON text — the in-memory counterpart of
/// [`load_manifest`], which reads the same text from a file first and then
/// appends that path to the shared parse wording as context.
///
/// Returns `Err` (never panics) when the text is not valid JSON / does not
/// match the expected manifest shape.
pub fn parse_manifest(contents: &str) -> Result<RawManifest> {
    serde_json::from_str(contents).context(PARSE_MANIFEST_ERR)
}
