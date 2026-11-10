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
    Dag, RawConfig, RawDependsOn, RawExposure, RawExposureOwner, RawManifest, RawMetadata, RawNode,
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
    // (uid, Jinja-comment-stripped SQL) deferred to the ref/source pass below.
    let mut node_sql: Vec<(String, String)> = Vec::new();
    // uid -> dbt_project.yml tree materialization, applied AFTER schema.yml
    // (precedence: in-file config() > schema.yml > project tree > default view).
    let mut tree_mat: HashMap<String, String> = HashMap::new();
    // uid -> dbt_project.yml tree `+meta` dict, merged AFTER schema.yml like
    // `tree_mat` (the meta precedence chain is the same shape: in-file
    // config(meta=…) > schema.yml meta/config.meta > project tree).
    let mut tree_meta: HashMap<String, serde_json::Map<String, serde_json::Value>> = HashMap::new();

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
        // Keep the pre-strip text for the SQL preview modal (comments intact);
        // the ref/source/config extractors still see the Jinja-comment-stripped
        // copy (dbt's Jinja-before-SQL-comments semantics).
        let raw = read(&file);
        let stripped = strip_jinja_comments(&raw);
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
        if pat.disabled(&stripped) || tree.enabled == Some(false) {
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
        nodes.insert(
            uid.clone(),
            RawNode {
                name: name.clone(),
                resource_type: "model".to_string(),
                path: Some(path),
                original_file_path: Some(rel_path(dir, &file)),
                config: RawConfig {
                    materialized: pat.materialized(&stripped),
                },
                // In-file `config(meta={…})` is the HIGHEST precedence level;
                // schema.yml and the project tree merge under it per key.
                meta: config_meta(&stripped).unwrap_or_default(),
                raw_code: Some(raw),
                ..Default::default()
            },
        );
        name_index.insert(name, uid.clone());
        node_sql.push((uid, stripped));
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
        let stripped = strip_jinja_comments(&raw);
        let path = rel_path(&root, &file);
        let ofp = rel_path(dir, &file);
        // An in-file `enabled=false` applies to every block in the file (rare).
        if pat.disabled(&stripped) {
            continue;
        }
        // Refs in a multi-block file are attributed to every block in it (rare).
        for name in pat.snapshots(&stripped) {
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
            nodes.insert(
                uid.clone(),
                RawNode {
                    name: name.clone(),
                    resource_type: "snapshot".to_string(),
                    path: Some(path.clone()),
                    original_file_path: Some(ofp.clone()),
                    config: RawConfig {
                        materialized: Some("snapshot".to_string()),
                    },
                    // The `{{ config(…) }}` inside a `{% snapshot %}` block is
                    // extracted file-wide like the other pattern extractors
                    // (multi-block files share the config — rare).
                    meta: config_meta(&stripped).unwrap_or_default(),
                    // The whole (multi-block) file is stored per block; raw_code
                    // is never compared, and snapshots are rare, so this is fine.
                    raw_code: Some(raw.clone()),
                    ..Default::default()
                },
            );
            name_index.insert(name, uid.clone());
            node_sql.push((uid, stripped.clone()));
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
                    },
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
        for ref_name in pat.refs(sql) {
            // Unresolved refs (cross-package, typo, gated) are dropped, not faked.
            if let Some(parent) = name_index.get(&ref_name) {
                parents.push(parent.clone());
            }
        }
        for (src, tbl) in pat.sources(sql) {
            // A source referenced but not declared in schema.yml dangles here and
            // is pruned later — it never becomes a node.
            parents.push(format!("source.{proj}.{src}.{tbl}"));
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
                        schema: src.schema.clone(),
                        database: src.database.clone(),
                        // Source mode synthesizes a manifest for the local
                        // project only — no foreign package can appear, so
                        // package identity is vacuous here.
                        package_name: None,
                        // meta merge: `sources:` tree `+meta` < source-level
                        // `meta:` (cascades to tables) < table-level `meta:`
                        // — each overrides the previous per key (dbt's
                        // shallow merge, verified).
                        meta: source_meta(tree.meta, src, tbl, &mut warnings),
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
            &docs,
            &mut warnings,
        );
        apply_meta(
            &mut nodes,
            &mut test_ctx,
            "seed",
            &schema.seeds,
            &docs,
            &mut warnings,
        );
        apply_meta(
            &mut nodes,
            &mut test_ctx,
            "snapshot",
            &schema.snapshots,
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
        },
        warnings,
    ))
}

/// The shared context [`synth_tests`] and [`apply_meta`] thread through every
/// schema-file test pass: the running synth-key counter, the project name, and
/// the `ref()`/`source()` extractors + name index a `relationships` test's
/// `to:` resolves against (the same lookups the .sql scan uses).
struct TestCtx<'a> {
    seq: usize,
    proj: &'a str,
    pat: &'a Patterns,
    name_index: &'a HashMap<String, String>,
}

/// Attach schema-file metadata (description / materialization / tags / columns)
/// and tests to already-discovered nodes, matched by `<type>.<proj>.<name>`. A
/// schema entry with no matching node (e.g. a documented but absent model) is
/// skipped — we never fabricate a node from docs alone.
fn apply_meta(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    type_prefix: &str,
    entries: &[SchemaModel],
    docs: &DocBlocks,
    warnings: &mut Vec<String>,
) {
    for entry in entries {
        let uid = format!("{type_prefix}.{}.{}", ctx.proj, entry.name);
        // Schema-level `config: enabled: false` parks the node like dbt's
        // `disabled`: drop it (its edges are pruned by Dag::build's
        // prune-first step) and record none of its docs/tests.
        if entry.config.enabled == Some(false) {
            nodes.remove(&uid);
            continue;
        }
        // Skip docs for an absent node; this also gates the synth_tests calls
        // below (a documented-but-absent model contributes no tests).
        let Some(node) = nodes.get_mut(&uid) else {
            continue;
        };
        if node.description.is_none() {
            node.description = entry.description.clone().map(|d| docs.render(&d, warnings));
        }
        // An explicit schema materialization wins over the `view` default but
        // not over an in-file `config()` (already set, so only fill if None).
        if node.config.materialized.is_none() {
            node.config.materialized = entry.config.materialized.clone();
        }
        if !entry.tags.is_empty() {
            node.tags = entry.tags.clone();
        }
        // Schema `meta:` / `config.meta:` merge per top-level key: the entry's
        // `config.meta` wins over its top-level `meta:` (declaring both is a
        // dbt parse error — degraded here to a warning + that merge), and any
        // in-file `config(meta=…)` wins over both (`entry().or_insert`). The
        // bare top-level `meta:` spelling is accepted for EVERY entry type —
        // not just models — verified against real dbt 1.12.6 (seeds and
        // snapshots honour it identically to `config.meta`).
        if entry.meta.is_object() && entry.config.meta.is_object() {
            warnings.push(format!(
                "{} '{}' declares meta: both at the entry level and under config: (a dbt parse error); merging with config: winning",
                type_prefix, entry.name
            ));
        }
        let what = format!("{type_prefix} '{}'", entry.name);
        let mut schema_meta = meta_map(&entry.meta, warnings, &what);
        schema_meta.extend(meta_map(
            &entry.config.meta,
            warnings,
            &format!("{what} config"),
        ));
        for (k, v) in schema_meta {
            node.meta.entry(k).or_insert(v);
        }
        if !entry.columns.is_empty() {
            node.columns = columns_json(&entry.columns, docs, warnings);
        }
        // `node`'s borrow ends here; synth_tests re-borrows `nodes` mutably.
        synth_tests(nodes, ctx, &uid, &entry.name, None, &entry.tests);
        synth_tests(nodes, ctx, &uid, &entry.name, None, &entry.data_tests);
        for col in &entry.columns {
            synth_tests(nodes, ctx, &uid, &entry.name, Some(&col.name), &col.tests);
            synth_tests(
                nodes,
                ctx,
                &uid,
                &entry.name,
                Some(&col.name),
                &col.data_tests,
            );
        }
    }
}

/// A YAML `meta:` mapping → the `serde_json` map `RawNode.meta` /
/// `RawSource.meta` carry. A `Null`/absent `meta:` yields the empty map; a
/// non-mapping `meta:` (a dbt error — meta must be a key/value dict) warns
/// and degrades to empty, never a parse failure.
fn meta_map(
    v: &serde_json::Value,
    warnings: &mut Vec<String>,
    what: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                out.insert(k.clone(), v.clone());
            }
            out
        }
        serde_json::Value::Null => out,
        _ => {
            warnings.push(format!(
                "{what} declares a non-mapping 'meta:' — a key/value dict is expected; ignoring it"
            ));
            out
        }
    }
}

/// A source table's merged `meta`: the `sources:` tree `+meta` (lowest
/// precedence), then the source-level `meta:` (cascades to every table),
/// then the table-level `meta:` — each level overrides the previous per
/// top-level key (dbt's shallow merge, verified).
fn source_meta(
    tree: serde_json::Map<String, serde_json::Value>,
    src: &SchemaSource,
    tbl: &SchemaSourceTable,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = tree;
    out.extend(meta_map(
        &src.meta,
        warnings,
        &format!("source '{}'", src.name),
    ));
    out.extend(meta_map(
        &tbl.meta,
        warnings,
        &format!("source '{}' table '{}'", src.name, tbl.name),
    ));
    out
}

