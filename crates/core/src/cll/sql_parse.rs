//! CLL v1 (Issue 08): [`sqlparser`]-backed column resolution.
//!
//! [`SqlParseResolver`] implements [`ColumnResolver`] like
//! [`NameMatchResolver`], but instead of matching raw identifier *names*
//! against parent column lists it walks the SQL AST with real scope
//! resolution: FROM/JOIN bindings, table aliases, CTEs, subqueries,
//! qualified column references, wildcard expansion, and set operations.
//! Parser-derived edges carry [`Confidence::Exact`] (or
//! [`Confidence::Stale`] when the compiled SQL is older than its source).
//!
//! When a model has no resolvable SQL, or the parse fails, the resolver
//! degrades **per model** to [`NameMatchResolver`] — the model's edges and
//! column list then come from Issue 07's approximation and carry
//! [`Confidence::NameMatch`]. The set of uids that fell back is recorded so
//! the CLI can count them (`dbtl columns` prints a stderr summary) and the
//! detail modal can tag the node's resolution `[name-match]`.
//!
//! Scope semantics (documented approximations):
//! - A qualified `t.c` binds `t` in the current scope chain (the enclosing
//!   query's FROM items, then outer scopes for correlated subqueries).
//! - An unqualified `c` resolves against every binding *in the innermost
//!   scope* where it exists; when it exists in several bindings the edge
//!   carries all candidates (documented over-connection, same policy as v0
//!   but now scoped to FROM instead of "every parent in the manifest").
//! - `t.*` expands to the binding's known column list when one exists
//!   (catalog → documented → its own analyzed outputs); an unknown binding
//!   contributes nothing rather than guessing names.
//! - An unqualified column that resolves to no binding but *could* come from
//!   a binding with an unknown column list is attributed to that binding —
//!   "can't disprove" beats silently dropping the edge. Crediting is
//!   LEVEL-scoped: an unknown binding is only credited when no KNOWN
//!   binding at its own scope level — or any level OUTER of it — resolves
//!   the ref (innermost-scope-wins; inner unknowns can't poison a provable
//!   outer hit).
//! - `EXCEPT`/`INTERSECT`/`MINUS` right arms contribute their refs to the
//!   row-shaping pool (they shape WHICH rows survive, not the values);
//!   `UNION` merges positionally, or by output name under `BY NAME`.
//! - Lambda parameters (`x -> …`), `MATCH_RECOGNIZE` symbols, and
//!   `named => value` argument names are LOCAL names — never column refs.
//!
//! Jinja handling (raw-SQL fallback only — compiled SQL is already
//! rendered): `{{ ref('x') }}` / `{{ source('a','b') }}` render to the quoted
//! relation names dbt would emit, other `{{ }}` expressions render as
//! `NULL`, `{% %}` tags drop so `{% if %}…{% endif %}` bodies keep their SQL
//! (union coverage of every branch — the same answer some warehouse
//! configuration would give), `{# #}` comments drop. Anything the render or
//! the dialect parse can't handle falls back to the name-match resolver.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use sqlparser::ast::{
    AccessExpr, Distinct, ExcludeSelectItem, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentClause, FunctionArguments, GroupByExpr, Ident, InTarget, Join, JoinConstraint,
    JoinOperator, NamedWindowExpr, ObjectName, ObjectNamePart, OrderByKind, Query,
    RenameSelectItem, Select, SelectItem, SelectItemQualifiedWildcardKind, SetExpr, SetOperator,
    SetQuantifier, Statement, TableAliasColumnDef, TableFactor, TableWithJoins,
    WildcardAdditionalOptions, WindowSpec, WindowType,
};
use sqlparser::dialect::{
    BigQueryDialect, ClickHouseDialect, DatabricksDialect, Dialect, DuckDbDialect, GenericDialect,
    HiveDialect, MsSqlDialect, MySqlDialect, OracleDialect, PostgreSqlDialect, RedshiftSqlDialect,
    SQLiteDialect, SnowflakeDialect, SparkSqlDialect, TeradataDialect,
};
use sqlparser::parser::Parser;

use crate::catalog::Catalog;
use crate::dag::Dag;

