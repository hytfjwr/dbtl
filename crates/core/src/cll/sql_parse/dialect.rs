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

// ---------------------------------------------------------------------------
// Date/time-part arguments
// ---------------------------------------------------------------------------

/// Function name → the argument positions (0-based) that hold a date/time
/// PART keyword: `dateadd(day, 1, d)`'s `day`, `date_trunc(d, month)`'s
/// `month`. A bare identifier at that slot is the unit keyword — the
/// signature can't take a column there — so it is never counted as a
/// column reference. Two spellings exist: the standard form leads with
/// the part (`datediff(day, a, b)`); BigQuery's `_trunc`/`_diff` family
/// takes it LAST (`date_diff(a, b, day)`). Each row is
/// `(name, standard positions, bigquery positions)` — `date_part`/`extract`
/// forms that sqlparser keeps off the function-call path need no row.
const DATE_PART_ARG_POSITIONS: &[(&str, &[usize], &[usize])] = &[
    // Part-first wherever they're documented.
    ("dateadd", &[0], &[0]),
    ("datediff", &[0], &[0]),
    ("datename", &[0], &[0]),
    ("datepart", &[0], &[0]),
    ("date_part", &[0], &[0]),
    ("timediff", &[0], &[0]),
    ("timestampadd", &[0], &[0]),
    ("timestampdiff", &[0], &[0]),
    ("last_day", &[1], &[1]),
    // `_trunc`: part leads everywhere but BigQuery, where it ends.
    ("date_trunc", &[0], &[1]),
    ("datetrunc", &[0], &[1]),
    ("time_trunc", &[0], &[1]),
    ("timestamp_trunc", &[0], &[1]),
    ("datetime_trunc", &[0], &[1]),
    // `_diff`: part leads everywhere but BigQuery, where it ends.
    ("date_diff", &[0], &[2]),
    ("time_diff", &[0], &[2]),
    ("timestamp_diff", &[0], &[2]),
    ("datetime_diff", &[0], &[2]),
];

/// Is `index` a date-part argument position of `name` under `dialect`?
/// `name` is normalized (lowercase, last name segment). When the dialect
/// is [`Generic`](SqlDialect::Generic) — never pinned — every documented
/// position counts, so a bare unit word in either slot stays uncounted
/// rather than guessed.
pub(crate) fn date_part_arg_position(name: &str, index: usize, dialect: SqlDialect) -> bool {
    let Some((_, standard, bigquery)) = DATE_PART_ARG_POSITIONS.iter().find(|(n, ..)| *n == name)
    else {
        return false;
    };
    match dialect {
        SqlDialect::BigQuery => bigquery.contains(&index),
        SqlDialect::Generic => standard.contains(&index) || bigquery.contains(&index),
        _ => standard.contains(&index),
    }
}

/// The words that name a date/time part — matched against a normalized
/// (lowercase, unquoted) identifier at a [`date_part_arg_position`] slot.
/// Vendor abbreviations ride along (`yy`, `mcs`, `dw`…): at a part slot
/// they read as the unit keyword regardless.
const DATE_PART_WORDS: &[&str] = &[
    "century",
    "d",
    "day",
    "days",
    "dayofweek",
    "dayofyear",
    "dd",
    "decade",
    "dow",
    "doy",
    "dw",
    "dy",
    "epoch",
    "hh",
    "hour",
    "hours",
    "isodow",
    "isoweek",
    "iso_week",
    "isoyear",
    "m",
    "mcs",
    "microsecond",
    "microseconds",
    "millennium",
    "millisecond",
    "milliseconds",
    "minute",
    "minutes",
    "mi",
    "mm",
    "mon",
    "mons",
    "month",
    "months",
    "ms",
    "n",
    "nanosecond",
    "nanoseconds",
    "ns",
    "q",
    "qq",
    "quarter",
    "quarters",
    "s",
    "second",
    "seconds",
    "ss",
    "timezone",
    "timezone_hour",
    "timezone_minute",
    "us",
    "week",
    "weekday",
    "weekofyear",
    "weeks",
    "wk",
    "ww",
    "y",
    "year",
    "yearofweek",
    "yearofweekiso",
    "years",
    "yy",
    "yyyy",
];

/// Is `name` (normalized) a documented date/time-part word?
pub(crate) fn is_date_part_word(name: &str) -> bool {
    DATE_PART_WORDS.contains(&name)
}
