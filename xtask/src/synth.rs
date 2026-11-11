//! Deterministic synthetic dbt projects for benches, perf tests, and the
//! `cargo xtask synth` fixture generator.
//!
//! [`synth_manifest`] builds a [`RawManifest`] in memory — the same shape
//! `load_manifest` returns — and [`write_project`] writes the SAME graph to
//! disk as a real dbt project tree (`dbt_project.yml` + `models/*.sql` +
//! `schema.yml`) plus a canonical `target/manifest.json`. Both sides then
//! load identically: `load_dag(manifest)` and `load_dag_from_source(dir)`
//! see the same node set and edges.
//!
//! Everything is a pure function of [`SynthParams`] — no randomness, no
//! clock — so two runs produce byte-identical output and benches stay
//! comparable across runs.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use dbtl_core::{
    RawConfig, RawDependsOn, RawExposure, RawExposureOwner, RawManifest, RawMetadata, RawNode,
    RawSource, RawTestMetadata,
};

/// Knobs for the synthetic graph.
///
/// [`Default`] reproduces the historical model-only shape (staging /
/// intermediate / marts, fixed-stride parents, `select {k} as id` bodies) so
/// the existing benchmark ids measure the same `Dag` as before. The extras —
/// sources, tests, exposures, declared columns, ref-bearing SQL — opt in per
/// field.
#[derive(Debug, Clone)]
pub struct SynthParams {
    /// Total model count.
    pub models: usize,
    /// Max parents each non-staging model draws from the earlier layers.
    pub fan_out: usize,
    /// Weights for (staging, intermediate, marts); normalized internally.
    pub layer_ratio: [usize; 3],
    /// Source tables feeding the staging layer (`source.<pkg>.raw.tbl_*`);
    /// `0` keeps staging parent-free (the historical shape).
    pub sources: usize,
    /// Declared `cNNNN` siblings per model/source table, on top of `id`.
    /// `0` = no declared columns.
    pub columns: usize,
    /// Column tests (`not_null` / `unique` alternating) per model. `0` =
    /// none. When >0 the `id` column is declared even with `columns == 0` —
    /// a test needs something to attach to.
    pub tests_per_model: usize,
    /// `dashboard` exposures consuming strided marts picks. `0` = none.
    pub exposures: usize,
    /// Whether each node's `raw_code` carries the ref()/source()-bearing SQL
    /// body (what [`write_project`] puts in `.sql` files — the input
    /// column-level lineage parses). `false` keeps the historical
    /// `select {k} as id` placeholder.
    pub sql: bool,
    /// The project/package name: `model.<package>.<name>` uids and the
    /// `dbt_project.yml` `name:`.
    pub package: String,
}

impl Default for SynthParams {
    /// A realistic default: 2000 models, ~40/30/30 layer split, fan-out 3.
    fn default() -> Self {
        SynthParams {
            models: 2000,
            fan_out: 3,
            layer_ratio: [4, 3, 3],
            sources: 0,
            columns: 0,
            tests_per_model: 0,
            exposures: 0,
            sql: false,
            package: "bench".to_string(),
        }
    }
}

/// (dir, default materialization) per layer.
const LAYERS: [(&str, &str); 3] = [
    ("staging", "view"),
    ("intermediate", "ephemeral"),
    ("marts", "table"),
];

/// The source namespace every `source()` call resolves into.
const SOURCE_NAME: &str = "raw";

/// The column-test kinds the generator alternates through (both bare schema
/// tests — the shapes `Dag::build` captures deterministically).
const TEST_KINDS: [&str; 2] = ["not_null", "unique"];

