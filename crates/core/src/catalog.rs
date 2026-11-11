//! `catalog.json` — the warehouse truth `dbt docs generate` produces. Where the
//! manifest records what schema.yml DECLARES (column names, descriptions), the
//! catalog records what the warehouse ACTUALLY has (real data types, row
//! counts, byte sizes). Side-map like [`crate::run_results`]: auto-detected
//! `target/catalog.json`, never an error when absent.
//!
//! Drift check: comparing a node's documented column set against its catalog
//! columns finds `schema.yml` ↔ warehouse drift (documented but missing,
//! present but undocumented).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The default side-map location: `target/catalog.json` next to the data
/// source — a `manifest.json` lives inside `target/` so its sibling is used,
/// a project dir gets `target/catalog.json` appended (the shared
/// [`crate::compiled::default_target_dir`] rule, same as
/// [`crate::run_results::default_run_results_path`]).
pub fn default_catalog_path(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    crate::compiled::default_target_dir(manifest_path, project_dir)
        .map(|dir| dir.join("catalog.json"))
}

/// Where the catalog side-map comes from: an explicit `--catalog` path when
/// given (its load failure is then the caller's error to report), else the
/// auto-detected sibling [`default_catalog_path`] locates (`None` = no data,
/// never an error). The single resolution rule the CLI and the App share, so
/// "explicit wins over auto-detect" can't drift between surfaces.
pub fn resolve_catalog_path(
    explicit: Option<&Path>,
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| default_catalog_path(manifest_path, project_dir))
}

/// A parsed `catalog.json`, keyed by `unique_id` — the same key space as the
/// manifest nodes, so `catalog.get("model.proj.foo")` just works.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// `metadata.generated_at` — the artifact's own timestamp, for staleness
    /// hints (a catalog older than the last `dbt run` lies about today's
    /// warehouse).
    pub generated_at: Option<String>,
    nodes: HashMap<String, CatalogNode>,
}

/// One node's warehouse truth: real column types + relation stats.
#[derive(Debug, Clone, Default)]
pub struct CatalogNode {
    /// `columns.<name>.type` — the warehouse type (`INT64`, `timestamp`, …),
    /// lowercase keys per dbt's canonical naming.
    pub columns: BTreeMap<String, CatalogColumn>,
    /// `stats.<id>` — dbt emits `row_count`, `size`/`bytes`, `last_modified`
    /// (value + optional human label); presence varies by adapter.
    pub stats: BTreeMap<String, CatalogStat>,
}

/// A single catalog column: just its warehouse type (dbt also emits
/// `index`/`comment`, neither of which the TUI needs today).
#[derive(Debug, Clone, Default)]
pub struct CatalogColumn {
    pub data_type: Option<String>,
}

/// One stat entry: `{id, description, value, include}` — only `value` matters
/// for display; `include: false` means the adapter couldn't collect it.
#[derive(Debug, Clone, Default)]
pub struct CatalogStat {
    pub value: serde_json::Value,
}

/// One documented column whose warehouse type disagrees with the declared
/// `data_type` — or whose declared type is absent while the warehouse knows
/// one. The TUI's catalog note lists every entry (surfacing the real type);
/// lint's `warehouse_type_drift` consumes only the `declared: Some` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDrift {
    /// The documented column name, as declared.
    pub name: String,
    /// The manifest-declared `data_type` (`None` when schema.yml left it off).
    pub declared: Option<String>,
    /// The warehouse's recorded type.
    pub warehouse: String,
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    #[serde(default)]
    nodes: HashMap<String, RawCatalogNode>,
    // Sources are keyed `source.<proj>.<src>.<tbl>` — same key space, same
    // treatment: merge into one lookup so sources get types/rows too.
    #[serde(default)]
    sources: HashMap<String, RawCatalogNode>,
    #[serde(default)]
    metadata: RawCatalogMetadata,
}