use super::{
    dedup_columns, normalize_column, resolution_sql, ColumnEdge, ColumnEdgeKind, ColumnRef,
    ColumnResolver, Confidence, NameMatchResolver,
};

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

    /// The manifest's `adapter_type` when it doesn't name a known dialect —
    /// [`from_dag`](Self::from_dag) maps it to `Generic` and the caller
    /// decides how to surface the warning (a notice/toast in the TUI,
    /// stderr in the CLI). `None` when the adapter is absent or recognized.
    pub fn unrecognized_adapter(dag: &Dag) -> Option<&str> {
        dag.adapter_type().filter(|a| Self::parse_name(a).is_none())
    }

    /// The concrete `sqlparser` dialect object.
    fn sqlparser(self) -> Box<dyn Dialect> {
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
// Jinja render — compiled-level text from raw model SQL
// ---------------------------------------------------------------------------

/// What `{{ this }}` renders to: parses as a normal identifier and never
/// matches a parent, so self-references contribute no edges instead of
/// killing the whole parse.
const THIS_RELATION: &str = "\"__dbtl_this__\"";

/// Turn raw dbt SQL into something a plain-SQL parser can read:
/// `ref()`/`source()` render to quoted relation names, other `{{ }}` render
/// as `NULL`, `{% %}`/`{# #}` drop. Compiled SQL needs nothing (dbt already
/// rendered it) — this only runs on the raw fallback text, where the
/// approximation is documented.
///
/// Markers are only recognized OUTSIDE SQL string literals and quoted
/// identifiers: `'{{ x' AND y = 1 AND 'z }}'` would otherwise pair the
/// in-literal `{{` with the in-literal `}}` and eat the predicate between
/// them (real Jinja *would* render inside a SQL string, but a `{{`/`}}`
/// pair spanning two separate literals is always our scanner misreading —
/// shielding them keeps the surrounding SQL intact).
fn render_jinja(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + 16);
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            // `-- …` line comments and `/* … */` block comments pass
            // through: a `{{` inside one must not pair with a `}}` in later
            // real code (the same cross-region mispairing as literals).
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2);
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            // `'…'` string literals and `"…"`/`` `…` `` quoted identifiers
            // pass through verbatim (doubled-quote escapes keep them open)
            // — a `{`-pair inside is literal text, never a marker.
            q @ (b'\'' | b'"' | b'`') => {
                out.push(q);
                i += 1;
                while i < b.len() {
                    out.push(b[i]);
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            out.push(q); // '' escape stays inside the literal
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            // Dollar-quoted strings (`$$…$$` / `$tag$…$tag$`) pass through —
            // the same marker-shielding rule as other literals.
            b'$' => match dollar_quote_open(b, i) {
                Some(dlen) => {
                    let from = i + dlen;
                    let end =
                        dollar_quote_close(b, from, &b[i..i + dlen]).map_or(b.len(), |c| c + dlen);
                    out.extend_from_slice(&b[i..end]);
                    i = end;
                }
                None => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            b'{' if matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) => {
                let kind = b[i + 1];
                let close: &[u8] = match kind {
                    b'{' => b"}}",
                    b'%' => b"%}",
                    _ => b"#}",
                };
                // Same depth tracking as v0's masker: `{{ a({'x':1}) }}` doesn't
                // close at the inner `}}`. `{%`/`{#` close at the first tag end.
                let tracked = kind == b'{';
                let mut depth = 0usize;
                let mut end = None;
                let mut j = i + 2;
                while j + 1 < b.len() {
                    if b[j] == close[0] && b[j + 1] == close[1] && (!tracked || depth == 0) {
                        end = Some(j);
                        break;
                    }
                    match b[j] {
                        b'(' | b'[' | b'{' if tracked => depth += 1,
                        b')' | b']' | b'}' if tracked => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    j += 1;
                }
                match end {
                    Some(e) => {
                        match kind {
                            b'{' => {
                                let inner = &sql[i + 2..e];
                                out.extend_from_slice(render_jinja_expr(inner).as_bytes());
                            }
                            // `{% … %}` tags and `{# … #}` comments drop
                            // entirely — the tag's body (if/for branches)
                            // keeps flowing into the output. A
                            // `{% else %}`/`{% elif %}` branch split becomes
                            // a `,` instead of a space: inside a select list
                            // `a , b` covers BOTH branches as separate
                            // items (the documented branch union), where
                            // `a b` would parse as `a AS b` — the false
                            // branch's name stealing the true branch's
                            // value. An adjacent literal comma (the
                            // `{% if %}, b{% else %}, c{% endif %}`
                            // conditional-column idiom) already separates
                            // the items, so a space there — and clause-level
                            // splits (`select 1 , select 2`) just fail to
                            // parse → the safe per-model fallback.
                            b'%' => {
                                let split = jinja_branch_split(&sql[i + 2..e]);
                                let literal_comma = split
                                    && (out.iter().rev().find(|c| !c.is_ascii_whitespace())
                                        == Some(&b',')
                                        || b[e + 2..].iter().find(|c| !c.is_ascii_whitespace())
                                            == Some(&b','));
                                out.push(if split && !literal_comma { b',' } else { b' ' });
                            }
                            _ => out.push(b' '),
                        }
                        i = e + 2;
                    }
                    // An unterminated marker is malformed Jinja: emit the
                    // remainder verbatim rather than eating it — inside a
                    // SQL comment it stays invisible to the parser; in real
                    // SQL it fails the parse and the model falls back to
                    // name-match. Swallowing the tail would fabricate a
                    // shorter query that parses to wrong edges.
                    None => {
                        out.extend_from_slice(&b[i..]);
                        i = b.len();
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    // Jinja interiors are ASCII for anything we render; copied bytes keep
    // the original UTF-8 untouched.
    String::from_utf8(out).unwrap_or_default()
}

/// Is `body` (the inside of a `{% … %}` tag, whitespace-control dashes and
/// spaces stripped) a Jinja branch split — `else`, `elif`, or `elseif`? The
/// keyword must be followed by nothing or a non-identifier char so
/// `{%- else -%}` matches but an identifier like `elsewhere` can't.
fn jinja_branch_split(body: &str) -> bool {
    let body = body.trim_matches(|c: char| c.is_whitespace() || c == '-' || c == '+');
    ["elseif", "else", "elif"].iter().any(|kw| {
        body.strip_prefix(kw).is_some_and(|rest| {
            rest.is_empty()
                || !rest
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

/// If `b[i]` opens a dollar-quoted string, the opening delimiter's byte
/// length (`$$` → 2, `$tag$` → tag length + 2). Tags follow identifier
/// rules (no leading digit) so a Postgres `$1` positional parameter or a
/// `$`-identifier never opens one.
fn dollar_quote_open(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        if j == i + 1 && b[j].is_ascii_digit() {
            return None;
        }
        j += 1;
    }
    (j < b.len() && b[j] == b'$').then_some(j - i + 1)
}

/// The byte offset where the dollar-quoted string opened at `from - delim.len()`
/// closes — the START of the closing delimiter, `None` when unterminated
/// (the literal then runs to EOF, matching Postgres).
fn dollar_quote_close(b: &[u8], from: usize, delim: &[u8]) -> Option<usize> {
    b[from.min(b.len())..]
        .windows(delim.len())
        .position(|w| w == delim)
        .map(|p| from + p)
}

/// What a `{{ … }}` expression renders to. `config(...)` disappears (it
/// emits no SQL); `ref()`/`source()` emit their quoted relation name(s);
/// `this` a fake relation; everything else (`{{ var(...) }}`, macro calls,
/// `{{ col }}`…) → `NULL`, an expression that yields no column refs.
fn render_jinja_expr(inner: &str) -> String {
    let e = inner
        .trim()
        .trim_start_matches(['-', '+', '~'])
        .trim_end_matches(['-', '+', '~'])
        .trim();
    if e.starts_with("config") && e[6..].trim_start().starts_with('(') {
        return " ".to_string();
    }
    if let Some(args) = jinja_call_args(e, "ref") {
        // ref('model') / ref("pkg","model") — the LAST literal arg is the
        // model name (a two-arg ref's first arg is the package).
        if let Some(name) = args.last() {
            return quoted_ident(name);
        }
    }
    if let Some(args) = jinja_call_args(e, "source") {
        // source('src','tbl') → "src"."tbl" — the two-arg form dbt renders.
        if args.len() >= 2 {
            return format!(
                "{}.{}",
                quoted_ident(&args[args.len() - 2]),
                quoted_ident(&args[args.len() - 1])
            );
        }
    }
    if e == "this" || e.starts_with("this.") {
        return THIS_RELATION.to_string();
    }
    " NULL ".to_string()
}

/// Double-quote an identifier for the rendered SQL (inner `"`s doubled).
fn quoted_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The string-literal arguments of a `name('a','b',…)` call at the start of
/// `e` — just enough to read `ref`/`source` argument lists. Non-literal
/// args (`var('x')`, identifiers) contribute nothing.
fn jinja_call_args(e: &str, fname: &str) -> Option<Vec<String>> {
    let rest = e.strip_prefix(fname)?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let mut args = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' || c == b'"' {
            let q = c;
            let mut j = i + 1;
            let mut s = String::new();
            while j < bytes.len() {
                if bytes[j] == q {
                    if bytes.get(j + 1) == Some(&q) {
                        s.push(q as char);
                        j += 2;
                        continue;
                    }
                    break;
                }
                s.push(bytes[j] as char);
                j += 1;
            }
            args.push(s);
            i = j + 1;
        } else if c == b')' {
            break;
        } else {
            i += 1;
        }
    }
    Some(args)
}

// ---------------------------------------------------------------------------
// Scope model
// ---------------------------------------------------------------------------

/// `(parent uid, parent column)` — the resolved provenance of a reference.
type ColSources = BTreeSet<(String, String)>;

/// No lambda parameters in force — the shared empty `locals` set for
/// every [`RefScope`]/[`Env`] outside a lambda body.
static NO_LOCALS: BTreeSet<String> = BTreeSet::new();

/// The placeholder name prefix for unnamed outputs — see
/// [`OutCol::synthetic`]. (A parent column literally named `__col1` still
/// resolves normally; the marker only suppresses INVENTED names.)
const ANON_COL_PREFIX: &str = "__col";

/// One output column of a resolved query.
#[derive(Debug, Clone)]
struct OutCol {
    /// Normalized output name.
    name: String,
    /// The output is a bare column reference (`x`, `t.x`) — possibly renamed
    /// by `AS`. `false` for every computed expression (→ Transform).
    bare: bool,
    /// Parent columns feeding this output.
    sources: ColSources,
    /// `true` when `name` is an invented `__colN` placeholder for an
    /// unnameable output (a literal, `(expr).*`, …). Synthetic names never
    /// surface in `columns()` or the FilterOrJoin fan — they're positional
    /// bookkeeping only — and they don't pair in `UNION BY NAME` merges.
    synthetic: bool,
}

/// What a SELECT-level construct resolved to: positional output columns
/// plus every parent column consumed in a *row-shaping* position
/// (join/filter/group/sort — the `FilterOrJoin` pool).
#[derive(Debug, Clone, Default)]
struct QueryRes {
    out: Vec<OutCol>,
    foj: ColSources,
}

/// What a name in the FROM scope stands for.
#[derive(Debug, Clone)]
enum TableSrc {
    /// A direct parent node of the model being resolved (its uid).
    Parent(String),
    /// A CTE or derived table — resolved to its own output columns, each
    /// carrying its parent-column sources transitively.
    Derived(Rc<QueryRes>),
    /// A name we can't map to a parent (an un-resolved relation, a table
    /// function…): columns off it contribute nothing.
    External,
}

/// One FROM-visible name → what it binds.
#[derive(Debug, Clone)]
struct Binding {
    /// The name SQL sees: the alias when present, else the relation's last
    /// dotted segment.
    name: String,
    src: TableSrc,
}

/// The FROM bindings of one query level, chained to the enclosing query's
/// scope for correlated subqueries, plus its `WINDOW w AS (…)` definitions
/// (select-local, so they live per scope level).
#[derive(Debug, Default)]
struct Scope<'a> {
    bindings: Vec<Binding>,
    /// `WINDOW name AS spec` — looked up by `OVER name`.
    windows: HashMap<String, WindowSpec>,
    outer: Option<&'a Scope<'a>>,
}

/// The environment visible while resolving a query: the outer scope for
/// correlation, the CTE names in force, and the enclosing lambda
/// parameters (so a lambda body inside a subquery still shadows).
#[derive(Clone, Copy)]
struct Env<'a> {
    outer: Option<&'a Scope<'a>>,
    ctes: &'a HashMap<String, Rc<QueryRes>>,
    locals: &'a BTreeSet<String>,
}

/// The context threaded through *expression* resolution: the scope chain
/// (column binding + correlation), the CTE table for subqueries that
/// reference `WITH` names, and the lambda-parameter names in force — those
/// shadow same-named columns AND binding names inside the lambda body.
#[derive(Clone, Copy)]
struct RefScope<'a> {
    scope: Option<&'a Scope<'a>>,
    ctes: &'a HashMap<String, Rc<QueryRes>>,
    locals: &'a BTreeSet<String>,
}

/// One direct parent of the model being resolved: the names SQL can call it
/// (node name, dbt `alias`/`identifier` override, uid tail) and the
/// qualifiers (`schema`.`relation`, `database`.`schema`.`relation`, uid
/// segments) that disambiguate it when several parents share a relation
/// name.
#[derive(Debug)]
struct Parent {
    uid: String,
    /// Normalized names that match this parent in FROM position.
    names: BTreeSet<String>,
    /// Normalized schema/database/uid segments usable as qualifier evidence.
    qualifiers: BTreeSet<String>,
}

/// The result of analyzing one model's SQL: its top-level scope (for the
/// declared-column fallback probe), its output columns, and its
/// filter/join source set.
#[derive(Debug)]
struct Analyzed {
    scope: Vec<Binding>,
    out: Vec<OutCol>,
    foj: ColSources,
    /// `true` when an output-producing projection contained a `*`/`t.*` —
    /// the only way a declared column can be carried without appearing in
    /// `out` (behind a wildcard no binding's column list could expand).
    /// Without one, every output is explicitly named and probing an
    /// unproduced declared name can only fabricate edges.
    unbounded_wildcard: bool,
}

/// Per-query context: the `Dag`, the model's direct-parent matcher table,
/// and the recursion budget (a `Cell` so all walkers share `&Ctx`).
struct Ctx<'a> {
    dag: &'a Dag,
    parents: &'a [Parent],
    depth: Cell<usize>,
}

/// The [`RefScope`] for expressions in `env` resolved against `scope`
/// (a one-line convenience — `scope` reborrows `&mut` as `&` here).
fn ref_scope<'a>(env: Env<'a>, scope: &'a Scope<'a>) -> RefScope<'a> {
    RefScope {
        scope: Some(scope),
        ctes: env.ctes,
        locals: env.locals,
    }
}

// ---------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------

/// The Issue 08 resolver: parses each model's SQL (compiled first, raw as
/// the documented fallback) with `sqlparser`, resolves column references
/// against a real FROM/JOIN/CTE scope, and classifies edges passthrough /
/// rename / transform / filter-or-join. Per-model degradation to
/// [`NameMatchResolver`] on unparseable or absent SQL.
pub struct SqlParseResolver {
    catalog: Option<Rc<Catalog>>,
    dialect: SqlDialect,
    /// The v0 resolver — the per-model fallback, and the column-list source
    /// for parents whose own analysis failed.
    fallback: NameMatchResolver,
    /// uid → analyzed model (`None` = analyzed and failed → fallback path).
    analyzed_memo: RefCell<BTreeMap<String, Option<Rc<Analyzed>>>>,
    /// uid → `resolve` answer (memoized like `NameMatchResolver`'s).
    resolved_memo: RefCell<BTreeMap<String, Rc<Vec<ColumnEdge>>>>,
    /// uid → the node's *column list* (`Some` = known, `None` = unknown).
    cols_memo: RefCell<BTreeMap<String, Option<Rc<Vec<String>>>>>,
    /// uids whose resolution went through `NameMatchResolver` — the
    /// `[name-match]` tag / CLI fallback count source.
    fallbacks: RefCell<BTreeSet<String>>,
    /// In-flight `analyze` stack: a pathological manifest with a uid cycle
    /// degrades the re-entered node to "unresolved" instead of recursing.
    analyzing: RefCell<BTreeSet<String>>,
}

impl SqlParseResolver {
    /// A parser-backed resolver for `dialect`. `catalog` is the shared
    /// warehouse column truth (first priority like v0).
    pub fn new(catalog: Option<Rc<Catalog>>, dialect: SqlDialect) -> Self {
        SqlParseResolver {
            catalog: catalog.clone(),
            dialect,
            fallback: NameMatchResolver::new(catalog),
            analyzed_memo: RefCell::new(BTreeMap::new()),
            resolved_memo: RefCell::new(BTreeMap::new()),
            cols_memo: RefCell::new(BTreeMap::new()),
            fallbacks: RefCell::new(BTreeSet::new()),
            analyzing: RefCell::new(BTreeSet::new()),
        }
    }

    /// A resolver whose dialect comes from `dag`'s adapter metadata — the
    /// shared "auto" path for the TUI and `dbtl columns` (CLI overrides
    /// pre-select `SqlDialect` before reaching here).
    pub fn for_dag(catalog: Option<Rc<Catalog>>, dag: &Dag) -> Self {
        Self::new(catalog, SqlDialect::from_dag(dag))
    }

    /// The configured dialect (status surfaces show it for context).
    pub fn dialect(&self) -> SqlDialect {
        self.dialect
    }

    /// How many nodes fell back to name-match resolution so far.
    pub fn fallback_count(&self) -> usize {
        self.fallbacks.borrow().len()
    }

    /// The uids that fell back to name-match resolution (sorted).
    pub fn fallback_nodes(&self) -> Vec<String> {
        self.fallbacks.borrow().iter().cloned().collect()
    }

    /// Is `node`'s resolution the name-match approximation?
    pub fn is_fallback(&self, node: &str) -> bool {
        self.fallbacks.borrow().contains(node)
    }

    // -- column lists -------------------------------------------------------

    /// The node's column list, or `None` when nothing can name it.
    /// Precedence: catalog → documented → its own analyzed outputs →
    /// name-match extraction (the same "last resort" v0 uses).
    fn col_set(&self, dag: &Dag, uid: &str) -> Option<Rc<Vec<String>>> {
        if let Some(v) = self.cols_memo.borrow().get(uid) {
            return v.clone();
        }
        let out = self.compute_col_set(dag, uid);
        self.cols_memo
            .borrow_mut()
            .insert(uid.to_string(), out.clone());
        out
    }

    fn compute_col_set(&self, dag: &Dag, uid: &str) -> Option<Rc<Vec<String>>> {
        if let Some(cat) = &self.catalog {
            if let Some(n) = cat.get(uid) {
                if !n.columns.is_empty() {
                    let mut cols: Vec<String> =
                        n.columns.keys().map(|k| normalize_column(k)).collect();
                    dedup_columns(&mut cols);
                    return Some(Rc::new(cols));
                }
            }
        }
        if let Some(d) = dag.detail(uid) {
            if !d.columns.is_empty() {
                let mut cols: Vec<String> = d
                    .columns
                    .iter()
                    .map(|c| normalize_column(&c.name))
                    .collect();
                dedup_columns(&mut cols);
                return Some(Rc::new(cols));
            }
        }
        let analyzed = self.analyzed(dag, uid);
        if let Some(an) = &analyzed {
            if an.out.iter().any(|o| !o.synthetic) {
                return Some(Rc::new(
                    an.out
                        .iter()
                        .filter(|o| !o.synthetic)
                        .map(|o| o.name.clone())
                        .collect(),
                ));
            }
        }
        if analyzed.is_none() && resolution_sql(dag, uid).is_some() {
            // Same flag `compute_resolve` sets: a node whose SQL exists but
            // didn't parse IS a fallback node — including when only its
            // `columns()` path took the name-match route (`resolve` never
            // invoked).
            self.fallbacks.borrow_mut().insert(uid.to_string());
        }
        let cols = self.fallback.columns(dag, uid);
        (!cols.is_empty()).then(|| Rc::new(cols))
    }

    // -- the analyze pass ---------------------------------------------------

    /// The model's analyzed SQL, memoized (`None` = couldn't analyze → the
    /// caller takes the name-match path). Cycle-guarded.
    ///
    /// Recursion budget: `Ctx::depth` (256) bounds nesting INSIDE one
    /// model's query tree; the `analyzing` set bounds recursion ACROSS
    /// models (`resolve_in_binding` → `col_set(parent)` → `analyzed`) to
    /// one frame per DAG edge along a linear chain — memoized results and
    /// the cycle guard make it proportional to path length, not graph size.
    fn analyzed(&self, dag: &Dag, node: &str) -> Option<Rc<Analyzed>> {
        if let Some(v) = self.analyzed_memo.borrow().get(node) {
            return v.clone();
        }
        let out = self.compute_analyzed(dag, node);
        self.analyzed_memo
            .borrow_mut()
            .insert(node.to_string(), out.clone());
        out
    }

    fn compute_analyzed(&self, dag: &Dag, node: &str) -> Option<Rc<Analyzed>> {
        let (text, _) = resolution_sql(dag, node)?;
        if !self.analyzing.borrow_mut().insert(node.to_string()) {
            return None; // uid cycle — degrade the re-entered node
        }
        let out = (|| {
            let sql = render_jinja(text);
            let dialect = self.dialect.sqlparser();
            let stmts = Parser::parse_sql(&*dialect, &sql).ok()?;
            let query = stmts.iter().find_map(|s| match s {
                Statement::Query(q) => Some(&**q),
                _ => None,
            })?;
            let parents = Self::build_parents(dag, node);
            let ctx = Ctx {
                dag,
                parents: &parents,
                depth: Cell::new(0),
            };
            let ctes = HashMap::new();
            let env = Env {
                outer: None,
                ctes: &ctes,
                locals: &NO_LOCALS,
            };
            let (res, scope) = self.resolve_query_full(&ctx, env, query)?;
            Some(Rc::new(Analyzed {
                scope,
                out: res.out,
                foj: res.foj,
                unbounded_wildcard: query_has_output_wildcard(query),
            }))
        })();
        self.analyzing.borrow_mut().remove(node);
        out
    }

    /// The node's direct parents as [`Parent`] matchers: relation names the
    /// compiled SQL can print — dbt renders `ref('m')` to the model's
    /// *alias* when set, `source()` to the source's `identifier`, plus the
    /// plain names for hand-written/raw SQL.
    fn build_parents(dag: &Dag, node: &str) -> Vec<Parent> {
        let mut parents: Vec<String> = dag.direct_parents(node).to_vec();
        parents.sort();
        parents
            .into_iter()
            .map(|uid| {
                let mut names = BTreeSet::new();
                let mut qualifiers = BTreeSet::new();
                if let Some(info) = dag.nodes().get(&uid) {
                    names.insert(normalize_column(&info.name));
                }
                if let Some(d) = dag.detail(&uid) {
                    if let Some(rel) = &d.relation_name {
                        names.insert(normalize_column(rel));
                    }
                    for q in [&d.database, &d.schema].into_iter().flatten() {
                        qualifiers.insert(normalize_column(q));
                    }
                }
                // uid segments (`model.pkg.name` / `source.proj.src.tbl`) are
                // stable qualifiers too — `schema.tbl` in compiled SQL often
                // matches several parents sharing the relation name.
                for seg in uid.split('.') {
                    qualifiers.insert(normalize_column(seg));
                }
                // The uid's last segment is also a usable relation name —
                // dbt emits `schema.name` when no alias override is set.
                if let Some(last) = uid.rsplit('.').next() {
                    names.insert(normalize_column(last));
                }
                Parent {
                    uid,
                    names,
                    qualifiers,
                }
            })
            .collect()
    }

    /// Which parent does the (normalized, dotted) relation `parts` name?
    /// Candidates carry `parts`' last segment in `names`; when several
    /// match, the earlier segments (`db.sch.rel`, `sch.rel`) score against
    /// each parent's qualifiers — best score, then smallest uid.
    fn match_parent<'p>(parents: &'p [Parent], parts: &[String]) -> Option<&'p Parent> {
        let last = parts.last()?;
        let cands: Vec<&Parent> = parents.iter().filter(|p| p.names.contains(last)).collect();
        match cands.len() {
            0 => None,
            1 => Some(cands[0]),
            _ => {
                let quals = &parts[..parts.len() - 1];
                cands.into_iter().max_by(|a, b| {
                    let score =
                        |p: &Parent| quals.iter().filter(|q| p.qualifiers.contains(*q)).count();
                    score(a)
                        .cmp(&score(b))
                        // Equal scores → the smaller uid wins (deterministic).
                        .then_with(|| b.uid.cmp(&a.uid))
                })
            }
        }
    }

    // -- query / select resolution ------------------------------------------

    /// Resolve a `Query` (WITH + body + ORDER BY), returning the body's
    /// result AND the outermost select's scope bindings (needed by the
    /// declared-column probe at the model level; nested calls drop them).
    fn resolve_query_full(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        q: &Query,
    ) -> Option<(QueryRes, Vec<Binding>)> {
        // Cumulative sub-resolution budget per model — pathological nesting
        // can't spin forever; exceeding it fails the whole model into the
        // name-match fallback.
        ctx.depth.set(ctx.depth.get() + 1);
        if ctx.depth.get() > 256 {
            return None;
        }
        let mut ctes: HashMap<String, Rc<QueryRes>> = env.ctes.clone();
        if let Some(with) = &q.with {
            for cte in &with.cte_tables {
                // A CTE sees the env's CTEs plus the earlier ones — resolve
                // BEFORE inserting itself so `WITH r AS (SELECT … FROM r)`
                // can't resolve its own name (recursive CTE → External).
                let sub = self.resolve_query(
                    ctx,
                    Env {
                        outer: env.outer,
                        ctes: &ctes,
                        locals: env.locals,
                    },
                    &cte.query,
                )?;
                let sub = rename_outputs(sub, &cte.alias.columns);
                ctes.insert(norm_ident(&cte.alias.name), Rc::new(sub));
            }
        }
        let inner = Env {
            outer: env.outer,
            ctes: &ctes,
            locals: env.locals,
        };
        let (mut res, scope) = match &*q.body {
            SetExpr::Select(sel) => self.resolve_select(ctx, inner, sel)?,
            body => (self.resolve_set_expr(ctx, inner, body)?, Vec::new()),
        };
        // Query-level tail clauses all shape the row set → FilterOrJoin.
        // ORDER BY / LIMIT BY can see BOTH the body's outputs (an
        // unqualified `order by <alias>` binds the alias, shadowing a
        // same-named FROM column) and the body's FROM scope (a qualified
        // or never-projected column) — resolve exprs against a probe scope
        // of the returned bindings, falling back to the output-name match
        // when nothing in the FROM scope claims the ref.
        let probe = Scope {
            bindings: scope.clone(),
            windows: HashMap::new(),
            outer: env.outer,
        };
        let probe_refs = RefScope {
            scope: Some(&probe),
            ctes: inner.ctes,
            locals: env.locals,
        };
        if let Some(ob) = &q.order_by {
            if let OrderByKind::Expressions(exprs) = &ob.kind {
                for obe in exprs {
                    let ex = &obe.expr;
                    // `ORDER BY <alias>` (a bare identifier naming an
                    // output) binds the alias — SQL shadows the FROM column.
                    if let Expr::Identifier(i) = ex {
                        if let Some(o) = res.out.iter().find(|o| o.name == norm_ident(i)) {
                            res.foj.extend(o.sources.iter().cloned());
                            continue;
                        }
                    }
                    let mut s = self.expr_refs(ctx, probe_refs, ex)?;
                    if s.is_empty() {
                        for name in expr_ident_names(ex) {
                            if let Some(o) = res.out.iter().find(|o| o.name == name) {
                                s.extend(o.sources.iter().cloned());
                            }
                        }
                    }
                    res.foj.extend(s);
                    if let Some(wf) = &obe.with_fill {
                        for e in [&wf.from, &wf.to, &wf.step].into_iter().flatten() {
                            res.foj.extend(self.expr_refs(ctx, probe_refs, e)?);
                        }
                    }
                }
            }
        }
        if let Some(lc) = &q.limit_clause {
            match lc {
                sqlparser::ast::LimitClause::LimitOffset {
                    limit,
                    offset,
                    limit_by,
                } => {
                    for e in limit.iter().chain(limit_by.iter()) {
                        let mut s = self.expr_refs(ctx, probe_refs, e)?;
                        if s.is_empty() {
                            // `LIMIT BY` groups on select aliases too —
                            // the same output-name fallback as ORDER BY.
                            for name in expr_ident_names(e) {
                                if let Some(o) = res.out.iter().find(|o| o.name == name) {
                                    s.extend(o.sources.iter().cloned());
                                }
                            }
                        }
                        res.foj.extend(s);
                    }
                    if let Some(off) = offset {
                        res.foj.extend(self.expr_refs(ctx, probe_refs, &off.value)?);
                    }
                }
                sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                    for e in [offset, limit] {
                        res.foj.extend(self.expr_refs(ctx, probe_refs, e)?);
                    }
                }
            }
        }
        if let Some(f) = &q.fetch {
            if let Some(qe) = &f.quantity {
                res.foj.extend(self.expr_refs(ctx, probe_refs, qe)?);
            }
        }
        if let Some(settings) = &q.settings {
            for s in settings {
                res.foj.extend(self.expr_refs(ctx, probe_refs, &s.value)?);
            }
        }
        // `for_clause`/`format_clause`/`locks` carry no column expressions
        // (output-format and lock-mode decorations) — nothing to walk.
        self.resolve_pipe_operators(ctx, inner, &mut res, &q.pipe_operators)?;
        Some((res, scope))
    }

    fn resolve_query(&self, ctx: &Ctx<'_>, env: Env<'_>, q: &Query) -> Option<QueryRes> {
        self.resolve_query_full(ctx, env, q).map(|(r, _)| r)
    }

    fn resolve_set_expr(&self, ctx: &Ctx<'_>, env: Env<'_>, e: &SetExpr) -> Option<QueryRes> {
        match e {
            SetExpr::Select(sel) => self.resolve_select(ctx, env, sel).map(|(r, _)| r),
            SetExpr::Query(q) => self.resolve_query(ctx, env, q),
            SetExpr::SetOperation {
                left,
                right,
                op,
                set_quantifier,
            } => {
                let l = self.resolve_set_expr(ctx, env, left)?;
                let r = self.resolve_set_expr(ctx, env, right)?;
                if !matches!(op, SetOperator::Union) {
                    // EXCEPT / INTERSECT / MINUS: the right arm only shapes
                    // WHICH of the left arm's rows survive — its refs join
                    // the row-shaping pool (never value edges) and the
                    // outputs come from the left arm alone.
                    let mut foj = l.foj;
                    foj.extend(r.foj.iter().cloned());
                    for o in &r.out {
                        foj.extend(o.sources.iter().cloned());
                    }
                    return Some(QueryRes { out: l.out, foj });
                }
                // UNION: positional merge — output i carries every arm's
                // column-i sources; name/bare come from the left arm when
                // present (SQL's own rule). `BY NAME` quantifiers pair on
                // output names instead of position (DuckDB) — a
                // single-arm name stands alone.
                let by_name = matches!(
                    set_quantifier,
                    SetQuantifier::ByName
                        | SetQuantifier::AllByName
                        | SetQuantifier::DistinctByName
                );
                let mut out = Vec::new();
                if by_name {
                    for a in &l.out {
                        match r
                            .out
                            .iter()
                            .find(|b| !a.synthetic && !b.synthetic && b.name == a.name)
                        {
                            Some(b) => out.push(union_out(a, b)),
                            None => out.push(a.clone()),
                        }
                    }
                    for b in &r.out {
                        if l.out
                            .iter()
                            .all(|a| a.synthetic || b.synthetic || a.name != b.name)
                        {
                            out.push(b.clone());
                        }
                    }
                } else {
                    for i in 0..l.out.len().max(r.out.len()) {
                        match (l.out.get(i), r.out.get(i)) {
                            (Some(a), Some(b)) => out.push(union_out(a, b)),
                            (Some(a), None) => out.push(a.clone()),
                            (None, Some(b)) => out.push(b.clone()),
                            (None, None) => {}
                        }
                    }
                }
                Some(QueryRes {
                    out,
                    foj: l.foj.union(&r.foj).cloned().collect(),
                })
            }
            SetExpr::Values(v) => {
                // VALUES rows: literal columns (columnN) with no sources; the
                // row expressions' refs still count as row-shaping inputs.
                let mut foj = ColSources::new();
                let refs = RefScope {
                    scope: env.outer,
                    ctes: env.ctes,
                    locals: env.locals,
                };
                for row in &v.rows {
                    for e in &row.content {
                        foj.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
                let width = v.rows.first().map(|r| r.content.len()).unwrap_or(0);
                let out = (1..=width)
                    .map(|i| OutCol {
                        name: format!("column{i}"),
                        bare: false,
                        sources: ColSources::new(),
                        synthetic: false,
                    })
                    .collect();
                Some(QueryRes { out, foj })
            }
            SetExpr::Table(t) => {
                // `TABLE [sch.]name` — the whole relation's column list is
                // the output (CTE names resolve like a FROM reference).
                let mut parts: Vec<String> = Vec::new();
                if let Some(s) = &t.schema_name {
                    parts.push(norm_ident(s));
                }
                if let Some(n) = &t.table_name {
                    parts.push(norm_ident(n));
                }
                let mut out = Vec::new();
                let mut foj = ColSources::new();
                if let Some(last) = parts.last() {
                    if parts.len() == 1 && env.ctes.get(last).is_some() {
                        // CTE names are unqualified — `TABLE c` reads `c`.
                        let q = env.ctes.get(last).expect("checked above");
                        out = q.out.clone();
                        foj.extend(q.foj.iter().cloned());
                    } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                        if let Some(cs) = self.col_set(ctx.dag, &p.uid) {
                            for c in cs.iter() {
                                out.push(OutCol {
                                    name: c.clone(),
                                    bare: true,
                                    sources: BTreeSet::from([(p.uid.clone(), c.clone())]),
                                    synthetic: false,
                                });
                            }
                        }
                    }
                }
                Some(QueryRes { out, foj })
            }
            // Insert/Update/Delete/Merge bodies don't describe a model's
            // SELECT — nothing to resolve (documented non-statement).
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
                Some(QueryRes::default())
            }
        }
    }

    /// Apply the `|>` pipe-operator chain (DuckDB/BigQuery pipe syntax) to
    /// the running result `res`, in order. The pipe input is the only
    /// visible table: a synthetic Derived binding exposes the current
    /// `res.out` so unqualified refs read the CURRENT outputs (renames /
    /// `SELECT` lists keep their sources); `|> AS` names that binding for
    /// qualified refs and `|> JOIN` adds real bindings beside it.
    ///
    /// Documented approximations: `|> PIVOT`/`UNPIVOT`/`CALL` reshape the
    /// result in ways no column list can describe, so the output collapses
    /// to one synthetic column carrying every prior source (the whole
    /// input provably feeds it); `|> TABLESAMPLE` carries no column exprs.
    fn resolve_pipe_operators(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        res: &mut QueryRes,
        ops: &[sqlparser::ast::PipeOperator],
    ) -> Option<()> {
        use sqlparser::ast::PipeOperator as P;
        // Slot 0 is the pipe input; `|> JOIN` appends bindings beside it.
        let mut scope = Scope {
            bindings: vec![Binding {
                name: String::new(),
                src: TableSrc::Derived(Rc::new(res.clone())),
            }],
            windows: HashMap::new(),
            outer: env.outer,
        };
        for po in ops {
            let refs = ref_scope(env, &scope);
            match po {
                P::Where { expr } => {
                    res.foj.extend(self.expr_refs(ctx, refs, expr)?);
                }
                P::OrderBy { exprs } => {
                    for e in exprs {
                        res.foj.extend(self.expr_refs(ctx, refs, &e.expr)?);
                    }
                }
                P::Limit { expr, offset } => {
                    for e in [expr].into_iter().chain(offset.iter()) {
                        res.foj.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
                P::Select { exprs } => {
                    let mut out = Vec::new();
                    for item in exprs {
                        self.resolve_select_item(ctx, refs, &scope, item, &mut out)?;
                    }
                    res.out = out;
                }
                P::Extend { exprs } => {
                    for item in exprs {
                        self.resolve_select_item(ctx, refs, &scope, item, &mut res.out)?;
                    }
                }
                P::Set { assignments } => {
                    for a in assignments {
                        let sources = self.expr_refs(ctx, refs, &a.value)?;
                        let names: Vec<String> = match &a.target {
                            sqlparser::ast::AssignmentTarget::ColumnName(on) => {
                                object_name_parts(on)
                                    .into_iter()
                                    .last()
                                    .into_iter()
                                    .collect()
                            }
                            sqlparser::ast::AssignmentTarget::Tuple(ons) => ons
                                .iter()
                                .filter_map(|on| object_name_parts(on).into_iter().last())
                                .collect(),
                        };
                        for n in names {
                            if let Some(o) = res.out.iter_mut().find(|o| o.name == n) {
                                o.sources = sources.clone();
                                o.bare = false;
                            } else {
                                // `SET x = expr` on a non-output name: the
                                // refs were still evaluated — row-shaping.
                                res.foj.extend(sources.iter().cloned());
                            }
                        }
                    }
                }
                P::Drop { columns } => {
                    let drop: BTreeSet<String> = columns.iter().map(norm_ident).collect();
                    res.out.retain(|o| !drop.contains(&o.name));
                }
                P::As { alias } => {
                    scope.bindings[0].name = norm_ident(alias);
                }
                P::Aggregate {
                    full_table_exprs,
                    group_by_expr,
                } => {
                    let mut out = Vec::new();
                    for ewa in full_table_exprs.iter().chain(group_by_expr.iter()) {
                        let sources = self.expr_refs(ctx, refs, &ewa.expr.expr)?;
                        out.push(OutCol {
                            name: ewa
                                .expr
                                .alias
                                .as_ref()
                                .map(norm_ident)
                                .or_else(|| derive_name(&ewa.expr.expr))
                                .unwrap_or_default(),
                            bare: self.projection_bare(ctx, refs, &ewa.expr.expr),
                            sources,
                            synthetic: false,
                        });
                    }
                    res.out = out;
                }
                P::Rename { mappings } => {
                    let pairs: Vec<(String, String)> = mappings
                        .iter()
                        .map(|ia| (norm_ident(&ia.ident), norm_ident(&ia.alias)))
                        .collect();
                    for o in &mut res.out {
                        if let Some((_, to)) = pairs.iter().find(|(from, _)| from == &o.name) {
                            o.name = to.clone();
                            o.synthetic = false;
                        }
                    }
                }
                P::Union {
                    set_quantifier,
                    queries,
                } => {
                    let by_name = matches!(
                        set_quantifier,
                        SetQuantifier::ByName
                            | SetQuantifier::AllByName
                            | SetQuantifier::DistinctByName
                    );
                    for sub in queries {
                        let r = self.resolve_query(ctx, env, sub)?;
                        res.foj.extend(r.foj.iter().cloned());
                        if by_name {
                            for a in &mut res.out {
                                if let Some(b) = r
                                    .out
                                    .iter()
                                    .find(|b| !a.synthetic && !b.synthetic && b.name == a.name)
                                {
                                    a.sources = a.sources.union(&b.sources).cloned().collect();
                                }
                            }
                            for b in r.out {
                                if res
                                    .out
                                    .iter()
                                    .all(|a| a.synthetic || b.synthetic || a.name != b.name)
                                {
                                    res.out.push(b);
                                }
                            }
                        } else {
                            for i in 0..res.out.len().max(r.out.len()) {
                                match (res.out.get(i), r.out.get(i)) {
                                    (Some(a), Some(b)) => res.out[i] = union_out(a, b),
                                    (None, Some(b)) => res.out.push(b.clone()),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                P::Intersect { queries, .. } | P::Except { queries, .. } => {
                    // Like the binary EXCEPT/INTERSECT arms: each right-hand
                    // query only shapes which pipe-input rows survive.
                    for sub in queries {
                        let r = self.resolve_query(ctx, env, sub)?;
                        res.foj.extend(r.foj.iter().cloned());
                        for o in &r.out {
                            res.foj.extend(o.sources.iter().cloned());
                        }
                    }
                }
                P::Join(j) => {
                    // `|> JOIN t ON …` — the relation binds beside the pipe
                    // input; the constraint's refs are row-shaping.
                    self.bind_relation(ctx, env, &mut scope, res, &j.relation)?;
                    let refs = ref_scope(env, &scope);
                    if let JoinOperator::AsOf {
                        match_condition, ..
                    } = &j.join_operator
                    {
                        res.foj.extend(self.expr_refs(ctx, refs, match_condition)?);
                    }
                    match join_constraint(j) {
                        Some(JoinConstraint::On(e)) => {
                            res.foj.extend(self.expr_refs(ctx, refs, e)?);
                        }
                        Some(JoinConstraint::Using(names))
                        | Some(JoinConstraint::UsingAlias(names, _)) => {
                            for on in names {
                                let parts = object_name_parts(on);
                                res.foj.extend(self.resolve_parts(ctx, refs, &parts));
                            }
                        }
                        Some(JoinConstraint::Natural) => {
                            res.foj.extend(self.natural_join_sources(ctx, &scope));
                        }
                        _ => {}
                    }
                }
                P::Call { function, .. } => {
                    res.foj.extend(self.function_refs(ctx, refs, function)?);
                    res.out = vec![collapse_pipe_output(res)];
                }
                P::Pivot {
                    aggregate_functions,
                    value_column,
                    value_source,
                    ..
                } => {
                    for ef in aggregate_functions {
                        res.foj.extend(self.expr_refs(ctx, refs, &ef.expr)?);
                    }
                    for i in value_column {
                        res.foj
                            .extend(self.resolve_parts(ctx, refs, &[norm_ident(i)]));
                    }
                    self.pivot_value_source_refs(ctx, refs, value_source, &mut res.foj)?;
                    res.out = vec![collapse_pipe_output(res)];
                }
                P::Unpivot {
                    value_column,
                    name_column,
                    unpivot_columns,
                    ..
                } => {
                    // The unpivoted columns' sources flow into the value
                    // output; `name_column` carries the names themselves.
                    let drop: BTreeSet<String> = unpivot_columns.iter().map(norm_ident).collect();
                    let mut sources = ColSources::new();
                    res.out.retain(|o| {
                        if drop.contains(&o.name) {
                            sources.extend(o.sources.iter().cloned());
                            false
                        } else {
                            true
                        }
                    });
                    res.out.push(OutCol {
                        name: norm_ident(value_column),
                        bare: false,
                        sources,
                        synthetic: false,
                    });
                    res.out.push(OutCol {
                        name: norm_ident(name_column),
                        bare: false,
                        sources: ColSources::new(),
                        synthetic: false,
                    });
                }
                P::TableSample { .. } => {
                    // Sampling quantities/seeds/buckets are numerics — no
                    // column refs to walk.
                }
            }
            // The input binding re-exposes the (possibly reshaped) outputs.
            scope.bindings[0].src = TableSrc::Derived(Rc::new(res.clone()));
        }
        Some(())
    }

    /// The refs inside a pivot `IN (...)`/`ANY`/`(subquery)` value source —
    /// shared by the `PIVOT` table factor and the `|> PIVOT` pipe operator.
    fn pivot_value_source_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        vs: &sqlparser::ast::PivotValueSource,
        foj: &mut ColSources,
    ) -> Option<()> {
        use sqlparser::ast::PivotValueSource as PVS;
        match vs {
            PVS::List(items) => {
                for it in items {
                    foj.extend(self.expr_refs(ctx, refs, &it.expr)?);
                }
            }
            PVS::Any(order_by) => {
                for ob in order_by {
                    foj.extend(self.expr_refs(ctx, refs, &ob.expr)?);
                }
            }
            PVS::Subquery(q) => {
                foj.extend(self.subquery_refs(ctx, refs, q)?);
            }
        }
        Some(())
    }

    /// Resolve one SELECT: FROM bindings → the row-shaping tail → the
    /// projection's output columns. Returns the FROM scope too (the model
    /// level reuses it for the declared-column probe).
    fn resolve_select(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        sel: &Select,
    ) -> Option<(QueryRes, Vec<Binding>)> {
        let mut scope = Scope {
            bindings: Vec::new(),
            windows: HashMap::new(),
            outer: env.outer,
        };
        let mut res = QueryRes::default();

        // FROM: bind every relation + join, evaluating each ON/USING against
        // the scope *as it stands* (left side already bound).
        for twj in &sel.from {
            self.bind_twj(ctx, env, &mut scope, &mut res, twj)?;
        }

        // Named windows (`WINDOW w AS (…)`) visible to `OVER w` — they live
        // on this scope level so the function walker can find them. A
        // `WINDOW w AS prev` alias chases the earlier def it points at.
        for def in &sel.named_window {
            let spec = match &def.1 {
                NamedWindowExpr::WindowSpec(spec) => Some(spec.clone()),
                NamedWindowExpr::NamedWindow(other) => {
                    scope.windows.get(&norm_ident(other)).cloned()
                }
            };
            if let Some(spec) = spec {
                scope.windows.insert(norm_ident(&def.0), spec);
            }
        }

        // LATERAL VIEWs explode a column into rows — their input expressions
        // are row-shaping, and `… lv AS c1,c2` binds a derived relation of
        // the exploded values (so `lv.c1` / bare `c1` in the projection
        // resolve to the input expr's sources).
        for lv in &sel.lateral_views {
            let lv_refs = {
                let r = ref_scope(env, &scope);
                self.expr_refs(ctx, r, &lv.lateral_view)?
            };
            res.foj.extend(lv_refs.iter().cloned());
            if let Some(name) = object_name_parts(&lv.lateral_view_name).last().cloned() {
                let out = if lv.lateral_col_alias.is_empty() {
                    vec![OutCol {
                        name: String::new(),
                        bare: false,
                        sources: lv_refs,
                        synthetic: true,
                    }]
                } else {
                    lv.lateral_col_alias
                        .iter()
                        .map(|a| OutCol {
                            name: norm_ident(a),
                            bare: false,
                            sources: lv_refs.clone(),
                            synthetic: false,
                        })
                        .collect()
                };
                scope.bindings.push(Binding {
                    name,
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
        }

        let refs = ref_scope(env, &scope);

        // `TOP (n)` — the quantity is row-shaping (usually a literal, but
        // `TOP (expr)` can carry a column ref).
        if let Some(top) = &sel.top {
            if let Some(sqlparser::ast::TopQuantity::Expr(e)) = &top.quantity {
                res.foj.extend(self.expr_refs(ctx, refs, e)?);
            }
        }

        // The row-shaping tail: PREWHERE/WHERE/GROUP BY/HAVING/QUALIFY/
        // CONNECT BY/CLUSTER BY/DISTRIBUTE BY/SORT BY/DISTINCT ON.
        for e in [&sel.prewhere, &sel.selection, &sel.having, &sel.qualify]
            .into_iter()
            .flatten()
        {
            res.foj.extend(self.expr_refs(ctx, refs, e)?);
        }
        if let GroupByExpr::Expressions(exprs, _) = &sel.group_by {
            for e in exprs {
                // `GROUP BY 1` positional ordinals stay unresolved — a
                // literal int never names a column (documented degenerate
                // case; the output at that position is untouched).
                res.foj.extend(self.expr_refs(ctx, refs, e)?);
            }
        }
        for cb in &sel.connect_by {
            match cb {
                sqlparser::ast::ConnectByKind::ConnectBy { relationships, .. } => {
                    for e in relationships {
                        res.foj.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
                sqlparser::ast::ConnectByKind::StartWith { condition, .. } => {
                    res.foj.extend(self.expr_refs(ctx, refs, condition)?);
                }
            }
        }
        for e in sel
            .cluster_by
            .iter()
            .chain(&sel.distribute_by)
            .chain(sel.sort_by.iter().map(|o| &o.expr))
        {
            res.foj.extend(self.expr_refs(ctx, refs, e)?);
        }
        if let Some(Distinct::On(exprs)) = &sel.distinct {
            for e in exprs {
                res.foj.extend(self.expr_refs(ctx, refs, e)?);
            }
        }

        // The projection → output columns. Items resolve against the FROM
        // scope alone: a same-SELECT alias is NOT visible to sibling items
        // (`select p.y as z, z + 1` reads `z` from the FROM scope, matching
        // the SQL standard — dialect-local alias visibility like Postgres'
        // LATERAL isn't modeled).
        let mut out: Vec<OutCol> = Vec::new();
        for item in &sel.projection {
            self.resolve_select_item(ctx, refs, &scope, item, &mut out)?;
        }

        // SELECT-level EXCLUDE (Redshift `SELECT * EXCLUDE(col)` parses at
        // the Select level, not as wildcard options).
        if let Some(excl) = &sel.exclude {
            let names = exclude_names(excl);
            out.retain(|o| !names.contains(&o.name));
        }

        // Unnamed outputs (literals, unnameable exprs) get a positional
        // placeholder so positional merges stay aligned — but they're
        // marked `synthetic` and never surface in `columns()`, the
        // row-shaping fan, or `UNION BY NAME` matching (a real column
        // literally named `__col1` is never marked).
        let mut anon = 0usize;
        for o in &mut out {
            if o.name.is_empty() {
                anon += 1;
                o.name = format!("{ANON_COL_PREFIX}{anon}");
                o.synthetic = true;
            }
        }
        res.out = out;
        Some((res, scope.bindings))
    }

    /// Resolve one projection item into output columns — shared by
    /// `resolve_select` and the `|> SELECT`/`|> EXTEND` pipe operators.
    fn resolve_select_item(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        scope: &Scope<'_>,
        item: &SelectItem,
        out: &mut Vec<OutCol>,
    ) -> Option<()> {
        match item {
            SelectItem::UnnamedExpr(e) => {
                let sources = self.expr_refs(ctx, refs, e)?;
                out.push(OutCol {
                    name: derive_name(e).unwrap_or_default(),
                    bare: self.projection_bare(ctx, refs, e),
                    sources,
                    synthetic: false,
                });
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                let sources = self.expr_refs(ctx, refs, expr)?;
                out.push(OutCol {
                    name: norm_ident(alias),
                    bare: self.projection_bare(ctx, refs, expr),
                    sources,
                    synthetic: false,
                });
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                let sources = self.expr_refs(ctx, refs, expr)?;
                for a in aliases {
                    out.push(OutCol {
                        name: norm_ident(a),
                        bare: self.projection_bare(ctx, refs, expr),
                        sources: sources.clone(),
                        synthetic: false,
                    });
                }
            }
            SelectItem::Wildcard(opts) => {
                self.expand_wildcard(ctx, refs, scope, None, opts, out)?;
            }
            SelectItem::QualifiedWildcard(kind, opts) => match kind {
                SelectItemQualifiedWildcardKind::ObjectName(on) => {
                    let parts = object_name_parts(on);
                    if let Some(qual) = parts.last() {
                        self.expand_wildcard(ctx, refs, scope, Some(qual.clone()), opts, out)?;
                    }
                }
                SelectItemQualifiedWildcardKind::Expr(e) => {
                    // `(expr).*` — a struct expansion: the fields are
                    // un-nameable, so only the expr's refs register… as
                    // an unnamed output, skipped by name matching.
                    let sources = self.expr_refs(ctx, refs, e)?;
                    out.push(OutCol {
                        name: String::new(),
                        bare: false,
                        sources,
                        synthetic: false,
                    });
                }
            },
        }
        Some(())
    }

    /// Bind a `TableWithJoins`: the base relation, then each join's relation
    /// and its ON/USING constraint (evaluated with BOTH sides in scope).
    fn bind_twj(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        scope: &mut Scope<'_>,
        res: &mut QueryRes,
        twj: &TableWithJoins,
    ) -> Option<()> {
        self.bind_relation(ctx, env, scope, res, &twj.relation)?;
        for j in &twj.joins {
            self.bind_relation(ctx, env, scope, res, &j.relation)?;
            let refs = ref_scope(env, scope);
            if let JoinOperator::AsOf {
                match_condition, ..
            } = &j.join_operator
            {
                res.foj.extend(self.expr_refs(ctx, refs, match_condition)?);
            }
            match join_constraint(j) {
                Some(JoinConstraint::On(e)) => {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                Some(JoinConstraint::Using(names)) | Some(JoinConstraint::UsingAlias(names, _)) => {
                    for on in names {
                        let parts = object_name_parts(on);
                        res.foj.extend(self.resolve_parts(ctx, refs, &parts));
                    }
                }
                Some(JoinConstraint::Natural) => {
                    res.foj.extend(self.natural_join_sources(ctx, scope));
                }
                _ => {}
            }
        }
        Some(())
    }

    /// Register one table factor in `scope` (and walk its expression args /
    /// inner factors for row-shaping refs).
    fn bind_relation(
        &self,
        ctx: &Ctx<'_>,
        env: Env<'_>,
        scope: &mut Scope<'_>,
        res: &mut QueryRes,
        factor: &TableFactor,
    ) -> Option<()> {
        match factor {
            TableFactor::Table {
                name,
                alias,
                args,
                with_hints,
                ..
            } => {
                let refs = ref_scope(env, scope);
                for e in with_hints {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                if let Some(args) = args {
                    for a in &args.args {
                        res.foj.extend(self.function_arg_refs(ctx, refs, a)?);
                    }
                }
                let parts = object_name_parts(name);
                let last = parts.last().cloned().unwrap_or_default();
                let binding_name = alias
                    .as_ref()
                    .map(|a| norm_ident(&a.name))
                    .unwrap_or_else(|| last.clone());
                // A CTE is only ever addressable UNqualified — `sch.c` is a
                // real relation (compiled `ref('c')` → `analytics.c`), never
                // the CTE `c`.
                let cte = if parts.len() == 1 {
                    env.ctes.get(&last)
                } else {
                    None
                };
                let alias_cols = alias.as_ref().map(|a| a.columns.as_slice()).unwrap_or(&[]);
                let src = if let Some(q) = cte {
                    // The CTE's own row-shaping deps (WHERE/JOIN inside it)
                    // shape every row this select reads — merge them.
                    res.foj.extend(q.foj.iter().cloned());
                    TableSrc::Derived(rename_rc(q, alias_cols))
                } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                    if let (Some(cols), false) =
                        (self.col_set(ctx.dag, &p.uid), alias_cols.is_empty())
                    {
                        // `FROM up t(a, b)` positionally renames the parent's
                        // outputs — same alias-list semantics as a CTE, so
                        // `t.b` reads the parent's second column.
                        let out = cols
                            .iter()
                            .map(|c| OutCol {
                                name: c.clone(),
                                bare: true,
                                sources: BTreeSet::from([(p.uid.clone(), c.clone())]),
                                synthetic: false,
                            })
                            .collect();
                        TableSrc::Derived(Rc::new(rename_outputs(
                            QueryRes {
                                out,
                                foj: ColSources::new(),
                            },
                            alias_cols,
                        )))
                    } else {
                        TableSrc::Parent(p.uid.clone())
                    }
                } else {
                    TableSrc::External
                };
                scope.bindings.push(Binding {
                    name: binding_name,
                    src,
                });
            }
            TableFactor::Derived {
                lateral,
                subquery,
                alias,
                ..
            } => {
                let sub = self.resolve_query(
                    ctx,
                    Env {
                        // LATERAL derived tables see the FROM items bound so
                        // far; a plain derived table sees only outer scopes.
                        outer: if *lateral { Some(&*scope) } else { env.outer },
                        ctes: env.ctes,
                        locals: env.locals,
                    },
                    subquery,
                )?;
                let sub = rename_outputs(
                    sub,
                    alias.as_ref().map(|a| a.columns.as_slice()).unwrap_or(&[]),
                );
                // Same propagation as CTE bindings: a derived table's inner
                // WHERE/JOIN shapes the rows this select reads.
                res.foj.extend(sub.foj.iter().cloned());
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| format!("__anon{}", scope.bindings.len())),
                    src: TableSrc::Derived(Rc::new(sub)),
                });
            }
            TableFactor::Function { args, alias, .. } => {
                let refs = ref_scope(env, scope);
                for a in args {
                    res.foj.extend(self.function_arg_refs(ctx, refs, a)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__func".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::TableFunction { expr, alias } => {
                res.foj
                    .extend(self.expr_refs(ctx, ref_scope(env, scope), expr)?);
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__func".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.bind_twj(ctx, env, scope, res, table_with_joins)?;
            }
            TableFactor::UNNEST {
                alias,
                array_exprs,
                with_offset_alias,
                with_ordinality,
                ..
            } => {
                // UNNEST explodes an array into rows — row-shaping input,
                // and its alias binds a derived relation of the exploded
                // value (`UNNEST(a) AS u` → `u` carries `a`'s refs).
                let refs = ref_scope(env, &*scope);
                let mut elem_sources = ColSources::new();
                for e in array_exprs {
                    elem_sources.extend(self.expr_refs(ctx, refs, e)?);
                }
                res.foj.extend(elem_sources.iter().cloned());
                let mut out: Vec<OutCol> = {
                    let names: Vec<String> = alias
                        .as_ref()
                        .map(|a| a.columns.iter().map(|c| norm_ident(&c.name)).collect())
                        .unwrap_or_else(|| vec!["unnest".to_string()]);
                    names
                        .into_iter()
                        .map(|name| OutCol {
                            name,
                            bare: false,
                            sources: elem_sources.clone(),
                            synthetic: false,
                        })
                        .collect()
                };
                // Offsets/ordinality are generated, not read — no sources.
                let ord_name = with_offset_alias
                    .as_ref()
                    .map(norm_ident)
                    .or_else(|| with_ordinality.then(|| "ordinality".to_string()));
                if let Some(name) = ord_name {
                    out.push(OutCol {
                        name,
                        bare: false,
                        sources: ColSources::new(),
                        synthetic: false,
                    });
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__unnest".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::Pivot {
                table,
                aggregate_functions,
                value_column,
                value_source,
                default_on_null,
                alias,
            } => {
                // The inner table still binds (its columns feed the pivot
                // values); the pivoted result itself binds un-nameable.
                self.bind_relation(ctx, env, scope, res, table)?;
                let refs = ref_scope(env, scope);
                for ef in aggregate_functions {
                    res.foj.extend(self.expr_refs(ctx, refs, &ef.expr)?);
                }
                for e in value_column {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                self.pivot_value_source_refs(ctx, refs, value_source, &mut res.foj)?;
                if let Some(e) = default_on_null {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__pivot".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::Unpivot {
                table,
                value,
                columns,
                alias,
                ..
            } => {
                self.bind_relation(ctx, env, scope, res, table)?;
                let refs = ref_scope(env, scope);
                res.foj.extend(self.expr_refs(ctx, refs, value)?);
                for c in columns {
                    res.foj.extend(self.expr_refs(ctx, refs, &c.expr)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__unpivot".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::UnpivotExpr {
                expression,
                value_alias,
                attribute_alias,
            } => {
                // `UNPIVOT expr AS v AT a` (Redshift SUPER): the exploded
                // value/attribute columns read the input expression.
                let sources = self.expr_refs(ctx, ref_scope(env, scope), expression)?;
                res.foj.extend(sources.iter().cloned());
                let mut out = vec![OutCol {
                    name: norm_ident(value_alias),
                    bare: false,
                    sources: sources.clone(),
                    synthetic: false,
                }];
                if let Some(a) = attribute_alias {
                    out.push(OutCol {
                        name: norm_ident(a),
                        bare: false,
                        sources,
                        synthetic: false,
                    });
                }
                scope.bindings.push(Binding {
                    name: norm_ident(value_alias),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::JsonTable {
                json_expr,
                columns,
                alias,
                ..
            } => {
                // JSON_TABLE: the document expr is row-shaping input; the
                // declared COLUMNS bind named outputs fed by it (paths and
                // error-handling clauses carry no column refs).
                let doc = self.expr_refs(ctx, ref_scope(env, scope), json_expr)?;
                res.foj.extend(doc.iter().cloned());
                let mut names: Vec<String> = Vec::new();
                let mut stack: Vec<&sqlparser::ast::JsonTableColumn> = columns.iter().collect();
                while let Some(c) = stack.pop() {
                    match c {
                        sqlparser::ast::JsonTableColumn::Named(n) => {
                            names.push(norm_ident(&n.name));
                        }
                        sqlparser::ast::JsonTableColumn::ForOrdinality(i) => {
                            names.push(norm_ident(i));
                        }
                        sqlparser::ast::JsonTableColumn::Nested(n) => {
                            stack.extend(n.columns.iter());
                        }
                    }
                }
                let out = names
                    .into_iter()
                    .map(|name| OutCol {
                        name,
                        bare: false,
                        sources: doc.clone(),
                        synthetic: false,
                    })
                    .collect();
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__json".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::OpenJsonTable {
                json_expr,
                columns,
                alias,
                ..
            } => {
                // Same shape as JSON_TABLE with the OpenJson column type.
                let doc = self.expr_refs(ctx, ref_scope(env, scope), json_expr)?;
                res.foj.extend(doc.iter().cloned());
                let out = columns
                    .iter()
                    .map(|c| OutCol {
                        name: norm_ident(&c.name),
                        bare: false,
                        sources: doc.clone(),
                        synthetic: false,
                    })
                    .collect();
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__json".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::MatchRecognize {
                table,
                partition_by,
                order_by,
                measures,
                symbols,
                alias,
                ..
            } => {
                self.bind_relation(ctx, env, scope, res, table)?;
                // DEFINE symbols are row variables (like lambda params):
                // local names, never bindings — `A.price` still reaches the
                // input column through the first-segment fallback.
                let mut locals = env.locals.clone();
                for sd in symbols {
                    locals.insert(norm_ident(&sd.symbol));
                }
                let refs = RefScope {
                    scope: Some(scope),
                    ctes: env.ctes,
                    locals: &locals,
                };
                for e in partition_by {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                for o in order_by {
                    res.foj.extend(self.expr_refs(ctx, refs, &o.expr)?);
                }
                for m in measures {
                    res.foj.extend(self.expr_refs(ctx, refs, &m.expr)?);
                }
                for sd in symbols {
                    res.foj.extend(self.expr_refs(ctx, refs, &sd.definition)?);
                }
                // `rows_per_match`/`after_match_skip`/`pattern` carry no
                // column exprs (row-count modes and a regex over symbols).
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__mr".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::XmlTable {
                namespaces,
                row_expression,
                passing,
                columns,
                alias,
            } => {
                // XMLTABLE: namespaces/passing args read outer columns; the
                // extracted COLUMNS bind named outputs fed by the passed
                // document (the PATH/default exprs stay in the row-shaping
                // pool — column-list lineage for them is unknowable).
                let refs = ref_scope(env, scope);
                res.foj.extend(self.expr_refs(ctx, refs, row_expression)?);
                let mut doc = ColSources::new();
                for ns in namespaces {
                    doc.extend(self.expr_refs(ctx, refs, &ns.uri)?);
                }
                for arg in &passing.arguments {
                    doc.extend(self.expr_refs(ctx, refs, &arg.expr)?);
                }
                res.foj.extend(doc.iter().cloned());
                for c in columns {
                    if let sqlparser::ast::XmlTableColumnOption::NamedInfo {
                        path, default, ..
                    } = &c.option
                    {
                        for e in [path, default].into_iter().flatten() {
                            res.foj.extend(self.expr_refs(ctx, refs, e)?);
                        }
                    }
                }
                let out = columns
                    .iter()
                    .map(|c| OutCol {
                        name: norm_ident(&c.name),
                        bare: false,
                        sources: doc.clone(),
                        synthetic: false,
                    })
                    .collect();
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__xml".to_string()),
                    src: TableSrc::Derived(Rc::new(QueryRes {
                        out,
                        foj: ColSources::new(),
                    })),
                });
            }
            TableFactor::SemanticView {
                dimensions,
                metrics,
                facts,
                where_clause,
                alias,
                ..
            } => {
                // SEMANTIC_VIEW names objects inside a Snowflake semantic
                // model — not lineage relations. Their refs can still land
                // on same-named scope columns (the general qualified-name
                // ambiguity), so they join the row-shaping pool and the
                // view binds External.
                let refs = ref_scope(env, scope);
                for e in dimensions.iter().chain(metrics).chain(facts) {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                if let Some(e) = where_clause {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__semantic".to_string()),
                    src: TableSrc::External,
                });
            }
        }
        Some(())
    }

    // -- expression references -----------------------------------------------

    /// Every parent-column reference inside `expr`. `None` propagates the
    /// depth-abort sentinel.
    fn expr_refs(&self, ctx: &Ctx<'_>, refs: RefScope<'_>, expr: &Expr) -> Option<ColSources> {
        let mut s = ColSources::new();
        match expr {
            Expr::Identifier(i) => {
                s.extend(self.resolve_parts(ctx, refs, &[norm_ident(i)]));
            }
            Expr::CompoundIdentifier(parts) => {
                let parts: Vec<String> = parts.iter().map(norm_ident).collect();
                s.extend(self.resolve_parts(ctx, refs, &parts));
            }
            Expr::CompoundFieldAccess {
                root, access_chain, ..
            } => {
                s.extend(self.expr_refs(ctx, refs, root)?);
                for a in access_chain {
                    if let AccessExpr::Subscript(sub) = a {
                        s.extend(self.subscript_refs(ctx, refs, sub)?);
                    }
                }
            }
            Expr::JsonAccess { value, .. } => {
                s.extend(self.expr_refs(ctx, refs, value)?);
            }
            Expr::IsFalse(e)
            | Expr::IsNotFalse(e)
            | Expr::IsTrue(e)
            | Expr::IsNotTrue(e)
            | Expr::IsNull(e)
            | Expr::IsNotNull(e)
            | Expr::IsUnknown(e)
            | Expr::IsNotUnknown(e)
            | Expr::Nested(e)
            | Expr::Prior(e)
            | Expr::OuterJoin(e)
            | Expr::IsJson { expr: e, .. }
            | Expr::IsNormalized { expr: e, .. }
            | Expr::Collate { expr: e, .. }
            | Expr::Named { expr: e, .. }
            | Expr::Prefixed { value: e, .. }
            | Expr::UnaryOp { expr: e, .. }
            | Expr::Ceil { expr: e, .. }
            | Expr::Floor { expr: e, .. }
            | Expr::Extract { expr: e, .. } => {
                s.extend(self.expr_refs(ctx, refs, e)?);
            }
            Expr::AtTimeZone {
                timestamp,
                time_zone,
                ..
            } => {
                s.extend(self.expr_refs(ctx, refs, timestamp)?);
                s.extend(self.expr_refs(ctx, refs, time_zone)?);
            }
            Expr::IsDistinctFrom(a, b)
            | Expr::IsNotDistinctFrom(a, b)
            | Expr::BinaryOp {
                left: a, right: b, ..
            }
            | Expr::AnyOp {
                left: a, right: b, ..
            }
            | Expr::AllOp {
                left: a, right: b, ..
            }
            | Expr::Position {
                expr: a, r#in: b, ..
            } => {
                s.extend(self.expr_refs(ctx, refs, a)?);
                s.extend(self.expr_refs(ctx, refs, b)?);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                for e in [expr, low, high] {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Like { expr, pattern, .. }
            | Expr::ILike { expr, pattern, .. }
            | Expr::SimilarTo { expr, pattern, .. }
            | Expr::RLike { expr, pattern, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                s.extend(self.expr_refs(ctx, refs, pattern)?);
            }
            Expr::In { expr, target, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                match target {
                    InTarget::List(exprs) => {
                        for e in exprs {
                            s.extend(self.expr_refs(ctx, refs, e)?);
                        }
                    }
                    InTarget::Subquery(q) => {
                        s.extend(self.subquery_refs(ctx, refs, q)?);
                    }
                    InTarget::Unnest(e) => {
                        s.extend(self.expr_refs(ctx, refs, e)?);
                    }
                    InTarget::Table { name, args } => {
                        // `x IN table_name` (SQLite) reads every row of the
                        // named relation — like `IN (SELECT * FROM name)`:
                        // all of that table's columns feed the test.
                        for a in args.iter().flatten() {
                            s.extend(self.function_arg_refs(ctx, refs, a)?);
                        }
                        let parts = object_name_parts(name);
                        if let Some(last) = parts.last() {
                            // Same rule as FROM position: a CTE is only ever
                            // addressable UNqualified (`sch.c` is a real
                            // relation, never the CTE `c`).
                            let cte = if parts.len() == 1 {
                                refs.ctes.get(last)
                            } else {
                                None
                            };
                            if let Some(q) = cte {
                                s.extend(q.foj.iter().cloned());
                                for o in &q.out {
                                    s.extend(o.sources.iter().cloned());
                                }
                            } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                                if let Some(cs) = self.col_set(ctx.dag, &p.uid) {
                                    s.extend(cs.iter().map(|c| (p.uid.clone(), c.clone())));
                                }
                            }
                        }
                    }
                }
            }
            Expr::Convert { expr, styles, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                for st in styles {
                    s.extend(self.expr_refs(ctx, refs, st)?);
                }
            }
            Expr::Cast { expr, .. } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                // CastFormat's Value/ValueAtTimeZone carry format-string
                // literals, not column refs — nothing to walk.
            }
            Expr::Substring {
                expr,
                substring_from,
                substring_for,
                ..
            } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                for e in [substring_from.as_ref(), substring_for.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Trim {
                expr,
                trim_what,
                trim_characters,
                ..
            } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                if let Some(e) = trim_what {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
                for e in trim_characters.iter().flatten() {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Overlay {
                expr,
                overlay_what,
                overlay_from,
                overlay_for,
            } => {
                s.extend(self.expr_refs(ctx, refs, expr)?);
                s.extend(self.expr_refs(ctx, refs, overlay_what)?);
                s.extend(self.expr_refs(ctx, refs, overlay_from)?);
                if let Some(e) = overlay_for {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Function(f) => {
                s.extend(self.function_refs(ctx, refs, f)?);
            }
            Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                if let Some(op) = operand {
                    s.extend(self.expr_refs(ctx, refs, op)?);
                }
                for cw in conditions {
                    s.extend(self.expr_refs(ctx, refs, &cw.condition)?);
                    s.extend(self.expr_refs(ctx, refs, &cw.result)?);
                }
                if let Some(e) = else_result {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
                s.extend(self.subquery_refs(ctx, refs, subquery)?);
            }
            Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) => {
                for set in sets {
                    for e in set {
                        s.extend(self.expr_refs(ctx, refs, e)?);
                    }
                }
            }
            Expr::Tuple(exprs) => {
                for e in exprs {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Struct { values, .. } => {
                for e in values {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Dictionary(fields) => {
                for f in fields {
                    s.extend(self.expr_refs(ctx, refs, &f.value)?);
                }
            }
            Expr::Map(m) => {
                for e in &m.entries {
                    s.extend(self.expr_refs(ctx, refs, &e.key)?);
                    s.extend(self.expr_refs(ctx, refs, &e.value)?);
                }
            }
            Expr::Array(a) => {
                for e in &a.elem {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            Expr::Interval(i) => {
                s.extend(self.expr_refs(ctx, refs, &i.value)?);
            }
            Expr::MatchAgainst { columns, .. } => {
                for c in columns {
                    let parts = object_name_parts(c);
                    s.extend(self.resolve_parts(ctx, refs, &parts));
                }
                // `match_value` is a ValueWithSpan literal — no column refs.
            }
            Expr::MemberOf(m) => {
                s.extend(self.expr_refs(ctx, refs, &m.value)?);
                s.extend(self.expr_refs(ctx, refs, &m.array)?);
            }
            Expr::Lambda(l) => {
                // Lambda params are LOCAL names: `x -> x > 0` never reads a
                // column `x`. The body still sees every other in-scope
                // column (`filter(arr, x -> x > y)` does read `y`).
                let mut locals = refs.locals.clone();
                for p in l.params.iter() {
                    locals.insert(norm_ident(&p.name));
                }
                s.extend(self.expr_refs(
                    ctx,
                    RefScope {
                        scope: refs.scope,
                        ctes: refs.ctes,
                        locals: &locals,
                    },
                    &l.body,
                )?);
            }
            // Literals and wildcards contribute no column refs (wildcards
            // are expanded by the projection path, not walked as exprs).
            // The exhaustive match means a NEW sqlparser variant fails the
            // build rather than silently under-resolving.
            Expr::Value(_)
            | Expr::TypedString { .. }
            | Expr::Wildcard(_)
            | Expr::QualifiedWildcard(..) => {}
        }
        Some(s)
    }

    /// A scalar/EXISTS subquery's refs: resolve it with `refs.scope` as the
    /// outer scope (correlated refs work) and `refs.ctes` still in force
    /// (a `WITH` name is visible inside a scalar subquery), then report
    /// every source it consumed — its output columns' sources AND its own
    /// row-shaping ones.
    fn subquery_refs(&self, ctx: &Ctx<'_>, refs: RefScope<'_>, q: &Query) -> Option<ColSources> {
        let res = self.resolve_query(
            ctx,
            Env {
                outer: refs.scope,
                ctes: refs.ctes,
                locals: refs.locals,
            },
            q,
        )?;
        let mut s = res.foj;
        for o in &res.out {
            s.extend(o.sources.iter().cloned());
        }
        Some(s)
    }

    /// `Expr::Subscript` inside CompoundFieldAccess chains (`a[expr]`).
    fn subscript_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        sub: &sqlparser::ast::Subscript,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        match sub {
            sqlparser::ast::Subscript::Index { index } => {
                s.extend(self.expr_refs(ctx, refs, index)?);
            }
            sqlparser::ast::Subscript::Slice {
                lower_bound,
                upper_bound,
                stride,
            } => {
                for e in [lower_bound, upper_bound, stride].into_iter().flatten() {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
        }
        Some(s)
    }

    /// A function call's refs: parameters + args + arg-clause exprs +
    /// WITHIN GROUP + FILTER + OVER (window specs).
    fn function_refs(&self, ctx: &Ctx<'_>, refs: RefScope<'_>, f: &Function) -> Option<ColSources> {
        let mut s = ColSources::new();
        for args in [&f.parameters, &f.args] {
            match args {
                FunctionArguments::List(l) => {
                    for a in &l.args {
                        s.extend(self.function_arg_refs(ctx, refs, a)?);
                    }
                    for c in &l.clauses {
                        match c {
                            FunctionArgumentClause::OrderBy(ob) => {
                                for e in ob {
                                    s.extend(self.expr_refs(ctx, refs, &e.expr)?);
                                }
                            }
                            FunctionArgumentClause::Limit(e) | FunctionArgumentClause::Where(e) => {
                                s.extend(self.expr_refs(ctx, refs, e)?);
                            }
                            // `HAVING MIN/MAX <expr>` on `ANY_VALUE`.
                            FunctionArgumentClause::Having(bound) => {
                                s.extend(self.expr_refs(ctx, refs, &bound.1)?);
                            }
                            // IgnoreOrRespectNulls/OnOverflow/Separator/
                            // JsonNullClause/JsonReturningClause carry no
                            // column exprs — documented no-walk.
                            _ => {}
                        }
                    }
                }
                FunctionArguments::Subquery(q) => {
                    s.extend(self.subquery_refs(ctx, refs, q)?);
                }
                _ => {}
            }
        }
        for ob in &f.within_group {
            s.extend(self.expr_refs(ctx, refs, &ob.expr)?);
        }
        if let Some(flt) = &f.filter {
            s.extend(self.expr_refs(ctx, refs, flt)?);
        }
        if let Some(over) = &f.over {
            match over {
                WindowType::WindowSpec(spec) => {
                    s.extend(self.window_spec_refs(ctx, refs, spec)?);
                }
                WindowType::NamedWindow(name) => {
                    // `OVER w` resolves against the scope chain's WINDOW
                    // defs (select-local — nested selects get their own).
                    let key = norm_ident(name);
                    let mut sc = refs.scope;
                    while let Some(scope) = sc {
                        if let Some(spec) = scope.windows.get(&key) {
                            let spec = spec.clone();
                            s.extend(self.window_spec_refs(ctx, refs, &spec)?);
                            break;
                        }
                        sc = scope.outer;
                    }
                }
            }
        }
        Some(s)
    }

    /// PARTITION BY + ORDER BY of a window spec (frame bounds are
    /// literals/rows — no column refs).
    fn window_spec_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        spec: &WindowSpec,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        for e in &spec.partition_by {
            s.extend(self.expr_refs(ctx, refs, e)?);
        }
        for ob in &spec.order_by {
            s.extend(self.expr_refs(ctx, refs, &ob.expr)?);
        }
        Some(s)
    }

    /// A function argument's refs (wildcards skipped — `count(*)` depends on
    /// the rows, not any column).
    fn function_arg_refs(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        arg: &FunctionArg,
    ) -> Option<ColSources> {
        let mut s = ColSources::new();
        match arg {
            FunctionArg::Named { arg, .. } | FunctionArg::Unnamed(arg) => {
                if let FunctionArgExpr::Expr(e) = arg {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
            FunctionArg::ExprNamed { arg, .. } => {
                // `name => value` / `name := value`: the LHS expr is an
                // argument NAME, not data — `named_struct(name => 'v')`
                // never reads a column `name`. Only the value walks.
                if let FunctionArgExpr::Expr(e) = arg {
                    s.extend(self.expr_refs(ctx, refs, e)?);
                }
            }
        }
        Some(s)
    }

    // -- column reference resolution -----------------------------------------

    /// Resolve a (normalized, dotted) column reference → the parent
    /// `(uid, column)` sources it provably draws from.
    fn resolve_parts(&self, ctx: &Ctx<'_>, refs: RefScope<'_>, parts: &[String]) -> ColSources {
        let Some(scope) = refs.scope else {
            return ColSources::new();
        };
        if parts.len() >= 2 {
            let col = parts.last().expect("len >= 2");
            let qual = &parts[parts.len() - 2];
            // `qual` names a binding in this scope or an outer one — but a
            // LOCAL name (lambda param, MATCH_RECOGNIZE symbol) shadows it.
            let mut sc = Some(scope);
            while let Some(s) = sc {
                // `rfind`: a duplicated binding name (`FROM a t, b t`) —
                // the rightmost binding shadows, per SQL semantics.
                if let Some(b) = s.bindings.iter().rfind(|b| b.name == *qual) {
                    if !refs.locals.contains(qual) {
                        return self.resolve_in_binding(ctx, b, col);
                    }
                }
                sc = s.outer;
            }
            if parts.len() >= 3 {
                // `a.b.c…` — two readings, in order: `a` a bound table and
                // `b` its struct column (deeper segments are field access),
                // or `a` itself a column (a struct/map chain — `db.tbl.c`
                // can't be told apart from `tbl.c` once `tbl` is unbound,
                // documented).
                let mut sc = Some(scope);
                while let Some(s) = sc {
                    if let Some(b) = s.bindings.iter().rfind(|b| b.name == parts[0]) {
                        if !refs.locals.contains(&parts[0]) {
                            return self.resolve_in_binding(ctx, b, &parts[1]);
                        }
                    }
                    sc = s.outer;
                }
            }
            if refs.locals.contains(&parts[0]) {
                return ColSources::new();
            }
            return self.resolve_unqualified(ctx, scope, &parts[0]);
        }
        let Some(first) = parts.first() else {
            return ColSources::new();
        };
        if refs.locals.contains(first) {
            return ColSources::new();
        }
        self.resolve_unqualified(ctx, scope, first)
    }

    /// `t.col` against a bound `t`: the parent uid + col (trusting the
    /// qualification when the parent's list is unknown), the derived
    /// output's transitive sources, or nothing for External bindings.
    fn resolve_in_binding(&self, ctx: &Ctx<'_>, b: &Binding, col: &str) -> ColSources {
        let mut s = ColSources::new();
        match &b.src {
            TableSrc::Parent(uid) => match self.col_set(ctx.dag, uid) {
                Some(cols) => {
                    if cols.iter().any(|c| c == col) {
                        s.insert((uid.clone(), col.to_string()));
                    }
                    // A KNOWN column list without `col` → no edge (the
                    // precision win over v0's blind name-match).
                }
                // Unknown column list → trust the qualification.
                None => {
                    s.insert((uid.clone(), col.to_string()));
                }
            },
            TableSrc::Derived(q) => {
                if let Some(o) = q.out.iter().find(|o| o.name == *col) {
                    s.extend(o.sources.iter().cloned());
                }
            }
            TableSrc::External => {}
        }
        s
    }

    /// The enumerable output names a binding exposes — `None` when its
    /// column list is unknowable (a parent nothing names, an External
    /// relation).
    fn binding_col_names(&self, ctx: &Ctx<'_>, b: &Binding) -> Option<BTreeSet<String>> {
        match &b.src {
            TableSrc::Parent(uid) => self
                .col_set(ctx.dag, uid)
                .map(|cs| cs.iter().cloned().collect()),
            TableSrc::Derived(q) => Some(
                q.out
                    .iter()
                    .filter(|o| !o.synthetic)
                    .map(|o| o.name.clone())
                    .collect(),
            ),
            TableSrc::External => None,
        }
    }

    /// `NATURAL JOIN` keys on every column name BOTH sides share — so both
    /// sides' shared columns are row-shaping. Computable only where the
    /// just-bound (right) relation and an earlier binding each expose a
    /// known column list.
    fn natural_join_sources(&self, ctx: &Ctx<'_>, scope: &Scope<'_>) -> ColSources {
        let mut s = ColSources::new();
        let Some((right, prior)) = scope.bindings.split_last() else {
            return s;
        };
        let Some(right_cols) = self.binding_col_names(ctx, right) else {
            return s;
        };
        for lb in prior {
            let Some(left_cols) = self.binding_col_names(ctx, lb) else {
                continue;
            };
            for c in left_cols.intersection(&right_cols) {
                s.extend(self.resolve_in_binding(ctx, lb, c));
                s.extend(self.resolve_in_binding(ctx, right, c));
            }
        }
        s
    }

    /// An unqualified `col`: every binding in the innermost scope that HAS
    /// it contributes (ambiguous → all candidates, the documented
    /// over-connection). Unknown-crediting is LEVEL-scoped: a binding with
    /// an UNKNOWN column list is credited only when no KNOWN binding at its
    /// own level — or any level OUTER of it — resolves the ref. (An inner
    /// unknown can't poison a provable outer hit, and vice versa.)
    fn resolve_unqualified(&self, ctx: &Ctx<'_>, scope: &Scope<'_>, col: &str) -> ColSources {
        // Collect per-level (known hits, unknown uids), innermost first.
        let mut levels: Vec<(ColSources, BTreeSet<String>)> = Vec::new();
        let mut sc = Some(scope);
        while let Some(s) = sc {
            let mut found = ColSources::new();
            let mut unknown = BTreeSet::new();
            for b in &s.bindings {
                match &b.src {
                    TableSrc::Parent(uid) => match self.col_set(ctx.dag, uid) {
                        Some(cols) => {
                            if cols.iter().any(|c| c == col) {
                                found.insert((uid.clone(), col.to_string()));
                            }
                        }
                        None => {
                            unknown.insert(uid.clone());
                        }
                    },
                    TableSrc::Derived(q) => {
                        if let Some(o) = q.out.iter().find(|o| o.name == *col) {
                            found.extend(o.sources.iter().cloned());
                        }
                    }
                    TableSrc::External => {}
                }
            }
            levels.push((found, unknown));
            sc = s.outer;
        }
        match levels.iter().position(|(found, _)| !found.is_empty()) {
            Some(k) => {
                let mut result = levels[k].0.clone();
                // Unknowns OUTER of the outermost known level still can't be
                // disproven — credit them; everything at or inside a
                // resolving level is settled.
                let m = levels
                    .iter()
                    .rposition(|(found, _)| !found.is_empty())
                    .unwrap_or(k);
                for (_, unknown) in &levels[m + 1..] {
                    for uid in unknown {
                        result.insert((uid.clone(), col.to_string()));
                    }
                }
                result
            }
            // Nothing provable anywhere → every unknown stays a candidate.
            None => {
                let mut result = ColSources::new();
                for (_, unknown) in &levels {
                    for uid in unknown {
                        result.insert((uid.clone(), col.to_string()));
                    }
                }
                result
            }
        }
    }

    /// Whether a projection stays *bare*: its expression is a column
    /// reference AND every binding it can resolve to serves a bare column.
    /// A `t.c` where `c` was itself computed inside the CTE/derived table is
    /// not bare — the transform happened upstream of this reference but
    /// still belongs to the value.
    fn projection_bare(&self, ctx: &Ctx<'_>, refs: RefScope<'_>, e: &Expr) -> bool {
        is_bare_ref(e)
            && bare_parts(e)
                .and_then(|parts| self.bare_through_binding(ctx, refs, &parts))
                .unwrap_or(true)
    }

    /// The bare-ness of the column `parts` resolves to — mirrors
    /// [`resolve_parts`](Self::resolve_parts)' scope walk. `None` when the
    /// ref resolves to nothing bare-meaningful (External binding, no match).
    fn bare_through_binding(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        parts: &[String],
    ) -> Option<bool> {
        let scope = refs.scope?;
        if parts.len() >= 2 {
            let col = parts.last()?;
            let qual = &parts[parts.len() - 2];
            let mut sc = Some(scope);
            while let Some(s) = sc {
                // `rfind` like `resolve_parts` — a duplicated binding name
                // resolves through the rightmost `FROM a t, b t` binding.
                if let Some(b) = s.bindings.iter().rfind(|b| b.name == *qual) {
                    if !refs.locals.contains(qual) {
                        return Self::bare_in_binding(b, col);
                    }
                }
                sc = s.outer;
            }
            if parts.len() >= 3 {
                // `a.b.c…` — mirrors `resolve_parts`: a bound `a` reads its
                // column `b`; else `a` is the struct column itself.
                let mut sc = Some(scope);
                while let Some(s) = sc {
                    if let Some(b) = s.bindings.iter().rfind(|b| b.name == parts[0]) {
                        if !refs.locals.contains(&parts[0]) {
                            return Self::bare_in_binding(b, &parts[1]);
                        }
                    }
                    sc = s.outer;
                }
            }
            if refs.locals.contains(&parts[0]) {
                return None;
            }
            return self.bare_unqualified(ctx, scope, &parts[0]);
        }
        let first = parts.first()?;
        if refs.locals.contains(first) {
            return None;
        }
        self.bare_unqualified(ctx, scope, first)
    }

    /// `t.col`'s bare-ness: a parent column is always a raw value (any
    /// transform inside the parent is that model's own edge set); a derived
    /// column carries its producing projection's flag.
    fn bare_in_binding(b: &Binding, col: &str) -> Option<bool> {
        match &b.src {
            TableSrc::Parent(_) => Some(true),
            TableSrc::Derived(q) => q.out.iter().find(|o| o.name == col).map(|o| o.bare),
            TableSrc::External => None,
        }
    }

    /// Unqualified bare-ness — mirrors [`resolve_unqualified`](Self::resolve_unqualified):
    /// the innermost RESOLVING scope's candidates win (with the same
    /// level-scoped unknown crediting), and the ref stays bare only when
    /// EVERY candidate is a bare column (ambiguous mixed sources keep the
    /// conservative transform flag).
    fn bare_unqualified(&self, ctx: &Ctx<'_>, scope: &Scope<'_>, col: &str) -> Option<bool> {
        let mut levels: Vec<(Vec<bool>, bool)> = Vec::new();
        let mut sc = Some(scope);
        while let Some(s) = sc {
            let mut bares: Vec<bool> = Vec::new();
            let mut unknown = false;
            for b in &s.bindings {
                match &b.src {
                    TableSrc::Parent(uid) => match self.col_set(ctx.dag, uid) {
                        Some(cols) => {
                            if cols.iter().any(|c| c == col) {
                                bares.push(true);
                            }
                        }
                        None => unknown = true,
                    },
                    TableSrc::Derived(q) => {
                        if let Some(o) = q.out.iter().find(|o| o.name == col) {
                            bares.push(o.bare);
                        }
                    }
                    TableSrc::External => {}
                }
            }
            levels.push((bares, unknown));
            sc = s.outer;
        }
        match levels.iter().position(|(bares, _)| !bares.is_empty()) {
            Some(k) => {
                let mut all = levels[k].0.clone();
                // The same outer-of-outermost-known unknowns get credited —
                // they'd resolve raw, so they vote bare.
                let m = levels
                    .iter()
                    .rposition(|(bares, _)| !bares.is_empty())
                    .unwrap_or(k);
                let outer_unknown = levels[m + 1..].iter().any(|(_, u)| *u);
                if outer_unknown {
                    all.push(true);
                }
                Some(all.iter().all(|b| *b))
            }
            None => {
                // No provable hit anywhere: any unknown binding resolves
                // raw; a fully-known miss resolves nothing.
                let any_unknown = levels.iter().any(|(_, u)| *u);
                any_unknown.then_some(true)
            }
        }
    }

    /// `*` / `t.*` expansion: each binding's known columns become bare
    /// outputs; wildcard options (`EXCEPT`/`EXCLUDE`/`RENAME`/`REPLACE`/
    /// `ILIKE`) then reshape the list.
    fn expand_wildcard(
        &self,
        ctx: &Ctx<'_>,
        refs: RefScope<'_>,
        scope: &Scope<'_>,
        qualifier: Option<String>,
        opts: &WildcardAdditionalOptions,
        out: &mut Vec<OutCol>,
    ) -> Option<()> {
        let mut cols: Vec<OutCol> = Vec::new();
        for b in &scope.bindings {
            if let Some(q) = &qualifier {
                if &b.name != q {
                    continue;
                }
            }
            match &b.src {
                TableSrc::Parent(uid) => {
                    if let Some(cs) = self.col_set(ctx.dag, uid) {
                        for c in cs.iter() {
                            cols.push(OutCol {
                                name: c.clone(),
                                bare: true,
                                sources: BTreeSet::from([(uid.clone(), c.clone())]),
                                synthetic: false,
                            });
                        }
                    }
                }
                TableSrc::Derived(q) => {
                    cols.extend(q.out.iter().cloned());
                }
                TableSrc::External => {}
            }
        }

        // `* ILIKE '<pattern>'` — Snowflake's column-name filter.
        if let Some(ilike) = &opts.opt_ilike {
            cols.retain(|o| ilike_match(&ilike.pattern, &o.name));
        }
        // `* EXCEPT col` — BigQuery/ClickHouse.
        if let Some(except) = &opts.opt_except {
            let drop: BTreeSet<String> = std::iter::once(&except.first_element)
                .chain(except.additional_elements.iter())
                .map(norm_ident)
                .collect();
            cols.retain(|o| !drop.contains(&o.name));
        }
        // `* EXCLUDE col` — Snowflake.
        if let Some(exclude) = &opts.opt_exclude {
            let names = exclude_names(exclude);
            cols.retain(|o| !names.contains(&o.name));
        }
        // `* RENAME col AS alias` — the output name changes (the edge still
        // resolves to Rename downstream since `bare` stays true).
        if let Some(rename) = &opts.opt_rename {
            let pairs: Vec<(String, String)> = match rename {
                RenameSelectItem::Single(ia) => {
                    vec![(norm_ident(&ia.ident), norm_ident(&ia.alias))]
                }
                RenameSelectItem::Multiple(ias) => ias
                    .iter()
                    .map(|ia| (norm_ident(&ia.ident), norm_ident(&ia.alias)))
                    .collect(),
            };
            for o in &mut cols {
                if let Some((_, to)) = pairs.iter().find(|(from, _)| from == &o.name) {
                    o.name = to.clone();
                    // An explicit rename names the output — no longer a
                    // positional placeholder.
                    o.synthetic = false;
                }
            }
        }
        // `* REPLACE(expr AS col)` — the column's value is recomputed:
        // sources = the replacement expr's refs, and it's no longer bare.
        if let Some(repl) = &opts.opt_replace {
            for item in &repl.items {
                let target = norm_ident(&item.column_name);
                if let Some(o) = cols.iter_mut().find(|o| o.name == target) {
                    o.sources = self.expr_refs(ctx, refs, &item.expr)?;
                    o.bare = false;
                }
            }
        }
        out.extend(cols);
        Some(())
    }

    // -- edge construction ----------------------------------------------------

    /// Edges into `node` — the analyzed model when it exists, else the
    /// name-match fallback (recorded for the `[name-match]` tag).
    fn compute_resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge> {
        let Some((_, base_conf)) = resolution_sql(dag, node) else {
            // No SQL at all (seed/source): the fallback answers the same
            // ([] edges) without counting as a fallback event.
            return self.fallback.resolve(dag, node);
        };
        let Some(an) = self.analyzed(dag, node) else {
            self.fallbacks.borrow_mut().insert(node.to_string());
            return self.fallback.resolve(dag, node);
        };
        let confidence = if base_conf == Confidence::Stale {
            Confidence::Stale
        } else {
            Confidence::Exact
        };
        let mut edges: BTreeSet<ColumnEdge> = BTreeSet::new();
        let mut direct_sources: ColSources = BTreeSet::new();
        let mut produced: BTreeSet<String> = BTreeSet::new();
        let parents: Vec<Parent> = Vec::new();
        let ctx = Ctx {
            dag,
            parents: &parents,
            depth: Cell::new(0),
        };

        for o in &an.out {
            // Lowercase-canonical: `AS "User_ID"` and documented `user_id`
            // name the same column in the normalized space — comparing raw
            // would call the declared name "unproduced" and probe a phantom
            // edge for it.
            produced.insert(o.name.to_lowercase());
            for (uid, col) in &o.sources {
                direct_sources.insert((uid.clone(), col.clone()));
                let kind = if !o.bare {
                    ColumnEdgeKind::Transform
                } else if col == &o.name {
                    ColumnEdgeKind::Passthrough
                } else {
                    ColumnEdgeKind::Rename
                };
                edges.insert(ColumnEdge {
                    from: ColumnRef::new(uid.clone(), col.as_str()),
                    to: ColumnRef::new(node, &o.name),
                    kind,
                    confidence,
                });
            }
        }

        // The declared-column fallback probe (v0 parity): a visible output
        // column the projection couldn't name still gets a Passthrough edge
        // — but ONLY when it could genuinely hide behind a `*`: the
        // projection must contain a wildcard (`an.unbounded_wildcard`) AND
        // only a binding whose column list is UNKNOWN can have carried the
        // name. A known-list binding's columns were all enumerated by the
        // wildcard already — crediting one fabricates edges to
        // documented-but-never-selected columns (schema.yml drift), and
        // probing without a wildcard fabricates them unconditionally.
        let out_cols: Vec<String> = self
            .col_set(dag, node)
            .map(|rc| (*rc).clone())
            .unwrap_or_else(|| {
                an.out
                    .iter()
                    .filter(|o| !o.synthetic)
                    .map(|o| o.name.clone())
                    .collect()
            });
        if an.unbounded_wildcard {
            for c in &out_cols {
                if produced.contains(&c.to_lowercase()) {
                    continue;
                }
                for b in &an.scope {
                    if let TableSrc::Parent(uid) = &b.src {
                        if self.col_set(ctx.dag, uid).is_none() {
                            direct_sources.insert((uid.clone(), c.clone()));
                            edges.insert(ColumnEdge {
                                from: ColumnRef::new(uid.clone(), c),
                                to: ColumnRef::new(node, c),
                                kind: ColumnEdgeKind::Passthrough,
                                confidence,
                            });
                        }
                    }
                }
            }
        }

        // FilterOrJoin: row-shaping sources with no value edge fan out to
        // every visible output column (same attachment policy as v0) —
        // synthetic placeholders are never visible targets.
        let mut foj_targets: BTreeSet<String> = out_cols.into_iter().collect();
        foj_targets.extend(
            an.out
                .iter()
                .filter(|o| !o.synthetic)
                .map(|o| o.name.clone()),
        );
        for (uid, col) in &an.foj {
            if direct_sources.contains(&(uid.clone(), col.clone())) {
                continue;
            }
            for c in &foj_targets {
                edges.insert(ColumnEdge {
                    from: ColumnRef::new(uid.clone(), col),
                    to: ColumnRef::new(node, c),
                    kind: ColumnEdgeKind::FilterOrJoin,
                    confidence,
                });
            }
        }

        edges.into_iter().collect()
    }
}

impl ColumnResolver for SqlParseResolver {
    fn columns(&self, dag: &Dag, node: &str) -> Vec<String> {
        self.col_set(dag, node)
            .map(|rc| (*rc).clone())
            .unwrap_or_default()
    }

    fn resolve(&self, dag: &Dag, node: &str) -> Vec<ColumnEdge> {
        if let Some(v) = self.resolved_memo.borrow().get(node) {
            return v.as_ref().clone();
        }
        let rc = Rc::new(self.compute_resolve(dag, node));
        self.resolved_memo
            .borrow_mut()
            .insert(node.to_string(), rc.clone());
        rc.as_ref().clone()
    }
}

// ---------------------------------------------------------------------------
// Small AST helpers
// ---------------------------------------------------------------------------

/// `Ident` → the normalized column-space name: quoted idents keep their
/// case (sans quotes), unquoted fold to lowercase — the same rule
/// [`normalize_column`] applies to hand-written names.
fn norm_ident(i: &Ident) -> String {
    if i.quote_style.is_some() {
        i.value.clone()
    } else {
        i.value.to_lowercase()
    }
}

/// `ObjectName` → normalized dotted segments.
fn object_name_parts(on: &ObjectName) -> Vec<String> {
    on.0.iter()
        .filter_map(|p| match p {
            ObjectNamePart::Identifier(i) => Some(norm_ident(i)),
            _ => None,
        })
        .collect()
}

/// Is `e` a bare column reference (`x` / `t.x`), possibly through parens?
fn is_bare_ref(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        Expr::Nested(inner) => is_bare_ref(inner),
        _ => false,
    }
}

/// The normalized parts of a bare reference — `x` → `[x]`, `t.x` → `[t, x]`.
fn bare_parts(e: &Expr) -> Option<Vec<String>> {
    match e {
        Expr::Identifier(i) => Some(vec![norm_ident(i)]),
        Expr::CompoundIdentifier(parts) => Some(parts.iter().map(norm_ident).collect()),
        Expr::Nested(inner) => bare_parts(inner),
        _ => None,
    }
}

/// The output name an un-aliased projection expr wants: bare refs → their
/// last segment; casts unwrap to the inner expr's name; a function → its
/// name; a CASE → `case`; field access → the last field; everything else →
/// no name (positional fallback).
fn derive_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(i) => Some(norm_ident(i)),
        Expr::CompoundIdentifier(parts) => parts.last().map(norm_ident),
        Expr::CompoundFieldAccess {
            root, access_chain, ..
        } => access_chain
            .iter()
            .rev()
            .find_map(|a| match a {
                AccessExpr::Dot(e) => derive_name(e),
                _ => None,
            })
            .or_else(|| derive_name(root)),
        Expr::Nested(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Collate { expr: inner, .. } => derive_name(inner),
        // `expr AS name` in expression position (MySQL-derived syntax):
        // the declared name is the output name, like ExprWithAlias.
        Expr::Named { name, .. } => Some(norm_ident(name)),
        Expr::Function(f) => object_name_parts(&f.name).last().cloned(),
        Expr::Case { .. } => Some("case".to_string()),
        Expr::Prefixed { value, .. } => derive_name(value),
        _ => None,
    }
}

/// The `ON`/`USING` of a join operator, when it has one.
fn join_constraint(j: &Join) -> Option<&JoinConstraint> {
    use JoinOperator::*;
    match &j.join_operator {
        Join(c) | Inner(c) | Left(c) | LeftOuter(c) | Right(c) | RightOuter(c) | FullOuter(c)
        | CrossJoin(c) | Semi(c) | LeftSemi(c) | RightSemi(c) | Anti(c) | LeftAnti(c)
        | RightAnti(c) | StraightJoin(c) => Some(c),
        AsOf { constraint, .. } => Some(constraint),
        _ => None,
    }
}

/// `EXCLUDE` item set → normalized names (the last segment of each).
fn exclude_names(excl: &ExcludeSelectItem) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let names: Vec<&ObjectName> = match excl {
        ExcludeSelectItem::Single(on) => vec![on],
        ExcludeSelectItem::Multiple(ons) => ons.iter().collect(),
    };
    for on in names {
        if let Some(last) = object_name_parts(on).into_iter().last() {
            out.insert(last);
        }
    }
    out
}

/// The bare identifier names appearing at the top level of `e` — the
/// `ORDER BY <output-name>` probe (Query-level ORDER BY can't see the
/// select's FROM scope, so only the output-name match is available).
fn expr_ident_names(e: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match e {
        Expr::Identifier(i) => {
            out.insert(norm_ident(i));
        }
        Expr::CompoundIdentifier(parts) => {
            if let Some(last) = parts.last() {
                out.insert(norm_ident(last));
            }
        }
        Expr::Nested(inner) => out.extend(expr_ident_names(inner)),
        _ => {}
    }
    out
}

/// Could this query's output carry columns no projection enumerated? A
/// `*`/`t.*` over a binding whose column list is unknown leaves carried
/// columns invisible to `Analyzed::out` — the only case the declared-column
/// probe can legitimately fill (without a wildcard every output is named,
/// so an unproduced declared name was never selected).
fn query_has_output_wildcard(q: &Query) -> bool {
    set_expr_has_wildcard(&q.body)
        || q.pipe_operators.iter().any(|po| {
            use sqlparser::ast::PipeOperator as P;
            match po {
                P::Select { exprs } | P::Extend { exprs } => {
                    exprs.iter().any(select_item_is_wildcard)
                }
                P::Union { queries, .. } => queries.iter().any(query_has_output_wildcard),
                _ => false,
            }
        })
}

fn set_expr_has_wildcard(e: &SetExpr) -> bool {
    match e {
        SetExpr::Select(sel) => sel.projection.iter().any(select_item_is_wildcard),
        SetExpr::SetOperation {
            left, right, op, ..
        } => {
            // Only UNION arms merge outputs; an EXCEPT/INTERSECT right arm
            // shapes rows, never output names.
            set_expr_has_wildcard(left)
                || (*op == SetOperator::Union && set_expr_has_wildcard(right))
        }
        SetExpr::Query(q) => query_has_output_wildcard(q),
        _ => false,
    }
}

fn select_item_is_wildcard(i: &SelectItem) -> bool {
    matches!(
        i,
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
    )
}

/// A resolved query whose outputs are positionally renamed by a
/// `WITH x(a,b) AS` / `(sub) AS t(a,b)` column-alias list.
fn rename_outputs(mut res: QueryRes, cols: &[TableAliasColumnDef]) -> QueryRes {
    if cols.is_empty() {
        return res;
    }
    for (o, c) in res.out.iter_mut().zip(cols.iter()) {
        o.name = norm_ident(&c.name);
        // `WITH c(a, b)` / `t(a, b)` gives the output a real name — a
        // positional placeholder it lands on becomes declared.
        o.synthetic = false;
    }
    res
}

/// Merge two positional (or `BY NAME`-matched) UNION arms' output columns:
/// the left arm's name/bare when the left exists (SQL's naming rule), the
/// union of both sides' sources. An arm contributing no sources (a literal
/// `null` arm) doesn't vote on bareness.
fn union_out(a: &OutCol, b: &OutCol) -> OutCol {
    OutCol {
        name: a.name.clone(),
        bare: match (a.sources.is_empty(), b.sources.is_empty()) {
            (false, true) => a.bare,
            (true, false) => b.bare,
            _ => a.bare && b.bare,
        },
        sources: a.sources.union(&b.sources).cloned().collect(),
        // The kept name is `a`'s — its synthetic flag answers "is this name
        // invented?" regardless of what the right arm called the column.
        synthetic: a.synthetic,
    }
}

/// The collapsed output of a `|> PIVOT`/`|> CALL` pipe step — one synthetic
/// column carrying every source the input consumed (the reshape's real
/// column list is unknowable).
fn collapse_pipe_output(res: &QueryRes) -> OutCol {
    let mut sources: ColSources = res
        .out
        .iter()
        .flat_map(|o| o.sources.iter().cloned())
        .collect();
    sources.extend(res.foj.iter().cloned());
    OutCol {
        name: String::new(),
        bare: false,
        sources,
        synthetic: true,
    }
}

/// `rename_outputs` over an `Rc<QueryRes>` (clones the inner value — CTE
/// results are shared, the rename only applies at this binding).
fn rename_rc(q: &Rc<QueryRes>, cols: &[TableAliasColumnDef]) -> Rc<QueryRes> {
    if cols.is_empty() {
        return q.clone();
    }
    Rc::new(rename_outputs((**q).clone(), cols))
}

/// Case-insensitive `LIKE` match for `* ILIKE '<pat>'`: `%` matches any
/// run, every other char (including `_` — the approximation noted in the
/// module docs) matches itself, compared on lowercase.
fn ilike_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(&'%') => (0..=n.len()).any(|k| go(&p[1..], &n[k..])),
            Some(&c) => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    go(&p, &n)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cll::ColumnLineage;
    use crate::manifest::{RawManifest, RawNode};
    use std::collections::HashMap;

    /// A `model` node: `sql` lands in `compiled_code` (a dbt-compile product —
    /// parses to Exact edges), with the given declared columns and topology
    /// coming from the `parent_map`/`child_map` args of [`manifest`].
    fn model(uid: &str, sql: &str, cols: &[&str]) -> (String, RawNode) {
        let columns: serde_json::Map<String, serde_json::Value> = cols
            .iter()
            .map(|c| {
                (
                    c.to_string(),
                    serde_json::json!({"name": c, "data_type": "int"}),
                )
            })
            .collect();
        let name = uid.rsplit('.').next().unwrap_or(uid).to_string();
        let file = format!("models/{name}.sql");
        (
            uid.to_string(),
            RawNode {
                name,
                resource_type: "model".to_string(),
                compiled_code: Some(sql.to_string()),
                columns,
                original_file_path: Some(file),
                ..Default::default()
            },
        )
    }

    /// Same, but the SQL is *raw* (Jinja-bearing) — the resolver renders
    /// `ref`/`source`/block tags itself when no compiled text exists.
    fn raw_model(uid: &str, sql: &str, cols: &[&str]) -> (String, RawNode) {
        let (k, mut n) = model(uid, sql, cols);
        n.raw_code = n.compiled_code.take();
        (k, n)
    }

    /// A minimal manifest: `uid -> node` models plus `parent -> child` edges.
    fn manifest(nodes: Vec<(String, RawNode)>, edges: &[(&str, &str)]) -> RawManifest {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for (p, c) in edges {
            parent_map
                .entry(c.to_string())
                .or_default()
                .push(p.to_string());
            child_map
                .entry(p.to_string())
                .or_default()
                .push(c.to_string());
        }
        RawManifest {
            nodes: nodes.into_iter().collect(),
            sources: HashMap::new(),
            exposures: HashMap::new(),
            parent_map,
            child_map,
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            metadata: Default::default(),
        }
    }

    /// `(from.node.from.col, to.col, kind, confidence)` — the comparable view
    /// of a node's in-edges.
    fn edge_set(
        dag: &Dag,
        resolver: &SqlParseResolver,
        node: &str,
    ) -> BTreeSet<(String, String, ColumnEdgeKind, Confidence)> {
        ColumnLineage::new(dag, resolver)
            .in_edges(node)
            .iter()
            .map(|e| {
                (
                    format!("{}.{}", e.from.node, e.from.column),
                    e.to.column.clone(),
                    e.kind,
                    e.confidence,
                )
            })
            .collect()
    }

    fn e(
        from: &str,
        to: &str,
        kind: ColumnEdgeKind,
    ) -> (String, String, ColumnEdgeKind, Confidence) {
        (from.to_string(), to.to_string(), kind, Confidence::Exact)
    }

    fn resolver() -> SqlParseResolver {
        SqlParseResolver::new(None, SqlDialect::Generic)
    }

    // -- 1. projections: rename / passthrough / transform ---------------------

    #[test]
    fn projection_kinds() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "status"]),
                model(
                    "model.t.m",
                    "select u.id as order_id, u.status, upper(u.status) as big \
                     from \"up\" u",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "order_id", ColumnEdgeKind::Rename),
            e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "big", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- 2. qualified refs bind to the aliased parent only --------------------

    #[test]
    fn qualified_refs_use_the_table_alias() {
        // Both parents declare `id`/`k`: `x.id` must bind to `a` alone (v0
        // name-match would have bound `b` too). `k` in the join is FOJ.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "k"]),
                model("model.t.b", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "select x.id as aid, y.id as bid \
                     from \"a\" x join \"b\" y on x.k = y.k",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "aid", ColumnEdgeKind::Rename),
            e("model.t.b.id", "bid", ColumnEdgeKind::Rename),
            e("model.t.a.k", "aid", ColumnEdgeKind::FilterOrJoin),
            e("model.t.a.k", "bid", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.k", "aid", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.k", "bid", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- 3. wildcards ---------------------------------------------------------

    #[test]
    fn bare_wildcard_expands_over_declared_columns() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "status"]),
                model("model.t.m", "select * from \"up\"", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn qualified_wildcard_only_expands_its_own_table() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["x"]),
                model("model.t.b", "select 1", &["y"]),
                model(
                    "model.t.m",
                    "select a.*, b.y as by from \"a\" a join \"b\" b on a.x = b.y",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.x", "x", ColumnEdgeKind::Passthrough),
            e("model.t.b.y", "by", ColumnEdgeKind::Rename),
            // join keys: a.x already a value source → skipped; b.y same.
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn unknown_wildcard_invents_no_names() {
        // The parent has NO declared columns and no analyzable SQL: `*` must
        // not fabricate output columns (the declared-column probe still lands
        // the model's own schema.yml list — empty here).
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "", &[]),
                model("model.t.m", "select * from \"up\"", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        assert!(edge_set(&dag, &r, "model.t.m").is_empty());
        assert!(r.columns(&dag, "model.t.m").is_empty());
    }

    // -- 4. CTEs --------------------------------------------------------------

    #[test]
    fn cte_columns_flow_to_the_outer_select() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model(
                    "model.t.m",
                    "with c as (select u.id, u.id * 2 as dbl from \"up\" u) \
                     select c.id as cid, c.dbl from c",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "cid", ColumnEdgeKind::Rename),
            e("model.t.up.id", "dbl", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- 5. derived subqueries --------------------------------------------------

    #[test]
    fn derived_subquery_binds_through_its_alias() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model(
                    "model.t.m",
                    "select s.id as x from (select u.id from \"up\" u) s",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [e("model.t.up.id", "x", ColumnEdgeKind::Rename)]
            .into_iter()
            .collect();
        assert_eq!(got, want);
    }

    // -- 6. correlated subqueries ----------------------------------------------

    #[test]
    fn correlated_subquery_sees_the_outer_scope() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id"]),
                model("model.t.b", "select 1", &["k", "v"]),
                model(
                    "model.t.m",
                    "select a.id, (select b.v from \"b\" b where b.k = a.id) as v \
                     from \"a\" a",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        // `v`'s sources: the projected `b.v`, the predicate's `b.k`, and the
        // correlated outer `a.id` — all folded into the value's sources.
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.a.id", "v", ColumnEdgeKind::Transform),
            e("model.t.b.k", "v", ColumnEdgeKind::Transform),
            e("model.t.b.v", "v", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- 7. join / where-only references are FilterOrJoin -----------------------

    #[test]
    fn row_shaping_refs_fan_out_to_every_output() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "flag"]),
                model("model.t.b", "select 1", &["id", "cc"]),
                model(
                    "model.t.m",
                    "select a.id as aid from \"a\" a join \"b\" b on a.id = b.id \
                     where a.flag and b.cc = 'JP'",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "aid", ColumnEdgeKind::Rename),
            e("model.t.a.flag", "aid", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.id", "aid", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.cc", "aid", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- 8. set operations --------------------------------------------------------

    #[test]
    fn union_merges_positional_sources() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id"]),
                model("model.t.b", "select 1", &["id"]),
                model(
                    "model.t.m",
                    "select id from \"a\" union all select id from \"b\"",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.b.id", "id", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- unqualified ambiguity keeps every candidate ----------------------------

    #[test]
    fn ambiguous_unqualified_keeps_all_candidates() {
        // Both parents own `id`; bare `id` in the projection stays a
        // documented ambiguity → BOTH candidate edges, never a silent pick.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "k"]),
                model("model.t.b", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "select id from \"a\" a join \"b\" b on a.k = b.k",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.b.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.a.k", "id", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.k", "id", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- fallback ----------------------------------------------------------------

    #[test]
    fn unparseable_sql_falls_back_and_is_counted() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model("model.t.m", "select from where (", &["id"]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        let edges = r.resolve(&dag, "model.t.m");
        assert!(r.is_fallback("model.t.m"), "parse failure must fall back");
        assert_eq!(r.fallback_count(), 1);
        assert_eq!(r.fallback_nodes(), vec!["model.t.m".to_string()]);
        // The name-match resolver still answers: `id` is a declared column on
        // both sides → the approximation's Passthrough edge survives.
        assert!(edges.iter().all(|e| e.confidence == Confidence::NameMatch));
    }

    #[test]
    fn missing_sql_uses_the_fallback_without_counting() {
        // A seed (resource_type `seed`, no SQL): same answer as v0 — [] edges —
        // but NOT a "fallback event" (nothing was ever parseable).
        let (k, mut n) = model("model.t.up", "select 1", &["id"]);
        n.resource_type = "seed".to_string();
        n.compiled_code = None;
        let dag = Dag::build(&manifest(vec![(k, n)], &[]));
        let r = resolver();
        assert!(r.resolve(&dag, "model.t.up").is_empty());
        assert!(!r.is_fallback("model.t.up"));
        assert_eq!(r.fallback_count(), 0);
    }

    // -- confidence ---------------------------------------------------------------

    #[test]
    fn parsed_edges_are_exact_raw_or_compiled() {
        // raw_code (Jinja rendered by the resolver) and compiled_code both
        // yield Confidence::Exact once they parse.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                raw_model("model.t.m", "select u.id from {{ ref('up') }} u", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        let edges = r.resolve(&dag, "model.t.m");
        assert!(!r.is_fallback("model.t.m"));
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].confidence, Confidence::Exact);
        assert_eq!(edges[0].from.node, "model.t.up");
    }

    #[test]
    fn stale_compiled_sql_marks_edges_stale() {
        let m = manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model("model.t.m", "select u.id from \"up\" u", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        );
        let mut dag = Dag::build(&m);
        dag.set_compiled_for_test("model.t.m", "select u.id from \"up\" u", true);
        let r = resolver();
        let edges = r.resolve(&dag, "model.t.m");
        assert!(edges.iter().all(|e| e.confidence == Confidence::Stale));
    }

    // -- Jinja rendering -----------------------------------------------------------

    #[test]
    fn jinja_refs_sources_and_blocks_render() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "flag"]),
                raw_model(
                    "model.t.m",
                    "{{ config(materialized='view') }}\n\
                     select u.id\n\
                     from {{ ref('up') }} u\n\
                     {% if true %}\n\
                     where u.flag\n\
                     {% endif %}\n\
                     {# {{ ref('ghost') }} #}",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.flag", "id", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn this_relation_produces_no_parent_edges() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                raw_model(
                    "model.t.m",
                    "select t.id from {{ this }} t, {{ ref('up') }} u where t.id = u.id",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        let edges = r.resolve(&dag, "model.t.m");
        // `u.id` only surfaces as the join's FilterOrJoin fan — never a
        // self-edge, and `{{ this }}` never resolves to a parent.
        assert!(!r.is_fallback("model.t.m"));
        assert!(edges
            .iter()
            .all(|e| e.from.node == "model.t.up" && e.from.column == "id"));
        assert!(edges.iter().all(|e| e.kind == ColumnEdgeKind::FilterOrJoin));
    }

    // -- dialect ---------------------------------------------------------------

    #[test]
    fn dialect_names_parse() {
        for (s, d) in [
            ("auto", SqlDialect::Generic),
            ("generic", SqlDialect::Generic),
            ("postgres", SqlDialect::Postgres),
            ("bigquery", SqlDialect::BigQuery),
            ("BQ", SqlDialect::BigQuery),
            ("snowflake", SqlDialect::Snowflake),
            ("sqlite3", SqlDialect::Sqlite),
            ("redshift", SqlDialect::Redshift),
        ] {
            assert_eq!(SqlDialect::parse_name(s), Some(d), "{s}");
        }
        assert_eq!(SqlDialect::parse_name("not-a-dialect"), None);
    }

    #[test]
    fn dialect_follows_the_adapter_metadata() {
        let mut m = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
        m.metadata.adapter_type = Some("snowflake".to_string());
        let dag = Dag::build(&m);
        assert_eq!(SqlDialect::from_dag(&dag), SqlDialect::Snowflake);
        // No metadata → Generic (source mode never guesses).
        let m2 = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
        assert_eq!(SqlDialect::from_dag(&Dag::build(&m2)), SqlDialect::Generic);
    }

    // -- determinism --------------------------------------------------------------

    #[test]
    fn repeated_resolution_is_deterministic() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "k"]),
                model("model.t.b", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "select a.id as x, id as amb, count(*) as n \
                     from \"a\" a join \"b\" b on a.k = b.k",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let (ra, rb) = (resolver(), resolver());
        assert_eq!(ra.resolve(&dag, "model.t.m"), rb.resolve(&dag, "model.t.m"));
    }

    // -- EXCEPT/INTERSECT: the right arm shapes rows, never values ------------

    #[test]
    fn except_right_arm_shapes_rows_not_values() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "x"]),
                model("model.t.b", "select 1", &["id", "y"]),
                model(
                    "model.t.m",
                    "select id, x from \"a\" except select id, y from \"b\"",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.a.x", "x", ColumnEdgeKind::Passthrough),
            // `b`'s columns only decide which `a` rows survive — FilterOrJoin
            // on every output, never a Passthrough/Rename value edge.
            e("model.t.b.id", "id", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.id", "x", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.y", "id", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.y", "x", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn intersect_right_arm_shapes_rows_not_values() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id"]),
                model("model.t.b", "select 1", &["id"]),
                model(
                    "model.t.m",
                    "select id from \"a\" intersect select id from \"b\"",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.b.id", "id", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
        // And never a value edge from the right arm.
        assert!(got
            .iter()
            .all(|e| e.0 != "model.t.b.id" || e.2 == ColumnEdgeKind::FilterOrJoin));
    }

    #[test]
    fn union_by_name_pairs_on_names_not_positions() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "x"]),
                model("model.t.b", "select 1", &["x", "id"]),
                model(
                    "model.t.m",
                    "select id, x from \"a\" \
                     union all by name select x, id from \"b\"",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.b.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.a.x", "x", ColumnEdgeKind::Passthrough),
            e("model.t.b.x", "x", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- wildcard options -----------------------------------------------------

    #[test]
    fn wildcard_except_and_exclude_drop_columns() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "secret", "status"]),
                model(
                    "model.t.m_except",
                    "select * except (secret) from \"up\"",
                    &[],
                ),
                model(
                    "model.t.m_exclude",
                    "select * exclude (secret) from \"up\"",
                    &[],
                ),
            ],
            &[
                ("model.t.up", "model.t.m_except"),
                ("model.t.up", "model.t.m_exclude"),
            ],
        ));
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        let r = resolver();
        for m in ["model.t.m_except", "model.t.m_exclude"] {
            assert_eq!(edge_set(&dag, &r, m), want, "{m}");
        }
    }

    #[test]
    fn wildcard_rename_and_replace_reshape_columns() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "status"]),
                model(
                    "model.t.m_rename",
                    "select * rename status as state from \"up\"",
                    &[],
                ),
                model(
                    "model.t.m_replace",
                    "select * replace (upper(status) as status) from \"up\"",
                    &[],
                ),
            ],
            &[
                ("model.t.up", "model.t.m_rename"),
                ("model.t.up", "model.t.m_replace"),
            ],
        ));
        let r = resolver();
        // `RENAME` keeps the column raw — a renamed passthrough.
        let want_rename: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "state", ColumnEdgeKind::Rename),
        ]
        .into_iter()
        .collect();
        assert_eq!(edge_set(&dag, &r, "model.t.m_rename"), want_rename);
        // `REPLACE` recomputes the value — a Transform on the same name.
        let want_replace: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "status", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(edge_set(&dag, &r, "model.t.m_replace"), want_replace);
    }

    #[test]
    fn wildcard_ilike_filters_by_name() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "secret", "status"]),
                model("model.t.m", "select * ilike 's%' from \"up\"", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.secret", "secret", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- CTE column-alias lists and sequencing ---------------------------------

    #[test]
    fn cte_column_alias_list_renames_outputs() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "with c(a, b) as (select u.id, u.k * 2 from \"up\" u) \
                     select c.a, c.b from c",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "a", ColumnEdgeKind::Rename),
            e("model.t.up.k", "b", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn sequential_and_shadowing_ctes() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model("model.t.up2", "select 1", &["k"]),
                model(
                    "model.t.seq",
                    "with a as (select u.id from \"up\" u), \
                     b as (select a.id from a) \
                     select b.id from b",
                    &[],
                ),
                // The second `x` shadows the first — SQL binds the latest.
                model(
                    "model.t.sh",
                    "with x as (select u.id as v from \"up\" u), \
                     x as (select w.k as v from \"up2\" w) \
                     select x.v from x",
                    &[],
                ),
            ],
            &[
                ("model.t.up", "model.t.seq"),
                ("model.t.up", "model.t.sh"),
                ("model.t.up2", "model.t.sh"),
            ],
        ));
        let r = resolver();
        let want_seq: BTreeSet<_> = [e("model.t.up.id", "id", ColumnEdgeKind::Passthrough)]
            .into_iter()
            .collect();
        assert_eq!(edge_set(&dag, &r, "model.t.seq"), want_seq);
        let want_sh: BTreeSet<_> = [e("model.t.up2.k", "v", ColumnEdgeKind::Rename)]
            .into_iter()
            .collect();
        assert_eq!(edge_set(&dag, &r, "model.t.sh"), want_sh);
    }

    // -- USING ------------------------------------------------------------------

    #[test]
    fn using_binds_both_sides_of_the_join() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "k"]),
                model("model.t.b", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "select a.id as aid from \"a\" a join \"b\" b using (k)",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "aid", ColumnEdgeKind::Rename),
            e("model.t.a.k", "aid", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.k", "aid", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- local names: lambda params and named-argument names -------------------

    #[test]
    fn lambda_params_never_read_columns() {
        // DuckDB parses arrow lambdas (`x -> expr`): `x` inside the lambda is
        // a LOCAL name — even when a same-named column exists upstream.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["arr", "x", "thresh"]),
                model(
                    "model.t.m",
                    "select list_filter(u.arr, x -> x > 0) as f, \
                     list_filter(u.arr, x -> x > u.thresh) as g \
                     from \"up\" u",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = SqlParseResolver::new(None, SqlDialect::DuckDb);
        let got = edge_set(&dag, &r, "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.arr", "f", ColumnEdgeKind::Transform),
            e("model.t.up.arr", "g", ColumnEdgeKind::Transform),
            e("model.t.up.thresh", "g", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
        // The lambda param `x` drew NO edge to the real `x` column.
        assert!(got.iter().all(|e| e.0 != "model.t.up.x"));
    }

    #[test]
    fn expr_named_args_drop_the_name() {
        // Postgres parses `f(x => v)` with the name as an arbitrary expr —
        // `ExprNamed`. `x` is the argument's NAME, not a column read.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["x", "v"]),
                model("model.t.m", "select f(x => u.v) as s from \"up\" u", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = SqlParseResolver::new(None, SqlDialect::Postgres);
        let got = edge_set(&dag, &r, "model.t.m");
        let want: BTreeSet<_> = [e("model.t.up.v", "s", ColumnEdgeKind::Transform)]
            .into_iter()
            .collect();
        assert_eq!(got, want);
        assert!(got.iter().all(|e| e.0 != "model.t.up.x"));

        // The `name := value` spelling (DuckDB) lands in `FunctionArg::Named`
        // — same rule.
        let dag2 = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["x", "v"]),
                model("model.t.m", "select f(x := u.v) as s from \"up\" u", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r2 = SqlParseResolver::new(None, SqlDialect::DuckDb);
        let got2 = edge_set(&dag2, &r2, "model.t.m");
        assert_eq!(got2, want);
    }

    // -- nested field access ----------------------------------------------------

    #[test]
    fn deep_field_access_reads_the_struct_column() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["cfg", "id"]),
                model(
                    "model.t.m",
                    "select u.cfg.inner.deep as d, cfg.sub as s, \
                     sch.tbl.no_such as n from \"up\" u",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        // `u.cfg.inner.deep`: `u` is bound → its column `cfg`.
        // `cfg.sub`: unqualified struct access → column `cfg`.
        // `sch.tbl.no_such`: `sch` is neither a binding nor a column →
        // nothing (catalog-qualified struct access can't be told apart —
        // documented).
        let want: BTreeSet<_> = [
            e("model.t.up.cfg", "d", ColumnEdgeKind::Rename),
            e("model.t.up.cfg", "s", ColumnEdgeKind::Rename),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- query-level ORDER BY ---------------------------------------------------

    #[test]
    fn order_by_sees_unprojected_columns_and_aliases() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "ts", "f"]),
                model(
                    "model.t.m",
                    "select u.id as iid from \"up\" u order by u.ts, iid",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        // `u.ts` was never projected — the ORDER BY probe still resolves it
        // through the body's scope (this is the regression fix).
        let want: BTreeSet<_> = [
            e("model.t.up.id", "iid", ColumnEdgeKind::Rename),
            e("model.t.up.ts", "iid", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
        // `iid` in ORDER BY binds the output alias — but `up.id` already
        // has a value edge, so it adds nothing.
        assert!(got.iter().all(|e| e.0 != "model.t.up.f"));
    }

    // -- Jinja rendering: literal shielding and malformed input ------------------

    #[test]
    fn jinja_markers_inside_literals_and_comments_do_not_pair() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "s", "t"]),
                raw_model(
                    "model.t.m",
                    "select u.id from {{ ref('up') }} u \
                     where u.s = '{{ not' and u.t = 'jinja }}' \
                     -- {{ also_not }}",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        let got = edge_set(&dag, &r, "model.t.m");
        // The `{{`/`}}` in separate string literals never pair — the SQL
        // parses instead of being eaten, so it must NOT fall back.
        assert!(!r.is_fallback("model.t.m"));
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.s", "id", ColumnEdgeKind::FilterOrJoin),
            e("model.t.up.t", "id", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn malformed_unterminated_jinja_falls_back_not_truncates() {
        // An unterminated `{{` keeps the tail verbatim → the parse fails →
        // per-model fallback. (Silently eating it would fabricate a shorter
        // query with wrong edges.)
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                raw_model(
                    "model.t.m",
                    "select u.id from {{ ref('up') }} u where u.k = {{ unclosed",
                    &["id"],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        let edges = r.resolve(&dag, "model.t.m");
        assert!(r.is_fallback("model.t.m"));
        assert_eq!(r.fallback_nodes(), vec!["model.t.m".to_string()]);
        assert!(edges.iter().all(|e| e.confidence == Confidence::NameMatch));
    }

    // -- synthetic `__colN` outputs never leak -----------------------------------

    #[test]
    fn synthetic_unnamed_outputs_never_leak() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "k", "f", "__col1"]),
                model(
                    "model.t.m",
                    "select u.id, u.k + 1 from \"up\" u where u.f",
                    &[],
                ),
                // A REAL parent column named `__col1` resolves normally.
                model("model.t.real", "select u.__col1 from \"up\" u", &[]),
            ],
            &[("model.t.up", "model.t.m"), ("model.t.up", "model.t.real")],
        ));
        let r = resolver();
        // The unnamed `u.k + 1` output is `__col1` positionally — invisible:
        // `columns()` never invents it, and the WHERE fan doesn't target it.
        assert_eq!(r.columns(&dag, "model.t.m"), vec!["id".to_string()]);
        let got = edge_set(&dag, &r, "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.k", "__col1", ColumnEdgeKind::Transform),
            // `u.f` fans ONLY to the real output — not the placeholder.
            e("model.t.up.f", "id", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);

        let got_real = edge_set(&dag, &r, "model.t.real");
        let want_real: BTreeSet<_> = [e(
            "model.t.up.__col1",
            "__col1",
            ColumnEdgeKind::Passthrough,
        )]
        .into_iter()
        .collect();
        assert_eq!(got_real, want_real);
        assert_eq!(r.columns(&dag, "model.t.real"), vec!["__col1".to_string()]);
    }

    // -- correlated unknown bindings vs. provable outer hits --------------------

    #[test]
    fn inner_unknowns_do_not_steal_provable_outer_hits() {
        // `u`'s column list is unknown (nothing declared, unparseable SQL);
        // `t`'s is known. Bare `z` inside the subquery resolves in the OUTER
        // scope — the inner unknown binding must NOT also claim it.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.t", "select 1", &["id", "z"]),
                model("model.t.u", "", &[]),
                model(
                    "model.t.m",
                    "select t.id as tid, \
                     (select z from \"u\" where u.w = t.id) as q \
                     from \"t\" t",
                    &[],
                ),
            ],
            &[("model.t.t", "model.t.m"), ("model.t.u", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.t.id", "tid", ColumnEdgeKind::Rename),
            e("model.t.t.id", "q", ColumnEdgeKind::Transform),
            e("model.t.t.z", "q", ColumnEdgeKind::Transform),
            // `u.w` stays a candidate — no provable hit exists for `w`.
            e("model.t.u.w", "q", ColumnEdgeKind::Transform),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
        assert!(got.iter().all(|e| e.0 != "model.t.u.z"));

        // Same rule within one level: a KNOWN hit in one binding doesn't get
        // joined by the sibling binding's unknown list.
        let dag2 = Dag::build(&manifest(
            vec![
                model("model.t.t", "select 1", &["id", "x"]),
                model("model.t.u", "", &[]),
                model(
                    "model.t.m",
                    "select x from \"t\" t join \"u\" u on t.id = u.id",
                    &[],
                ),
            ],
            &[("model.t.t", "model.t.m"), ("model.t.u", "model.t.m")],
        ));
        let got2 = edge_set(&dag2, &resolver(), "model.t.m");
        let want2: BTreeSet<_> = [
            e("model.t.t.x", "x", ColumnEdgeKind::Passthrough),
            e("model.t.t.id", "x", ColumnEdgeKind::FilterOrJoin),
            e("model.t.u.id", "x", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got2, want2);
        assert!(got2.iter().all(|e| e.0 != "model.t.u.x"));
    }

    // -- `TABLE name` bodies -----------------------------------------------------

    #[test]
    fn table_statement_reads_the_whole_relation() {
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "select x.id, x.k as kk from (table \"up\") x",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.k", "kk", ColumnEdgeKind::Rename),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- declared-column probe gating (schema.yml drift can't fabricate) ---------

    #[test]
    fn declared_but_unselected_columns_do_not_fabricate_edges() {
        // `region` is documented on `m` (schema drift) but the projection
        // provably never produced it — there is no `*` it could hide
        // behind. The old probe credited `up.region` anyway and emitted an
        // Exact Passthrough edge for a column that was never selected.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "name", "region"]),
                model(
                    "model.t.m",
                    "select u.id as order_id, u.name as customer_name from \"up\" u",
                    &["order_id", "customer_name", "region"],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "order_id", ColumnEdgeKind::Rename),
            e("model.t.up.name", "customer_name", ColumnEdgeKind::Rename),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn wildcard_probe_still_covers_columns_behind_an_unknown_star() {
        // The legitimate probe case: `select *` CAN carry a declared
        // column the analyzer couldn't enumerate when the parent's column
        // list is unknowable — the gate admits exactly that binding.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "", &[]),
                model("model.t.m", "select * from \"up\"", &["id", "region"]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.region", "region", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn quoted_alias_case_matches_the_declared_name() {
        // `AS "User_ID"` produces `User_ID`; documented `user_id` is the
        // same column in the normalized space — it must count as produced
        // (a raw compare would probe a phantom `up.user_id → user_id`
        // edge). `region` stays a real `*`-carried probe hit.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "", &[]),
                model(
                    "model.t.m",
                    "select u.x as \"User_ID\", u.* from \"up\" u",
                    &["user_id", "region"],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            // Edge targets fold through `ColumnRef`'s normalization too —
            // the declared `user_id` spelling is the one that lands.
            e("model.t.up.x", "user_id", ColumnEdgeKind::Rename),
            e("model.t.up.region", "region", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- qualified names never bind CTEs -----------------------------------------

    #[test]
    fn qualified_relation_name_never_binds_a_cte() {
        // `sch.c` is a real relation — compiled `ref('c')` prints
        // `analytics.c` — and can never address the CTE `c`. The parent's
        // `z2` resolves; the CTE's `z` stays unreferenced.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model("model.t.c", "select 1", &["z2"]),
                model(
                    "model.t.m",
                    "with c as (select u.id as z from \"up\" u) \
                     select sch.c.z2 from sch.c",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m"), ("model.t.c", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [e("model.t.c.z2", "z2", ColumnEdgeKind::Passthrough)]
            .into_iter()
            .collect();
        assert_eq!(got, want);
    }

    // -- Jinja branch splits ------------------------------------------------------

    #[test]
    fn jinja_else_and_elif_split_select_items_into_the_branch_union() {
        // `{% else %}`/`{% elif %}` render as `,` so every branch stays a
        // separate select item — union coverage — instead of fusing into
        // `a AS b` (the false branch's name stealing the true branch's
        // value). Whitespace-control dashes parse the same.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "name", "status"]),
                raw_model(
                    "model.t.m",
                    "select {% if x %}a.id{%- elif y -%}a.name{% else %}a.status{% endif %} \
                     from {{ ref('up') }} a",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.up.name", "name", ColumnEdgeKind::Passthrough),
            e("model.t.up.status", "status", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn jinja_else_preserves_the_conditional_comma_idiom() {
        // `{% if %}, b{% else %}, c{% endif %}` — the else sits BETWEEN two
        // literal commas; another `,` there would render `, ,` and kill
        // the parse into a whole-model fallback.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["a", "b", "c"]),
                raw_model(
                    "model.t.m",
                    "select a {% if x %}, b{% else %}, c{% endif %} from {{ ref('up') }}",
                    &[],
                ),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.up.a", "a", ColumnEdgeKind::Passthrough),
            e("model.t.up.b", "b", ColumnEdgeKind::Passthrough),
            e("model.t.up.c", "c", ColumnEdgeKind::Passthrough),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- table alias column lists -------------------------------------------------

    #[test]
    fn parent_table_alias_columns_rename_positionally() {
        // `FROM up t(a, b)` renames the parent's outputs positionally —
        // `t.b` reads the parent's SECOND column (`status`), like a CTE
        // alias list.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id", "status"]),
                model("model.t.m", "select t.b from \"up\" t(a, b)", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [e("model.t.up.status", "b", ColumnEdgeKind::Rename)]
            .into_iter()
            .collect();
        assert_eq!(got, want);
    }

    // -- NATURAL JOIN ----------------------------------------------------------------

    #[test]
    fn natural_join_fans_out_the_shared_columns() {
        // `NATURAL JOIN` keys on every same-named column of both sides:
        // shared {id, k} is row-shaping — `a.id` is already a value source
        // so only the other shared names fan out.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["id", "k"]),
                model("model.t.b", "select 1", &["id", "k"]),
                model(
                    "model.t.m",
                    "select a.id from \"a\" a natural join \"b\" b",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [
            e("model.t.a.id", "id", ColumnEdgeKind::Passthrough),
            e("model.t.b.id", "id", ColumnEdgeKind::FilterOrJoin),
            e("model.t.a.k", "id", ColumnEdgeKind::FilterOrJoin),
            e("model.t.b.k", "id", ColumnEdgeKind::FilterOrJoin),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    // -- duplicate binding names -----------------------------------------------------

    #[test]
    fn a_later_binding_shadows_an_earlier_same_name() {
        // `FROM a t, b t`: the rightmost `t` wins — `t.x` must not reach
        // the earlier binding's `x` column.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.a", "select 1", &["x"]),
                model("model.t.b", "select 1", &["y"]),
                model(
                    "model.t.m",
                    "select t.x as ox, t.y as oy from \"a\" t, \"b\" t",
                    &[],
                ),
            ],
            &[("model.t.a", "model.t.m"), ("model.t.b", "model.t.m")],
        ));
        let got = edge_set(&dag, &resolver(), "model.t.m");
        let want: BTreeSet<_> = [e("model.t.b.y", "oy", ColumnEdgeKind::Rename)]
            .into_iter()
            .collect();
        assert_eq!(got, want);
    }

    // -- adapter dialect reporting ---------------------------------------------------

    #[test]
    fn unknown_adapter_maps_to_generic_and_is_reported() {
        let mut m = manifest(vec![model("model.t.m", "select 1", &[])], &[]);
        m.metadata.adapter_type = Some("not_a_dialect".to_string());
        let dag = Dag::build(&m);
        assert_eq!(SqlDialect::from_dag(&dag), SqlDialect::Generic);
        assert_eq!(
            SqlDialect::unrecognized_adapter(&dag),
            Some("not_a_dialect")
        );
        m.metadata.adapter_type = Some("postgres".to_string());
        let dag = Dag::build(&m);
        assert_eq!(SqlDialect::from_dag(&dag), SqlDialect::Postgres);
        assert_eq!(SqlDialect::unrecognized_adapter(&dag), None);
    }

    #[test]
    fn every_advertised_dialect_name_parses() {
        // `NAMES` feeds `--help` and the `--sql-dialect` error message — it
        // must be a subset of what `parse_name` actually accepts.
        for n in SqlDialect::NAMES {
            assert!(
                SqlDialect::parse_name(n).is_some(),
                "NAMES lists '{n}' but parse_name rejects it"
            );
        }
    }

    // -- columns()-only fallback flagging --------------------------------------------

    #[test]
    fn columns_only_fallback_still_marks_the_node() {
        // A caller that only asks `columns()` (never `resolve()`) must see
        // the same `[name-match]` marking — the flag isn't resolve-only.
        let dag = Dag::build(&manifest(
            vec![
                model("model.t.up", "select 1", &["id"]),
                model("model.t.m", "select from where (", &[]),
            ],
            &[("model.t.up", "model.t.m")],
        ));
        let r = resolver();
        let _ = r.columns(&dag, "model.t.m");
        assert!(r.is_fallback("model.t.m"));
        assert_eq!(r.fallback_nodes(), vec!["model.t.m".to_string()]);
    }
}
