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

use crate::SqlDialect;

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
    /// The warehouse type for a column — case-insensitive on BOTH sides:
    /// dbt lowercases catalog column keys but a manifest's `data_type`
    /// author and some adapters (Snowflake's uppercase catalog) don't
    /// follow, so a `USER_ID`-keyed column still resolves for a
    /// `user_id` declaration.
    pub fn column_type(&self, column: &str) -> Option<&str> {
        self.columns
            .get(column)
            .or_else(|| self.columns.get(&column.to_lowercase()))
            .or_else(|| {
                let lower = column.to_lowercase();
                self.columns
                    .iter()
                    .find(|(k, _)| k.to_lowercase() == lower)
                    .map(|(_, c)| c)
            })
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
    /// Membership compares case-insensitively on BOTH sides (an
    /// uppercase-keyed catalog doesn't re-flag every declared column);
    /// output keeps the declaring side's spelling — the documented names
    /// for `missing_in_warehouse`, the catalog's for `missing_in_docs`.
    pub fn drift(&self, documented: &[String]) -> (Vec<String>, Vec<String>) {
        let actual: BTreeSet<String> = self.columns.keys().map(|k| k.to_lowercase()).collect();
        let declared: BTreeSet<String> = documented.iter().map(|c| c.to_lowercase()).collect();
        (
            documented
                .iter()
                .filter(|c| !actual.contains(&c.to_lowercase()))
                .cloned()
                .collect(),
            self.columns
                .keys()
                .filter(|k| !declared.contains(&k.to_lowercase()))
                .cloned()
                .collect(),
        )
    }

    /// Per-column documented↔warehouse type disagreement, in documented-
    /// column order: one [`TypeDrift`] per column the catalog knows whose
    /// recorded type [`types_equivalent`] calls different from the declared
    /// `data_type` (a missing declared type still lists — the TUI's note
    /// surfaces the real type as information). The single comparison both
    /// `catalog_note` and lint's `warehouse_type_drift` call — the dialect
    /// selects which adapter alias table applies (`Generic` = the common
    /// table only).
    pub fn type_drift(
        &self,
        documented: &[crate::ColumnInfo],
        dialect: SqlDialect,
    ) -> Vec<TypeDrift> {
        documented
            .iter()
            .filter_map(|col| {
                let warehouse = self.column_type(&col.name)?;
                let differs = match col.data_type.as_deref() {
                    Some(declared) => !types_equivalent(declared, warehouse, dialect),
                    None => true,
                };
                differs.then(|| TypeDrift {
                    name: col.name.clone(),
                    declared: col.data_type.clone(),
                    warehouse: warehouse.to_string(),
                })
            })
            .collect()
    }
}

/// Whether two type names name the same warehouse type under `dialect` —
/// the `warehouse_type_drift` comparison:
///
/// - names lowercase, internal whitespace collapses (`double  precision`),
/// - a parenthesized args tail (`varchar(255)`, `number(38,0)`) compares
///   ONLY when both sides carry one — a bare `varchar` declaration never
///   promised a width,
/// - base names equal by table lookup: `COMMON_TYPE_FAMILIES` plus the
///   `dialect`'s adapter table (`dialect_type_families`) — a name pair
///   in no shared family is NOT equal (distinct types are the drift the
///   rule reports).
pub fn types_equivalent(declared: &str, warehouse: &str, dialect: SqlDialect) -> bool {
    let (db, da) = type_parts(declared);
    let (wb, wa) = type_parts(warehouse);
    if da.is_some() && wa.is_some() && da != wa {
        return false;
    }
    // Snowflake's `number`/`decimal`/`numeric`/`dec` all spell NUMBER —
    // scale decides the shape: bare (dbt's `(38,0)` default), `(p)` or
    // `(p, 0)` is integer-family, `(p, s>0)` is decimal-family. Read every
    // spelling through that pivot so `decimal(10,2)` ≡ `number(10,2)` and
    // a bare `decimal` ≡ `int` ≡ `number` — all genuinely the same stored
    // type — while `int` vs `number(10,2)` stays drift.
    let snow_shape = |base: String, args: Option<&str>| -> String {
        if dialect == SqlDialect::Snowflake
            && matches!(base.as_str(), "number" | "numeric" | "decimal" | "dec")
        {
            if number_scale_zero(args) {
                "number".to_string()
            } else {
                "numeric".to_string()
            }
        } else {
            base
        }
    };
    let db = snow_shape(db, da.as_deref());
    let wb = snow_shape(wb, wa.as_deref());
    if db == wb {
        return true;
    }
    COMMON_TYPE_FAMILIES
        .iter()
        .chain(dialect_type_families(dialect))
        .any(|family| family.contains(&db.as_str()) && family.contains(&wb.as_str()))
}

