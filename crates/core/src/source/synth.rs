//! The synthesis pipeline behind [`manifest_from_source_with_warnings`]: a
//! [`Synthesis`] carries the shared state the stages mutate — project config,
//! collected docs blocks, the node/source/exposure maps, the deferred SQL
//! list, and the project-tree fallback tables — and each stage is a method so
//! the public entry point reads top-to-bottom in the order dbt's own pipeline
//! applies them. Warning ORDER is significant (first-seen dedup happens in
//! `finish`), so the stage order is a contract, not a preference.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::*;
use crate::{RawConfig, RawDoc, RawExposure, RawExposureOwner, RawMetadata};

/// The shared state the source-synthesis stages build up: the parsed project
/// file and its resource paths, the collected docs blocks, the synthesized
/// node/source/exposure maps, the name index `ref()` resolves through, the
/// deferred comment-masked SQL the edge pass scans, the project-tree fallback
/// config tables, and the recoverable warnings collected along the way.
pub(super) struct Synthesis {
    dir: PathBuf,
    /// `dbt_project.yml`'s `name` — the root project, and the uid segment
    /// every synthesized resource carries.
    proj: String,
    /// The whole deserialized project file — the resource config trees
    /// (`models:`/`seeds:`/`snapshots:`/`sources:`) live on it.
    project: DbtProject,
    vars: Vars,
    model_paths: Vec<String>,
    seed_paths: Vec<String>,
    snapshot_paths: Vec<String>,
    pat: Patterns,
    /// The collected `{% docs %}` blocks every description intake renders
    /// through (`{{ doc() }}` expansion).
    docs: DocBlocks,
    nodes: HashMap<String, RawNode>,
    sources: HashMap<String, RawSource>,
    exposures: HashMap<String, RawExposure>,
    /// name -> unique_id for models/seeds/snapshots, so `ref('x')` can resolve.
    name_index: HashMap<String, String>,
    /// (uid, Jinja-comment-masked SQL) deferred to the ref/source pass —
    /// masking preserves byte length, so call-site offsets index the raw text.
    node_sql: Vec<(String, String)>,
    /// uid -> dbt_project.yml tree materialization, applied AFTER schema.yml
    /// (precedence: in-file config() > schema.yml > project tree > default view).
    tree_mat: HashMap<String, String>,
    /// uid -> dbt_project.yml tree `+meta` dict, merged AFTER schema.yml like
    /// `tree_mat` (the meta precedence chain is the same shape: in-file
    /// config(meta=…) > schema.yml meta/config.meta > project tree).
    tree_meta: HashMap<String, serde_json::Map<String, serde_json::Value>>,
    /// uid -> dbt_project.yml tree `+unique_key` — filled like `tree_mat`
    /// (in-file config() > schema.yml config: > tree), the whole declared
    /// value (dbt accepts a string or a string list).
    tree_uk: HashMap<String, serde_json::Value>,
    /// uid -> the whole resolved tree config, for the governance keys
    /// (`+access`/`+group`/`+contract`/`+incremental_strategy`) — the same
    /// lowest-precedence slot `tree_mat`/`tree_uk` occupy.
    tree_gov: HashMap<String, TreeConfig>,
    /// Recoverable oddities (an unparsable schema.yml, a duplicate resource
    /// name, a non-mapping `meta:`) are reported as warnings, not errors.
    warnings: Vec<String>,
}

/// One parsed schema/properties file plus the roots it was collected under:
/// `(schema, file, resource-root)` — the `apply_schema_files` pass needs the
/// file (for `original_file_path`) and the resource root (exposure `path` is
/// recorded relative to it, the way dbt does for models).
type ParsedSchemaFile = (SchemaFile, PathBuf, PathBuf);