/// Record each schema-file test as a synthetic `resource_type: "test"` node so
/// the existing [`Dag::build`] captures it pre-prune. The map KEY uses a counter
/// so two tests on the same (node, column) can never collide and silently drop;
/// `attached_node` points the capture straight at the target.
///
/// A single-key-map entry's config value rides along as `test_metadata.kwargs`
/// (the same shape a real manifest records), and a `relationships`-style `to:`
/// is resolved through the SAME `ref()`/`source()` extractors the .sql scan
/// uses into `depends_on.nodes` ([attached, ref] — the real manifest's shape),
/// so `Dag::build` fills `TestInfo::ref_uid`/`ref_column` identically in both
/// load modes. An unresolvable `to:` leaves only the attached dep — the test
/// stays registered, just target-less.
fn synth_tests(
    nodes: &mut HashMap<String, RawNode>,
    ctx: &mut TestCtx<'_>,
    target_uid: &str,
    target_name: &str,
    column: Option<&str>,
    tests: &[serde_json::Value],
) {
    for test in tests {
        let Some(kind) = test_kind(test) else {
            continue;
        };
        let key = format!("test.{}.{}", ctx.proj, ctx.seq);
        ctx.seq += 1;
        let name = match column {
            Some(c) => format!("{kind}_{target_name}_{c}"),
            None => format!("{kind}_{target_name}"),
        };
        // The config map under `{kind: {...}}` — a `relationships` test's
        // `to:`/`field:` live there; a bare-string test has none.
        let kwargs: serde_json::Map<String, serde_json::Value> = match test {
            serde_json::Value::Object(m) => m
                .get(&kind)
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default(),
            _ => Default::default(),
        };
        // `to:` is a Jinja-string reference — resolve through the same
        // extractors the .sql scan uses. Unresolved refs (and non-ref values
        // like `var('x')`) drop out, exactly like a model's.
        let mut deps = vec![target_uid.to_string()];
        if let Some(to) = kwargs.get("to").and_then(|v| v.as_str()) {
            for ref_name in ctx.pat.refs(to) {
                if let Some(uid) = ctx.name_index.get(&ref_name) {
                    deps.push(uid.clone());
                }
            }
            for (src, tbl) in ctx.pat.sources(to) {
                deps.push(format!("source.{}.{}.{}", ctx.proj, src, tbl));
            }
        }
        deps.sort();
        deps.dedup();
        nodes.insert(
            key,
            RawNode {
                name,
                resource_type: "test".to_string(),
                test_metadata: Some(RawTestMetadata {
                    name: Some(kind),
                    kwargs: Some(kwargs),
                }),
                attached_node: Some(target_uid.to_string()),
                column_name: column.map(str::to_string),
                depends_on: RawDependsOn {
                    nodes: deps,
                    macros: vec![],
                },
                ..Default::default()
            },
        );
    }
}

/// The test kind from a schema-file `tests:`/`data_tests:` entry: a bare string
/// (`unique`) is the kind; a single-key map (`accepted_values: {...}`) uses its
/// key. Anything else is ignored.
fn test_kind(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => m.keys().next().cloned(),
        _ => None,
    }
}

/// The `{% docs %}` blocks collected from a project's `.md` files, ready to
/// expand `{{ doc('name') }}` refs in descriptions.
struct DocBlocks {
    /// `name -> trimmed body` (dbt `.strip()`s `block_contents` — matched).
    blocks: BTreeMap<String, String>,
    /// The local project name: a 2-arg `doc('proj', 'x')` resolves locally
    /// (verified against real dbt); a different package segment never resolves
    /// in source mode, which sees no installed packages.
    proj: String,
    /// `{{ doc('x') }}` / `{{ doc('pkg', 'x') }}` — the ONE Jinja call resolved
    /// in descriptions. Jinja whitespace control is honoured: `{{-` eats the
    /// whitespace immediately before the tag (group 2) and `-}}` eats the
    /// whitespace after (group 5), so the adjacent whitespace is captured in
    /// the match and only re-emitted when the dash is absent.
    doc_ref_re: Regex,
    /// A loose `{{ doc... }}` detector: any `doc` call the strict pattern
    /// above can't interpret (bad arg count, a non-literal arg like `var`) is
    /// a parse error for real dbt — surfaced as a warning instead of silently
    /// passing the raw text through.
    doc_detect_re: Regex,
    /// Dedup for warning emission: each distinct warning is pushed once into
    /// `warnings` (the shared Vec would make `contains` O(n) per ref).
    warned: RefCell<BTreeSet<String>>,
}

impl DocBlocks {
    fn new(proj: String, blocks: BTreeMap<String, String>) -> Self {
        DocBlocks {
            blocks,
            proj,
            doc_ref_re: Regex::new(
                r#"(\s*)\{\{(-?)\s*doc\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*['"]([^'"]+)['"]\s*)?\)\s*(-?)\}\}(\s*)"#,
            )
            .unwrap(),
            doc_detect_re: Regex::new(r#"\{\{-?\s*doc\b[^}]*-?\}\}"#).unwrap(),
            warned: RefCell::new(BTreeSet::new()),
        }
    }

    /// Push `warn` into `warnings` exactly once per distinct message.
    fn warn_once(&self, warnings: &mut Vec<String>, warn: String) {
        if self.warned.borrow_mut().insert(warn.clone()) {
            warnings.push(warn);
        }
    }

    /// Expand `{{ doc('x') }}` / `{{ doc('pkg', 'x') }}` refs the way dbt
    /// renders them in descriptions: substitute the block's body. dbt renders
    /// the whole description as Jinja, so `{# #}` comments (and any doc()
    /// inside them) disappear first; `{{-`/`-}}` whitespace control consumes
    /// the adjacent whitespace. An unresolved ref (no such block — where dbt
    /// would hard-error — or a foreign package source mode can't see) keeps
    /// the raw `{{ doc(...) }}` text and pushes one dedup'd warning, like the
    /// loader's other recoverable oddities; an unparseable doc() call warns
    /// too (dbt parse-errors there as well).
    fn render(&self, text: &str, warnings: &mut Vec<String>) -> String {
        let stripped = strip_jinja_comments_text(text);
        for m in self.doc_detect_re.find_iter(&stripped) {
            if self.doc_ref_re.captures(m.as_str()).is_none() {
                self.warn_once(
                    warnings,
                    format!(
                        "uninterpretable doc() call {} in a description; leaving the raw text",
                        m.as_str()
                    ),
                );
            }
        }
        self.doc_ref_re
            .replace_all(&stripped, |c: &regex::Captures| {
                let (pkg, name) = match c.get(4) {
                    Some(n) => (Some(c.get(3).unwrap().as_str()), n.as_str()),
                    None => (None, c.get(3).unwrap().as_str()),
                };
                let hit = match pkg {
                    Some(p) if p != self.proj => None,
                    _ => self.blocks.get(name),
                };
                match hit {
                    Some(body) => {
                        // Whitespace control: a dash eats the adjacent
                        // captured whitespace; no dash re-emits it.
                        let left = if c.get(2).unwrap().as_str() == "-" {
                            ""
                        } else {
                            c.get(1).unwrap().as_str()
                        };
                        let right = if c.get(5).unwrap().as_str() == "-" {
                            ""
                        } else {
                            c.get(6).unwrap().as_str()
                        };
                        format!("{left}{body}{right}")
                    }
                    None => {
                        let warn = match pkg {
                            Some(p) => format!(
                                "unresolved doc('{p}', '{name}') in a description: \
                                 package '{p}' is not this project; leaving the raw text"
                            ),
                            None => format!(
                                "unresolved doc('{name}') in a description: \
                                 no matching {{% docs %}} block; leaving the raw text"
                            ),
                        };
                        self.warn_once(warnings, warn);
                        c.get(0).unwrap().as_str().to_string()
                    }
                }
            })
            .into_owned()
    }
}

/// Collect `{% docs name %}…{% enddocs %}` blocks from every `.md` file under
/// `doc_paths` into a `name -> body` map, first-wins on a duplicate name (a
/// duplicate is a hard error for dbt; the loader warns instead, like its
/// duplicate model-name handling).
fn collect_doc_blocks(
    dir: &Path,
    doc_paths: &[String],
    pat: &Patterns,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    // Overlapping doc/resource paths can collect the same file twice — dedup
    // by file (sorted, so equal files are adjacent) or its blocks double-report.
    let mut files = collect_with_root(dir, doc_paths, "md");
    files.dedup_by(|a, b| a.0 == b.0);
    let mut blocks = BTreeMap::new();
    for (file, _root) in files {
        // A `{% docs %}` tag inside `{# #}` is invisible to dbt (verified), so
        // extract from the Jinja-comment-stripped text like the .sql pass —
        // the text (not SQL) variant, because dbt renders a block body as
        // Jinja and a `{# c #}` inside it disappears entirely.
        let stripped = strip_jinja_comments_text(&read(&file));
        // Every `{% docs %}` must pair with `{% enddocs %}` — real dbt
        // hard-errors on a stray or unclosed tag; warn (degraded) and keep
        // collecting the well-formed blocks.
        let mut open: Option<String> = None;
        for tag in pat.doc_tags(&stripped) {
            match tag {
                DocTag::Open(name) => {
                    if let Some(prev) = open.replace(name) {
                        warnings.push(format!(
                            "unclosed {{% docs {prev} %}} in {}: nested {{% docs %}} tag",
                            rel_path(dir, &file)
                        ));
                    }
                }
                DocTag::Close => {
                    if open.take().is_none() {
                        warnings.push(format!(
                            "stray {{% enddocs %}} in {}: no open {{% docs %}} tag",
                            rel_path(dir, &file)
                        ));
                    }
                }
            }
        }
        if let Some(name) = open {
            warnings.push(format!(
                "unclosed {{% docs {name} %}} in {}: no matching {{% enddocs %}}",
                rel_path(dir, &file)
            ));
        }
        for (name, body) in pat.doc_blocks(&stripped) {
            if blocks.contains_key(&name) {
                warnings.push(format!(
                    "duplicate docs block '{name}': ignoring {}",
                    rel_path(dir, &file)
                ));
                continue;
            }
            blocks.insert(name, body);
        }
    }
    blocks
}

/// Build a manifest-shaped `columns` object (`{name: {name,data_type,description}}`)
/// preserving definition order (the `Map` is order-preserving via `preserve_order`).
/// Column descriptions render `{{ doc() }}` refs like every other description.
fn columns_json(
    cols: &[SchemaColumn],
    docs: &DocBlocks,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for c in cols {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "name".to_string(),
            serde_json::Value::String(c.name.clone()),
        );
        if let Some(dt) = &c.data_type {
            obj.insert(
                "data_type".to_string(),
                serde_json::Value::String(dt.clone()),
            );
        }
        if let Some(d) = &c.description {
            obj.insert(
                "description".to_string(),
                serde_json::Value::String(docs.render(d, warnings)),
            );
        }
        map.insert(c.name.clone(), serde_json::Value::Object(obj));
    }
    map
}

