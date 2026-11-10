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
//!   "can't disprove" beats silently dropping the edge.
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
    RenameSelectItem, Select, SelectItem, SelectItemQualifiedWildcardKind, SetExpr, Statement,
    TableAliasColumnDef, TableFactor, TableWithJoins, WildcardAdditionalOptions, WindowSpec,
    WindowType,
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
    /// Every `--sql-dialect` value `parse_name` accepts (for `--help`/tests).
    pub const NAMES: &'static [&'static str] = &[
        "bigquery",
        "clickhouse",
        "databricks",
        "duckdb",
        "generic",
        "hive",
        "mssql",
        "mysql",
        "oracle",
        "postgres",
        "postgresql",
        "redshift",
        "snowflake",
        "spark",
        "sqlite",
        "teradata",
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
fn render_jinja(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + 16);
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'{' && matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) {
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
            let (inner_end, advance) = match end {
                Some(e) => (e, e + 2),
                None => (b.len(), b.len()),
            };
            match kind {
                b'{' => {
                    let inner = &sql[i + 2..inner_end];
                    out.extend_from_slice(render_jinja_expr(inner).as_bytes());
                }
                // `{% … %}` tags and `{# … #}` comments drop entirely — the
                // tag's body (if/for branches) keeps flowing into the output.
                _ => out.push(b' '),
            }
            i = advance;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    // Jinja interiors are ASCII for anything we render; copied bytes keep
    // the original UTF-8 untouched.
    String::from_utf8(out).unwrap_or_default()
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
/// correlation plus the CTE names in force.
#[derive(Clone, Copy)]
struct Env<'a> {
    outer: Option<&'a Scope<'a>>,
    ctes: &'a HashMap<String, Rc<QueryRes>>,
}

