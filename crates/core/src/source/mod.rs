//! Build a [`Dag`] by parsing a dbt project's SOURCE files — no `dbt compile`,
//! no `manifest.json`.
//!
//! The strategy is deliberately thin: scan `dbt_project.yml` (project name,
//! resource paths, and the `models:`/`seeds:`/`snapshots:`/`sources:` config
//! trees) + the
//! model `.sql` files (extracting `ref()`/`source()` and the `config(…)`
//! kwargs `materialized`/`enabled`/`meta`) + the `schema.yml` files under
//! the configured resource paths (sources, columns, descriptions, tests) +
//! the `.md` files under `docs-paths` (`{% docs %}` blocks, so `{{ doc() }}`
//! refs in descriptions expand the way dbt renders them), synthesize a
//! [`RawManifest`] of the exact shape [`Dag::build`] already consumes, then
//! hand it off. Everything downstream (layout, list, modal, lineage) is
//! unchanged.
//!
//! Consistency contract: the synthesized Dag must match the one built from a
//! real `dbt parse` manifest of the same project (asserted by
//! `tests/consistency.rs` against a committed real-dbt fixture). Notably:
//! - Only Jinja `{# #}` comments hide a `ref()`; dbt renders Jinja BEFORE SQL
//!   comments exist, so a ref in a `--`/`/* */` comment IS a real dependency.
//! - Materialization precedence is in-file `config()` > schema.yml `config:` >
//!   `dbt_project.yml` tree (deepest path wins) > dbt's default `view`.
//! - The governance configs follow the same chain: `access` (models only,
//!   dbt's undeclared default is `protected`), `group`, `contract`, and
//!   `incremental_strategy` resolve in-file `config()` > schema.yml
//!   (top-level or `config:`) > `+`-prefixed tree keys. `deprecation_date`
//!   and `constraints` are model properties — schema.yml is their only
//!   intake. `language` is `"sql"` for `.sql`-defined models and snapshots.
//! - Property yml files are read ONLY from the configured model/seed/snapshot
//!   paths — never `target/`, `dbt_packages/`, or stray yml elsewhere.
//! - `config(enabled=false)` (in-file) / `+enabled: false` (project tree) /
//!   `config: enabled: false` (schema.yml) drops the node, like dbt parking it
//!   under `disabled`. `var('name'[, default])` (`| as_bool` allowed) in
//!   either `+enabled`/`config(enabled=)` position resolves through the same
//!   var tables dbt renders it in — `vars.yml` for the project file's own
//!   `+enabled` strings, `vars:` (project-scoped keys shadowing the top
//!   level) for a node's `config()` calls; a value dbt's boolean validation
//!   would reject keeps the node enabled with a warning.
//!
//! Scope: this covers the common project shape, not dbt's full resolver. NOT
//! handled (a `ref` to one of these simply drops out, which `prune_adjacency`
//! tolerates): cross-package `ref`, `{% if %}`/`var()`-gated refs (only the
//! `var()` in `enabled` positions resolves — see vars.rs), and
//! singular (`.sql`) tests. A versioned ref resolves to the unversioned name
//! (approximation — the `.vN` node a `versions:` block would define is not
//! synthesized); YAML-defined snapshots (dbt 1.9+) ARE synthesized as nodes.
//! Recoverable oddities (an unparsable schema.yml, a duplicate resource name)
//! are reported as warnings, not errors.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use regex::Regex;
use serde::Deserialize;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::{
    Dag, RawConstraint, RawContract, RawDependsOn, RawFreshness, RawFreshnessThreshold,
    RawManifest, RawNode, RawSource, RawTestMetadata,
};

/// Parse a dbt project directory and build its [`Dag`] from source.
pub fn load_dag_from_source<P: AsRef<Path>>(project_dir: P) -> Result<Dag> {
    load_dag_from_source_with_warnings(project_dir).map(|(dag, _)| dag)
}

/// Like [`load_dag_from_source`], but also returns the recoverable warnings
/// from [`manifest_from_source_with_warnings`] instead of discarding them.
pub fn load_dag_from_source_with_warnings<P: AsRef<Path>>(
    project_dir: P,
) -> Result<(Dag, Vec<String>)> {
    let dir = project_dir.as_ref();
    let (manifest, warnings) = manifest_from_source_with_warnings(dir)?;
    let mut dag = Dag::build(&manifest);
    // Same auto-detect as `load_dag`: a project the user already compiled
    // (`dbt compile`/`build`) has Jinja-expanded SQL under target/compiled/ —
    // absent files are simply "no compiled SQL".
    if let Some(compiled_dir) = crate::compiled::default_compiled_dir(None, Some(dir)) {
        dag.ingest_compiled(&compiled_dir, Some(dir));
    }
    Ok((dag, warnings))
}

/// Parse a dbt project directory into a synthesized [`RawManifest`].
///
/// Exposed (not just `load_dag_from_source`) so the synthesis can be asserted
/// directly in tests without building the whole DAG.
pub fn manifest_from_source<P: AsRef<Path>>(project_dir: P) -> Result<RawManifest> {
    manifest_from_source_with_warnings(project_dir).map(|(m, _)| m)
}

/// Like [`manifest_from_source`], but also returns recoverable warnings (an
/// unparsable schema.yml, a duplicate resource name) instead of silently
/// dropping them — dbtl is a viewer, not `dbt`, so these are surfaced rather
/// than treated as hard errors.
///
/// The pipeline itself lives in `synth` — one `Synthesis` state the stages
/// below mutate in order. The stage ORDER is the contract dbt's own pipeline
/// implies: resources are discovered before edges resolve, edges before the
/// schema pass writes metadata/tests, and the project-tree config fills last;
/// warnings collect along the way and are deduplicated first-seen in
/// `Synthesis::finish`.
pub fn manifest_from_source_with_warnings<P: AsRef<Path>>(
    project_dir: P,
) -> Result<(RawManifest, Vec<String>)> {
    let mut synth = Synthesis::load(project_dir.as_ref())?;
    synth.collect_models();
    synth.collect_seeds();
    synth.collect_snapshots();
    let parsed = synth.parse_schema_files();
    synth.synthesize_yaml_snapshots(&parsed);
    let (parent_map, child_map) = synth.resolve_edges();
    synth.apply_schema_files(&parsed);
    synth.apply_tree_fallback();
    let manifest_docs = synth.manifest_docs();
    Ok(synth.finish(parent_map, child_map, manifest_docs))
}

mod collect;
mod config_tree;
mod docs;
mod patterns;
mod schema;
mod synth;
#[cfg(test)]
mod tests;
mod tests_synth;
mod vars;
mod yaml;

use collect::*;
use config_tree::*;
use docs::*;
use patterns::*;
use schema::*;
use synth::*;
use tests_synth::*;
use vars::*;
use yaml::*;

// The span-carrying call extractors lint uses for finding positions — same
// machinery as the loaders' own pass, not a second implementation.
pub use collect::mask_jinja_comments;
pub use patterns::{
    arms_exclusive, call_sites_masked, jinja_branches, ArmStep, CallKind, CallSite, JinjaBranches,
};
pub(crate) use patterns::{kwarg, lit_value};