/// Column names from a seed CSV header row (no types — and no descriptions to
/// render, but the shared builder keeps the signature uniform). Empty if
/// unreadable.
fn csv_header_columns(
    file: &Path,
    docs: &DocBlocks,
    warnings: &mut Vec<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let contents = read(file);
    let header = contents.lines().next().unwrap_or("");
    let cols: Vec<SchemaColumn> = header
        .split(',')
        .map(|c| SchemaColumn {
            name: c.trim().trim_matches('"').to_string(),
            ..Default::default()
        })
        .filter(|c| !c.name.is_empty())
        .collect();
    columns_json(&cols, docs, warnings)
}

/// Read a file to a string, or empty on error (a single unreadable model should
/// not abort the whole project scan).
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// The file stem (name without extension) as an owned string.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Keep only resource paths that are safe to join onto the project root:
/// relative, with no `..` component. `dbt_project.yml` is untrusted input, so
/// a traversal value (`../../etc`, `/etc`) is skipped rather than letting the
/// model/seed/snapshot scan (and the schema.yml scan, which walks the same
/// lists) read files outside the project directory.
fn contained_resource_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|p| {
            Path::new(p)
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        })
        .collect()
}

/// Path relative to the project root (for `original_file_path`), forward-slashed.
fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Collect `(file, sub_root)` pairs with `ext` under each of `subdirs` (relative
/// to `root`), sorted by file path so unique-id generation and name-collision
/// resolution are OS-independent. `sub_root` (`root/<subdir>`) lets `path` be made
/// relative to the paths root the way dbt records it, distinct from the
/// project-root-relative `original_file_path`.
fn collect_with_root(root: &Path, subdirs: &[String], ext: &str) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    for sub in subdirs {
        let sub_root = root.join(sub);
        let mut files = Vec::new();
        collect_files(&sub_root, ext, &mut files);
        for file in files {
            out.push((file, sub_root.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Recursively collect files with extension `ext` under `dir`. A missing dir is
/// silently skipped. Recursion is decided by `entry.file_type()`, which does
/// NOT follow symlinks: a symlinked directory is never entered, so a link loop
/// (e.g. `models/loop` -> `models`) can't recurse forever.
fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_files(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

/// Strip Jinja comments (`{# #}`) ONLY — deliberately NOT `--` / `/* */`.
///
/// dbt renders Jinja BEFORE the SQL (and its comments) exists, so a
/// `{{ ref() }}` inside a `--` or `/* */` SQL comment IS a real dependency in
/// the manifest; only `{# #}` hides Jinja from dbt. The extractors must see
/// exactly what dbt sees, or source mode silently drops edges the manifest has
/// (the consistency contract).
fn strip_jinja_comments(sql: &str) -> String {
    strip_jinja_comments_with(sql, " ")
}

/// Strip Jinja comments from NON-SQL text (descriptions, docs-block bodies).
/// Jinja removes a `{# #}` comment entirely — no replacement space — so
/// `a{# c #}b` renders as `"ab"`. SQL keeps the space variant instead, so
/// tokens can never fuse across a comment (`from{# c #}t` stays `from t`).
fn strip_jinja_comments_text(text: &str) -> String {
    strip_jinja_comments_with(text, "")
}

fn strip_jinja_comments_with(text: &str, replacement: &str) -> String {
    static JINJA: OnceLock<Regex> = OnceLock::new();
    let jinja = JINJA.get_or_init(|| Regex::new(r"(?s)\{#.*?#\}").unwrap());
    jinja.replace_all(text, replacement).into_owned()
}

/// A resolved directory-hierarchical config from a `dbt_project.yml` resource
/// tree (`models:` / `seeds:` / `snapshots:`).
#[derive(Debug, Clone, Default)]
struct TreeConfig {
    materialized: Option<String>,
    enabled: Option<bool>,
    /// The `+meta` dict merged down the tree: deeper levels override per
    /// TOP-LEVEL key (a nested dict is replaced wholesale, not deep-merged —
    /// verified against real dbt).
    meta: serde_json::Map<String, serde_json::Value>,
}

/// The config-tree lookup path for a node: the project name, then each
/// directory of its resource-root-relative `path`, then the file stem (so a
/// file-level `models: p: marts: my_model: +materialized:` resolves too).
fn tree_segments(proj: &str, rel: &str) -> Vec<String> {
    let mut segs = vec![proj.to_string()];
    let mut parts: Vec<&str> = rel.split('/').collect();
    let file = parts.pop().unwrap_or("");
    segs.extend(parts.iter().map(|s| s.to_string()));
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    if !stem.is_empty() {
        segs.push(stem.to_string());
    }
    segs
}

/// Walk a resource tree along `segments`, collecting `+materialized` /
/// `+enabled` / `+meta` at every level — a deeper (more specific) value
/// overrides a shallower one, dbt's most-specific-path rule. Descent stops at
/// the first missing segment (configs collected above it still apply). Only
/// `+`-prefixed keys are configs; bare keys are path segments. `+meta` maps
/// merge by top-level key (a deeper key replaces the whole value) through the
/// same [`meta_map`] conversion schema.yml uses — non-string keys stringify
/// the same way and a non-mapping `+meta` warns once per tree level.
/// `tree_path` names the walked path (`models`, `models.sample.staging`, …)
/// so the warning says which level misdeclared it.
fn resolve_tree_config(
    tree: &serde_json::Value,
    segments: &[String],
    tree_path: String,
    warnings: &mut Vec<String>,
) -> TreeConfig {
    let mut out = TreeConfig::default();
    if !tree.is_object() {
        return out; // missing level (indexing Null yields Null) — nothing deeper
    }
    if let Some(m) = tree["+materialized"].as_str() {
        out.materialized = Some(m.to_string());
    }
    if let Some(e) = tree["+enabled"].as_bool() {
        out.enabled = Some(e);
    }
    out.meta.extend(meta_map(
        &tree["+meta"],
        warnings,
        &format!("dbt_project.yml '+meta' at '{tree_path}'"),
    ));
    if let Some((seg, rest)) = segments.split_first() {
        let deeper = resolve_tree_config(
            &tree[seg.as_str()],
            rest,
            format!("{tree_path}.{seg}"),
            warnings,
        );
        out.materialized = deeper.materialized.or(out.materialized);
        out.enabled = deeper.enabled.or(out.enabled);
        out.meta.extend(deeper.meta); // deeper keys replace same-named ones
    }
    out
}

// ---- YAML → serde_json::Value, with REAL dbt's scalar semantics ----
//
// Every dbt YAML file (dbt_project.yml, schema.yml) is loaded by PyYAML,
// whose YAML-1.1 implicit resolvers make a PLAIN `yes`/`no`/`on`/`off` a
// bool, `017` octal (15), `1:30` sexagesimal (90), `1_000` a thousand —
// while a QUOTED spelling stays a string and `0o17`/`08`/`y`/`n`/`1e5` are
// strings either way (all verified against real dbt 1.12.6). The serde_yaml
// family of `Value` types (serde_norway included) erases the plain-vs-quoted
// scalar style before callers can see it, so the parse below works off
// yaml-rust2's event stream — `Event::Scalar` carries `TScalarStyle` — and
// applies PyYAML's resolution rules itself. Anchors, aliases, `<<` merge
// keys, explicit `!!` tags, and duplicate keys (last wins) all resolve like
// PyYAML. Known residual gaps: YAML timestamps keep their text (a `date`
// meta value is vanishingly rare) and non-finite floats become `null` (JSON
// has no representation).

/// The `(event, source mark)` pairs of one YAML stream — collected so the
/// tree build can recurse freely (marks ride along for error messages).
struct EventSink(Vec<(Event, Marker)>);
impl MarkedEventReceiver for EventSink {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        self.0.push((ev, mark));
    }
}

/// Parse a single-document YAML stream into a [`serde_json::Value`] (the wire
/// type `RawNode.meta` / `RawSource.meta` / tree fields already carry).
/// Malformed YAML and multi-document streams are errors — like PyYAML's
/// `safe_load`, which dbt's own files must satisfy.
fn yaml_to_json(text: &str, warnings: &mut Vec<String>) -> Result<serde_json::Value, String> {
    let mut sink = EventSink(Vec::new());
    yaml_rust2::parser::Parser::new(text.chars())
        .load(&mut sink, false)
        .map_err(|e| e.to_string())?;
    let mut b = YamlBuilder {
        evs: &sink.0,
        pos: 0,
        anchors: HashMap::new(),
        warnings,
    };
    // Walk to the first content event of the first document.
    let doc = loop {
        match b.evs.get(b.pos).map(|(e, _)| e) {
            None | Some(Event::StreamEnd) => return Ok(serde_json::Value::Null),
            Some(Event::StreamStart | Event::DocumentStart | Event::Nothing) => b.pos += 1,
            Some(Event::DocumentEnd) => return Ok(serde_json::Value::Null),
            Some(_) => break b.node(),
        }
    };
    // Anything past DocumentEnd is a second document — a hard error, like
    // `yaml.safe_load` ("expected a single document").
    for (ev, _) in &b.evs[b.pos..] {
        if matches!(ev, Event::DocumentStart) {
            return Err("expected a single document in the stream".to_string());
        }
    }
    Ok(doc)
}

/// Fold the collected event stream into [`serde_json::Value`]s. Anchored
/// nodes register by anchor id as they complete (a self-referencing alias —
/// which PyYAML resolves recursively — lands `null` + warning instead).
struct YamlBuilder<'a> {
    evs: &'a [(Event, Marker)],
    pos: usize,
    anchors: HashMap<usize, serde_json::Value>,
    warnings: &'a mut Vec<String>,
}

impl YamlBuilder<'_> {
    fn node(&mut self) -> serde_json::Value {
        let (ev, _) = &self.evs[self.pos];
        match ev {
            Event::Scalar(v, style, aid, tag) => {
                self.pos += 1;
                let out = yaml_scalar_to_json(v, *style, tag.as_ref());
                self.anchor(*aid, &out);
                out
            }
            Event::SequenceStart(aid, _tag) => {
                self.pos += 1;
                let mut seq = Vec::new();
                while !matches!(
                    self.evs.get(self.pos).map(|(e, _)| e),
                    None | Some(Event::SequenceEnd)
                ) {
                    seq.push(self.node());
                }
                self.pos += 1;
                let out: serde_json::Value = seq.into();
                self.anchor(*aid, &out);
                out
            }
            Event::MappingStart(aid, _tag) => {
                self.pos += 1;
                let mut map = serde_json::Map::new();
                while !matches!(
                    self.evs.get(self.pos).map(|(e, _)| e),
                    None | Some(Event::MappingEnd)
                ) {
                    // `<<` merge key: entries merge UNDER the explicit keys
                    // (earlier merge sources win over later ones — the YAML
                    // merge spec PyYAML implements).
                    let is_merge = matches!(
                        &self.evs.get(self.pos).map(|(e, _)| e),
                        Some(Event::Scalar(k, TScalarStyle::Plain, _, tag))
                            if k == "<<" && tag.is_none()
                    );
                    if is_merge {
                        self.pos += 1;
                        let v = self.node();
                        merge_into(&mut map, v, self.warnings);
                        continue;
                    }
                    let key = match self.node() {
                        serde_json::Value::String(s) => s,
                        // A `yes:`/`5:`/`~:` key resolves like the value —
                        // PyYAML's dict key serializes as "true"/"5"/"null".
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Null => "null".to_string(),
                        other => other.to_string(), // seq/map key: compact JSON
                    };
                    let v = self.node();
                    if map.insert(key.clone(), v).is_some() {
                        self.warnings.push(format!(
                            "duplicate mapping key '{key}' — the last value wins"
                        ));
                    }
                }
                self.pos += 1;
                let out: serde_json::Value = map.into();
                self.anchor(*aid, &out);
                out
            }
            Event::Alias(id) => {
                self.pos += 1;
                match self.anchors.get(id) {
                    Some(v) => v.clone(),
                    None => {
                        self.warnings
                            .push("dangling YAML alias — resolved as null".to_string());
                        serde_json::Value::Null
                    }
                }
            }
            // DocumentEnd/StreamEnd/Nothing mid-stream: treat as absent.
            _ => {
                self.pos += 1;
                serde_json::Value::Null
            }
        }
    }

    fn anchor(&mut self, aid: usize, v: &serde_json::Value) {
        if aid > 0 {
            self.anchors.insert(aid, v.clone());
        }
    }
}

/// Merge a `<<` key's value into `map`: a mapping contributes its entries
/// (existing — explicit — keys always win); a sequence merges each mapping
/// member in order, earlier sources winning (the YAML merge spec).
fn merge_into(
    map: &mut serde_json::Map<String, serde_json::Value>,
    v: serde_json::Value,
    warnings: &mut Vec<String>,
) {
    let merge_map = |map: &mut serde_json::Map<String, serde_json::Value>,
                     src: serde_json::Map<String, serde_json::Value>| {
        for (k, val) in src {
            map.entry(k).or_insert(val);
        }
    };
    match v {
        serde_json::Value::Object(src) => merge_map(map, src),
        serde_json::Value::Array(srcs) => {
            for src in srcs {
                match src {
                    serde_json::Value::Object(src) => merge_map(map, src),
                    _ => warnings
                        .push("ignoring a non-mapping entry in a `<<` merge sequence".to_string()),
                }
            }
        }
        serde_json::Value::Null => {} // `<<:` with no value — nothing to merge
        _ => warnings.push("ignoring a non-mapping `<<` merge value".to_string()),
    }
}

/// The [`serde_json::Value`] one YAML scalar holds under PyYAML. `style` is
/// the detail serde `Value`s erased: only a PLAIN scalar resolves by content
/// (a quoted `'yes'`/`"017"` or block literal is always a string). Explicit
/// `!!`-tags use PyYAML's tagged constructors; unknown local tags (`!foo`)
/// unwrap to their content, the serde_yaml `!Tagged` behaviour.
fn yaml_scalar_to_json(v: &str, style: TScalarStyle, tag: Option<&Tag>) -> serde_json::Value {
    if let Some(t) = tag {
        if t.handle == "tag:yaml.org,2002:" {
            match t.suffix.as_str() {
                "str" => return v.into(),
                "null" => return serde_json::Value::Null,
                "bool" => return pyyaml_tagged_bool(v).map_or_else(|| v.into(), Into::into),
                "int" => return pyyaml_int(v).map_or_else(|| v.into(), Into::into),
                "float" => {
                    return pyyaml_float(v).map_or(serde_json::Value::Null, |f| {
                        serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into)
                    });
                }
                _ => {}
            }
        }
    }
    if style != TScalarStyle::Plain {
        return v.into();
    }
    match v {
        // PyYAML's null spellings, incl. the empty plain scalar (`key:`).
        "" | "~" | "null" | "Null" | "NULL" => return serde_json::Value::Null,
        _ => {}
    }
    if let Some(b) = yaml11_bool(v) {
        return b.into();
    }
    if let Some(i) = pyyaml_int(v) {
        return i.into();
    }
    if let Some(f) = pyyaml_float(v) {
        return serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into);
    }
    v.into()
}

/// The bool a PLAIN scalar holds under PyYAML's implicit YAML-1.1 resolver,
/// or `None` when it's a string. `y`/`n` are NOT bools even under PyYAML
/// (verified against real dbt: `y_key: y` lands in the manifest as the
/// string `"y"`); `true`/`false` spellings are here too so every bool lands
/// by one rule.
fn yaml11_bool(s: &str) -> Option<bool> {
    match s {
        "yes" | "Yes" | "YES" | "on" | "On" | "ON" | "true" | "True" | "TRUE" => Some(true),
        "no" | "No" | "NO" | "off" | "Off" | "OFF" | "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// The bool an EXPLICIT `!!bool`-tagged scalar holds under PyYAML — the
/// tagged constructor's `bool_values` table is broader than the implicit
/// resolver and DOES accept `y`/`n`.
fn pyyaml_tagged_bool(s: &str) -> Option<bool> {
    match s {
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Some(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Some(false)
        }
        _ => None,
    }
}

/// An integer under PyYAML's YAML-1.1 implicit resolver: binary (`0b101`),
/// octal (`017` → 15 — `0o17`/`08` stay strings!), decimal with `_`
/// separators (`1_000`), hex (`0x1F`), and sexagesimal (`1:30` → 90).
/// Overflow beyond `i64` degrades to "not an int" (a string) rather than a
/// wrong number.
fn pyyaml_int(v: &str) -> Option<i64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^([-+]?)(?:0b([0-1_]+)|0([0-7_]+)|(0|[1-9][0-9_]*)|0x([0-9a-fA-F_]+)|([1-9][0-9_]*(?::[0-5]?[0-9])+))$",
        )
        .unwrap()
    });
    let m = re.captures(v)?;
    let neg = m.get(1).is_some_and(|s| s.as_str() == "-");
    let val: i64 = if let Some(d) = m.get(2) {
        i64::from_str_radix(&d.as_str().replace('_', ""), 2).ok()?
    } else if let Some(d) = m.get(3) {
        i64::from_str_radix(&format!("0{}", d.as_str().replace('_', "")), 8).ok()?
    } else if let Some(d) = m.get(4) {
        d.as_str().replace('_', "").parse::<i64>().ok()?
    } else if let Some(d) = m.get(5) {
        i64::from_str_radix(&d.as_str().replace('_', ""), 16).ok()?
    } else {
        let d = m.get(6)?;
        let mut acc: i64 = 0;
        for part in d.as_str().replace('_', "").split(':') {
            acc = acc
                .checked_mul(60)?
                .checked_add(part.parse::<i64>().ok()?)?;
        }
        acc
    };
    Some(if neg { -val } else { val })
}