/// A resolved model record — the single plan both the manifest and the
/// `.sql`/schema files render from, so the two on-disk forms can't drift.
struct ModelPlan {
    /// `sta_00042` (layer dir's first three letters)
    name: String,
    /// `model.<pkg>.sta_00042`
    uid: String,
    /// `staging` | `intermediate` | `marts`
    dir: &'static str,
    /// Index inside the layer (drives names, tags, constant SQL values).
    k: usize,
    materialized: &'static str,
    /// Parent uids (models + sources), sorted + deduped.
    parents: Vec<String>,
    /// Declared columns (`id` first, then `cNNNN`).
    columns: Vec<String>,
    /// (column, kind) schema column tests.
    tests: Vec<(String, &'static str)>,
    description: String,
    tags: Vec<String>,
}

/// The full deterministic plan [`synth_manifest`] and [`write_project`]
/// share.
struct Plan {
    models: Vec<ModelPlan>,
    /// Source table names (`tbl_NNNN`).
    sources: Vec<String>,
    /// Source table declared columns.
    source_columns: Vec<String>,
    /// (exposure name, dependency uids).
    exposures: Vec<(String, Vec<String>)>,
}

fn layer_counts(params: &SynthParams) -> [usize; 3] {
    let total_w: usize = params.layer_ratio.iter().sum();
    if total_w == 0 {
        return [params.models, 0, 0];
    }
    let mut counts = [0usize; 3];
    let mut assigned = 0;
    for (i, w) in params.layer_ratio.iter().enumerate() {
        counts[i] = params.models * w / total_w;
        assigned += counts[i];
    }
    // Round-off remainder lands on staging (the widest layer in practice).
    counts[0] += params.models - assigned;
    counts
}

/// The declared column list for a model: `id` plus `columns` synthetic
/// siblings — or just `id` when only tests need an anchor.
fn declared_columns(params: &SynthParams) -> Vec<String> {
    if params.columns == 0 && params.tests_per_model == 0 {
        return Vec::new();
    }
    let mut cols = vec!["id".to_string()];
    cols.extend((0..params.columns).map(|i| format!("c{i:04}")));
    cols
}

fn build_plan(params: &SynthParams) -> Plan {
    let counts = layer_counts(params);
    let pkg = params.package.as_str();
    let declared = declared_columns(params);
    let source_columns: Vec<String> = if params.columns == 0 {
        Vec::new()
    } else {
        declared.clone()
    };
    let sources: Vec<String> = (0..params.sources).map(|i| format!("tbl_{i:04}")).collect();

    let mut models: Vec<ModelPlan> = Vec::with_capacity(params.models);
    // Uids of every model created so far (parents must already exist).
    let mut earlier: Vec<String> = Vec::with_capacity(params.models);
    let mut idx = 0usize;

    for (li, (dir, materialized)) in LAYERS.iter().enumerate() {
        let count = counts[li];
        // Parents come ONLY from earlier layers — `pool_len` freezes the pick
        // domain before this layer's own nodes start accumulating.
        let pool_len = earlier.len();
        for k in 0..count {
            let name = format!("{}_{:05}", &dir[..3], k);
            let uid = format!("model.{pkg}.{name}");
            // `fan_out` strided picks (deduped + sorted for determinism).
            // Staging (li == 0) has an empty model pool; with `sources > 0`
            // it hangs off a source table instead — the real stg_* shape.
            let mut parents: Vec<String> = (0..params.fan_out)
                .filter(|_| pool_len > 0)
                .map(|j| earlier[(idx.wrapping_mul(31).wrapping_add(j * 7)) % pool_len].clone())
                .collect();
            if li == 0 && !sources.is_empty() {
                parents.push(format!(
                    "source.{pkg}.{SOURCE_NAME}.{}",
                    sources[k % sources.len()]
                ));
            }
            parents.sort();
            parents.dedup();
            let tests: Vec<(String, &'static str)> = (0..params.tests_per_model)
                .map(|i| {
                    (
                        declared[i % declared.len()].clone(),
                        TEST_KINDS[i % TEST_KINDS.len()],
                    )
                })
                .collect();
            models.push(ModelPlan {
                name,
                uid: uid.clone(),
                dir,
                k,
                materialized,
                parents,
                columns: declared.clone(),
                tests,
                description: format!("synthetic {dir} model {k}"),
                tags: vec![if k % 2 == 0 { "nightly" } else { "hourly" }.to_string()],
            });
            earlier.push(uid);
            idx += 1;
        }
    }

    // Exposures fan out over the marts tail by stride — always downstream
    // leaves, exactly like real dashboard declarations.
    let marts: Vec<&str> = models
        .iter()
        .filter(|m| m.dir == "marts")
        .map(|m| m.uid.as_str())
        .collect();
    let exposures: Vec<(String, Vec<String>)> = (0..params.exposures)
        .filter(|_| !marts.is_empty())
        .map(|i| {
            let mut deps: Vec<String> = [(i * 7), (i * 7 + 3)]
                .iter()
                .map(|j| marts[j % marts.len()].to_string())
                .collect();
            deps.sort();
            deps.dedup();
            (format!("exp_{i:04}"), deps)
        })
        .collect();

    Plan {
        models,
        sources,
        source_columns,
        exposures,
    }
}

/// The `.sql` body a model would carry in a real project — also the manifest
/// `raw_code` when [`SynthParams::sql`] is on. Parent-free models emit a
/// constant select; parented models emit a `FROM` + `JOIN` chain so the
/// source loader's `ref()`/`source()` extractors reproduce `parents` exactly
/// and column-level lineage has passthrough edges to walk.
fn model_sql(model: &ModelPlan) -> String {
    let mut sql = format!(
        "{{{{ config(materialized='{}') }}}}\n\n",
        model.materialized
    );
    if model.parents.is_empty() {
        let mut cols = vec![format!("{} as id", model.k)];
        cols.extend(
            model
                .columns
                .iter()
                .filter(|c| c.as_str() != "id")
                .enumerate()
                .map(|(j, c)| format!("{} as {c}", model.k * 100 + j)),
        );
        sql.push_str(&format!("select\n    {}\n", cols.join(",\n    ")));
        return sql;
    }
    let select_cols: Vec<String> = if model.columns.is_empty() {
        vec!["id".to_string()]
    } else {
        model.columns.clone()
    };
    let mut from_seen = false;
    let mut from_block = String::new();
    for (i, parent) in model.parents.iter().enumerate() {
        let alias = format!("p{}", i + 1);
        let relation = parent_relation(parent);
        if !from_seen {
            from_block.push_str(&format!("from {relation} as {alias}"));
            from_seen = true;
        } else {
            from_block.push_str(&format!(
                "\njoin {relation} as {alias} on p1.id = {alias}.id"
            ));
        }
    }
    // Columns fan across parents round-robin (p1.id, p2.c0000, p3.c0001, …)
    // — the shape a real multi-parent model has — so a single column's
    // upstream closure walks the whole parent cone, not just the p1 chain.
    let np = model.parents.len();
    sql.push_str(&format!(
        "select\n    {}\n{from_block}\n",
        select_cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("p{}.{} as {}", 1 + i % np, c, c))
            .collect::<Vec<_>>()
            .join(",\n    ")
    ));
    sql
}

/// The `{{ ref(...) }}` / `{{ source(...) }}` expression a parent uid
/// encodes, mirroring the source loader's extraction: `model.<pkg>.<name>`
/// → `ref('name')`, `source.<pkg>.<src>.<tbl>` → `source('src','tbl')`.
fn parent_relation(uid: &str) -> String {
    let seg: Vec<&str> = uid.split('.').collect();
    match seg.as_slice() {
        [_, _, name] => format!("{{{{ ref('{name}') }}}}"),
        [_, _, src, tbl] => format!("{{{{ source('{src}','{tbl}') }}}}"),
        _ => format!("{{{{ ref('{uid}') }}}}"),
    }
}

/// The `columns` JSON object both manifests record — same shape the source
/// loader's `columns_json` produces from schema.yml (`name` + optional
/// `data_type` per entry).
fn columns_json(cols: &[String]) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for c in cols {
        map.insert(
            c.clone(),
            serde_json::json!({
                "name": c,
                "data_type": if c == "id" { "int" } else { "text" },
            }),
        );
    }
    map
}