impl Synthesis {
    /// Read `dbt_project.yml`, load the `var()` tables, resolve the resource
    /// path lists, and collect the `{% docs %}` blocks — everything the later
    /// stages read before a single resource is synthesized.
    pub(super) fn load(dir: &Path) -> Result<Self> {
        let proj_path = dir.join("dbt_project.yml");
        let proj_str = fs::read_to_string(&proj_path)
            .with_context(|| format!("failed to read dbt_project.yml: {}", proj_path.display()))?;
        // Recoverable oddities (an unparsable schema.yml, a duplicate resource
        // name, a non-mapping `meta:`) are reported as warnings, not errors.
        let mut warnings: Vec<String> = Vec::new();
        // dbt's YAML is PyYAML's: the project file parses through the same
        // style-aware loader the schema files do (a quoted `"yes"` stays a
        // string; a bare `yes` is the bool dbt records).
        let mut project: DbtProject = yaml_to_json(&proj_str, &mut warnings)
            .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse dbt_project.yml: {}: {e}",
                    proj_path.display()
                )
            })?;

        // `var('name')` scope for `+enabled` / `config(enabled=)` — the
        // project-file render and a node's render consult different var
        // tables (see vars.rs), so both are loaded once here.
        let vars = Vars::load(dir, &project, &mut warnings);

        let proj = project.name.clone();
        // `source-paths` is the pre-0.17 alias for `model-paths`. All three lists
        // are filtered through `contained_resource_paths`: every later join site
        // (the .sql/.csv collectors AND the schema.yml scan) trusts them to stay
        // under the project root.
        let model_paths = contained_resource_paths(
            project
                .model_paths
                .clone()
                .or(project.source_paths.clone())
                .unwrap_or_else(|| vec!["models".to_string()]),
        );
        let seed_paths = contained_resource_paths(
            project
                .seed_paths
                .clone()
                .unwrap_or_else(|| vec!["seeds".to_string()]),
        );
        let snapshot_paths = contained_resource_paths(
            project
                .snapshot_paths
                .clone()
                .unwrap_or_else(|| vec!["snapshots".to_string()]),
        );

        let pat = Patterns::new();

        // --- docs blocks (.md) ---
        // dbt renders `{{ doc('x') }}` in descriptions from `{% docs x %}…{% enddocs %}`
        // blocks collected from .md files under `docs-paths` — unset means EVERY
        // resource path (models/seeds/snapshots/macros/analyses/tests, verified
        // against real dbt). Collect them up front so every description intake
        // below renders through the one resolver.
        let doc_paths: Vec<String> = match project.docs_paths.take() {
            Some(paths) => contained_resource_paths(paths),
            None => [
                model_paths.clone(),
                seed_paths.clone(),
                snapshot_paths.clone(),
                contained_resource_paths(
                    project
                        .macro_paths
                        .clone()
                        .unwrap_or_else(|| vec!["macros".to_string()]),
                ),
                contained_resource_paths(
                    project
                        .analysis_paths
                        .clone()
                        .unwrap_or_else(|| vec!["analyses".to_string()]),
                ),
                contained_resource_paths(
                    project
                        .test_paths
                        .clone()
                        .unwrap_or_else(|| vec!["tests".to_string()]),
                ),
            ]
            .concat(),
        };
        let docs = DocBlocks::new(
            proj.clone(),
            collect_doc_blocks(dir, &doc_paths, &pat, &mut warnings),
        );

        Ok(Synthesis {
            dir: dir.to_path_buf(),
            proj,
            project,
            vars,
            model_paths,
            seed_paths,
            snapshot_paths,
            pat,
            docs,
            nodes: HashMap::new(),
            sources: HashMap::new(),
            exposures: HashMap::new(),
            name_index: HashMap::new(),
            node_sql: Vec::new(),
            tree_mat: HashMap::new(),
            tree_meta: HashMap::new(),
            tree_uk: HashMap::new(),
            tree_gov: HashMap::new(),
            warnings,
        })
    }

    /// The models stage: every `.sql` under `model-paths` becomes a `model.*`
    /// node — enabled-filtered (in-file `config()` and tree `+enabled`),
    /// duplicate-named files dropped with a warning, in-file governance/
    /// unique_key/alias/meta extracted, and the comment-masked SQL deferred
    /// for the edge pass.
    pub(super) fn collect_models(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.model_paths, "sql") {
            // Keep the pre-mask text for the SQL preview modal (comments intact);
            // the ref/source/config extractors still see the Jinja-comment-masked
            // copy (dbt's Jinja-before-SQL-comments semantics) — masking preserves
            // byte length, so offsets in it index `raw` too.
            let raw = read(&file);
            // `raw` moves into `RawNode` below, so the mask is owned here —
            // comment-free files skip the byte loop and keep just the copy.
            let masked = mask_jinja_comments(&raw).into_owned();
            let name = file_stem(&file);
            let uid = format!("model.{}.{name}", self.proj);
            // dbt's `path` is relative to the model-paths root (e.g. `staging/x.sql`)
            // — the left-pane layer grouping splits it on the first `/`.
            // `original_file_path` is project-root-relative.
            let path = rel_path(&root, &file);
            let tree = resolve_tree_config(
                &self.project.models,
                &tree_segments(&self.proj, &path),
                "models".to_string(),
                &mut self.warnings,
                Some(&self.vars),
            );
            // Disabled models never reach the manifest's `nodes` (dbt parks them
            // under `disabled`); drop them here too, edges included.
            if self.pat.disabled(
                &masked,
                &self.vars,
                &mut self.warnings,
                &rel_path(&self.dir, &file),
            ) || tree.enabled == Some(false)
            {
                continue;
            }
            // `collect_with_root` sorts by path, so the first file to claim a
            // name wins deterministically; drop (and warn on) a later same-stem
            // file before it can touch `tree_mat`/`nodes`/`name_index`/`node_sql`.
            if self.name_index.contains_key(&name) {
                self.warnings.push(format!(
                    "duplicate resource name '{name}': ignoring {}",
                    rel_path(&self.dir, &file)
                ));
                continue;
            }
            if let Some(m) = &tree.materialized {
                self.tree_mat.insert(uid.clone(), m.clone());
            }
            if !tree.meta.is_empty() {
                self.tree_meta.insert(uid.clone(), tree.meta.clone());
            }
            if let Some(uk) = &tree.unique_key {
                self.tree_uk.insert(uid.clone(), uk.clone());
            }
            self.tree_gov.insert(uid.clone(), tree.clone());
            // The in-file `config(…)` spellings of the model governance fields —
            // each the highest level of the same precedence chain `materialized`
            // follows (`access`/`contract` are model-only; `group` is not) —
            // extracted in one pass over the file's config() calls.
            let gov = config_governance(&masked);
            self.nodes.insert(
                uid.clone(),
                RawNode {
                    name: name.clone(),
                    resource_type: "model".to_string(),
                    path: Some(path),
                    original_file_path: Some(rel_path(&self.dir, &file)),
                    config: RawConfig {
                        materialized: self.pat.materialized(&masked),
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
            self.name_index.insert(name, uid.clone());
            self.node_sql.push((uid, masked));
        }
    }

    /// The seeds stage: every `.csv` under `seed-paths` becomes a `seed.*`
    /// node — tree `+enabled` filtered, duplicate names dropped, CSV header
    /// columns synthesized into the wire `columns` object.
    pub(super) fn collect_seeds(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.seed_paths, "csv") {
            let name = file_stem(&file);
            let uid = format!("seed.{}.{name}", self.proj);
            let path = rel_path(&root, &file);
            let tree = resolve_tree_config(
                &self.project.seeds,
                &tree_segments(&self.proj, &path),
                "seeds".to_string(),
                &mut self.warnings,
                Some(&self.vars),
            );
            if tree.enabled == Some(false) {
                continue;
            }
            if self.name_index.contains_key(&name) {
                self.warnings.push(format!(
                    "duplicate resource name '{name}': ignoring {}",
                    rel_path(&self.dir, &file)
                ));
                continue;
            }
            if !tree.meta.is_empty() {
                self.tree_meta.insert(uid.clone(), tree.meta.clone());
            }
            if let Some(uk) = &tree.unique_key {
                self.tree_uk.insert(uid.clone(), uk.clone());
            }
            self.tree_gov.insert(uid.clone(), tree.clone());
            self.nodes.insert(
                uid.clone(),
                RawNode {
                    name: name.clone(),
                    resource_type: "seed".to_string(),
                    path: Some(path),
                    original_file_path: Some(rel_path(&self.dir, &file)),
                    // dbt records seeds with materialization "seed".
                    config: RawConfig {
                        materialized: Some("seed".to_string()),
                        ..Default::default()
                    },
                    columns: csv_header_columns(&file, &self.docs, &mut self.warnings),
                    ..Default::default()
                },
            );
            self.name_index.insert(name, uid);
        }
    }

    /// The `{% snapshot %}` stage: each block in a `snapshot-paths` `.sql`
    /// file becomes a `snapshot.*` node — file-wide `enabled` filter, then
    /// per-block tree config keyed on dbt's `[pkg, *dirs, <file-stem>,
    /// <name>]` fqn.
    pub(super) fn collect_snapshots(&mut self) {
        for (file, root) in collect_with_root(&self.dir, &self.snapshot_paths, "sql") {
            let raw = read(&file);
            let masked = mask_jinja_comments(&raw);
            let path = rel_path(&root, &file);
            let ofp = rel_path(&self.dir, &file);
            // An in-file `enabled=false` applies to every block in the file (rare).
            if self
                .pat
                .disabled(&masked, &self.vars, &mut self.warnings, &ofp)
            {
                continue;
            }
            // Refs in a multi-block file are attributed to every block in it (rare).
            for name in self.pat.snapshots(&masked) {
                let uid = format!("snapshot.{}.{name}", self.proj);
                // dbt's {% snapshot %} fqn KEEPS the file stem and appends the
                // snapshot name on top (`[pkg, *dirs, <file-stem>, <name>]` —
                // SnapshotParser.get_fqn: "the fqn includes the filename"), so a
                // `snapshots:` tree can configure a whole file's blocks at the
                // stem level AND a single block under it. YAML-defined snapshots
                // are the different, prefix-only shape (handled below).
                let mut segments = tree_segments(&self.proj, &path);
                segments.push(name.clone());
                let tree = resolve_tree_config(
                    &self.project.snapshots,
                    &segments,
                    "snapshots".to_string(),
                    &mut self.warnings,
                    Some(&self.vars),
                );
                if tree.enabled == Some(false) {
                    continue;
                }
                if self.name_index.contains_key(&name) {
                    self.warnings
                        .push(format!("duplicate resource name '{name}': ignoring {ofp}"));
                    continue;
                }
                if !tree.meta.is_empty() {
                    self.tree_meta.insert(uid.clone(), tree.meta.clone());
                }
                if let Some(uk) = &tree.unique_key {
                    self.tree_uk.insert(uid.clone(), uk.clone());
                }
                self.tree_gov.insert(uid.clone(), tree.clone());
                self.nodes.insert(
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
                self.name_index.insert(name, uid.clone());
                self.node_sql.push((uid, masked.clone().into_owned()));
            }
        }
    }

    /// Collect and parse the `schema.yml`/properties files once — dbt reads
    /// properties yml ONLY from the configured resource paths, so the scan is
    /// restricted to them (scanning the whole project dir would ingest
    /// vendored projects, build artifacts, and virtualenvs — synthesizing
    /// phantom sources/tests under this project's namespace). `(file,
    /// resource root)` pairs dedup by FILE alone (sorting puts equal files
    /// adjacent): overlapping resource paths collect the same file under two
    /// roots, and scanning it twice would duplicate its synthesized tests.
    ///
    /// The parsed result is kept for BOTH the YAML-snapshot synthesis pass
    /// AND the schema-metadata pass, so a file is never re-parsed. A
    /// non-schema yml deserializes to all-empty defaults; only a YAML shape
    /// error (e.g. a top-level sequence) fails to parse — reported as a
    /// warning (not silently skipped) so one broken file never sinks the rest
    /// of the project.
    pub(super) fn parse_schema_files(&mut self) -> Vec<ParsedSchemaFile> {
        let mut yml_files: Vec<(PathBuf, PathBuf)> = Vec::new();
        for sub in self
            .model_paths
            .iter()
            .chain(&self.seed_paths)
            .chain(&self.snapshot_paths)
        {
            let sub_root = self.dir.join(sub);
            let mut files = Vec::new();
            collect_files(&sub_root, "yml", &mut files);
            collect_files(&sub_root, "yaml", &mut files);
            for file in files {
                yml_files.push((file, sub_root.clone()));
            }
        }
        yml_files.sort();
        yml_files.dedup_by(|a, b| a.0 == b.0);

        let mut parsed: Vec<ParsedSchemaFile> = Vec::new();
        for (file, yml_root) in yml_files {
            match yaml_to_json(&read(&file), &mut self.warnings)
                .and_then(|v| serde_json::from_value::<SchemaFile>(v).map_err(|e| e.to_string()))
            {
                Ok(schema) => parsed.push((schema, file, yml_root)),
                Err(err) => self.warnings.push(format!(
                    "skipped {}: invalid schema file ({err})",
                    rel_path(&self.dir, &file)
                )),
            }
        }
        parsed
    }

    /// YAML-defined snapshots (dbt 1.9+ `snapshots:` schema entries with a
    /// `relation:`, replacing the `{% snapshot %}` block) become nodes here,
    /// BEFORE ref/source resolution, so both a model that refs one AND the
    /// relation's own ref/source resolve.
    pub(super) fn synthesize_yaml_snapshots(&mut self, parsed: &[ParsedSchemaFile]) {
        for (schema, file, yml_root) in parsed {
            for entry in &schema.snapshots {
                let Some(rel) = &entry.relation else {
                    continue;
                };
                if entry.config.enabled == Some(false) {
                    continue;
                }
                if self.name_index.contains_key(&entry.name) {
                    self.warnings.push(format!(
                        "duplicate resource name '{}': ignoring {}",
                        entry.name,
                        rel_path(&self.dir, file)
                    ));
                    continue;
                }
                // dbt keys snapshot configs by NAME (same rule as the
                // `{% snapshot %}` path).
                let yml_rel = rel_path(yml_root, file);
                let mut segments = tree_segments(&self.proj, &yml_rel);
                segments.pop();
                segments.push(entry.name.clone());
                let tree = resolve_tree_config(
                    &self.project.snapshots,
                    &segments,
                    "snapshots".to_string(),
                    &mut self.warnings,
                    Some(&self.vars),
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
                let uid = format!("snapshot.{}.{}", self.proj, entry.name);
                if !tree.meta.is_empty() {
                    self.tree_meta.insert(uid.clone(), tree.meta.clone());
                }
                if let Some(uk) = &tree.unique_key {
                    self.tree_uk.insert(uid.clone(), uk.clone());
                }
                self.tree_gov.insert(uid.clone(), tree.clone());
                // dbt records a YAML-defined snapshot's `path` as a virtual
                // `<name>.sql` (there is no real .sql file; dbt ≤1.11 prefixed the
                // declaring yml, 1.12 records the bare name — the loader tracks the
                // current convention so source and manifest mode agree).
                let path = format!("{}.sql", entry.name);
                self.nodes.insert(
                    uid.clone(),
                    RawNode {
                        name: entry.name.clone(),
                        resource_type: "snapshot".to_string(),
                        path: Some(path),
                        original_file_path: Some(rel_path(&self.dir, file)),
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
                self.name_index.insert(entry.name.clone(), uid.clone());
                self.node_sql.push((uid, sql));
            }
        }
    }

    /// Resolve `ref()`/`source()` call sites in the deferred masked SQL into
    /// `(parent_map, child_map)` — unresolved refs (cross-package, typo,
    /// gated) drop out; a source referenced but not declared in schema.yml
    /// dangles here and is pruned later.
    pub(super) fn resolve_edges(
        &self,
    ) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for (uid, sql) in &self.node_sql {
            let mut parents: Vec<String> = Vec::new();
            // `sql` is already comment-masked — no second mask pass.
            for site in self.pat.call_sites_masked(sql) {
                match site.kind {
                    // Unresolved refs (cross-package, typo, gated) are dropped, not faked.
                    CallKind::Ref { name, .. } => {
                        if let Some(parent) = self.name_index.get(&sql[name]) {
                            parents.push(parent.clone());
                        }
                    }
                    // A source referenced but not declared in schema.yml dangles
                    // here and is pruned later — it never becomes a node.
                    CallKind::Source { source, table } => {
                        parents.push(format!(
                            "source.{}.{}.{}",
                            self.proj, &sql[source], &sql[table]
                        ));
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
        (parent_map, child_map)
    }

    /// The schema-metadata pass: source nodes, exposures, semantic-model
    /// dependents, and metadata/tests for every resource — all from the
    /// parsed schema files. The test-synthesis context borrows `name_index`
    /// shared — built only now that every resource discovery pass above has
    /// finished mutating it.
    pub(super) fn apply_schema_files(&mut self, parsed: &[ParsedSchemaFile]) {
        let mut test_ctx = TestCtx {
            seq: 0,
            proj: &self.proj,
            pat: &self.pat,
            name_index: &self.name_index,
        };
        for (schema, file, yml_root) in parsed {
            // dbt records a source's `path`/`original_file_path` as the declaring
            // schema file (project-root-relative) — match it so the detail modal
            // and the `$EDITOR` jump work identically in both modes.
            let yml_path = rel_path(&self.dir, file);
            for src in &schema.sources {
                for tbl in &src.tables {
                    let uid = format!("source.{}.{}.{}", self.proj, src.name, tbl.name);
                    // The `sources:` tree keys by the source fqn: the project
                    // name, the declaring yml's directories relative to the
                    // resource root, then `<source>: <table>` — e.g.
                    // `sample: staging: raw: orders` (verified against real
                    // dbt's recorded fqn). `tree_segments` includes the file
                    // stem, which the fqn replaces with source+table.
                    let mut segments = tree_segments(&self.proj, &rel_path(yml_root, file));
                    segments.pop();
                    segments.push(src.name.clone());
                    segments.push(tbl.name.clone());
                    // `+enabled` is not applied on `sources:` (the walk is
                    // for `+meta` only) — passing no var scope keeps it the
                    // silent bool read instead of warning on a key dbtl
                    // ignores.
                    let tree = resolve_tree_config(
                        &self.project.sources,
                        &segments,
                        "sources".to_string(),
                        &mut self.warnings,
                        None,
                    );
                    self.sources.insert(
                        uid.clone(),
                        RawSource {
                            name: tbl.name.clone(),
                            resource_type: "source".to_string(),
                            path: Some(yml_path.clone()),
                            original_file_path: Some(yml_path.clone()),
                            description: tbl
                                .description
                                .clone()
                                .map(|d| self.docs.render(&d, &mut self.warnings)),
                            columns: columns_json(&tbl.columns, &self.docs, &mut self.warnings),
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
                            meta: source_meta(tree.meta, src, tbl, &mut self.warnings),
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
                    synth_tests(
                        &mut self.nodes,
                        &mut test_ctx,
                        &uid,
                        &tbl.name,
                        None,
                        &tbl.tests,
                    );
                    synth_tests(
                        &mut self.nodes,
                        &mut test_ctx,
                        &uid,
                        &tbl.name,
                        None,
                        &tbl.data_tests,
                    );
                    for col in &tbl.columns {
                        synth_tests(
                            &mut self.nodes,
                            &mut test_ctx,
                            &uid,
                            &tbl.name,
                            Some(&col.name),
                            &col.tests,
                        );
                        synth_tests(
                            &mut self.nodes,
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
                let uid = format!("exposure.{}.{}", self.proj, exp.name);
                let mut deps: Vec<String> = Vec::new();
                for dep in &exp.depends_on {
                    for ref_name in self.pat.refs(dep) {
                        if let Some(parent) = self.name_index.get(&ref_name) {
                            deps.push(parent.clone());
                        }
                    }
                    for (src, tbl) in self.pat.sources(dep) {
                        deps.push(format!("source.{}.{src}.{tbl}", self.proj));
                    }
                }
                deps.sort();
                deps.dedup();
                self.exposures.insert(
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
                            .map(|d| self.docs.render(&d, &mut self.warnings)),
                        path: Some(rel_path(yml_root, file)),
                        original_file_path: Some(yml_path.clone()),
                        depends_on: RawDependsOn {
                            nodes: deps,
                            macros: vec![],
                        },
                    },
                );
            }

            // Semantic models: a YAML-only consumer like the exposures above —
            // but no node is synthesized for it (a real manifest would carry
            // `semantic_models` records + edges; the source loader leaves those
            // maps empty). Instead each entry's `model: ref('x')` stamps the
            // consumed node's `semantic_dependents`, the `#[serde(skip)]`
            // channel `detail_from_node` copies into `NodeDetail` — so "the
            // semantic layer reads this model" answers identically in both
            // modes (`unused_models` depends on it).
            for sm in &schema.semantic_models {
                // `enabled: false` — either spelling — parks the entry like
                // dbt's `disabled` (it never consumes its `model:` then);
                // the `config:` spelling wins when both are declared.
                if sm.config.enabled.or(sm.enabled) == Some(false) {
                    continue;
                }
                let Some(model) = &sm.model else {
                    continue;
                };
                for ref_name in self.pat.refs(model) {
                    if let Some(uid) = self.name_index.get(&ref_name) {
                        if let Some(node) = self.nodes.get_mut(uid) {
                            node.semantic_dependents.push(sm.name.clone());
                        }
                    }
                }
            }

            apply_meta(
                &mut self.nodes,
                &mut test_ctx,
                "model",
                &schema.models,
                &yml_path,
                &self.docs,
                &mut self.warnings,
            );
            apply_meta(
                &mut self.nodes,
                &mut test_ctx,
                "seed",
                &schema.seeds,
                &yml_path,
                &self.docs,
                &mut self.warnings,
            );
            apply_meta(
                &mut self.nodes,
                &mut test_ctx,
                "snapshot",
                &schema.snapshots,
                &yml_path,
                &self.docs,
                &mut self.warnings,
            );
        }
    }

    /// Fill remaining model materializations from the dbt_project.yml tree, then
    /// dbt's default `view` (the tree is applied AFTER schema.yml so the full
    /// precedence chain is in-file > schema.yml > tree > default, like dbt).
    /// The tree's `+meta`/`+unique_key`/governance keys are likewise the
    /// LOWEST precedence, filling only keys not already set by schema.yml or
    /// in-file `config()`. `semantic_dependents` are sorted/deduped here once
    /// every schema file has stamped them.
    pub(super) fn apply_tree_fallback(&mut self) {
        for (uid, node) in self.nodes.iter_mut() {
            if node.resource_type == "model" && node.config.materialized.is_none() {
                node.config.materialized = self
                    .tree_mat
                    .get(uid)
                    .cloned()
                    .or_else(|| Some("view".to_string()));
            }
            if let Some(tree) = self.tree_meta.get(uid) {
                for (k, v) in tree {
                    node.meta.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            // `+unique_key` sits under in-file `config(unique_key=…)` and
            // schema.yml `config: unique_key:` — the whole declared value (a
            // bare string or a string list; `Dag::build` normalizes either).
            if node.config.unique_key.is_none() {
                node.config.unique_key = self.tree_uk.get(uid).cloned();
            }
            // The governance keys — the tree is again the lowest rung, filling
            // only what in-file `config()` and schema.yml left unset.
            // `group` is resource-agnostic; `access`/`contract`/
            // `incremental_strategy` are model-only in dbt. An undeclared
            // model's `access` lands on dbt's `protected` default (verified:
            // every undeclared model records it on the wire); the other fields
            // simply stay absent.
            let gov = self.tree_gov.get(uid);
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
                    node.config.incremental_strategy =
                        gov.and_then(|g| g.incremental_strategy.clone());
                }
            }
            // Deterministic `semantic_dependents` — stamped per schema file,
            // so sort/dedup once here (a name can't legitimately repeat, but a
            // duplicate `semantic_models:` entry across files must not double).
            node.semantic_dependents.sort();
            node.semantic_dependents.dedup();
        }
    }

    /// The `{% docs __overview__ %}` block rides into the manifest's `docs`
    /// map in exactly the shape a real manifest carries
    /// (`doc.<proj>.__overview__`), so `Dag::build` picks the overview up
    /// through ONE intake path for both load modes. `DocBlocks::render`
    /// expands `doc()` refs and strips `{# #}` comments — what dbt writes to
    /// `block_contents`. Other docs blocks are not ingested (they already
    /// expanded where referenced; nothing else consumes them).
    pub(super) fn manifest_docs(&mut self) -> HashMap<String, RawDoc> {
        let mut manifest_docs = HashMap::new();
        if let Some(body) = self.docs.blocks.get("__overview__") {
            manifest_docs.insert(
                format!("doc.{}.__overview__", self.proj),
                RawDoc {
                    name: "__overview__".to_string(),
                    package_name: Some(self.proj.clone()),
                    block_contents: self.docs.render(body, &mut self.warnings),
                },
            );
        }
        manifest_docs
    }

    /// Assemble the synthesized `RawManifest` and hand back the deduplicated
    /// warnings — identical warnings collapse (a misdeclared `+meta` level
    /// every node descends through would otherwise warn once PER NODE; the
    /// message is the same string, so first-seen order survives dedup).
    pub(super) fn finish(
        mut self,
        parent_map: HashMap<String, Vec<String>>,
        child_map: HashMap<String, Vec<String>>,
        manifest_docs: HashMap<String, RawDoc>,
    ) -> (RawManifest, Vec<String>) {
        let mut seen_warnings = HashSet::new();
        self.warnings.retain(|w| seen_warnings.insert(w.clone()));

        (
            RawManifest {
                // `dbt_project.yml`'s `name` IS the root project — record it where
                // real dbt does so `Dag::project_name` resolves identically in
                // both load modes (the consistency suite asserts that).
                metadata: RawMetadata {
                    project_name: Some(self.proj),
                    // Source mode deliberately does NOT read profiles.yml, so the
                    // adapter is unknown here — CLL falls back to its default
                    // dialect (`--sql-dialect` is the override knob).
                    adapter_type: None,
                },
                nodes: self.nodes,
                sources: self.sources,
                exposures: self.exposures,
                parent_map,
                child_map,
                macros: HashMap::new(),
                // Source mode never synthesizes semantic-layer resources or unit
                // tests — they're YAML-only resources the source parser doesn't
                // scan. `semantic_models:` entries ARE scanned, but only to mark
                // each consumed node's `semantic_dependents` (above) — the same
                // fact `Dag::build` derives from this map's records in manifest
                // mode, so neither side needs the nodes themselves.
                semantic_models: HashMap::new(),
                metrics: HashMap::new(),
                saved_queries: HashMap::new(),
                unit_tests: HashMap::new(),
                functions: HashMap::new(),
                docs: manifest_docs,
            },
            self.warnings,
        )
    }
}