/// A float under PyYAML's YAML-1.1 implicit resolver: `1.5`, `5.`, `.5`,
/// `1.0e+5` (a sign-LESS exponent — `1e5`, `1.0e5` — stays a string!),
/// sexagesimal floats (`1:20.5` → 80.5), and `.inf`/`.nan` spellings.
fn pyyaml_float(v: &str) -> Option<f64> {
    match v {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Some(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?$|^\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?$|^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*$",
        )
        .unwrap()
    });
    if !re.is_match(v) {
        return None;
    }
    let v = v.replace('_', "");
    if v.contains(':') {
        // Sexagesimal float: fold int segments base-60, add the fraction.
        let (neg, v) = match v.strip_prefix('-') {
            Some(r) => (-1.0, r.to_string()),
            None => (1.0, v.strip_prefix('+').unwrap_or(&v).to_string()),
        };
        let mut acc = 0.0f64;
        for part in v.split(':') {
            acc = acc * 60.0 + part.parse::<f64>().ok()?;
        }
        Some(neg * acc)
    } else {
        v.parse::<f64>().ok()
    }
}

// ---- serde glue: the string coercions serde_norway used to apply ----
//
// The structs below deserialize via `serde_json::from_value`, which is
// STRICT where serde_norway coerced scalars (`description: 5` → `"5"`).
// The `de_*` helpers reproduce that coercion per field so a stray numeric
// description can't fail the whole file.