fn manifest_from_plan(params: &SynthParams, plan: &Plan, sql_bodies: bool) -> RawManifest {
    let pkg = params.package.as_str();
    let mut nodes: HashMap<String, RawNode> = HashMap::with_capacity(params.models);
    // Dag::build reads its edges from parent_map/child_map, not depends_on.
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();

    for model in &plan.models {
        for p in &model.parents {
            child_map
                .entry(p.clone())
                .or_default()
                .push(model.uid.clone());
        }
        parent_map.insert(model.uid.clone(), model.parents.clone());
        nodes.insert(
            model.uid.clone(),
            RawNode {
                name: model.name.clone(),
                resource_type: "model".to_string(),
                path: Some(format!("{}/{}.sql", model.dir, model.name)),
                original_file_path: Some(format!("models/{}/{}.sql", model.dir, model.name)),
                description: Some(model.description.clone()),
                config: RawConfig {
                    materialized: Some(model.materialized.to_string()),
                    ..Default::default()
                },
                columns: columns_json(&model.columns),
                tags: model.tags.clone(),
                schema: Some("bench".to_string()),
                database: Some("warehouse".to_string()),
                package_name: Some(pkg.to_string()),
                depends_on: RawDependsOn {
                    nodes: model.parents.clone(),
                    macros: vec![],
                },
                raw_code: Some(if sql_bodies {
                    model_sql(model)
                } else {
                    format!("select {} as id", model.k)
                }),
                ..Default::default()
            },
        );
    }

    // Column tests ride as `resource_type: "test"` nodes — captured pre-prune
    // into the `tests` side map — with the same key/name/kwargs shape the
    // source loader synthesizes, so `Dag::tests` matches in both modes.
    let mut seq = 0usize;
    for model in &plan.models {
        for (col, kind) in &model.tests {
            let key = format!("test.{pkg}.{seq}");
            seq += 1;
            nodes.insert(
                key,
                RawNode {
                    name: format!("{kind}_{name}_{col}", name = model.name),
                    resource_type: "test".to_string(),
                    test_metadata: Some(RawTestMetadata {
                        name: Some(kind.to_string()),
                        kwargs: Some(serde_json::Map::new()),
                    }),
                    attached_node: Some(model.uid.clone()),
                    column_name: Some(col.clone()),
                    depends_on: RawDependsOn {
                        nodes: vec![model.uid.clone()],
                        macros: vec![],
                    },
                    ..Default::default()
                },
            );
        }
    }

    let sources: HashMap<String, RawSource> = plan
        .sources
        .iter()
        .map(|tbl| {
            let uid = format!("source.{pkg}.{SOURCE_NAME}.{tbl}");
            (
                uid,
                RawSource {
                    name: tbl.clone(),
                    resource_type: "source".to_string(),
                    path: Some("models/staging/schema.yml".to_string()),
                    original_file_path: Some("models/staging/schema.yml".to_string()),
                    description: Some(format!("synthetic source table {tbl}")),
                    columns: columns_json(&plan.source_columns),
                    schema: Some(SOURCE_NAME.to_string()),
                    database: Some("warehouse".to_string()),
                    package_name: Some(pkg.to_string()),
                    ..Default::default()
                },
            )
        })
        .collect();

    let exposures: HashMap<String, RawExposure> = plan
        .exposures
        .iter()
        .map(|(name, deps)| {
            let uid = format!("exposure.{pkg}.{name}");
            (
                uid,
                RawExposure {
                    name: name.clone(),
                    exposure_type: Some("dashboard".to_string()),
                    owner: RawExposureOwner {
                        name: Some("analytics".to_string()),
                        email: None,
                    },
                    description: Some(format!("synthetic exposure {name}")),
                    // Declared in the marts schema file — `path` is
                    // resource-root-relative, `original_file_path` project-
                    // root-relative (the same split dbt records).
                    path: Some("marts/schema.yml".to_string()),
                    original_file_path: Some("models/marts/schema.yml".to_string()),
                    // Like source mode, exposure edges live ONLY in
                    // depends_on — Dag::build merges them into the adjacency.
                    depends_on: RawDependsOn {
                        nodes: deps.clone(),
                        macros: vec![],
                    },
                    ..Default::default()
                },
            )
        })
        .collect();

    RawManifest {
        metadata: RawMetadata {
            project_name: Some(pkg.to_string()),
            ..Default::default()
        },
        nodes,
        sources,
        exposures,
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
        docs: Default::default(),
    }
}

