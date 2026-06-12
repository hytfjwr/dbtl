//! Build a [`Dag`] by parsing a dbt project's SOURCE files — no `dbt compile`,
//! no `manifest.json`.
//!
//! The strategy is deliberately thin: scan `dbt_project.yml` (project name,
//! resource paths, and the `models:`/`seeds:`/`snapshots:` config trees) + the
//! model `.sql` files (extracting `ref()`/`source()` and
//! `config(materialized=…/enabled=…)` by regex) + the `schema.yml` files under
//! the configured resource paths (sources, columns, descriptions, tests),
//! synthesize a [`RawManifest`] of the exact shape [`Dag::build`] already
//! consumes, then hand it off. Everything downstream (layout, list, modal,
//! lineage) is unchanged.
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
//! singular (`.sql`) tests.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

use crate::{Dag, RawConfig, RawManifest, RawNode, RawSource, RawTestMetadata};

/// Parse a dbt project directory and build its [`Dag`] from source.
pub fn load_dag_from_source<P: AsRef<Path>>(project_dir: P) -> Result<Dag> {
    Ok(Dag::build(&manifest_from_source(project_dir)?))
}

/// Parse a dbt project directory into a synthesized [`RawManifest`].
///
/// Exposed (not just `load_dag_from_source`) so the synthesis can be asserted
/// directly in tests without building the whole DAG.
pub fn manifest_from_source<P: AsRef<Path>>(project_dir: P) -> Result<RawManifest> {
    let dir = project_dir.as_ref();
    let proj_path = dir.join("dbt_project.yml");
    let proj_str = fs::read_to_string(&proj_path)
        .with_context(|| format!("failed to read dbt_project.yml: {}", proj_path.display()))?;
    let project: DbtProject = serde_norway::from_str(&proj_str)
        .with_context(|| format!("failed to parse dbt_project.yml: {}", proj_path.display()))?;

    let proj = project.name;
    // `source-paths` is the pre-0.17 alias for `model-paths`.
    let model_paths = project
        .model_paths
        .or(project.source_paths)
        .unwrap_or_else(|| vec!["models".to_string()]);
    let seed_paths = project
        .seed_paths
        .unwrap_or_else(|| vec!["seeds".to_string()]);
    let snapshot_paths = project
        .snapshot_paths
        .unwrap_or_else(|| vec!["snapshots".to_string()]);

    let pat = Patterns::new();
    let mut nodes: HashMap<String, RawNode> = HashMap::new();
    let mut sources: HashMap<String, RawSource> = HashMap::new();
    // name -> unique_id for models/seeds/snapshots, so `ref('x')` can resolve.
    let mut name_index: HashMap<String, String> = HashMap::new();
    // (uid, Jinja-comment-stripped SQL) deferred to the ref/source pass below.
    let mut node_sql: Vec<(String, String)> = Vec::new();
    // uid -> dbt_project.yml tree materialization, applied AFTER schema.yml
    // (precedence: in-file config() > schema.yml > project tree > default view).
    let mut tree_mat: HashMap<String, String> = HashMap::new();
    let mut test_seq = 0usize;

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
        let tree = resolve_tree_config(&project.models, &tree_segments(&proj, &path));
        // Disabled models never reach the manifest's `nodes` (dbt parks them
        // under `disabled`); drop them here too, edges included.
        if pat.disabled(&stripped) || tree.enabled == Some(false) {
            continue;
        }
        if let Some(m) = &tree.materialized {
            tree_mat.insert(uid.clone(), m.clone());
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
        let tree = resolve_tree_config(&project.seeds, &tree_segments(&proj, &path));
        if tree.enabled == Some(false) {
            continue;
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
                columns: csv_header_columns(&file),
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
            let mut segments = tree_segments(&proj, &path);
            // dbt keys snapshot configs by the snapshot NAME (not the file stem);
            // swap the final segment when they differ.
            segments.pop();
            segments.push(name.clone());
            if resolve_tree_config(&project.snapshots, &segments).enabled == Some(false) {
                continue;
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
    // dbt reads "properties" yml ONLY from the configured resource paths, so the
    // scan is restricted to them. Scanning the whole project dir would ingest
    // vendored projects (`dbt_packages/`), build artifacts (`target/`), and
    // virtualenvs — synthesizing phantom sources/tests under this project's
    // namespace and silently diverging from the manifest.
    let mut yml_files = Vec::new();
    for sub in model_paths.iter().chain(&seed_paths).chain(&snapshot_paths) {
        let sub_root = dir.join(sub);
        collect_files(&sub_root, "yml", &mut yml_files);
        collect_files(&sub_root, "yaml", &mut yml_files);
    }
    yml_files.sort();
    yml_files.dedup();

    for file in &yml_files {
        // A non-schema yml deserializes to all-empty defaults; only a YAML shape
        // error (e.g. a top-level sequence) is skipped.
        let schema: SchemaFile = match serde_norway::from_str(&read(file)) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // dbt records a source's `path`/`original_file_path` as the declaring
        // schema file (project-root-relative) — match it so the detail modal
        // and the `$EDITOR` jump work identically in both modes.
        let yml_path = rel_path(dir, file);
        for src in &schema.sources {
            for tbl in &src.tables {
                let uid = format!("source.{proj}.{}.{}", src.name, tbl.name);
                sources.insert(
                    uid.clone(),
                    RawSource {
                        name: tbl.name.clone(),
                        resource_type: "source".to_string(),
                        path: Some(yml_path.clone()),
                        original_file_path: Some(yml_path.clone()),
                        description: tbl.description.clone(),
                        columns: columns_json(&tbl.columns),
                        schema: src.schema.clone(),
                        database: src.database.clone(),
                    },
                );
                synth_tests(
                    &mut nodes,
                    &mut test_seq,
                    &proj,
                    &uid,
                    &tbl.name,
                    None,
                    &tbl.tests,
                );
                for col in &tbl.columns {
                    synth_tests(
                        &mut nodes,
                        &mut test_seq,
                        &proj,
                        &uid,
                        &tbl.name,
                        Some(&col.name),
                        &col.tests,
                    );
                }
            }
        }

        apply_meta(&mut nodes, &mut test_seq, &proj, "model", &schema.models);
        apply_meta(&mut nodes, &mut test_seq, &proj, "seed", &schema.seeds);
        apply_meta(
            &mut nodes,
            &mut test_seq,
            &proj,
            "snapshot",
            &schema.snapshots,
        );
    }

    // Fill remaining model materializations from the dbt_project.yml tree, then
    // dbt's default `view` (the tree is applied AFTER schema.yml so the full
    // precedence chain is in-file > schema.yml > tree > default, like dbt).
    for (uid, node) in nodes.iter_mut() {
        if node.resource_type == "model" && node.config.materialized.is_none() {
            node.config.materialized = tree_mat
                .get(uid)
                .cloned()
                .or_else(|| Some("view".to_string()));
        }
    }

    Ok(RawManifest {
        nodes,
        sources,
        parent_map,
        child_map,
    })
}

/// Attach schema-file metadata (description / materialization / tags / columns)
/// and tests to already-discovered nodes, matched by `<type>.<proj>.<name>`. A
/// schema entry with no matching node (e.g. a documented but absent model) is
/// skipped — we never fabricate a node from docs alone.
fn apply_meta(
    nodes: &mut HashMap<String, RawNode>,
    seq: &mut usize,
    proj: &str,
    type_prefix: &str,
    entries: &[SchemaModel],
) {
    for entry in entries {
        let uid = format!("{type_prefix}.{proj}.{}", entry.name);
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
            node.description = entry.description.clone();
        }
        // An explicit schema materialization wins over the `view` default but
        // not over an in-file `config()` (already set, so only fill if None).
        if node.config.materialized.is_none() {
            node.config.materialized = entry.config.materialized.clone();
        }
        if !entry.tags.is_empty() {
            node.tags = entry.tags.clone();
        }
        if !entry.columns.is_empty() {
            node.columns = columns_json(&entry.columns);
        }
        // `node`'s borrow ends here; synth_tests re-borrows `nodes` mutably.
        synth_tests(nodes, seq, proj, &uid, &entry.name, None, &entry.tests);
        for col in &entry.columns {
            synth_tests(
                nodes,
                seq,
                proj,
                &uid,
                &entry.name,
                Some(&col.name),
                &col.tests,
            );
        }
    }
}

/// Record each schema-file test as a synthetic `resource_type: "test"` node so
/// the existing [`Dag::build`] captures it pre-prune. The map KEY uses a counter
/// so two tests on the same (node, column) can never collide and silently drop;
/// `attached_node` points the capture straight at the target.
fn synth_tests(
    nodes: &mut HashMap<String, RawNode>,
    seq: &mut usize,
    proj: &str,
    target_uid: &str,
    target_name: &str,
    column: Option<&str>,
    tests: &[serde_norway::Value],
) {
    for test in tests {
        let Some(kind) = test_kind(test) else {
            continue;
        };
        let key = format!("test.{proj}.{seq}");
        *seq += 1;
        let name = match column {
            Some(c) => format!("{kind}_{target_name}_{c}"),
            None => format!("{kind}_{target_name}"),
        };
        nodes.insert(
            key,
            RawNode {
                name,
                resource_type: "test".to_string(),
                test_metadata: Some(RawTestMetadata { name: Some(kind) }),
                attached_node: Some(target_uid.to_string()),
                column_name: column.map(str::to_string),
                ..Default::default()
            },
        );
    }
}

/// The test kind from a schema-file `tests:`/`data_tests:` entry: a bare string
/// (`unique`) is the kind; a single-key map (`accepted_values: {...}`) uses its
/// key. Anything else is ignored.
fn test_kind(v: &serde_norway::Value) -> Option<String> {
    match v {
        serde_norway::Value::String(s) => Some(s.clone()),
        serde_norway::Value::Mapping(m) => m
            .iter()
            .next()
            .and_then(|(k, _)| k.as_str().map(str::to_string)),
        _ => None,
    }
}

/// Build a manifest-shaped `columns` object (`{name: {name,data_type,description}}`)
/// preserving definition order (the `Map` is order-preserving via `preserve_order`).
fn columns_json(cols: &[SchemaColumn]) -> serde_json::Map<String, serde_json::Value> {
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
                serde_json::Value::String(d.clone()),
            );
        }
        map.insert(c.name.clone(), serde_json::Value::Object(obj));
    }
    map
}