/// `5` → `"5"`, `true` → `"true"`, a string → itself; non-scalars → `None`.
fn scalar_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn de_string<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    scalar_to_string(&v).ok_or_else(|| serde::de::Error::custom("expected a string scalar"))
}

fn de_opt_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => scalar_to_string(&v)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected a string scalar")),
    }
}

fn de_string_vec<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<String>, D::Error> {
    // `tags:`/`depends_on:` left empty (`key:` → null) is an empty list, not
    // a type error.
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    v.iter()
        .map(scalar_to_string)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| serde::de::Error::custom("expected a list of string scalars"))
}

fn de_opt_string_vec<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?;
    v.map(|elems| {
        elems
            .iter()
            .map(scalar_to_string)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| serde::de::Error::custom("expected a list of string scalars"))
    })
    .transpose()
}

/// The dbt-syntax extractors, compiled once. The `regex` crate has no
/// backreferences, so each quote is matched independently (`['"]`) — a mismatched
/// `'x"` is vanishingly rare in real SQL and harmless if it slips through. The
/// leading `\b` stops `my_ref(...)` / `data_source(...)` identifiers producing
/// spurious edges. A versioned or kwarg-bearing `ref('x', v=2)` /
/// `ref('x', version=2)` IS matched — the trailing `(?:,[^)]*)?` absorbs any
/// extra args up to (but never past) the call's closing `)`, so the version
/// is ignored and the edge resolves to the unversioned name `x` (an
/// approximation; the `.vN` node a `versions:` block would define is not
/// synthesized).
struct Patterns {
    ref_re: Regex,
    source_re: Regex,
    snapshot_re: Regex,
    docs_block_re: Regex,
    docs_tag_re: Regex,
}

/// One `{% docs name %}` or `{% enddocs %}` tag, in file order — the input to
/// the pairing state machine in [`collect_doc_blocks`].
enum DocTag {
    Open(String),
    Close,
}

impl Patterns {
    fn new() -> Self {
        Patterns {
            ref_re: Regex::new(
                r#"\bref\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*['"]([^'"]+)['"]\s*)?(?:,[^)]*)?\)"#,
            )
            .unwrap(),
            source_re: Regex::new(
                r#"\bsource\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#,
            )
            .unwrap(),
            snapshot_re: Regex::new(r"\{%-?\s*snapshot\s+([A-Za-z_][A-Za-z0-9_]*)\s*-?%\}")
                .unwrap(),
            // `{% docs name %}…{% enddocs %}` in .md files: Jinja whitespace
            // control (`{%- -%}`) allowed; non-greedy body up to the first
            // `{% enddocs %}`. Names follow dbt's rule (alnum + underscore,
            // never digit-leading).
            docs_block_re: Regex::new(
                r"(?s)\{%-?\s*docs\s+([A-Za-z_][A-Za-z0-9_]*)\s*-?%\}(.*?)\{%-?\s*enddocs\s*-?%\}",
            )
            .unwrap(),
            // Any `{% docs name %}` or `{% enddocs %}` tag, in order — the
            // pairing state machine (an unclosed or stray tag is a dbt error,
            // degraded to a warning here) walks these.
            docs_tag_re: Regex::new(r"\{%-?\s*(?:docs\s+([A-Za-z_][A-Za-z0-9_]*)|enddocs)\s*-?%\}")
                .unwrap(),
        }
    }

    /// The referenced model names. `ref('pkg','x')` uses the 2nd arg (`x`);
    /// `ref('x')` uses the 1st.
    fn refs(&self, sql: &str) -> Vec<String> {
        self.ref_re
            .captures_iter(sql)
            .filter_map(|c| {
                c.get(2)
                    .or_else(|| c.get(1))
                    .map(|m| m.as_str().to_string())
            })
            .collect()
    }

    /// The `(source_name, table_name)` pairs.
    fn sources(&self, sql: &str) -> Vec<(String, String)> {
        self.source_re
            .captures_iter(sql)
            .filter_map(|c| {
                Some((
                    c.get(1)?.as_str().to_string(),
                    c.get(2)?.as_str().to_string(),
                ))
            })
            .collect()
    }

    /// The snapshot names declared by `{% snapshot X %}` blocks.
    fn snapshots(&self, sql: &str) -> Vec<String> {
        self.snapshot_re
            .captures_iter(sql)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect()
    }

    /// Every `{% docs name %}` / `{% enddocs %}` tag in order, so
    /// [`collect_doc_blocks`] can detect tags dbt would reject (an open with
    /// no matching close, or a stray close).
    fn doc_tags(&self, md: &str) -> Vec<DocTag> {
        self.docs_tag_re
            .captures_iter(md)
            .map(|c| match c.get(1) {
                Some(n) => DocTag::Open(n.as_str().to_string()),
                None => DocTag::Close,
            })
            .collect()
    }

    /// The `(name, body)` pairs declared by `{% docs name %}…{% enddocs %}`
    /// blocks. The body is trimmed, matching dbt's `.strip()` on
    /// `block_contents` (verified: interior whitespace/newlines survive).
    fn doc_blocks(&self, md: &str) -> Vec<(String, String)> {
        self.docs_block_re
            .captures_iter(md)
            .map(|c| {
                (
                    c.get(1).unwrap().as_str().to_string(),
                    c.get(2).unwrap().as_str().trim().to_string(),
                )
            })
            .collect()
    }

    /// The materialization from `config(materialized='…')`, if present. Both
    /// this and [`disabled`](Patterns::disabled) are scoped to `config(`
    /// calls, so a bare `materialized='x'` / `enabled=false` in SQL text, a
    /// `-- comment`, or a WHERE clause can never configure the node (dbt
    /// only honours them inside `config()`). dbt applies config() calls in
    /// order — the LAST call naming `materialized` wins (verified:
    /// `config(materialized='view')` then `config(materialized='table')`
    /// yields `table`) — so the last parseable string literal is taken; an
    /// unresolvable expression (`var()`) can't override an earlier literal.
    fn materialized(&self, sql: &str) -> Option<String> {
        config_kwarg_exprs(sql, "materialized")
            .into_iter()
            .filter_map(|expr| match lit_value(expr) {
                Some(serde_json::Value::String(s)) => Some(s),
                _ => None,
            })
            .next_back()
    }

    /// Whether the file's config() calls leave it disabled — the LAST
    /// `enabled=<bool literal>` decides, matching dbt's ordered config()
    /// application (verified: `config(enabled=false)` then
    /// `config(enabled=true)` leaves the node enabled). A non-bool
    /// `enabled=` expression doesn't change the outcome.
    fn disabled(&self, sql: &str) -> bool {
        config_kwarg_exprs(sql, "enabled")
            .into_iter()
            .filter_map(|expr| match lit_value(expr) {
                Some(serde_json::Value::Bool(b)) => Some(b),
                _ => None,
            })
            .next_back()
            == Some(false)
    }
}

/// The merged `meta` dict of EVERY `config(meta={…})` call in `sql`, in call
/// order: dbt applies `config()` calls in sequence and merges each call's
/// meta dict onto the accumulated config PER TOP-LEVEL KEY — a later call
/// replaces only the keys it names (verified: `{'a':1,'b':1}` then `{'b':2}`
/// yields `{'a':1,'b':2}`). A non-dict or unresolvable expression (`var()`,
/// `env_var()`, an identifier) is skipped rather than guessed — an
/// unresolvable LATER call can't clear what an earlier literal set, so the
/// result is a subset of dbt's, never a fabrication.
fn config_meta(sql: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    let mut found = false;
    for expr in config_kwarg_exprs(sql, "meta") {
        if let Some(serde_json::Value::Object(m)) = lit_value(expr) {
            out.extend(m);
            found = true;
        }
    }
    found.then_some(out)
}

/// The argument text of EVERY `config(…)` call in `sql`, in order — each
/// string is the text between that call's parens, found by a balanced-
/// paren/quote scan so nested `(`/`)` inside dicts or macro calls can't end
/// the call early (the `[^)]*` approach can't). dbt applies config() calls
/// in order — each call's kwargs override the earlier ones per key
/// (verified: `config(materialized='v')` then `config(materialized='t')`
/// yields `t`), so kwarg readers take the LAST resolvable value.
fn config_call_args_list(sql: &str) -> Vec<&str> {
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    let re = CONFIG.get_or_init(|| Regex::new(r"\bconfig\s*\(").unwrap());
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(m) = re.find_at(sql, pos) {
        match scan_call_args(sql, m.end()) {
            Some((args, end)) => {
                out.push(args);
                pos = end;
            }
            // An unclosed call swallows the rest of the file — no more
            // config() calls can follow it.
            None => break,
        }
    }
    out
}

/// Scan the call whose `(` ends at `start`: returns the text between the
/// parens and the position just past the closing `)`. A bare `]`/`}` at
/// depth 0 or an unclosed call is malformed → `None`.
fn scan_call_args(sql: &str, start: usize) -> Option<(&str, usize)> {
    let mut depth = 0usize;
    let mut chars = sql[start..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    return (c == ')').then(|| (&sql[start..start + i], start + i + 1));
                }
                depth -= 1;
            }
            '\'' | '"' => {
                let it = chars.by_ref();
                while let Some((_, d)) = it.next() {
                    if d == '\\' {
                        it.next(); // consume the escaped char
                    } else if d == c {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    None // unclosed call
}

/// The expression bound to kwarg `name` in EVERY `config(…)` call in `sql`,
/// in call order — the input to last-wins scalar kwargs (`materialized`,
/// `enabled`) and per-key-merged dict kwargs (`meta`).
fn config_kwarg_exprs<'a>(sql: &'a str, name: &str) -> Vec<&'a str> {
    config_call_args_list(sql)
        .into_iter()
        .filter_map(|args| kwarg(args, name))
        .collect()
}