/// Build a deterministic `RawManifest` of `models` nodes.
///
/// Node `k` of layer L depends on `fan_out` nodes drawn from ALL earlier
/// layers' uid lists (strided picks, so the graph is a DAG with a realistic
/// multi-parent fan-in — not a chain). With `sources > 0` the staging layer
/// additionally hangs off `source.<pkg>.raw.tbl_*` tables.
pub fn synth_manifest(params: &SynthParams) -> RawManifest {
    let plan = build_plan(params);
    manifest_from_plan(params, &plan, params.sql)
}

/// Canonical JSON for a `RawManifest`: compact with every object key sorted
/// recursively, so two runs over the same params are byte-identical and
/// `load_manifest` reads back the same data.
pub fn manifest_json(manifest: &RawManifest) -> String {
    let mut value = serde_json::to_value(manifest).expect("RawManifest is serializable");
    sort_value(&mut value);
    let mut out = serde_json::to_string(&value).expect("sorted value is serializable");
    out.push('\n');
    out
}

fn sort_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> =
                std::mem::take(map).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, val) in &mut entries {
                sort_value(val);
            }
            *map = entries.into_iter().collect();
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(sort_value),
        _ => {}
    }
}

/// The schema.yml for one layer dir: `models:` metadata (columns + tests +
/// descriptions) always, `sources:` declarations in staging, `exposures:` in
/// marts. `None` when the layer has no models and nothing else to declare —
/// the caller then writes no file.
fn schema_yml(plan: &Plan, dir: &str) -> Option<String> {
    let models: Vec<&ModelPlan> = plan.models.iter().filter(|m| m.dir == dir).collect();
    let with_sources = dir == "staging" && !plan.sources.is_empty();
    let with_exposures = dir == "marts" && !plan.exposures.is_empty();
    if models.is_empty() && !with_sources && !with_exposures {
        return None;
    }
    let mut out = String::from("version: 2\n");
    if with_sources {
        out.push_str(&format!(
            "\nsources:\n  - name: {SOURCE_NAME}\n    schema: {SOURCE_NAME}\n    tables:\n"
        ));
        for tbl in &plan.sources {
            out.push_str(&format!("      - name: {tbl}\n"));
            if !plan.source_columns.is_empty() {
                out.push_str("        columns:\n");
                for c in &plan.source_columns {
                    out.push_str(&format!(
                        "          - name: {c}\n            data_type: {}\n",
                        if c == "id" { "int" } else { "text" }
                    ));
                }
            }
        }
    }
    if !models.is_empty() {
        out.push_str("\nmodels:\n");
        for m in models {
            out.push_str(&format!("  - name: {}\n", m.name));
            out.push_str(&format!("    description: {}\n", m.description));
            if !m.columns.is_empty() {
                out.push_str("    columns:\n");
                for c in &m.columns {
                    out.push_str(&format!(
                        "      - name: {c}\n        data_type: {}\n",
                        if c == "id" { "int" } else { "text" }
                    ));
                    let tests: Vec<&&str> = m
                        .tests
                        .iter()
                        .filter(|(tc, _)| tc == c)
                        .map(|(_, k)| k)
                        .collect();
                    if !tests.is_empty() {
                        out.push_str("        tests:\n");
                        for t in tests {
                            out.push_str(&format!("          - {t}\n"));
                        }
                    }
                }
            }
        }
    }
    if with_exposures {
        out.push_str("\nexposures:\n");
        for (name, deps) in &plan.exposures {
            out.push_str(&format!("  - name: {name}\n    type: dashboard\n    owner:\n      name: analytics\n    depends_on:\n"));
            for dep in deps {
                let seg: Vec<&str> = dep.split('.').collect();
                out.push_str(&format!("      - \"{{{{ ref('{}') }}}}\"\n", seg[2]));
            }
        }
    }
    Some(out)
}