/// Column names from a seed CSV header row (no types). Empty if unreadable.
fn csv_header_columns(file: &Path) -> serde_json::Map<String, serde_json::Value> {
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
    columns_json(&cols)
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
    // Compiled once per call; the loader is a cold path.
    let jinja = Regex::new(r"(?s)\{#.*?#\}").unwrap();
    jinja.replace_all(sql, " ").into_owned()
}

/// A resolved directory-hierarchical config from a `dbt_project.yml` resource
/// tree (`models:` / `seeds:` / `snapshots:`).
#[derive(Debug, Clone, Default)]
struct TreeConfig {
    materialized: Option<String>,
    enabled: Option<bool>,
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
/// `+enabled` at every level — a deeper (more specific) value overrides a
/// shallower one, dbt's most-specific-path rule. Descent stops at the first
/// missing segment (configs collected above it still apply). Only `+`-prefixed
/// keys are configs; bare keys are path segments.
fn resolve_tree_config(tree: &serde_norway::Value, segments: &[String]) -> TreeConfig {
    let mut out = TreeConfig::default();
    if !tree.is_mapping() {
        return out; // missing level (indexing Null yields Null) — nothing deeper
    }
    if let Some(m) = tree["+materialized"].as_str() {
        out.materialized = Some(m.to_string());
    }
    if let Some(e) = tree["+enabled"].as_bool() {
        out.enabled = Some(e);
    }
    if let Some((seg, rest)) = segments.split_first() {
        let deeper = resolve_tree_config(&tree[seg.as_str()], rest);
        out.materialized = deeper.materialized.or(out.materialized);
        out.enabled = deeper.enabled.or(out.enabled);
    }
    out
}

/// The dbt-syntax extractors, compiled once. The `regex` crate has no
/// backreferences, so each quote is matched independently (`['"]`) — a mismatched
/// `'x"` is vanishingly rare in real SQL and harmless if it slips through. The
/// leading `\b` stops `my_ref(...)` / `data_source(...)` identifiers producing
/// spurious edges. Versioned `ref('x', v=2)` does not match (the `v=` arg defeats
/// the trailing `)`) and is dropped — acceptable, like the other ref scope-outs.
struct Patterns {
    ref_re: Regex,
    source_re: Regex,
    snapshot_re: Regex,
    materialized_re: Regex,
    disabled_re: Regex,
}

impl Patterns {
    fn new() -> Self {
        Patterns {
            ref_re: Regex::new(r#"\bref\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*['"]([^'"]+)['"]\s*)?\)"#)
                .unwrap(),
            source_re: Regex::new(
                r#"\bsource\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#,
            )
            .unwrap(),
            snapshot_re: Regex::new(r"\{%-?\s*snapshot\s+([A-Za-z_][A-Za-z0-9_]*)\s*-?%\}")
                .unwrap(),
            // Both config extractors are scoped to a `config(` call so a bare
            // `materialized='x'` / `enabled=false` in SQL text, a `-- comment`,
            // or a WHERE clause can never configure the node (dbt only honours
            // them inside `config()`). `[^)]*` = "before the first `)` of the
            // call" — a paren-bearing arg (e.g. a macro call inside `meta=`)
            // BEFORE the key would defeat the match, which is vanishingly rare
            // and fails safe (no config picked up).
            materialized_re: Regex::new(
                r#"\bconfig\s*\([^)]*\bmaterialized\s*=\s*['"]([^'"]+)['"]"#,
            )
            .unwrap(),
            // `config(enabled=false)` / `enabled=False` — dbt parks the node
            // under `disabled`, off the manifest's `nodes`.
            disabled_re: Regex::new(r"\bconfig\s*\([^)]*\benabled\s*=\s*[Ff]alse\b").unwrap(),
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

    /// The materialization from a `config(materialized='…')`, if present.
    fn materialized(&self, sql: &str) -> Option<String> {
        self.materialized_re
            .captures(sql)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// Whether the file declares `enabled=false` (an in-file disable).
    fn disabled(&self, sql: &str) -> bool {
        self.disabled_re.is_match(sql)
    }
}

// ---- dbt_project.yml / schema.yml deserialization (only the fields we use) ----

#[derive(Debug, Deserialize, Default)]
struct DbtProject {
    name: String,
    #[serde(rename = "model-paths", default)]
    model_paths: Option<Vec<String>>,
    #[serde(rename = "seed-paths", default)]
    seed_paths: Option<Vec<String>>,
    #[serde(rename = "snapshot-paths", default)]
    snapshot_paths: Option<Vec<String>>,
    #[serde(rename = "source-paths", default)]
    source_paths: Option<Vec<String>>,
    /// The directory-hierarchical config trees (`+materialized` / `+enabled`
    /// under nested path keys). Kept as raw YAML and walked by
    /// [`resolve_tree_config`]; `Value::default()` is `Null`, which resolves to
    /// no config.
    #[serde(default)]
    models: serde_norway::Value,
    #[serde(default)]
    seeds: serde_norway::Value,
    #[serde(default)]
    snapshots: serde_norway::Value,
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
}

/// Shared shape for `models:` / `seeds:` / `snapshots:` entries (only what we read).
#[derive(Debug, Deserialize, Default)]
struct SchemaModel {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    config: SchemaConfig,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    columns: Vec<SchemaColumn>,
    #[serde(default, alias = "data_tests")]
    tests: Vec<serde_norway::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaConfig {
    #[serde(default)]
    materialized: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaColumn {
    name: String,
    #[serde(default)]
    data_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, alias = "data_tests")]
    tests: Vec<serde_norway::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaSource {
    name: String,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    tables: Vec<SchemaSourceTable>,
}

#[derive(Debug, Deserialize, Default)]
struct SchemaSourceTable {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    columns: Vec<SchemaColumn>,
    #[serde(default, alias = "data_tests")]
    tests: Vec<serde_norway::Value>,
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
}
