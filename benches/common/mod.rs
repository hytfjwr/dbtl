//! Synthetic `RawManifest` generator for the pipeline benchmarks.
//!
//! Deterministic (no RNG): layer sizes come from a ratio, each node picks its
//! `fan_out` upstream deps by fixed strides, and names/paths follow the layer
//! taxonomy (`staging/`/`intermediate/`/`marts/`) so `first_dir`/`layer_rank`
//! classify the synthetic graph exactly like a real project.

use std::collections::HashMap;

use dbtl::{RawConfig, RawDependsOn, RawManifest, RawNode};

/// A parameterized graph shape: `models` total nodes split across the three
/// layers by `layer_ratio` (staging / intermediate / marts weights), each
/// non-staging node wired to `fan_out` parents in the EARLIER layers (staging
/// roots stay dependency-free, like real `stg_*` models over sources).
pub struct SynthParams {
    pub models: usize,
    pub fan_out: usize,
    /// Weights for (staging, intermediate, marts); normalized internally.
    pub layer_ratio: [usize; 3],
}

impl Default for SynthParams {
    /// A realistic default: 2000 models, ~40/35/25 layer split, fan-out 3.
    fn default() -> Self {
        SynthParams {
            models: 2000,
            fan_out: 3,
            layer_ratio: [4, 3, 3],
        }
    }
}

/// (dir, default materialization) per layer.
const LAYERS: [(&str, &str); 3] = [
    ("staging", "view"),
    ("intermediate", "ephemeral"),
    ("marts", "table"),
];

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

/// Build a deterministic `RawManifest` of `models` nodes.
///
/// Node `k` of layer L depends on `fan_out` nodes drawn from ALL earlier
/// layers' uid lists (strided picks, so the graph is a DAG with a realistic
/// multi-parent fan-in — not a chain).
pub fn synth_manifest(params: &SynthParams) -> RawManifest {
    let counts = layer_counts(params);
    let mut nodes: HashMap<String, RawNode> = HashMap::with_capacity(params.models);
    // Uids of every node created so far (parents must already exist).
    let mut earlier: Vec<String> = Vec::with_capacity(params.models);
    // Dag::build reads its edges from parent_map/child_map, not depends_on.
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut idx = 0usize;

    for (li, _) in LAYERS.iter().enumerate() {
        let (dir, materialized) = LAYERS[li];
        let count = counts[li];
        // Parents come ONLY from earlier layers — `pool_len` freezes the pick
        // domain before this layer's own nodes start accumulating.
        let pool_len = earlier.len();
        for k in 0..count {
            let name = format!("{}_{:05}", &dir[..3], k);
            let uid = format!("model.bench.{name}");
            // `fan_out` strided picks (deduped + sorted for determinism).
            // Staging (li == 0) has an empty pool — real stg models hang off
            // dbt sources, which this graph omits; they stay parent-free.
            let mut parents: Vec<String> = if pool_len == 0 {
                Vec::new()
            } else {
                (0..params.fan_out)
                    .map(|j| earlier[(idx.wrapping_mul(31).wrapping_add(j * 7)) % pool_len].clone())
                    .collect()
            };
            parents.sort();
            parents.dedup();
            for p in &parents {
                child_map.entry(p.clone()).or_default().push(uid.clone());
            }
            parent_map.insert(uid.clone(), parents.clone());
            nodes.insert(
                uid.clone(),
                RawNode {
                    name: name.clone(),
                    resource_type: "model".to_string(),
                    path: Some(format!("{dir}/{name}.sql")),
                    original_file_path: Some(format!("models/{dir}/{name}.sql")),
                    description: Some(format!("synthetic {dir} model {k}")),
                    config: RawConfig {
                        materialized: Some(materialized.to_string()),
                    },
                    columns: serde_json::Map::new(),
                    tags: if k % 2 == 0 {
                        vec!["nightly".to_string()]
                    } else {
                        vec!["hourly".to_string()]
                    },
                    schema: Some("bench".to_string()),
                    database: Some("warehouse".to_string()),
                    package_name: Some("bench".to_string()),
                    test_metadata: None,
                    attached_node: None,
                    column_name: None,
                    depends_on: RawDependsOn {
                        nodes: parents,
                        macros: vec![],
                    },
                    raw_code: Some(format!("select {k} as id")),
                    compiled_code: None,
                    access: None,
                    group: None,
                    contract: None,
                    version: None,
                    latest_version: None,
                    meta: serde_json::Map::new(),
                },
            );
            earlier.push(uid);
            idx += 1;
        }
    }

    RawManifest {
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
    }
}
