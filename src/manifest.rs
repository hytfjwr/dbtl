//! The dbt `manifest.json` wire types: the minimal subset of the manifest
//! schema this tool reads, plus the disk loader. Both the real-manifest path
//! (`load_manifest`) and the no-compile source loader (`source.rs`, which
//! SYNTHESIZES a [`RawManifest`] of this exact shape) feed these types into
//! [`Dag::build`](crate::Dag::build) — so nothing downstream knows which mode
//! produced the data.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// `config` sub-object of a node (only the fields we surface).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawConfig {
    #[serde(default)]
    pub materialized: Option<String>,
}

/// `depends_on` sub-object of a node (the node refs it points at).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawDependsOn {
    #[serde(default)]
    pub nodes: Vec<String>,
}

/// `test_metadata` sub-object of a generic test node (`name` is the test kind:
/// `unique`, `not_null`, `relationships`, …). Absent for singular tests.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawTestMetadata {
    #[serde(default)]
    pub name: Option<String>,
}

/// A single node in the parsed manifest (`nodes` entry).
///
/// Only the subset of fields we surface is typed; unknown fields are ignored.
/// Every node must deserialize (exclusion happens *after* parsing), so anything
/// not guaranteed across all resource types is optional. `Default` is derived so
/// the unit-test literals can use `..Default::default()` and never churn again as
/// fields are added. `columns` keeps dbt definition order via `serde_json`'s
/// `preserve_order` (its `Map` is then an `IndexMap`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawNode {
    pub name: String,
    pub resource_type: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub original_file_path: Option<String>,
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
}

/// A single source in the parsed manifest (`sources` entry).
#[derive(Debug, Clone, Default, Deserialize)]
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
}

/// Minimal subset of the dbt manifest we care about.
///
/// `deny_unknown_fields` is intentionally NOT set: the v12 schema has many
/// fields we don't model, and they must be ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct RawManifest {
    pub nodes: HashMap<String, RawNode>,
    pub sources: HashMap<String, RawSource>,
    pub parent_map: HashMap<String, Vec<String>>,
    pub child_map: HashMap<String, Vec<String>>,
}

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
        .with_context(|| format!("failed to parse manifest JSON: {}", path.display()))?;
    Ok(manifest)
}