#[derive(Debug, Deserialize)]
struct RawCatalogNode {
    #[serde(default)]
    columns: BTreeMap<String, RawCatalogColumn>,
    #[serde(default)]
    stats: BTreeMap<String, RawCatalogStat>,
}

#[derive(Debug, Deserialize)]
struct RawCatalogColumn {
    #[serde(default, rename = "type")]
    data_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCatalogStat {
    #[serde(default)]
    value: serde_json::Value,
    #[serde(default)]
    include: bool,
}

#[derive(Debug, Deserialize, Default)]
struct RawCatalogMetadata {
    #[serde(default)]
    generated_at: Option<String>,
}

impl Catalog {
    /// Load + parse a `catalog.json`. Errors on unreadable/invalid JSON (the
    /// caller decides whether absence is fatal — auto-detect only calls this
    /// on a file that exists).
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_json(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Parse from a JSON string (test-friendly: no file needed).
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let raw: RawCatalog = serde_json::from_str(text)?;
        let mut nodes = HashMap::new();
        for (uid, node) in raw.nodes.into_iter().chain(raw.sources) {
            nodes.insert(
                uid,
                CatalogNode {
                    columns: node
                        .columns
                        .into_iter()
                        .map(|(k, c)| {
                            (
                                k,
                                CatalogColumn {
                                    data_type: c.data_type,
                                },
                            )
                        })
                        .collect(),
                    stats: node
                        .stats
                        .into_iter()
                        // `include: false` = the adapter didn't collect it —
                        // showing "rows: 0" for an uncollected stat lies.
                        .filter(|(_, s)| s.include)
                        .map(|(k, s)| (k, CatalogStat { value: s.value }))
                        .collect(),
                },
            );
        }
        Ok(Catalog {
            generated_at: raw.metadata.generated_at,
            nodes,
        })
    }

    /// The warehouse record for a manifest uid, if the catalog covers it.
    pub fn get(&self, unique_id: &str) -> Option<&CatalogNode> {
        self.nodes.get(unique_id)
    }

    /// Count of manifest uids present in this catalog — the status-bar
    /// coverage readout (`catalog 12/34`), same idea as run_results'.
    pub fn covered_in(&self, dag: &crate::Dag) -> usize {
        dag.nodes()
            .keys()
            .filter(|u| self.nodes.contains_key(*u))
            .count()
    }
}

impl CatalogNode {
    /// The warehouse type for a column (dbt lowercases catalog column keys
    /// but manifests may not — try both spellings).
    pub fn column_type(&self, column: &str) -> Option<&str> {
        self.columns
            .get(column)
            .or_else(|| self.columns.get(&column.to_lowercase()))
            .and_then(|c| c.data_type.as_deref())
    }

    /// A stat's numeric value (`row_count`, `size`, …) if the adapter
    /// collected it — stats arrive as `{"value": 123}`.
    pub fn stat(&self, name: &str) -> Option<&serde_json::Value> {
        self.stats.get(name).map(|s| &s.value)
    }

    /// Row count (`row_count` / `num_rows`) as an integer, whatever the
    /// adapter called it.
    pub fn row_count(&self) -> Option<i64> {
        ["row_count", "num_rows", "approx_row_count"]
            .iter()
            .find_map(|k| self.stat(k).and_then(|v| v.as_i64()))
    }

    /// Relation size in bytes (`size`/`bytes`) as an integer.
    pub fn size_bytes(&self) -> Option<i64> {
        ["size", "bytes", "size_bytes"]
            .iter()
            .find_map(|k| self.stat(k).and_then(|v| v.as_i64()))
    }

    /// Documented-but-missing / present-but-undocumented columns vs the
    /// manifest's declared set — the schema.yml ↔ warehouse drift readout.
    /// `(missing_in_warehouse, missing_in_docs)`, both sorted.
    pub fn drift(&self, documented: &[String]) -> (Vec<String>, Vec<String>) {
        let actual: BTreeSet<String> = self.columns.keys().cloned().collect();
        let declared: BTreeSet<String> = documented.iter().map(|c| c.to_lowercase()).collect();
        (
            declared.difference(&actual).cloned().collect(),
            actual.difference(&declared).cloned().collect(),
        )
    }