/// Write the synthetic project to `dir` as a real dbt project tree:
/// `dbt_project.yml`, `models/<layer>/<name>.sql`, `models/<layer>/schema.yml`,
/// and `target/manifest.json` — all from ONE shared plan, so loading
/// `dir/target/manifest.json` and parsing `dir` as a dbt project yield the
/// same node set and edges.
///
/// `.sql` files always carry the ref()/source()-bearing body — without it
/// the source loader would see no edges — so the on-disk manifest's
/// `raw_code` matches the file contents (`SynthParams::sql` only controls
/// the in-memory [`synth_manifest`]).
pub fn write_project(params: &SynthParams, dir: &Path) -> io::Result<()> {
    let plan = build_plan(params);
    let manifest = manifest_from_plan(params, &plan, true);

    fs::create_dir_all(dir)?;
    fs::write(
        dir.join("dbt_project.yml"),
        format!("name: {}\nmodel-paths: [\"models\"]\n", params.package),
    )?;
    for model in &plan.models {
        let layer_dir = dir.join("models").join(model.dir);
        fs::create_dir_all(&layer_dir)?;
        fs::write(
            layer_dir.join(format!("{}.sql", model.name)),
            model_sql(model),
        )?;
    }
    for (layer, _) in LAYERS {
        if let Some(yml) = schema_yml(&plan, layer) {
            fs::write(dir.join("models").join(layer).join("schema.yml"), yml)?;
        }
    }
    let target = dir.join("target");
    fs::create_dir_all(&target)?;
    fs::write(target.join("manifest.json"), manifest_json(&manifest))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn dag(manifest: &RawManifest) -> dbtl_core::Dag {
        dbtl_core::Dag::build(manifest)
    }

    #[test]
    fn default_shape_is_unchanged() {
        // The existing benchmark ids measure THIS Dag — layer counts, edge
        // count, uids and names must stay exactly as the original generator
        // produced them.
        let d = dag(&synth_manifest(&SynthParams::default()));
        assert_eq!(d.nodes().len(), 2000);
        assert_eq!(d.edges().len(), 3600);
        assert!(d.nodes().contains_key("model.bench.sta_00000"));
        assert!(d.nodes().contains_key("model.bench.mar_00599"));
    }

    #[test]
    fn extended_shape_adds_non_model_nodes() {
        let params = SynthParams {
            models: 300,
            sources: 4,
            columns: 3,
            tests_per_model: 2,
            exposures: 3,
            sql: true,
            ..SynthParams::default()
        };
        let m = synth_manifest(&params);
        assert_eq!(m.sources.len(), 4);
        assert_eq!(m.exposures.len(), 3);
        // 300 models + 300*2 test nodes.
        assert_eq!(m.nodes.len(), 300 + 600);
        let d = dag(&m);
        // Every model, source, and exposure is a graph node.
        assert_eq!(d.nodes().len(), 300 + 4 + 3);
        // Staging picks a source parent.
        let stg = d.direct_parents("model.bench.sta_00000");
        assert_eq!(stg, &["source.bench.raw.tbl_0000".to_string()]);
        // Tests land in the side map.
        assert_eq!(d.tests("model.bench.sta_00000").len(), 2);
        // An exposure terminates downstream of a marts model.
        assert!(!d.direct_parents("exposure.bench.exp_0000").is_empty());
    }

    #[test]
    fn write_project_is_byte_identical() {
        let params = SynthParams {
            models: 120,
            sources: 3,
            columns: 2,
            tests_per_model: 1,
            exposures: 2,
            sql: true,
            ..SynthParams::default()
        };
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_project(&params, a.path()).unwrap();
        write_project(&params, b.path()).unwrap();
        let files_a = tree(a.path());
        let files_b = tree(b.path());
        assert_eq!(files_a, files_b);
    }

    /// Every file under `root`, sorted by relative path, as (path, bytes).
    fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push((
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .to_string(),
                        fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn project_and_manifest_describe_the_same_graph() {
        let params = SynthParams {
            models: 300,
            sources: 4,
            columns: 3,
            tests_per_model: 2,
            exposures: 3,
            sql: true,
            ..SynthParams::default()
        };
        let dir = tempfile::tempdir().unwrap();
        write_project(&params, dir.path()).unwrap();

        let via_manifest = dbtl_core::load_dag(dir.path().join("target/manifest.json")).unwrap();
        let via_source = dbtl_core::load_dag_from_source(dir.path()).unwrap();

        let nodes_m: BTreeSet<_> = via_manifest.nodes().keys().collect();
        let nodes_s: BTreeSet<_> = via_source.nodes().keys().collect();
        assert_eq!(nodes_m, nodes_s);

        let mut edges_m = via_manifest.edges();
        let mut edges_s = via_source.edges();
        edges_m.sort();
        edges_s.sort();
        assert_eq!(edges_m, edges_s);

        // Tests attach identically in both modes (kind + column).
        for uid in via_manifest.nodes().keys() {
            let mut tm: Vec<_> = via_manifest
                .tests(uid)
                .iter()
                .map(|t| (t.kind.clone(), t.column_name.clone()))
                .collect();
            let mut ts: Vec<_> = via_source
                .tests(uid)
                .iter()
                .map(|t| (t.kind.clone(), t.column_name.clone()))
                .collect();
            tm.sort();
            ts.sort();
            assert_eq!(tm, ts, "tests differ on {uid}");
        }
    }
}