/// Parse `expr` as a complete literal: the whole text must be a single
/// [`LitParser`] value — trailing tokens mean it wasn't a clean literal
/// (`'x' or 'y'`-style expressions dbt evaluates but source mode can't).
fn lit_value(expr: &str) -> Option<serde_json::Value> {
    let mut p = LitParser { s: expr, i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return None;
    }
    Some(v)
}

/// Split `s` at commas sitting at bracket-depth 0 and outside string quotes —
/// the top-level kwargs of a `config(…)` call (a comma inside `{'a': 'b'}`,
/// `[x, y]`, or a quoted string must not split the argument list).
fn top_level_segments(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut last = 0;
    let mut depth = 0usize;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '\'' | '"' => {
                let it = chars.by_ref();
                while let Some((_, d)) = it.next() {
                    if d == '\\' {
                        it.next();
                    } else if d == c {
                        break;
                    }
                }
            }
            ',' if depth == 0 => {
                out.push(&s[last..i]);
                last = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[last..]);
    out
}

/// The expression text bound to keyword `name` inside a `config(…)` call's
/// arguments (`meta={…}` → `{…}`). `name` must match exactly — a
/// `metadata=` kwarg is not `meta`. The name/expr separator is the first
/// `=` at bracket-depth 0 outside quotes.
fn kwarg<'a>(args: &'a str, name: &str) -> Option<&'a str> {
    for seg in top_level_segments(args) {
        let seg = seg.trim();
        let mut depth = 0usize;
        let mut chars = seg.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                '\'' | '"' => {
                    let it = chars.by_ref();
                    while let Some((_, d)) = it.next() {
                        if d == '\\' {
                            it.next();
                        } else if d == c {
                            break;
                        }
                    }
                }
                '=' if depth == 0 => {
                    if seg[..i].trim() == name {
                        return Some(seg[i + 1..].trim_start());
                    }
                    break; // a different kwarg — its expr can't contain `meta=`
                }
                _ => {}
            }
        }
    }
    None
}

/// Recursive-descent parser for the Python/Jinja literals a `config()` kwarg
/// holds: `{'k': v}` dicts, `[…]`/`(…)` sequences, quoted strings, `True` /
/// `False` / `None`, and numbers. Anything else — `var('x')`, an identifier,
/// a Jinja expression — is unresolvable from source and returns `None`.
struct LitParser<'a> {
    s: &'a str,
    i: usize,
}

impl LitParser<'_> {
    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }
    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }
    fn value(&mut self) -> Option<serde_json::Value> {
        self.ws();
        match self.peek()? {
            '{' => self.dict(),
            '[' | '(' => self.list(),
            '\'' | '"' => self.string().map(serde_json::Value::from),
            'T' | 'F' | 'N' | 't' | 'f' | 'n' => self.keyword(),
            _ => self.number(),
        }
    }
    /// `{'k': v, …}` — keys may be any literal; non-string keys stringify
    /// to their compact-JSON form (the same rendering a non-string YAML
    /// mapping key gets in [`YamlBuilder`]).
    fn dict(&mut self) -> Option<serde_json::Value> {
        self.bump();
        let mut obj = serde_json::Map::new();
        loop {
            self.ws();
            if self.peek() == Some('}') {
                self.bump();
                return Some(obj.into());
            }
            let key = match self.value()? {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            self.ws();
            if self.bump() != Some(':') {
                return None;
            }
            obj.insert(key, self.value()?);
            self.ws();
            match self.bump()? {
                ',' => continue,
                '}' => return Some(obj.into()),
                _ => return None,
            }
        }
    }
    /// `[a, b]` / `(a, b)` — a tuple maps to a JSON array (JSON has no tuple).
    fn list(&mut self) -> Option<serde_json::Value> {
        let close = if self.bump()? == '[' { ']' } else { ')' };
        let mut items = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(close) {
                self.bump();
                return Some(items.into());
            }
            items.push(self.value()?);
            self.ws();
            match self.bump()? {
                ',' => continue,
                c if c == close => return Some(items.into()),
                _ => return None,
            }
        }
    }
    fn string(&mut self) -> Option<String> {
        let quote = self.bump()?;
        let mut out = String::new();
        loop {
            match self.bump()? {
                '\\' => out.push(match self.bump()? {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other, // \' \" \\ and any other escaped char
                }),
                c if c == quote => return Some(out),
                c => out.push(c),
            }
        }
    }
    /// `True` / `False` / `None`, delimited — a `Truex` identifier is not
    /// the literal (and can't be resolved anyway). Jinja also accepts the
    /// lowercase spellings (`true`/`false`/`none` are its canonical names).
    fn keyword(&mut self) -> Option<serde_json::Value> {
        for (tok, v) in [
            ("True", serde_json::Value::from(true)),
            ("true", serde_json::Value::from(true)),
            ("False", serde_json::Value::from(false)),
            ("false", serde_json::Value::from(false)),
            ("None", serde_json::Value::Null),
            ("none", serde_json::Value::Null),
            ("null", serde_json::Value::Null),
        ] {
            if self.s[self.i..].starts_with(tok) {
                let end = self.i + tok.len();
                if self.s[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    return None;
                }
                self.i = end;
                return Some(v);
            }
        }
        None
    }
    fn number(&mut self) -> Option<serde_json::Value> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
        {
            self.bump();
        }
        let tok = &self.s[start..self.i];
        if let Ok(i) = tok.parse::<i64>() {
            Some(i.into())
        } else if let Ok(u) = tok.parse::<u64>() {
            Some(u.into())
        } else {
            tok.parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Into::into)
        }
    }
}

// ---- dbt_project.yml / schema.yml deserialization (only the fields we use) ----

#[derive(Debug, Deserialize, Default)]
struct DbtProject {
    #[serde(deserialize_with = "de_string")]
    name: String,
    #[serde(
        rename = "model-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    model_paths: Option<Vec<String>>,
    #[serde(rename = "seed-paths", default, deserialize_with = "de_opt_string_vec")]
    seed_paths: Option<Vec<String>>,
    #[serde(
        rename = "snapshot-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    snapshot_paths: Option<Vec<String>>,
    #[serde(
        rename = "source-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    source_paths: Option<Vec<String>>,
    /// `docs-paths` scopes the `.md` docs-block scan; unset -> every resource
    /// path. The next three are read ONLY for that default union (verified:
    /// a block in `macros/` resolves with no `docs-paths` set).
    #[serde(rename = "docs-paths", default, deserialize_with = "de_opt_string_vec")]
    docs_paths: Option<Vec<String>>,
    #[serde(
        rename = "macro-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    macro_paths: Option<Vec<String>>,
    #[serde(
        rename = "analysis-paths",
        default,
        deserialize_with = "de_opt_string_vec"
    )]
    analysis_paths: Option<Vec<String>>,
    #[serde(rename = "test-paths", default, deserialize_with = "de_opt_string_vec")]
    test_paths: Option<Vec<String>>,
    /// The directory-hierarchical config trees (`+materialized` / `+enabled`
    /// under nested path keys). Kept as raw YAML and walked by
    /// [`resolve_tree_config`]; `Value::default()` is `Null`, which resolves to
    /// no config.
    #[serde(default)]
    models: serde_json::Value,
    #[serde(default)]
    seeds: serde_json::Value,
    #[serde(default)]
    snapshots: serde_json::Value,
    /// The `sources:` config tree — keyed by the source's fqn (`<pkg>`:
    /// `<declaring-yml dirs>`: `<source>`: `<table>` — verified against real
    /// dbt's recorded fqn). Only `+meta` is read (other `+`-configs like
    /// `+enabled`/`+event_time` are out of scope).
    #[serde(default)]
    sources: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaFile {
    #[serde(default)]
    models: Vec<SchemaModel>,
    #[serde(default)]
    sources: Vec<SchemaSource>,
    #[serde(default)]
    seeds: Vec<SchemaModel>,
    #[serde(default)]
    snapshots: Vec<SchemaModel>,
    #[serde(default)]
    exposures: Vec<SchemaExposure>,
}

