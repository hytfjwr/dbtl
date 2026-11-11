use super::*;

// ---------------------------------------------------------------------------
// Dialect selection
// ---------------------------------------------------------------------------

/// The SQL dialect a model's SQL is parsed with. Selection order (set up by
/// the caller): an explicit `--sql-dialect` override wins, then the
/// manifest's `metadata.adapter_type`, then [`Generic`](SqlDialect::Generic)
/// — the permissive superset that parses most vendor SQL well enough for
/// lineage.
///
/// Source-mode loads have no adapter metadata (we deliberately never read
/// `profiles.yml` to guess), so they land on Generic unless overridden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum SqlDialect {
    /// `sqlparser`'s permissive GenericDialect — the default when neither the
    /// manifest nor the user names an adapter.
    #[default]
    Generic,
    BigQuery,
    ClickHouse,
    Databricks,
    DuckDb,
    Hive,
    MsSql,
    MySql,
    Oracle,
    Postgres,
    Redshift,
    Snowflake,
    Spark,
    Sqlite,
    Teradata,
}

impl SqlDialect {
    /// Every `--sql-dialect` value `parse_name` accepts (for `--help`/tests
    /// and the CLI error message) — canonical names AND aliases, so the
    /// advertised list can never drift from what actually parses.
    pub const NAMES: &'static [&'static str] = &[
        "ansi",
        "auto",
        "big_query",
        "bigquery",
        "bq",
        "clickhouse",
        "databricks",
        "duckdb",
        "fabric",
        "generic",
        "hive",
        "hive2",
        "mariadb",
        "mssql",
        "mysql",
        "oracle",
        "pg",
        "postgres",
        "postgresql",
        "redshift",
        "snowflake",
        "spark",
        "sparksql",
        "sqlite",
        "sqlite3",
        "sqlserver",
        "teradata",
        "tsql",
    ];

    /// A dbt `metadata.adapter_type` string → dialect (`"duckdb"`,
    /// `"redshift"`, `"spark"`…). Unknown adapters map to
    /// [`Generic`](SqlDialect::Generic).
    pub fn from_adapter(adapter: &str) -> Self {
        Self::parse_name(adapter).unwrap_or(Self::Generic)
    }

    /// A `--sql-dialect` / adapter string → dialect. Case-insensitive;
    /// accepts the common spellings (`postgres`/`postgresql`,
    /// `sqlserver`/`mssql`, `tsql`, `sqlite3`, `sparksql`…).
    pub fn parse_name(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "generic" | "ansi" | "auto" | "" => Self::Generic,
            "bigquery" | "big_query" | "bq" => Self::BigQuery,
            "clickhouse" => Self::ClickHouse,
            "databricks" => Self::Databricks,
            "duckdb" => Self::DuckDb,
            "hive" | "hive2" => Self::Hive,
            "mssql" | "sqlserver" | "tsql" | "fabric" => Self::MsSql,
            "mysql" | "mariadb" => Self::MySql,
            "oracle" => Self::Oracle,
            "postgres" | "postgresql" | "pg" => Self::Postgres,
            "redshift" => Self::Redshift,
            "snowflake" => Self::Snowflake,
            "spark" | "sparksql" => Self::Spark,
            "sqlite" | "sqlite3" => Self::Sqlite,
            "teradata" => Self::Teradata,
            _ => return None,
        })
    }

    /// The dialect `Dag::adapter_type` selects (explicit CLI overrides are
    /// applied by the caller before this is consulted).
    pub fn from_dag(dag: &Dag) -> Self {
        dag.adapter_type().map_or(Self::Generic, Self::from_adapter)
    }

    /// The BASELINE side's dialect in a `--diff` pair, when nothing is
    /// pinned explicitly. Both sides must parse under ONE dialect:
    /// `normalized_expr` text is dialect-idiosyncratic, so a snowflake
    /// baseline vs a bigquery current would fabricate `expression changed`
    /// rows on identical SQL. The CURRENT side's adapter wins when it names
    /// a known dialect — the reviewer's working project sets the parse
    /// semantics; the baseline's own adapter is the fallback when the
    /// current side can't name one (a source-mode load carries none), and
    /// `Generic` when neither can. Deterministic: the choice reads only
    /// the two manifests' metadata, never filesystem or env state.
    pub fn for_diff_baseline(current: &Dag, base: &Dag) -> Self {
        current
            .adapter_type()
            .and_then(Self::parse_name)
            .unwrap_or_else(|| Self::from_dag(base))
    }

    /// The manifest's `adapter_type` when it doesn't name a known dialect —
    /// [`from_dag`](Self::from_dag) maps it to `Generic` and the caller
    /// decides how to surface the warning (a notice/toast in the TUI,
    /// stderr in the CLI). `None` when the adapter is absent or recognized.
    pub fn unrecognized_adapter(dag: &Dag) -> Option<&str> {
        dag.adapter_type().filter(|a| Self::parse_name(a).is_none())
    }

    /// The concrete `sqlparser` dialect object.
    pub(super) fn sqlparser(self) -> Box<dyn Dialect> {
        match self {
            Self::Generic => Box::new(GenericDialect),
            Self::BigQuery => Box::new(BigQueryDialect),
            Self::ClickHouse => Box::new(ClickHouseDialect {}),
            Self::Databricks => Box::new(DatabricksDialect),
            Self::DuckDb => Box::new(DuckDbDialect),
            Self::Hive => Box::new(HiveDialect {}),
            Self::MsSql => Box::new(MsSqlDialect {}),
            Self::MySql => Box::new(MySqlDialect {}),
            Self::Oracle => Box::new(OracleDialect),
            Self::Postgres => Box::new(PostgreSqlDialect {}),
            Self::Redshift => Box::new(RedshiftSqlDialect {}),
            Self::Snowflake => Box::new(SnowflakeDialect),
            Self::Spark => Box::new(SparkSqlDialect),
            Self::Sqlite => Box::new(SQLiteDialect {}),
            Self::Teradata => Box::new(TeradataDialect),
        }
    }
}