/// The context threaded through *expression* resolution: the scope chain
/// (column binding + correlation) and the CTE table for subqueries that
/// reference `WITH` names.
#[derive(Clone, Copy)]
struct RefScope<'a> {
    scope: Option<&'a Scope<'a>>,
    ctes: &'a HashMap<String, Rc<QueryRes>>,
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
        if let Some(an) = self.analyzed(dag, uid) {
            if !an.out.is_empty() {
                return Some(Rc::new(an.out.iter().map(|o| o.name.clone()).collect()));
            }
        }
        let cols = self.fallback.columns(dag, uid);
        (!cols.is_empty()).then(|| Rc::new(cols))
    }

    // -- the analyze pass ---------------------------------------------------

    /// The model's analyzed SQL, memoized (`None` = couldn't analyze → the
    /// caller takes the name-match path). Cycle-guarded.
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
            };
            let (res, scope) = self.resolve_query_full(&ctx, env, query)?;
            Some(Rc::new(Analyzed {
                scope,
                out: res.out,
                foj: res.foj,
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
        };
        let (mut res, scope) = match &*q.body {
            SetExpr::Select(sel) => self.resolve_select(ctx, inner, sel)?,
            body => (self.resolve_set_expr(ctx, inner, body)?, Vec::new()),
        };
        // Query-level ORDER BY over the body's outputs: `order by <name>`
        // matching an output name shapes the row set → FilterOrJoin. (It
        // can also see FROM columns, but the scope is gone here — documented
        // under-resolution, never a wrong edge.)
        if let Some(ob) = &q.order_by {
            if let OrderByKind::Expressions(exprs) = &ob.kind {
                for e in exprs {
                    for name in expr_ident_names(&e.expr) {
                        if let Some(o) = res.out.iter().find(|o| o.name == name) {
                            res.foj.extend(o.sources.iter().cloned());
                        }
                    }
                }
            }
        }
        Some((res, scope))
    }

    fn resolve_query(&self, ctx: &Ctx<'_>, env: Env<'_>, q: &Query) -> Option<QueryRes> {
        self.resolve_query_full(ctx, env, q).map(|(r, _)| r)
    }

    fn resolve_set_expr(&self, ctx: &Ctx<'_>, env: Env<'_>, e: &SetExpr) -> Option<QueryRes> {
        match e {
            SetExpr::Select(sel) => self.resolve_select(ctx, env, sel).map(|(r, _)| r),
            SetExpr::Query(q) => self.resolve_query(ctx, env, q),
            SetExpr::SetOperation { left, right, .. } => {
                // UNION/INTERSECT/EXCEPT: positional merge — output i carries
                // every arm's column-i sources; name/bare come from the left
                // arm when present (SQL's own rule).
                let l = self.resolve_set_expr(ctx, env, left)?;
                let r = self.resolve_set_expr(ctx, env, right)?;
                let mut out = Vec::new();
                for i in 0..l.out.len().max(r.out.len()) {
                    match (l.out.get(i), r.out.get(i)) {
                        (Some(a), Some(b)) => out.push(OutCol {
                            name: a.name.clone(),
                            // An arm contributing no sources (a `null` arm)
                            // doesn't touch the value's bareness — only real
                            // contributors vote.
                            bare: match (a.sources.is_empty(), b.sources.is_empty()) {
                                (false, true) => a.bare,
                                (true, false) => b.bare,
                                _ => a.bare && b.bare,
                            },
                            sources: a.sources.union(&b.sources).cloned().collect(),
                        }),
                        (Some(a), None) => out.push(a.clone()),
                        (None, Some(b)) => out.push(b.clone()),
                        (None, None) => {}
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
                    })
                    .collect();
                Some(QueryRes { out, foj })
            }
            _ => Some(QueryRes::default()),
        }
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
        // on this scope level so the function walker can find them.
        for def in &sel.named_window {
            if let NamedWindowExpr::WindowSpec(spec) = &def.1 {
                scope.windows.insert(norm_ident(&def.0), spec.clone());
            }
        }

        let refs = ref_scope(env, &scope);

        // LATERAL VIEWs explode a column into rows — their input expressions
        // are row-shaping.
        for lv in &sel.lateral_views {
            res.foj.extend(self.expr_refs(ctx, refs, &lv.lateral_view)?);
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

        // The projection → output columns.
        let mut out: Vec<OutCol> = Vec::new();
        for item in &sel.projection {
            match item {
                SelectItem::UnnamedExpr(e) => {
                    let sources = self.expr_refs(ctx, refs, e)?;
                    out.push(OutCol {
                        name: derive_name(e).unwrap_or_default(),
                        bare: self.projection_bare(ctx, refs, e),
                        sources,
                    });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    let sources = self.expr_refs(ctx, refs, expr)?;
                    out.push(OutCol {
                        name: norm_ident(alias),
                        bare: self.projection_bare(ctx, refs, expr),
                        sources,
                    });
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    let sources = self.expr_refs(ctx, refs, expr)?;
                    for a in aliases {
                        out.push(OutCol {
                            name: norm_ident(a),
                            bare: self.projection_bare(ctx, refs, expr),
                            sources: sources.clone(),
                        });
                    }
                }
                SelectItem::Wildcard(opts) => {
                    self.expand_wildcard(ctx, refs, &scope, None, opts, &mut out)?;
                }
                SelectItem::QualifiedWildcard(kind, opts) => match kind {
                    SelectItemQualifiedWildcardKind::ObjectName(on) => {
                        let parts = object_name_parts(on);
                        if let Some(qual) = parts.last() {
                            self.expand_wildcard(
                                ctx,
                                refs,
                                &scope,
                                Some(qual.clone()),
                                opts,
                                &mut out,
                            )?;
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
                        });
                    }
                },
            }
        }

        // SELECT-level EXCLUDE (Redshift `SELECT * EXCLUDE(col)` parses at
        // the Select level, not as wildcard options).
        if let Some(excl) = &sel.exclude {
            let names = exclude_names(excl);
            out.retain(|o| !names.contains(&o.name));
        }

        // Unnamed outputs (literals, unnameable exprs) get a positional
        // placeholder — `columns()` can list them without inventing names
        // that collide with real ones.
        let mut anon = 0usize;
        for o in &mut out {
            if o.name.is_empty() {
                anon += 1;
                o.name = format!("__col{anon}");
            }
        }
        res.out = out;
        Some((res, scope.bindings))
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
                let src = if let Some(q) = env.ctes.get(&last) {
                    // The CTE's own row-shaping deps (WHERE/JOIN inside it)
                    // shape every row this select reads — merge them.
                    res.foj.extend(q.foj.iter().cloned());
                    TableSrc::Derived(rename_rc(
                        q,
                        alias.as_ref().map(|a| a.columns.as_slice()).unwrap_or(&[]),
                    ))
                } else if let Some(p) = Self::match_parent(ctx.parents, &parts) {
                    TableSrc::Parent(p.uid.clone())
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
                alias, array_exprs, ..
            } => {
                let refs = ref_scope(env, scope);
                for e in array_exprs {
                    res.foj.extend(self.expr_refs(ctx, refs, e)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__unnest".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::Pivot {
                table,
                aggregate_functions,
                alias,
                ..
            } => {
                // The inner table still binds (its columns feed the pivot
                // values); the pivoted result itself binds un-nameable.
                self.bind_relation(ctx, env, scope, res, table)?;
                let refs = ref_scope(env, scope);
                for ef in aggregate_functions {
                    res.foj.extend(self.expr_refs(ctx, refs, &ef.expr)?);
                }
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__pivot".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::Unpivot { table, alias, .. } => {
                self.bind_relation(ctx, env, scope, res, table)?;
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__unpivot".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::JsonTable {
                json_expr, alias, ..
            }
            | TableFactor::OpenJsonTable {
                json_expr, alias, ..
            } => {
                res.foj
                    .extend(self.expr_refs(ctx, ref_scope(env, scope), json_expr)?);
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__json".to_string()),
                    src: TableSrc::External,
                });
            }
            TableFactor::MatchRecognize { table, alias, .. } => {
                self.bind_relation(ctx, env, scope, res, table)?;
                scope.bindings.push(Binding {
                    name: alias
                        .as_ref()
                        .map(|a| norm_ident(&a.name))
                        .unwrap_or_else(|| "__mr".to_string()),
                    src: TableSrc::External,
                });
            }
            _ => {
                // TableMethod / XmlTable / others we don't model: bind as
                // External so qualified refs can't poison the scope.
                scope.bindings.push(Binding {
                    name: "__other".to_string(),
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
                    _ => {}
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
                s.extend(self.expr_refs(ctx, refs, &l.body)?);
            }
            // Literals, wildcards (handled by the projection), and every
            // remaining variant contribute no column refs.
            _ => {}
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
            FunctionArg::ExprNamed { name, arg, .. } => {
                s.extend(self.expr_refs(ctx, refs, name)?);
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
            // `qual` names a binding in this scope or an outer one.
            let mut sc = Some(scope);
            while let Some(s) = sc {
                if let Some(b) = s.bindings.iter().find(|b| b.name == *qual) {
                    return self.resolve_in_binding(ctx, b, col);
                }
                sc = s.outer;
            }
            // `a.b` where `a` isn't a binding: struct access on a column —
            // the real source is column `a` (unqualified).
            if parts.len() == 2 {
                return self.resolve_unqualified(ctx, scope, qual);
            }
            return ColSources::new();
        }
        let Some(first) = parts.first() else {
            return ColSources::new();
        };
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

    /// An unqualified `col`: every binding in the innermost scope that HAS
    /// it contributes (ambiguous → all candidates, the documented
    /// over-connection). A binding with an UNKNOWN column list can't
    /// disprove the ref, so its (uid, col) also joins the candidates.
    fn resolve_unqualified(&self, ctx: &Ctx<'_>, scope: &Scope<'_>, col: &str) -> ColSources {
        let mut found = ColSources::new();
        let mut unknown: BTreeSet<String> = BTreeSet::new();
        let mut sc = Some(scope);
        while let Some(s) = sc {
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
            if !found.is_empty() {
                break; // inner scope wins — but unknowns still can't disprove
            }
            sc = s.outer;
        }
        for uid in unknown {
            found.insert((uid, col.to_string()));
        }
        found
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
                if let Some(b) = s.bindings.iter().find(|b| b.name == *qual) {
                    return Self::bare_in_binding(b, col);
                }
                sc = s.outer;
            }
            if parts.len() == 2 {
                return self.bare_unqualified(ctx, scope, qual);
            }
            return None;
        }
        self.bare_unqualified(ctx, scope, parts.first()?)
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
    /// the innermost scope's candidates win, and the ref stays bare only
    /// when EVERY candidate is a bare column (ambiguous mixed sources keep
    /// the conservative transform flag).
    fn bare_unqualified(&self, ctx: &Ctx<'_>, scope: &Scope<'_>, col: &str) -> Option<bool> {
        let mut bares: Vec<bool> = Vec::new();
        let mut unknown = false;
        let mut sc = Some(scope);
        while let Some(s) = sc {
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
            if !bares.is_empty() {
                break;
            }
            sc = s.outer;
        }
        if bares.is_empty() {
            // Unknown parents can't disprove the ref (they'd resolve it raw).
            return unknown.then_some(true);
        }
        Some(bares.iter().all(|b| *b))
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
            produced.insert(o.name.clone());
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
        // column the projection couldn't name (behind a `*` of an unknown
        // parent, an unexpanded alias, …) still gets a Passthrough edge when
        // the top-level scope can source it.
        let out_cols: Vec<String> = self
            .col_set(dag, node)
            .map(|rc| (*rc).clone())
            .unwrap_or_else(|| an.out.iter().map(|o| o.name.clone()).collect());
        let probe = Scope {
            bindings: an.scope.clone(),
            windows: HashMap::new(),
            outer: None,
        };
        for c in &out_cols {
            if produced.contains(c) {
                continue;
            }
            for (uid, col) in self.resolve_unqualified(&ctx, &probe, c) {
                direct_sources.insert((uid.clone(), col.clone()));
                edges.insert(ColumnEdge {
                    from: ColumnRef::new(uid, &col),
                    to: ColumnRef::new(node, c),
                    kind: ColumnEdgeKind::Passthrough,
                    confidence,
                });
            }
        }

        // FilterOrJoin: row-shaping sources with no value edge fan out to
        // every visible output column (same attachment policy as v0).
        let mut foj_targets: BTreeSet<String> = out_cols.into_iter().collect();
        foj_targets.extend(an.out.iter().map(|o| o.name.clone()));
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
        | Expr::Collate { expr: inner, .. }
        | Expr::Named { expr: inner, .. } => derive_name(inner),
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

/// A resolved query whose outputs are positionally renamed by a
/// `WITH x(a,b) AS` / `(sub) AS t(a,b)` column-alias list.
fn rename_outputs(mut res: QueryRes, cols: &[TableAliasColumnDef]) -> QueryRes {
    if cols.is_empty() {
        return res;
    }
    for (o, c) in res.out.iter_mut().zip(cols.iter()) {
        o.name = norm_ident(&c.name);
    }
    res
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
}