    /// Per-column documented↔warehouse type disagreement, in documented-
    /// column order: one [`TypeDrift`] per column the catalog knows whose
    /// recorded type differs from the declared `data_type` (a missing
    /// declared type still lists — the TUI's note surfaces the real type as
    /// information). The single comparison both `catalog_note` and lint's
    /// `warehouse_type_drift` call — byte-for-byte, no type-string
    /// normalization (normalizing `varchar`/`VARCHAR(255)` would change the
    /// TUI's output, so it is deliberately out of scope).
    pub fn type_drift(&self, documented: &[crate::ColumnInfo]) -> Vec<TypeDrift> {
        documented
            .iter()
            .filter_map(|col| {
                let warehouse = self.column_type(&col.name)?;
                (col.data_type.as_deref() != Some(warehouse)).then(|| TypeDrift {
                    name: col.name.clone(),
                    declared: col.data_type.clone(),
                    warehouse: warehouse.to_string(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Catalog {
        Catalog::from_json(
            r#"{
              "metadata": {"generated_at": "2024-06-01T10:00:00Z"},
              "nodes": {
                "model.p.orders": {
                  "metadata": {},
                  "columns": {
                    "order_id": {"type": "INT64", "index": 1},
                    "status": {"type": "STRING", "index": 2}
                  },
                  "stats": {
                    "row_count": {"id": "row_count", "value": 1234, "include": true},
                    "size": {"id": "size", "value": 56789, "include": true},
                    "last_modified": {"id": "lm", "value": null, "include": false}
                  }
                }
              },
              "sources": {
                "source.p.raw.events": {
                  "columns": {"id": {"type": "INT"}},
                  "stats": {}
                }
              }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_nodes_sources_and_stats() {
        let c = sample();
        let n = c.get("model.p.orders").unwrap();
        assert_eq!(n.row_count(), Some(1234));
        assert_eq!(n.size_bytes(), Some(56789));
        assert_eq!(n.column_type("order_id"), Some("INT64"));
        // `include: false` stats are dropped — a null wouldn't be 0.
        assert!(n.stat("last_modified").is_none());
        // Sources share the uid key space.
        assert_eq!(
            c.get("source.p.raw.events").unwrap().column_type("id"),
            Some("INT")
        );
        assert_eq!(c.generated_at.as_deref(), Some("2024-06-01T10:00:00Z"));
    }

    #[test]
    fn drift_compares_documented_vs_actual() {
        let c = sample();
        let n = c.get("model.p.orders").unwrap();
        let (missing_in_wh, missing_in_docs) = n.drift(&["order_id".into(), "ghost_col".into()]);
        assert_eq!(missing_in_wh, vec!["ghost_col"]);
        assert_eq!(missing_in_docs, vec!["status"]);
    }

    #[test]
    fn type_drift_reports_only_known_disagreements_in_declared_order() {
        let c = sample();
        let n = c.get("model.p.orders").unwrap();
        let col = |name: &str, ty: Option<&str>| crate::ColumnInfo {
            name: name.into(),
            data_type: ty.map(str::to_string),
            description: None,
        };
        // Declared order is preserved; a column the catalog doesn't know
        // contributes nothing; a matching declared type contributes nothing;
        // a missing declared type still lists the warehouse's real type.
        let drift = n.type_drift(&[
            col("order_id", Some("bigint")), // byte-for-byte: INT64 != bigint
            col("status", None),
            col("ghost_col", Some("INT")),
        ]);
        assert_eq!(
            drift,
            vec![
                TypeDrift {
                    name: "order_id".into(),
                    declared: Some("bigint".into()),
                    warehouse: "INT64".into(),
                },
                TypeDrift {
                    name: "status".into(),
                    declared: None,
                    warehouse: "STRING".into(),
                },
            ]
        );
    }
}