/// Type-name alias families: names in one row name the same type wherever
/// they print. Names are the already-normalized base form (lowercase,
/// whitespace-collapsed, args stripped — see [`type_parts`]). Kept as flat
/// data so an added alias is one list entry.
const COMMON_TYPE_FAMILIES: &[&[&str]] = &[
    &["varchar", "character varying", "text", "string"],
    &["int", "integer", "int4"],
    &["bigint", "int8"],
    &["smallint", "int2"],
    &["bool", "boolean"],
    &["float8", "double precision", "double"],
    &["float4", "real"],
    &["numeric", "decimal"],
    &["timestamp", "timestamp without time zone"],
    &["timestamptz", "timestamp with time zone"],
];

/// The adapter-specific alias families `types_equivalent` adds for
/// `dialect` — ONLY synonyms that adapter's official type docs state (the
/// catalog prints these spellings):
///
/// - BigQuery: `INT64` is the only integer type (declared `int`/`bigint`/
///   `smallint` all come back `INT64`); `FLOAT64` is the only float;
///   `BIGNUMERIC`/`BIGDECIMAL` alias like `NUMERIC`/`DECIMAL`.
/// - Snowflake: `INT`/`INTEGER`/`BIGINT`/`SMALLINT`/`TINYINT`/`BYTEINT` are
///   all `NUMBER(38,0)`; `FLOAT`/`FLOAT4`/`FLOAT8`/`DOUBLE`/
///   `DOUBLE PRECISION`/`REAL` are all `FLOAT`. `NUMBER` and its
///   `DECIMAL`/`NUMERIC`/`DEC` spellings join the integer family only at
///   scale 0 — [`types_equivalent`] pivots them before the family lookup.
fn dialect_type_families(dialect: SqlDialect) -> &'static [&'static [&'static str]] {
    match dialect {
        SqlDialect::BigQuery => &[
            &[
                "int64", "int", "integer", "int4", "bigint", "int8", "smallint", "int2",
            ],
            &["float64", "float8", "double precision", "double"],
            &["bignumeric", "bigdecimal"],
        ],
        SqlDialect::Snowflake => &[
            &[
                "number", "int", "integer", "int4", "bigint", "int8", "smallint", "int2",
                "tinyint", "byteint",
            ],
            &[
                "float",
                "float4",
                "float8",
                "double",
                "double precision",
                "real",
            ],
        ],
        _ => &[],
    }
}

/// Split a type name into `(base, args)`: the base lowercased with
/// whitespace collapsed (`DOUBLE  PRECISION` → `double precision`), args
/// the inside of the first `(...)` tail (`number(38,0)` → `Some("38,0")`,
/// `decimal` → `None`). A trailing args group after `)` is ignored — the
/// width is what comparison needs.
fn type_parts(t: &str) -> (String, Option<String>) {
    let lower = t.trim().to_lowercase();
    let (base, args) = match lower.split_once('(') {
        Some((b, a)) => (b.trim(), a.split(')').next().unwrap_or(a)),
        None => (lower.as_str(), ""),
    };
    let base = base.split_whitespace().collect::<Vec<_>>().join(" ");
    let args = args.trim();
    (base, (!args.is_empty()).then(|| args.to_string()))
}

