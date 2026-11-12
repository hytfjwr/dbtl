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
//!   under `disabled`.
//!
//! Scope: this covers the common project shape, not dbt's full resolver. NOT
//! handled (a `ref` to one of these simply drops out, which `prune_adjacency`
//! tolerates): cross-package `ref`, `{% if %}`/`var()`-gated refs, and
//! singular (`.sql`) tests. A versioned ref resolves to the unversioned name
//! (approximation — the `.vN` node a `versions:` block would define is not
//! synthesized); YAML-defined snapshots (dbt 1.9+) ARE synthesized as nodes.
//! Recoverable oddities (an unparsable schema.yml, a duplicate resource name)
//! are reported as warnings, not errors.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::{
    Dag, RawConfig, RawConstraint, RawContract, RawDependsOn, RawDoc, RawExposure,
    RawExposureOwner, RawFreshness, RawFreshnessThreshold, RawManifest, RawMetadata, RawNode,
    RawSource, RawTestMetadata,
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
pub fn manifest_from_source_with_warnings<P: AsRef<Path>>(
    project_dir: P,
) -> Result<(RawManifest, Vec<String>)> {
    let dir = project_dir.as_ref();
    let proj_path = dir.join("dbt_project.yml");
    let proj_str = fs::read_to_string(&proj_path)
        .with_context(|| format!("failed to read dbt_project.yml: {}", proj_path.display()))?;
    // Recoverable oddities (an unparsable schema.yml, a duplicate resource
    // name, a non-mapping `meta:`) are reported as warnings, not errors.
    let mut warnings: Vec<String> = Vec::new();
    // dbt's YAML is PyYAML's: the project file parses through the same
    // style-aware loader the schema files do (a quoted `"yes"` stays a
    // string; a bare `yes` is the bool dbt records).
    let project: DbtProject = yaml_to_json(&proj_str, &mut warnings)
        .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to parse dbt_project.yml: {}: {e}",
                proj_path.display()
            )
        })?;

    let proj = project.name;
    // `source-paths` is the pre-0.17 alias for `model-paths`. All three lists
    // are filtered through `contained_resource_paths`: every later join site
    // (the .sql/.csv collectors AND the schema.yml scan) trusts them to stay
    // under the project root.
    let model_paths = contained_resource_paths(
        project
            .model_paths
            .or(project.source_paths)
            .unwrap_or_else(|| vec!["models".to_string()]),
    );
    let seed_paths = contained_resource_paths(
        project
            .seed_paths
            .unwrap_or_else(|| vec!["seeds".to_string()]),
    );
    let snapshot_paths = contained_resource_paths(
        project
            .snapshot_paths
            .unwrap_or_else(|| vec!["snapshots".to_string()]),
    );

    let pat = Patterns::new();
    let mut nodes: HashMap<String, RawNode> = HashMap::new();
    let mut sources: HashMap<String, RawSource> = HashMap::new();
    let mut exposures: HashMap<String, RawExposure> = HashMap::new();
    // name -> unique_id for models/seeds/snapshots, so `ref('x')` can resolve.
    let mut name_index: HashMap<String, String> = HashMap::new();
    // (uid, Jinja-comment-masked SQL) deferred to the ref/source pass below —
    // masking preserves byte length, so call-site offsets index the raw text.
    let mut node_sql: Vec<(String, String)> = Vec::new();
    // uid -> dbt_project.yml tree materialization, applied AFTER schema.yml
    // (precedence: in-file config() > schema.yml > project tree > default view).
    let mut tree_mat: HashMap<String, String> = HashMap::new();
    // uid -> dbt_project.yml tree `+meta` dict, merged AFTER schema.yml like
    // `tree_mat` (the meta precedence chain is the same shape: in-file
    // config(meta=…) > schema.yml meta/config.meta > project tree).
    let mut tree_meta: HashMap<String, serde_json::Map<String, serde_json::Value>> = HashMap::new();
    // uid -> dbt_project.yml tree `+unique_key` — filled like `tree_mat`
    // (in-file config() > schema.yml config: > tree), the whole declared
    // value (dbt accepts a string or a string list).
    let mut tree_uk: HashMap<String, serde_json::Value> = HashMap::new();
    // uid -> the whole resolved tree config, for the governance keys
    // (`+access`/`+group`/`+contract`/`+incremental_strategy`) — the same
    // lowest-precedence slot `tree_mat`/`tree_uk` occupy.
    let mut tree_gov: HashMap<String, TreeConfig> = HashMap::new();

    // --- docs blocks (.md) ---
    // dbt renders `{{ doc('x') }}` in descriptions from `{% docs x %}…{% enddocs %}`
    // blocks collected from .md files under `docs-paths` — unset means EVERY
    // resource path (models/seeds/snapshots/macros/analyses/tests, verified
    // against real dbt). Collect them up front so every description intake
    // below renders through the one resolver.
    let doc_paths: Vec<String> = match project.docs_paths {
        Some(paths) => contained_resource_paths(paths),
        None => [
            model_paths.clone(),
            seed_paths.clone(),
            snapshot_paths.clone(),
            contained_resource_paths(
                project
                    .macro_paths
                    .unwrap_or_else(|| vec!["macros".to_string()]),
            ),
            contained_resource_paths(
                project
                    .analysis_paths
                    .unwrap_or_else(|| vec!["analyses".to_string()]),
            ),
            contained_resource_paths(
                project
                    .test_paths
                    .unwrap_or_else(|| vec!["tests".to_string()]),
            ),
        ]
        .concat(),
    };
    let docs = DocBlocks::new(
        proj.clone(),
        collect_doc_blocks(dir, &doc_paths, &pat, &mut warnings),
    );

    // --- models (.sql) ---
    for (file, root) in collect_with_root(dir, &model_paths, "sql") {
        // Keep the pre-mask text for the SQL preview modal (comments intact);
        // the ref/source/config extractors still see the Jinja-comment-masked
        // copy (dbt's Jinja-before-SQL-comments semantics) — masking preserves
        // byte length, so offsets in it index `raw` too.
        let raw = read(&file);
        // `raw` moves into `RawNode` below, so the mask is owned here —
        // comment-free files skip the byte loop and keep just the copy.
        let masked = mask_jinja_comments(&raw).into_owned();
        let name = file_stem(&file);
        let uid = format!("model.{proj}.{name}");
        // dbt's `path` is relative to the model-paths root (e.g. `staging/x.sql`)
        // — the left-pane layer grouping splits it on the first `/`.
        // `original_file_path` is project-root-relative.
        let path = rel_path(&root, &file);
        let tree = resolve_tree_config(
            &project.models,
            &tree_segments(&proj, &path),
            "models".to_string(),
            &mut warnings,
        );
        // Disabled models never reach the manifest's `nodes` (dbt parks them
        // under `disabled`); drop them here too, edges included.
        if pat.disabled(&masked) || tree.enabled == Some(false) {
            continue;
        }
        // `collect_with_root` sorts by path, so the first file to claim a
        // name wins deterministically; drop (and warn on) a later same-stem
        // file before it can touch `tree_mat`/`nodes`/`name_index`/`node_sql`.
        if name_index.contains_key(&name) {
            warnings.push(format!(
                "duplicate resource name '{name}': ignoring {}",
                rel_path(dir, &file)
            ));
            continue;
        }
        if let Some(m) = &tree.materialized {
            tree_mat.insert(uid.clone(), m.clone());
        }
        if !tree.meta.is_empty() {
            tree_meta.insert(uid.clone(), tree.meta.clone());
        }
        if let Some(uk) = &tree.unique_key {
            tree_uk.insert(uid.clone(), uk.clone());
        }
        tree_gov.insert(uid.clone(), tree.clone());
        // The in-file `config(…)` spellings of the model governance fields —
        // each the highest level of the same precedence chain `materialized`
        // follows (`access`/`contract` are model-only; `group` is not) —
        // extracted in one pass over the file's config() calls.
        let gov = config_governance(&masked);
        nodes.insert(
            uid.clone(),
            RawNode {
                name: name.clone(),
                resource_type: "model".to_string(),
                path: Some(path),
                original_file_path: Some(rel_path(dir, &file)),
                config: RawConfig {
                    materialized: pat.materialized(&masked),
                    // In-file `config(unique_key=…)` is the HIGHEST
                    // precedence level — the same chain `materialized`
                    // follows (schema.yml and the tree fill below it).
                    unique_key: config_unique_key(&masked),
                    incremental_strategy: gov.incremental_strategy.clone(),
                },
                access: gov.access.clone(),
                group: gov.group.clone(),
                contract: gov.contract.clone(),
                // dbt records `language: "sql"` on every .sql-defined model.
                language: Some("sql".to_string()),
                // In-file `config(meta={…})` is the HIGHEST precedence level;
                // schema.yml and the project tree merge under it per key.
                meta: config_meta(&masked).unwrap_or_default(),
                raw_code: Some(raw),
                // In-file `config(alias=…)` is the highest-precedence relation
                // name — schema.yml `config.alias` can only fill it when
                // unset (same precedence chain as `materialized`).
                alias: config_alias(&masked),
                ..Default::default()
            },
        );
        name_index.insert(name, uid.clone());
        node_sql.push((uid, masked));
    }

    // --- seeds (.csv) ---
    for (file, root) in collect_with_root(dir, &seed_paths, "csv") {
        let name = file_stem(&file);
        let uid = format!("seed.{proj}.{name}");
        let path = rel_path(&root, &file);
        let tree = resolve_tree_config(
            &project.seeds,
            &tree_segments(&proj, &path),
            "seeds".to_string(),
            &mut warnings,
        );
        if tree.enabled == Some(false) {
            continue;
        }
        if name_index.contains_key(&name) {
            warnings.push(format!(
                "duplicate resource name '{name}': ignoring {}",
                rel_path(dir, &file)
            ));
            continue;
        }
        if !tree.meta.is_empty() {
            tree_meta.insert(uid.clone(), tree.meta.clone());
        }
        if let Some(uk) = &tree.unique_key {
            tree_uk.insert(uid.clone(), uk.clone());
        }
        tree_gov.insert(uid.clone(), tree.clone());
        nodes.insert(
            uid.clone(),
            RawNode {
                name: name.clone(),
                resource_type: "seed".to_string(),
                path: Some(path),
                original_file_path: Some(rel_path(dir, &file)),
                // dbt records seeds with materialization "seed".
                config: RawConfig {
                    materialized: Some("seed".to_string()),
                    ..Default::default()
                },
                columns: csv_header_columns(&file, &docs, &mut warnings),
                ..Default::default()
            },
        );
        name_index.insert(name, uid);
    }

    // --- snapshots ({% snapshot X %} blocks in snapshot-path .sql) ---
    for (file, root) in collect_with_root(dir, &snapshot_paths, "sql") {
        let raw = read(&file);
        let masked = mask_jinja_comments(&raw);
        let path = rel_path(&root, &file);
        let ofp = rel_path(dir, &file);
        // An in-file `enabled=false` applies to every block in the file (rare).
        if pat.disabled(&masked) {
            continue;
        }
        // Refs in a multi-block file are attributed to every block in it (rare).
        for name in pat.snapshots(&masked) {
            let uid = format!("snapshot.{proj}.{name}");
            // dbt's {% snapshot %} fqn KEEPS the file stem and appends the
            // snapshot name on top (`[pkg, *dirs, <file-stem>, <name>]` —
            // SnapshotParser.get_fqn: "the fqn includes the filename"), so a
            // `snapshots:` tree can configure a whole file's blocks at the
            // stem level AND a single block under it. YAML-defined snapshots
            // are the different, prefix-only shape (handled below).
            let mut segments = tree_segments(&proj, &path);
            segments.push(name.clone());
            let tree = resolve_tree_config(
                &project.snapshots,
                &segments,
                "snapshots".to_string(),
                &mut warnings,
            );
            if tree.enabled == Some(false) {
                continue;
            }
            if name_index.contains_key(&name) {
                warnings.push(format!("duplicate resource name '{name}': ignoring {ofp}"));
                continue;
            }
            if !tree.meta.is_empty() {
                tree_meta.insert(uid.clone(), tree.meta.clone());
            }
            if let Some(uk) = &tree.unique_key {
                tree_uk.insert(uid.clone(), uk.clone());
            }
            tree_gov.insert(uid.clone(), tree.clone());
            nodes.insert(
                uid.clone(),
                RawNode {
                    name: name.clone(),
                    resource_type: "snapshot".to_string(),
                    path: Some(path.clone()),
                    original_file_path: Some(ofp.clone()),
                    config: RawConfig {
                        materialized: Some("snapshot".to_string()),
                        // The `{{ config(…) }}` inside a `{% snapshot %}`
                        // block carries `unique_key` — extracted file-wide
                        // like the other pattern extractors.
                        unique_key: config_unique_key(&masked),
                        ..Default::default()
                    },
                    // Snapshots can sit in a dbt `group`; `access`/`contract`
                    // stay None — dbt records them on models only.
                    group: config_governance(&masked).group,
                    // dbt records `language: "sql"` on .sql-defined snapshots.
                    language: Some("sql".to_string()),
                    // The `{{ config(…) }}` inside a `{% snapshot %}` block is
                    // extracted file-wide like the other pattern extractors
                    // (multi-block files share the config — rare).
                    meta: config_meta(&masked).unwrap_or_default(),
                    // The whole (multi-block) file is stored per block; raw_code
                    // is never compared, and snapshots are rare, so this is fine.
                    raw_code: Some(raw.clone()),
                    alias: config_alias(&masked),
                    ..Default::default()
                },
            );
            name_index.insert(name, uid.clone());
            node_sql.push((uid, masked.clone().into_owned()));
        }
    }

    // --- schema.yml: collect + parse once ---
    // dbt reads "properties" yml ONLY from the configured resource paths, so the
    // scan is restricted to them. Scanning the whole project dir would ingest
    // vendored projects (`dbt_packages/`), build artifacts (`target/`), and
    // virtualenvs — synthesizing phantom sources/tests under this project's
    // namespace and silently diverging from the manifest.
    // `(file, resource root)` pairs: exposures record their `path` relative to
    // the declaring resource root (the way dbt does for models), so the root
    // must survive the collection. Dedup by FILE alone (sorting puts equal
    // files adjacent): overlapping resource paths (e.g. a seed dir nested in a
    // model dir) collect the same file under two roots, and scanning it twice
    // would duplicate its synthesized tests — the pre-exposure contract.
    let mut yml_files: Vec<(PathBuf, PathBuf)> = Vec::new();
    for sub in model_paths.iter().chain(&seed_paths).chain(&snapshot_paths) {
        let sub_root = dir.join(sub);
        let mut files = Vec::new();
        collect_files(&sub_root, "yml", &mut files);
        collect_files(&sub_root, "yaml", &mut files);
        for file in files {
            yml_files.push((file, sub_root.clone()));
        }
    }
    yml_files.sort();
    yml_files.dedup_by(|a, b| a.0 == b.0);

    // Parsed once, kept for BOTH the YAML-snapshot synthesis pass below AND
    // the schema-metadata pass further down, so a file is never re-parsed. A
    // non-schema yml deserializes to all-empty defaults; only a YAML shape
    // error (e.g. a top-level sequence) fails to parse — reported as a
    // warning (not silently skipped) so one broken file never sinks the rest
    // of the project.
    let mut parsed: Vec<(SchemaFile, PathBuf, PathBuf)> = Vec::new();
    for (file, yml_root) in yml_files {
        match yaml_to_json(&read(&file), &mut warnings)
            .and_then(|v| serde_json::from_value::<SchemaFile>(v).map_err(|e| e.to_string()))
        {
            Ok(schema) => parsed.push((schema, file, yml_root)),
            Err(err) => warnings.push(format!(
                "skipped {}: invalid schema file ({err})",
                rel_path(dir, &file)
            )),
        }
    }

    // --- YAML-defined snapshots (dbt 1.9+ `snapshots:` schema entries with a
    // `relation:`, replacing the `{% snapshot %}` block) become nodes here,
    // BEFORE ref/source resolution, so both a model that refs one AND the
    // relation's own ref/source resolve.
    for (schema, file, yml_root) in &parsed {
        for entry in &schema.snapshots {
            let Some(rel) = &entry.relation else {
                continue;
            };
            if entry.config.enabled == Some(false) {
                continue;
            }
            if name_index.contains_key(&entry.name) {
                warnings.push(format!(
                    "duplicate resource name '{}': ignoring {}",
                    entry.name,
                    rel_path(dir, file)
                ));
                continue;
            }
            // dbt keys snapshot configs by NAME (same rule as the
            // `{% snapshot %}` path).
            let yml_rel = rel_path(yml_root, file);
            let mut segments = tree_segments(&proj, &yml_rel);
            segments.pop();
            segments.push(entry.name.clone());
            let tree = resolve_tree_config(
                &project.snapshots,
                &segments,
                "snapshots".to_string(),
                &mut warnings,
            );
            if tree.enabled == Some(false) {
                continue;
            }
            // dbt V2 synthesizes `select * from {relation}` (wrapping a bare
            // ref()/source() in {{ }}) and feeds it through the normal
            // pipeline — mirror that so the SQL preview and the edge
            // extractors see real text.
            let body = if rel.contains("{{") {
                rel.clone()
            } else {
                format!("{{{{ {rel} }}}}")
            };
            let sql = format!("select * from {body}");
            let uid = format!("snapshot.{proj}.{}", entry.name);
            if !tree.meta.is_empty() {
                tree_meta.insert(uid.clone(), tree.meta.clone());
            }
            if let Some(uk) = &tree.unique_key {
                tree_uk.insert(uid.clone(), uk.clone());
            }
            tree_gov.insert(uid.clone(), tree.clone());
            // dbt records a YAML-defined snapshot's `path` as a virtual
            // `<name>.sql` (there is no real .sql file; dbt ≤1.11 prefixed the
            // declaring yml, 1.12 records the bare name — the loader tracks the
            // current convention so source and manifest mode agree).
            let path = format!("{}.sql", entry.name);
            nodes.insert(
                uid.clone(),
                RawNode {
                    name: entry.name.clone(),
                    resource_type: "snapshot".to_string(),
                    path: Some(path),
                    original_file_path: Some(rel_path(dir, file)),
                    config: RawConfig {
                        materialized: Some("snapshot".to_string()),
                        ..Default::default()
                    },
                    // dbt records `language: "sql"` on YAML snapshots too.
                    language: Some("sql".to_string()),
                    raw_code: Some(sql.clone()),
                    ..Default::default()
                },
            );
            name_index.insert(entry.name.clone(), uid.clone());
            node_sql.push((uid, sql));
        }
    }

    // --- resolve ref()/source() -> parent_map + child_map ---
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    for (uid, sql) in &node_sql {
        let mut parents: Vec<String> = Vec::new();
        // `sql` is already comment-masked — no second mask pass.
        for site in pat.call_sites_masked(sql) {
            match site.kind {
                // Unresolved refs (cross-package, typo, gated) are dropped, not faked.
                CallKind::Ref { name, .. } => {
                    if let Some(parent) = name_index.get(&sql[name]) {
                        parents.push(parent.clone());
                    }
                }
                // A source referenced but not declared in schema.yml dangles
                // here and is pruned later — it never becomes a node.
                CallKind::Source { source, table } => {
                    parents.push(format!("source.{proj}.{}.{}", &sql[source], &sql[table]));
                }
                CallKind::Config => {}
            }
        }
        parents.sort();
        parents.dedup();
        for parent in &parents {
            child_map
                .entry(parent.clone())
                .or_default()
                .push(uid.clone());
        }
        if !parents.is_empty() {
            parent_map.insert(uid.clone(), parents);
        }
    }
    for children in child_map.values_mut() {
        children.sort();
        children.dedup();
    }

    // --- schema.yml: source nodes + metadata/tests for everything ---
    // The test-synthesis context borrows `name_index` shared — build it only
    // now that every resource discovery pass above has finished mutating it.
    let mut test_ctx = TestCtx {
        seq: 0,
        proj: &proj,
        pat: &pat,
        name_index: &name_index,
    };
    for (schema, file, yml_root) in &parsed {
        // dbt records a source's `path`/`original_file_path` as the declaring
        // schema file (project-root-relative) — match it so the detail modal
        // and the `$EDITOR` jump work identically in both modes.
        let yml_path = rel_path(dir, file);
        for src in &schema.sources {
            for tbl in &src.tables {
                let uid = format!("source.{proj}.{}.{}", src.name, tbl.name);
                // The `sources:` tree keys by the source fqn: the project
                // name, the declaring yml's directories relative to the
                // resource root, then `<source>: <table>` — e.g.
                // `sample: staging: raw: orders` (verified against real
                // dbt's recorded fqn). `tree_segments` includes the file
                // stem, which the fqn replaces with source+table.
                let mut segments = tree_segments(&proj, &rel_path(yml_root, file));
                segments.pop();
                segments.push(src.name.clone());
                segments.push(tbl.name.clone());
                let tree = resolve_tree_config(
                    &project.sources,
                    &segments,
                    "sources".to_string(),
                    &mut warnings,
                );
                sources.insert(
                    uid.clone(),
                    RawSource {
                        name: tbl.name.clone(),
                        resource_type: "source".to_string(),
                        path: Some(yml_path.clone()),
                        original_file_path: Some(yml_path.clone()),
                        description: tbl
                            .description
                            .clone()
                            .map(|d| docs.render(&d, &mut warnings)),
                        columns: columns_json(&tbl.columns, &docs, &mut warnings),
                        // A source with no `schema:` resolves to the SOURCE
                        // NAME — dbt's default (verified on 1.12.6), not null:
                        // two tables under schema-less sources of the same
                        // name are the same relation, which `duplicate_sources`
                        // can only see once the default is recorded.
                        schema: src.schema.clone().or_else(|| Some(src.name.clone())),
                        database: src.database.clone(),
                        // schema.yml's `identifier:` — the source table's real
                        // warehouse name when it differs from `name`.
                        identifier: tbl.identifier.clone(),
                        // Source mode synthesizes a manifest for the local
                        // project only — no foreign package can appear, so
                        // package identity is vacuous here.
                        package_name: None,
                        // meta merge: `sources:` tree `+meta` < source-level
                        // `meta:` (cascades to tables) < table-level `meta:`
                        // — each overrides the previous per key (dbt's
                        // shallow merge, verified).
                        meta: source_meta(tree.meta, src, tbl, &mut warnings),
                        // Source-level only: dbt cascades it to every table
                        // and ignores table-level/`+loader` declarations
                        // (verified on 1.12.6).
                        loader: src.loader.clone(),
                        // Table-level `loaded_at_field:` overrides the
                        // source's; the table's explicit `null` clears the
                        // inherited value (`+loaded_at_field` is one dbt
                        // ignores — deliberately not resolved).
                        loaded_at_field: match &tbl.loaded_at_field {
                            Some(v) => v.clone(),
                            None => src.loaded_at_field.clone(),
                        },
                        // `freshness:` merges PER THRESHOLD down
                        // table > source > `+freshness` — a table's
                        // `error_after` keeps the inherited `warn_after`
                        // (verified); `freshness: null` clears it all.
                        freshness: effective_freshness(
                            &tbl.freshness,
                            &src.freshness,
                            &tree.freshness,
                        ),
                    },
                );
                synth_tests(&mut nodes, &mut test_ctx, &uid, &tbl.name, None, &tbl.tests);
                synth_tests(
                    &mut nodes,
                    &mut test_ctx,
                    &uid,
                    &tbl.name,
                    None,
                    &tbl.data_tests,
                );
                for col in &tbl.columns {
                    synth_tests(
                        &mut nodes,
                        &mut test_ctx,
                        &uid,
                        &tbl.name,
                        Some(&col.name),
                        &col.tests,
                    );
                    synth_tests(
                        &mut nodes,
                        &mut test_ctx,
                        &uid,
                        &tbl.name,
                        Some(&col.name),
                        &col.data_tests,
                    );
                }
            }
        }

        // Exposures: pure leaf declarations. Their `depends_on` entries are
        // Jinja strings (`ref('x')` / `source('a','b')`), so the SAME extractors
        // the .sql scan uses resolve them — an unresolved ref drops out, exactly
        // like a model's. dbt records `path` resource-root-relative (like a
        // model's) and `original_file_path` project-root-relative.
        for exp in &schema.exposures {
            // `config: enabled: false` parks the exposure like dbt's `disabled`.
            if exp.config.enabled == Some(false) {
                continue;
            }
            let uid = format!("exposure.{proj}.{}", exp.name);
            let mut deps: Vec<String> = Vec::new();
            for dep in &exp.depends_on {
                for ref_name in pat.refs(dep) {
                    if let Some(parent) = name_index.get(&ref_name) {
                        deps.push(parent.clone());
                    }
                }
                for (src, tbl) in pat.sources(dep) {
                    deps.push(format!("source.{proj}.{src}.{tbl}"));
                }
            }
            deps.sort();
            deps.dedup();
            exposures.insert(
                uid,
                RawExposure {
                    name: exp.name.clone(),
                    exposure_type: exp.exposure_type.clone(),
                    owner: RawExposureOwner {
                        name: exp.owner.name.clone(),
                        email: exp.owner.email.clone(),
                    },
                    url: exp.url.clone(),
                    description: exp
                        .description
                        .clone()
                        .map(|d| docs.render(&d, &mut warnings)),
                    path: Some(rel_path(yml_root, file)),
                    original_file_path: Some(yml_path.clone()),
                    depends_on: RawDependsOn {
                        nodes: deps,
                        macros: vec![],
                    },
                },
            );
        }

        apply_meta(
            &mut nodes,
            &mut test_ctx,
            "model",
            &schema.models,
            &yml_path,
            &docs,
            &mut warnings,
        );
        apply_meta(
            &mut nodes,
            &mut test_ctx,
            "seed",
            &schema.seeds,
            &yml_path,
            &docs,
            &mut warnings,
        );
        apply_meta(
            &mut nodes,
            &mut test_ctx,
            "snapshot",
            &schema.snapshots,
            &yml_path,
            &docs,
            &mut warnings,
        );
    }

    // Fill remaining model materializations from the dbt_project.yml tree, then
    // dbt's default `view` (the tree is applied AFTER schema.yml so the full
    // precedence chain is in-file > schema.yml > tree > default, like dbt).
    // The tree's `+meta` is likewise the LOWEST precedence: it fills only keys
    // not already set by schema.yml or in-file `config(meta=…)` (dbt merges
    // meta dicts shallowly — a higher-precedence key wins whole-value).
    for (uid, node) in nodes.iter_mut() {
        if node.resource_type == "model" && node.config.materialized.is_none() {
            node.config.materialized = tree_mat
                .get(uid)
                .cloned()
                .or_else(|| Some("view".to_string()));
        }
        if let Some(tree) = tree_meta.get(uid) {
            for (k, v) in tree {
                node.meta.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        // `+unique_key` sits under in-file `config(unique_key=…)` and
        // schema.yml `config: unique_key:` — the whole declared value (a
        // bare string or a string list; `Dag::build` normalizes either).
        if node.config.unique_key.is_none() {
            node.config.unique_key = tree_uk.get(uid).cloned();
        }
        // The governance keys — the tree is again the lowest rung, filling
        // only what in-file `config()` and schema.yml left unset.
        // `group` is resource-agnostic; `access`/`contract`/
        // `incremental_strategy` are model-only in dbt. An undeclared
        // model's `access` lands on dbt's `protected` default (verified:
        // every undeclared model records it on the wire); the other fields
        // simply stay absent.
        let gov = tree_gov.get(uid);
        if node.group.is_none() {
            node.group = gov.and_then(|g| g.group.clone());
        }
        if node.resource_type == "model" {
            if node.access.is_none() {
                node.access = gov
                    .and_then(|g| g.access.clone())
                    .or_else(|| Some("protected".to_string()));
            }
            if node.contract.is_none() {
                node.contract = gov.and_then(|g| g.contract.clone());
            }
            if node.config.incremental_strategy.is_none() {
                node.config.incremental_strategy = gov.and_then(|g| g.incremental_strategy.clone());
            }
        }
    }

    // The `{% docs __overview__ %}` block rides into the manifest's `docs`
    // map in exactly the shape a real manifest carries
    // (`doc.<proj>.__overview__`), so `Dag::build` picks the overview up
    // through ONE intake path for both load modes. `DocBlocks::render`
    // expands `doc()` refs and strips `{# #}` comments — what dbt writes to
    // `block_contents`. Other docs blocks are not ingested (they already
    // expanded where referenced; nothing else consumes them).
    let mut manifest_docs = HashMap::new();
    if let Some(body) = docs.blocks.get("__overview__") {
        manifest_docs.insert(
            format!("doc.{proj}.__overview__"),
            RawDoc {
                name: "__overview__".to_string(),
                package_name: Some(proj.clone()),
                block_contents: docs.render(body, &mut warnings),
            },
        );
    }

    // Collapse identical warnings — a misdeclared `+meta` level every node
    // descends through would otherwise warn once PER NODE (the message is
    // the same string, so first-seen order survives dedup).
    let mut seen_warnings = HashSet::new();
    warnings.retain(|w| seen_warnings.insert(w.clone()));

    Ok((
        RawManifest {
            // `dbt_project.yml`'s `name` IS the root project — record it where
            // real dbt does so `Dag::project_name` resolves identically in
            // both load modes (the consistency suite asserts that).
            metadata: RawMetadata {
                project_name: Some(proj),
                // Source mode deliberately does NOT read profiles.yml, so the
                // adapter is unknown here — CLL falls back to its default
                // dialect (`--sql-dialect` is the override knob).
                adapter_type: None,
            },
            nodes,
            sources,
            exposures,
            parent_map,
            child_map,
            macros: HashMap::new(),
            // Source mode never synthesizes the semantic layer or unit tests —
            // they're YAML-only resources the source parser doesn't scan.
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            docs: manifest_docs,
        },
        warnings,
    ))
}

mod collect;
mod config_tree;
mod docs;
mod patterns;
mod schema;
#[cfg(test)]
mod tests;
mod tests_synth;
mod yaml;

use collect::*;
use config_tree::*;
use docs::*;
use patterns::*;
use schema::*;
use tests_synth::*;
use yaml::*;

// The span-carrying call extractors lint uses for finding positions — same
// machinery as the loaders' own pass, not a second implementation.
pub use collect::mask_jinja_comments;
pub use patterns::{call_sites_masked, CallKind, CallSite};
pub(crate) use patterns::{kwarg, lit_value};