/// Shared shape for `models:` / `seeds:` / `snapshots:` entries (only what we read).
///
/// `tests` and `data_tests` are kept as SEPARATE fields (not one aliased
/// field) so a file declaring both keys on the same entry still deserializes
/// — serde treats an `alias` as just another name for the SAME field, so two
/// keys mapping to it is a duplicate-field error that silently drops the
/// whole file; the two lists are unioned by the caller instead.
#[derive(Debug, Deserialize, Default)]
struct SchemaModel {
    #[serde(deserialize_with = "de_string")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    description: Option<String>,
    #[serde(default)]
    config: SchemaConfig,
    #[serde(default, deserialize_with = "de_string_vec")]
    tags: Vec<String>,
    #[serde(default)]
    columns: Vec<SchemaColumn>,
    #[serde(default)]
    tests: Vec<serde_json::Value>,
    #[serde(default)]
    data_tests: Vec<serde_json::Value>,
    /// The `relation:` Jinja expression of a YAML-defined snapshot (dbt
    /// 1.9+), e.g. `ref('stg_orders')`. Always `None` for models/seeds and
    /// for a `{% snapshot %}`-block snapshot (its relation is implicit).
    #[serde(default, deserialize_with = "de_opt_string")]
    relation: Option<String>,
    /// Top-level `meta:` dict — dbt treats it as equivalent to
    /// `config.meta` (both together are a dbt parse error; here it merges
    /// with `config.meta` winning per key). Kept as raw `Value` so a
    /// non-mapping `meta:` can't fail the whole file's deserialization.
    #[serde(default)]
    meta: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaConfig {
    #[serde(default, deserialize_with = "de_opt_string")]
    materialized: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    /// `config: meta:` — merges over a top-level `meta:` per key.
    #[serde(default)]
    meta: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaColumn {
    #[serde(deserialize_with = "de_string")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    data_type: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    description: Option<String>,
    #[serde(default)]
    tests: Vec<serde_json::Value>,
    #[serde(default)]
    data_tests: Vec<serde_json::Value>,
}

/// An `exposures:` entry (only what we surface). `depends_on` holds Jinja
/// strings (`ref('x')` / `source('a','b')`) per the dbt spec.
#[derive(Debug, Deserialize, Default)]
struct SchemaExposure {
    #[serde(deserialize_with = "de_string")]
    name: String,
    #[serde(rename = "type", default, deserialize_with = "de_opt_string")]
    exposure_type: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    description: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    url: Option<String>,
    #[serde(default)]
    owner: SchemaExposureOwner,
    #[serde(default)]
    config: SchemaConfig,
    #[serde(default, deserialize_with = "de_string_vec")]
    depends_on: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaExposureOwner {
    #[serde(default, deserialize_with = "de_opt_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    email: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaSource {
    #[serde(deserialize_with = "de_string")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    database: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string")]
    schema: Option<String>,
    /// `meta:` at the source level — cascades to every table (a table-level
    /// `meta:` overrides it per key, verified against real dbt).
    #[serde(default)]
    meta: serde_json::Value,
    #[serde(default)]
    tables: Vec<SchemaSourceTable>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaSourceTable {
    #[serde(deserialize_with = "de_string")]
    name: String,
    #[serde(default, deserialize_with = "de_opt_string")]
    description: Option<String>,
    /// `meta:` at the table level — overrides the source's `meta:` per key.
    #[serde(default)]
    meta: serde_json::Value,
    #[serde(default)]
    columns: Vec<SchemaColumn>,
    #[serde(default)]
    tests: Vec<serde_json::Value>,
    #[serde(default)]
    data_tests: Vec<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_extractors_scope_to_config_calls_in_any_arg_order() {
        let pat = Patterns::new();
        // The key matches even AFTER a prior argument inside the same call.
        assert_eq!(
            pat.materialized("{{ config(enabled=true, materialized='table') }}"),
            Some("table".to_string())
        );
        assert!(pat.disabled("{{ config(materialized='view', enabled=false) }}"));
        assert!(pat.disabled("{{ config(enabled=False) }}"));
        // Outside a config() call the keys are inert (dbt only honours them
        // inside config()).
        assert_eq!(pat.materialized("-- materialized='ephemeral'"), None);
        assert!(!pat.disabled("where enabled = false"));
        assert!(!pat.disabled("-- note: enabled=false someday"));
        assert!(!pat.disabled("{% set enabled=false %}"));
    }

    #[test]
    fn doc_blocks_extract_names_and_trim_bodies() {
        let pat = Patterns::new();
        // Multiline, inline, and whitespace-controlled (`{%- -%}`) blocks;
        // bodies are trimmed but interior whitespace survives (dbt's
        // `block_contents.strip()` — verified against real dbt).
        let md = "preamble\n\
                  {% docs a %}\n  Body A.\n  still A\n{% enddocs %}\n\
                  mid\n\
                  {% docs b %}Body B.{% enddocs %}\n\
                  {%- docs c -%}\n Body C. \n{%- enddocs -%}\n";
        assert_eq!(
            pat.doc_blocks(md),
            vec![
                ("a".to_string(), "Body A.\n  still A".to_string()),
                ("b".to_string(), "Body B.".to_string()),
                ("c".to_string(), "Body C.".to_string()),
            ]
        );
        // A `{% docs %}` tag inside `{# #}` is invisible to dbt — extraction
        // runs on the Jinja-comment-stripped text, so it finds nothing.
        let hidden = strip_jinja_comments_text("{# {% docs h %}x{% enddocs %} #}");
        assert!(pat.doc_blocks(&hidden).is_empty());
        // A `{# #}` inside a body disappears entirely (text variant, like
        // dbt's Jinja render of the block contents).
        let body = strip_jinja_comments_text("{% docs j %}a{# c #}b{% enddocs %}");
        assert_eq!(
            pat.doc_blocks(&body),
            vec![("j".to_string(), "ab".to_string())]
        );
        // A digit-leading name doesn't match dbt's block-name rule.
        assert!(pat.doc_blocks("{% docs 9bad %}x{% enddocs %}").is_empty());
    }

    #[test]
    fn render_doc_refs_substitutes_and_warns() {
        let docs = DocBlocks::new(
            "p".to_string(),
            BTreeMap::from([
                ("a".to_string(), "Body A.".to_string()),
                ("b".to_string(), "Body B.".to_string()),
            ]),
        );
        let mut w = Vec::new();
        // Pure ref, quote/space/whitespace-control variants, mixed text.
        assert_eq!(docs.render("{{ doc('a') }}", &mut w), "Body A.");
        assert_eq!(docs.render("{{doc(\"a\")}}", &mut w), "Body A.");
        assert_eq!(docs.render("{{- doc( 'a' ) -}}", &mut w), "Body A.");
        assert_eq!(
            docs.render("pre {{ doc('a') }} post", &mut w),
            "pre Body A. post"
        );
        // 2-arg with the project itself resolves locally (verified vs dbt).
        assert_eq!(docs.render("{{ doc('p', 'b') }}", &mut w), "Body B.");
        assert!(w.is_empty(), "resolved refs warn nothing: {w:?}");

        // Missing block: raw `{{ doc() }}` text is kept (dbt would error —
        // dbtl degrades), with ONE dedup'd warning per ref.
        assert_eq!(
            docs.render("pre {{ doc('ghost') }} post", &mut w),
            "pre {{ doc('ghost') }} post"
        );
        assert_eq!(
            docs.render("{{ doc('ghost') }}", &mut w),
            "{{ doc('ghost') }}"
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("ghost"), "{w:?}");

        // A foreign package never resolves — even when the name exists
        // locally — and the warning says why (package scope, not a missing
        // block).
        w.clear();
        assert_eq!(
            docs.render("{{ doc('other', 'a') }}", &mut w),
            "{{ doc('other', 'a') }}"
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("not this project"), "{w:?}");

        // Text without doc() refs passes through untouched.
        assert_eq!(docs.render("plain text", &mut w), "plain text");
        assert_eq!(docs.render("a doc( b", &mut w), "a doc( b");
    }

    #[test]
    fn render_doc_refs_strips_jinja_comments_like_dbt() {
        let docs = DocBlocks::new(
            "p".to_string(),
            BTreeMap::from([("a".to_string(), "Body A.".to_string())]),
        );
        let mut w = Vec::new();
        // dbt renders a description as Jinja first: `{# #}` comments (and
        // the doc() inside them) disappear ENTIRELY — no warning, no space.
        assert_eq!(docs.render("{# {{ doc('a') }} #} note", &mut w), " note");
        assert_eq!(docs.render("a{# c #}b", &mut w), "ab");
        // A doc() ref hiding in a comment is never evaluated — an undefined
        // one warns NOTHING (dbt wouldn't error either).
        assert_eq!(docs.render("{# {{ doc('ghost') }} #}x", &mut w), "x");
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn render_doc_refs_honours_whitespace_control() {
        let docs = DocBlocks::new(
            "p".to_string(),
            BTreeMap::from([("a".to_string(), "Body A.".to_string())]),
        );
        let mut w = Vec::new();
        // `{{-`/` -}}` eat the whitespace adjacent to the tag (Jinja's
        // whitespace control — verified against real dbt).
        assert_eq!(docs.render("x {{- doc('a') -}} y", &mut w), "xBody A.y");
        assert_eq!(docs.render("x\n{{- doc('a') }}\ny", &mut w), "xBody A.\ny");
        assert_eq!(docs.render("x {{ doc('a') -}} y", &mut w), "x Body A.y");
        assert!(w.is_empty(), "{w:?}");
        // An UNRESOLVED ref keeps the whole raw match — adjacent whitespace
        // included — so the raw text round-trips unchanged.
        assert_eq!(
            docs.render("x {{- doc('ghost') -}} y", &mut w),
            "x {{- doc('ghost') -}} y"
        );
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn render_warns_on_uninterpretable_doc_calls() {
        let docs = DocBlocks::new("p".to_string(), BTreeMap::new());
        let mut w = Vec::new();
        // A doc() call the strict pattern can't read (non-literal arg, bad
        // arg count) is a parse error for real dbt — degraded to a warning,
        // raw text kept, load unaffected.
        assert_eq!(
            docs.render("{{ doc(var('x')) }}", &mut w),
            "{{ doc(var('x')) }}"
        );
        assert_eq!(
            docs.render("{{ doc('a', 'b', 'c') }}", &mut w),
            "{{ doc('a', 'b', 'c') }}"
        );
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().all(|s| s.contains("uninterpretable")), "{w:?}");
        // A non-doc `{{ ... }}` expression is not our business — no warning.
        assert_eq!(docs.render("{{ 1 + 1 }}", &mut w), "{{ 1 + 1 }}");
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn config_meta_parses_literal_dicts() {
        let m = config_meta(
            "{{ config(materialized='table', meta={'owner': 'alice', 'deep': {'x': 1}}) }}",
        )
        .expect("a meta dict parses");
        assert_eq!(m["owner"], serde_json::json!("alice"));
        assert_eq!(m["deep"], serde_json::json!({"x": 1}));
        // Numbers/bools/lists/tuples and double quotes all convert.
        let m = config_meta(
            r#"{{ config(meta={'n': 2, 'f': 1.5, 'b': True, 'l': [1, "x"], 't': (3,)}) }}"#,
        )
        .unwrap();
        assert_eq!(m["n"], serde_json::json!(2));
        assert_eq!(m["f"], serde_json::json!(1.5));
        assert_eq!(m["b"], serde_json::json!(true));
        assert_eq!(m["l"], serde_json::json!([1, "x"]));
        assert_eq!(m["t"], serde_json::json!([3]));
        // Jinja's lowercase literal spellings convert the same way.
        let m = config_meta(r#"{{ config(meta={'b': true, 'x': none}) }}"#).unwrap();
        assert_eq!(m["b"], serde_json::json!(true));
        assert_eq!(m["x"], serde_json::Value::Null);
        // `metadata=` is not `meta`; a `meta` inside comments/strings elsewhere
        // can't configure the node; unresolvable exprs drop rather than guess.
        assert!(config_meta("{{ config(metadata={'a': 1}) }}").is_none());
        assert!(config_meta("{{ config(meta=var('m')) }}").is_none());
        assert!(config_meta("-- no config here").is_none());
        assert!(config_meta("{{ config(meta={'a': 1} if x else {}) }}").is_none());
        assert!(config_meta("{{ config(meta='not-a-dict') }}").is_none());
    }

    #[test]
    fn config_meta_merges_every_config_call_per_key() {
        // dbt applies config() calls in order and merges each meta dict per
        // top-level key — a later call replaces only the keys it names
        // (verified: {'a':1,'b':1} then {'b':2} → {'a':1,'b':2}).
        let m = config_meta(
            "{{ config(meta={'a': 1, 'b': 'first'}) }}\n\
             select 1\n\
             {{ config(meta={'b': 'second', 'c': 3}) }}",
        )
        .expect("both dicts merge");
        assert_eq!(m["a"], serde_json::json!(1));
        assert_eq!(m["b"], serde_json::json!("second"));
        assert_eq!(m["c"], serde_json::json!(3));
        // A NESTED dict is replaced wholesale, not deep-merged (dbt's shallow
        // per-key rule applies across calls the same as across intakes).
        let m = config_meta(
            "{{ config(meta={'d': {'x': 1}, 'keep': 1}) }}\n\
             {{ config(meta={'d': {'y': 2}}) }}",
        )
        .unwrap();
        assert_eq!(m["d"], serde_json::json!({"y": 2}));
        assert_eq!(m["keep"], serde_json::json!(1));
        // An unresolvable later kwarg can't clear an earlier literal — the
        // merge keeps what it could resolve (a subset of dbt's result).
        let m = config_meta("{{ config(meta={'a': 1}) }}\n{{ config(meta=var('x')) }}").unwrap();
        assert_eq!(m["a"], serde_json::json!(1));
    }

    #[test]
    fn last_config_call_wins_for_scalar_kwargs() {
        let pat = Patterns::new();
        // dbt applies config() calls in order, each call's kwargs overriding
        // the earlier ones per key (verified):
        //   config(materialized='view')  then config(materialized='table')
        //   → materialized = 'table'
        //   config(enabled=false)        then config(enabled=true)
        //   → node stays ENABLED
        assert_eq!(
            pat.materialized(
                "{{ config(materialized='view') }}\n{{ config(materialized='table') }}",
            ),
            Some("table".to_string())
        );
        assert!(!pat.disabled("{{ config(enabled=false) }}\n{{ config(enabled=true) }}"));
        assert!(pat.disabled("{{ config(enabled=true) }}\n{{ config(enabled=false) }}"));
        // An unresolvable later value can't override an earlier literal one.
        assert_eq!(
            pat.materialized(
                "{{ config(materialized='view') }}\n{{ config(materialized=var('m')) }}"
            ),
            Some("view".to_string())
        );
    }

    #[test]
    fn ref_regex_handles_versioned_and_extra_args() {
        let pat = Patterns::new();
        assert_eq!(pat.refs("{{ ref('m', v=2) }}"), vec!["m".to_string()]);
        assert_eq!(pat.refs("{{ ref('m', version=2) }}"), vec!["m".to_string()]);
        assert_eq!(
            pat.refs("{{ ref('pkg', 'm', v=2) }}"),
            vec!["m".to_string()]
        );
        // No cross-call over-matching: `[^)]*` must not cross the first `)`.
        assert_eq!(
            pat.refs("{{ ref('a') }} x {{ ref('b') }}"),
            vec!["a".to_string(), "b".to_string()]
        );
        // A non-literal arg (a macro call, not a quoted string) still drops,
        // like before this change.
        assert!(pat.refs("{{ ref(var('x')) }}").is_empty());
    }

    #[test]
    fn yaml_scalars_resolve_like_pyyaml() {
        // REAL dbt loads every .yml with PyYAML (YAML 1.1): a PLAIN
        // `yes`/`no`/`on`/`off` scalar (any casing) is a bool, `017` is
        // octal, `1:30` sexagesimal — while `y`/`n`, `0o17`, `08`, `1e5`
        // (sign-less exponent!) stay strings.
        let yaml = |s: &str| {
            let mut w = Vec::new();
            yaml_to_json(s, &mut w).expect("yaml parses")
        };
        for s in [
            "yes", "Yes", "YES", "on", "On", "ON", "true", "True", "TRUE",
        ] {
            assert_eq!(yaml(s), serde_json::json!(true), "{s}");
        }
        for s in [
            "no", "No", "NO", "off", "Off", "OFF", "false", "False", "FALSE",
        ] {
            assert_eq!(yaml(s), serde_json::json!(false), "{s}");
        }
        for s in ["y", "n", "maybe", "0o17", "08", "1e5", "1.0e5"] {
            assert_eq!(yaml(s), serde_json::json!(s), "{s}");
        }
        // The PyYAML number forms the old 1.2-schema parser got WRONG.
        assert_eq!(yaml("017"), serde_json::json!(15), "octal");
        assert_eq!(yaml("1:30"), serde_json::json!(90), "sexagesimal");
        assert_eq!(yaml("0x1F"), serde_json::json!(31));
        assert_eq!(yaml("0b101"), serde_json::json!(5));
        assert_eq!(yaml("1_000"), serde_json::json!(1000));
        assert_eq!(yaml(".5"), serde_json::json!(0.5));
        assert_eq!(yaml("5."), serde_json::json!(5.0));
        assert_eq!(yaml("1.0e+5"), serde_json::json!(100000.0));
        for s in ["~", "null", "Null", "NULL"] {
            assert_eq!(yaml(s), serde_json::Value::Null, "{s}");
        }
        // An empty plain scalar is null too (`key:` → `{"key": null}`).
        assert_eq!(yaml("key:"), serde_json::json!({"key": null}));
        // A bool/num spelling nested in a mapping/sequence resolves too, and
        // a `yes:` KEY resolves like a `yes` value (PyYAML `True` → "true").
        assert_eq!(
            yaml("{flag: yes, list: [on, off], yes: v, 5: five}"),
            serde_json::json!({"flag": true, "list": [true, false], "true": "v", "5": "five"})
        );
    }

    #[test]
    fn yaml_quoted_scalars_stay_strings() {
        // The whole point of the event-level parse: serde `Value`s erase
        // scalar style, but dbt's `"yes"`/`'no'`/`"017"` are STRINGS — only
        // the PLAIN spelling resolves. (The sample fixture's `shared: "yes"`
        // exercises this through the full loader.)
        let yaml = |s: &str| {
            let mut w = Vec::new();
            yaml_to_json(s, &mut w).expect("yaml parses")
        };
        for s in ["\"yes\"", "'no'", "\"on\"", "'017'", "\"1:30\"", "\"true\""] {
            assert_eq!(
                yaml(s),
                serde_json::json!(s.trim_matches(|c| c == '"' || c == '\'')),
                "{s}"
            );
        }
        // A block scalar keeps its literal text (and never resolves).
        assert_eq!(yaml("|\n  yes\n"), serde_json::json!("yes\n"));
        // A QUOTED mapping key stays a string where the plain one boolifies.
        assert_eq!(
            yaml("{ \"yes\": a, yes: b }"),
            serde_json::json!({"yes": "a", "true": "b"})
        );
    }

    #[test]
    fn yaml_anchors_aliases_and_merge_keys() {
        let mut w = Vec::new();
        let v = yaml_to_json(
            "base: &b\n  a: 1\n  shared: yes\nleft:\n  <<: *b\n  a: 9\nright:\n  <<: [*b, {extra: e}]\n",
            &mut w,
        )
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "base": {"a": 1, "shared": true},
                "left": {"a": 9, "shared": true},      // explicit key beats merged
                "right": {"a": 1, "shared": true, "extra": "e"},
            })
        );
    }

    #[test]
    fn meta_map_warns_on_non_mapping_meta() {
        // `meta: <scalar>` / `meta: [list]` is a dbt error — degraded to a
        // warning + empty map (the load still succeeds).
        let mut w = Vec::new();
        for bad in ["meta: 'a string'", "meta: [1, 2]", "meta: 7"] {
            let v = yaml_to_json(bad, &mut w).unwrap();
            let got = meta_map(&v["meta"], &mut w, "model 'x'");
            assert!(got.is_empty(), "{bad} yields no meta entries");
        }
        assert_eq!(w.len(), 3, "one warning per non-mapping meta: {w:?}");
        assert!(w[0].contains("model 'x'"), "warning names the entry: {w:?}");
        // Absent/Null meta is normal — no warning.
        let mut w = Vec::new();
        let v = yaml_to_json("meta:", &mut w).unwrap();
        assert!(meta_map(&v["meta"], &mut w, "model 'x'").is_empty());
        assert!(w.is_empty(), "null meta warns nothing: {w:?}");
    }

    #[test]
    fn tree_config_meta_stringifies_keys_and_warns() {
        // `+meta` keys stringify like schema.yml meta keys (a `yes:` key
        // boolifies); a non-mapping `+meta` warns with its tree path.
        let mut w = Vec::new();
        let tree = yaml_to_json(
            "sample:\n  +meta:\n    yes: v\n    5: five\n  staging:\n    +meta: not-a-map\n",
            &mut w,
        )
        .unwrap();
        let cfg = resolve_tree_config(
            &tree,
            &["sample".to_string(), "staging".to_string()],
            "models".to_string(),
            &mut w,
        );
        assert_eq!(cfg.meta["true"], serde_json::json!("v"));
        assert_eq!(cfg.meta["5"], serde_json::json!("five"));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("models.sample.staging"),
            "warning names the bad level: {w:?}"
        );
    }
}