/// Snowflake `NUMBER` args count as integer-shaped only at scale 0: bare
/// (dbt's default `(38,0)`), a bare precision `(p)`, or `(p, 0)`.
fn number_scale_zero(args: Option<&str>) -> bool {
    match args {
        None => true,
        Some(a) => {
            let mut parts = a.split(',').map(str::trim);
            parts.next(); // the precision itself doesn't matter
            matches!(parts.next(), None | Some("0")) && parts.next().is_none()
        }
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
            constraints: vec![],
        };
        // Declared order is preserved; a column the catalog doesn't know
        // contributes nothing; a matching declared type contributes nothing;
        // a missing declared type still lists the warehouse's real type.
        // Generic dialect: no adapter table, so `INT64` ≠ `bigint`.
        let drift = n.type_drift(
            &[
                col("order_id", Some("bigint")),
                col("status", None),
                col("ghost_col", Some("INT")),
            ],
            SqlDialect::Generic,
        );
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

    #[test]
    fn types_equivalent_normalizes_case_whitespace_and_arg_tails() {
        let d = SqlDialect::Generic;
        // Case + whitespace collapse.
        assert!(types_equivalent("DOUBLE  PRECISION", "double precision", d));
        assert!(types_equivalent("int", "INT", d));
        // Args compare ONLY when both sides carry them — a bare `varchar`
        // never promised a width; a pinned `varchar(10)` did.
        assert!(types_equivalent("varchar", "VARCHAR(255)", d));
        assert!(types_equivalent("varchar(255)", "varchar(255)", d));
        assert!(!types_equivalent("varchar(10)", "VARCHAR(255)", d));
        // Common families: int/integer, numeric/decimal, bool/boolean.
        assert!(types_equivalent("integer", "int", d));
        assert!(types_equivalent("numeric(10,2)", "decimal(10,2)", d));
        assert!(types_equivalent("bool", "BOOLEAN", d));
        // Genuinely different types differ — that IS the drift.
        assert!(!types_equivalent("int", "varchar", d));
        assert!(!types_equivalent("date", "timestamp", d));
    }

    #[test]
    fn types_equivalent_applies_adapter_alias_tables() {
        // BigQuery's only integer spelling is INT64 — every declared
        // integer alias reads as it.
        let bq = SqlDialect::BigQuery;
        assert!(types_equivalent("bigint", "INT64", bq));
        assert!(types_equivalent("smallint", "INT64", bq));
        assert!(types_equivalent("double", "FLOAT64", bq));
        // Under Generic there is no int64↔bigint family.
        assert!(!types_equivalent("bigint", "INT64", SqlDialect::Generic));
        // Snowflake integers are NUMBER(38,0); floats collapse to FLOAT.
        let sf = SqlDialect::Snowflake;
        assert!(types_equivalent("int", "NUMBER", sf));
        assert!(types_equivalent("bigint", "NUMBER(38,0)", sf));
        assert!(types_equivalent("byteint", "NUMBER(38,0)", sf));
        assert!(types_equivalent("double precision", "FLOAT", sf));
        // …but `NUMBER` is integer-shaped ONLY at scale 0.
        assert!(!types_equivalent("int", "NUMBER(10,2)", sf));
        assert!(!types_equivalent("number(10,2)", "int", sf));
        // A bare-precision `NUMBER(10)` still defaults to scale 0.
        assert!(types_equivalent("int", "NUMBER(10)", sf));
        // DECIMAL/NUMERIC/DEC are NUMBER spellings, so the same scale
        // pivot applies: `decimal(10,2)` ≡ `number(10,2)`, and a bare
        // `decimal`/`numeric` is `NUMBER(38,0)` — integer-shaped.
        assert!(types_equivalent("decimal(10,2)", "number(10,2)", sf));
        assert!(types_equivalent("number(10,2)", "numeric(10,2)", sf));
        assert!(types_equivalent("dec(5,2)", "DECIMAL(5,2)", sf));
        assert!(types_equivalent("decimal", "NUMBER(38,0)", sf));
        assert!(types_equivalent("int", "NUMERIC", sf));
        // …and the decimal shape never equates to the integer family.
        assert!(!types_equivalent("decimal(10,2)", "int", sf));
    }
}
