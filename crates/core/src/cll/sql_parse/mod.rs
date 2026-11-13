//! CLL v1: [`sqlparser`]-backed column resolution.
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
//! column list then come from the approximation and carry
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

mod ast;
mod bind;
mod dialect;
mod expr;
mod facts;
mod jinja;
mod positions;
mod query;
mod scan;
mod scope;
#[cfg(test)]
mod tests;

use ast::*;
pub use dialect::SqlDialect;
use dialect::{date_part_arg_position, is_date_part_word};
use facts::{outer_order_by_span, star_over_single_explicit_cte};
pub(crate) use jinja::trailing_semicolon;
use jinja::*;
use positions::{marked_relations, plain_positions};
pub use positions::{MarkedRelations, SqlPositions};
use scan::scan_statements;
use scope::*;
// ---------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------

/// uid → the node's per-column normalized SELECT expressions (`None` = no
/// expression-level view: missing or unparsable SQL) — the
/// [`SqlParseResolver::exprs_memo`] shape.
type ExprMap = BTreeMap<String, Option<Rc<BTreeMap<String, String>>>>;

/// The SQL-derived facts lint rules read about one node — the
/// [`SqlParseResolver::sql_facts`] payload, distilled from the shared
/// `Analyzed` parse so `select_star`/`cartesian_join`/
/// `unknown_column_reference` all share the one analysis per model.
/// `hard_coded` is deliberately NOT a field here: it comes from a
/// `JinjaMark::Marked` re-parse of the raw text only — see
/// [`SqlParseResolver::hard_coded_relations`].
#[derive(Debug, Clone)]
pub struct SqlFacts {
    /// An output-producing projection (`*`/`t.*`, incl. `|> SELECT *` and
    /// UNION arms) can't enumerate its columns — `select_star` flags it.
    pub unbounded_wildcard: bool,
    /// Occurrence count behind `unbounded_wildcard` (UNION arms and pipe
    /// steps can carry several). The positions pass attaches only when
    /// its raw-render wildcard count matches — a Jinja-dropped UNION arm
    /// can't then shift the underline onto a different arm.
    pub wildcard_count: usize,
    /// Display text of every JOIN arm with no join condition
    /// (a conditional operator with no ON/USING/NATURAL — an explicit
    /// `CROSS JOIN` never counts), sorted — `cartesian_join`'s input.
    pub cartesian: Vec<String>,
    /// Occurrence count behind `cartesian` (the list dedups display text —
    /// a model can hit the same conditionless join twice). The positions
    /// pass attaches only when its raw-render count matches this, so an
    /// arm added/removed by a macro can't misalign positional pairing.
    pub cartesian_count: usize,
    /// `(parent uid, normalized column)` refs the model names but the
    /// parent provably doesn't emit (a KNOWN column list missed them),
    /// sorted — `unknown_column_reference` filters these further by
    /// [`SqlParseResolver::trusted_parent_columns`].
    pub unresolved: Vec<(String, String)>,
    /// `(normalized name, occurrence count)` of every output column name
    /// the output-defining SELECT emits more than once, first-occurrence
    /// order — `duplicate_output_columns`. Expression-shape names
    /// (`sum`/`case`/a cast's operand) count only under Postgres +
    /// Redshift, the dialects that assign them. The positions pass pairs
    /// a judged name only when the raw render counted it exactly once or
    /// the same number of times — the shared name/count key.
    pub duplicate_outputs: Vec<(String, usize)>,
    /// Normalized names of `WITH` CTEs no FROM-visible reference reads,
    /// traversal order — `unused_cte`. A name can repeat (nested queries
    /// each declare one); the positions pass pairs a judged name only
    /// when the raw render's unread declarations counted it exactly once
    /// or the same number of times.
    pub unused_ctes: Vec<String>,
    /// The final query's `ORDER BY` has no `LIMIT`/`FETCH`/`TOP` bound —
    /// `order_by_without_limit`. Positions underlines the ORDER BY
    /// expressions when the raw render's own outer ORDER BY exists.
    pub unlimited_order_by: bool,
    /// The final query is `select * from <one CTE>` and that CTE's
    /// projections are all explicit columns — `select_star`'s
    /// `allow_final_star_over_single_cte` param exempts exactly this
    /// shape; every other wildcard still flags.
    pub star_over_single_explicit_cte: bool,
}

/// The parser-backed resolver: parses each model's SQL (compiled first, raw as
/// the documented fallback) with `sqlparser`, resolves column references
/// against a real FROM/JOIN/CTE scope, and classifies edges passthrough /
/// rename / transform / filter-or-join. Per-model degradation to
/// [`NameMatchResolver`] on unparsable or absent SQL.
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
    /// uid → the node's per-column SELECT expressions (`None` = no
    /// expression-level view: missing or unparsable SQL) — the `--diff`
    /// expression-diff input, memoized like the column list.
    exprs_memo: RefCell<ExprMap>,
    /// uid → the lint-facing [`SqlFacts`] bundle (`None` = no analyzable
    /// SQL) — memoized so every SQL rule shares the one parse.
    facts_memo: RefCell<BTreeMap<String, Option<Rc<SqlFacts>>>>,
    /// uid → hand-written relation names from the `Marked`-render re-parse
    /// (`None` = no `raw_code`, or the marked text didn't parse →
    /// `hard_coded_references` doesn't judge the node). A SEPARATE memo
    /// from `analyzed_memo`: the marked text is a different parse, and a
    /// run enabling only `hard_coded_references` shouldn't pay the
    /// resolution walk.
    hard_coded_memo: RefCell<BTreeMap<String, Option<Rc<MarkedRelations>>>>,
    /// uid → raw-SQL [`SqlPositions`] (`None` = no `raw_code`, a `.py`
    /// model, or the render/parse failed → the finding reports without
    /// positions rather than guessed ones). A SEPARATE memo populated
    /// lazily — the rules call it only for models a fact already flagged.
    positions_memo: RefCell<BTreeMap<String, Option<Rc<SqlPositions>>>>,
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
            exprs_memo: RefCell::new(BTreeMap::new()),
            facts_memo: RefCell::new(BTreeMap::new()),
            hard_coded_memo: RefCell::new(BTreeMap::new()),
            positions_memo: RefCell::new(BTreeMap::new()),
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

    /// The uids that fell back to name-match resolution (sorted). Test-only:
    /// production reads [`Self::fallback_count`] and [`Self::is_fallback`].
    #[cfg(test)]
    pub fn fallback_nodes(&self) -> Vec<String> {
        self.fallbacks.borrow().iter().cloned().collect()
    }

    /// Is `node`'s resolution the name-match approximation?
    pub fn is_fallback(&self, node: &str) -> bool {
        self.fallbacks.borrow().contains(node)
    }

    // -- lint facts -----------------------------------------------------------
    //
    // The lint SQL rules never touch the parser internals: they read these
    // memoized accessors instead. `sql_facts` reuses `analyzed` (one parse
    // per model shared by every SQL rule); `hard_coded_relations` is a
    // deliberately separate path — `JinjaMark::Marked` re-parses the RAW
    // text only, so a `{{ ref }}`-generated name is never mistaken for a
    // written one, and a run enabling only `hard_coded_references` never
    // pays the resolution walk.

    /// The lint-facing facts about `uid`'s SQL, `None` when the node has no
    /// analyzable SQL (missing or unparsable — the rule doesn't judge it).
    /// Memoized; fields come from the shared `analyzed` parse.
    pub fn sql_facts(&self, dag: &Dag, uid: &str) -> Option<Rc<SqlFacts>> {
        if let Some(v) = self.facts_memo.borrow().get(uid) {
            return v.clone();
        }
        let out = self.analyzed(dag, uid).map(|an| {
            Rc::new(SqlFacts {
                unbounded_wildcard: an.unbounded_wildcard,
                wildcard_count: an.wildcard_count,
                cartesian: an.cartesian.clone(),
                cartesian_count: an.cartesian_count,
                unresolved: an.unresolved.iter().cloned().collect(),
                duplicate_outputs: an.duplicate_outputs.clone(),
                unused_ctes: an.unused_ctes.clone(),
                unlimited_order_by: an.unlimited_order_by,
                star_over_single_explicit_cte: an.star_over_single_explicit_cte,
            })
        });
        self.facts_memo
            .borrow_mut()
            .insert(uid.to_string(), out.clone());
        out
    }

    /// The relation names literally written in the node's `raw_code`
    /// (`FROM analytics.orders`) — Jinja-derived names excluded because the
    /// `Marked` render tags `ref`/`source`/other `{{ }}` with a `__dbtl_`
    /// prefix the scan skips; CTE names and table-function calls likewise
    /// never count. `None` when the node has no `raw_code` (compiled-only
    /// models are deliberately NOT judged — a compiled `{{ ref('x') }}`
    /// renders to a written-looking name) or the marked text fails to
    /// parse. `names` are sorted + dotted (`sch.t`); `spans` carry each
    /// recorded occurrence's raw byte range for lint positions.
    ///
    /// Deliberately NOT `resolution_sql`-based: that accessor prefers
    /// compiled SQL, where a `ref()` is indistinguishable from a
    /// hand-written name.
    pub fn hard_coded_relations(&self, dag: &Dag, uid: &str) -> Option<Rc<MarkedRelations>> {
        if let Some(v) = self.hard_coded_memo.borrow().get(uid) {
            return v.clone();
        }
        let out = (|| {
            let raw = dag.raw_code(uid)?;
            if raw.trim().is_empty() {
                return None;
            }
            marked_relations(raw, self.dialect).map(Rc::new)
        })();
        self.hard_coded_memo
            .borrow_mut()
            .insert(uid.to_string(), out.clone());
        out
    }

    /// Source positions in the node's `raw_code` the lint SQL rules attach
    /// to findings. `None` when the node has no `raw_code` or the raw
    /// render/parse produced nothing usable — the finding then reports
    /// file-only, never a guessed position. (`raw_code` on a `.py` model
    /// is Python; the rules' `sql_anchor` refuses positions for it before
    /// reaching here.) Memoized; populated lazily — a rule calls it only
    /// after a fact flagged the model.
    pub fn sql_positions(&self, dag: &Dag, uid: &str) -> Option<Rc<SqlPositions>> {
        if let Some(v) = self.positions_memo.borrow().get(uid) {
            return v.clone();
        }
        let out = (|| {
            let raw = dag.raw_code(uid)?;
            if raw.trim().is_empty() {
                return None;
            }
            plain_positions(raw, self.dialect).map(Rc::new)
        })();
        self.positions_memo
            .borrow_mut()
            .insert(uid.to_string(), out.clone());
        out
    }

    /// Whether `parent`'s column list is a TRUSTED authority for
    /// `unknown_column_reference`: the catalog knows its columns, or the
    /// parent enforces a column contract (its declared `columns:` then ARE
    /// the schema). A declared-only, non-enforced list stays advisory —
    /// yml can lag reality, so "can't disprove" doesn't flag.
    pub fn trusted_parent_columns(&self, dag: &Dag, parent: &str) -> bool {
        if let Some(cat) = &self.catalog {
            if let Some(n) = cat.get(parent) {
                if !n.columns.is_empty() {
                    return true;
                }
            }
        }
        dag.detail(parent).is_some_and(|d| d.contract_enforced)
    }

    /// `analyzed_memo` size — the laziness test's probe: with no SQL rule
    /// enabled, running `run_rules` must leave this at 0 (no parse ever
    /// happened); with several SQL rules on, one entry per model (one
    /// shared parse).
    #[cfg(test)]
    pub(crate) fn analyzed_count(&self) -> usize {
        self.analyzed_memo.borrow().len()
    }

    /// `hard_coded_memo` size — the marked re-parse's laziness probe.
    #[cfg(test)]
    pub(crate) fn hard_coded_count(&self) -> usize {
        self.hard_coded_memo.borrow().len()
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

    // -- per-column expressions (the --diff expression-diff input) ----------

    /// The node's outermost-SELECT output expressions, normalized for
    /// cross-manifest comparison (`None` = no expression-level view:
    /// missing or unparsable SQL — "unknown", never "changed").
    fn expr_map(&self, dag: &Dag, uid: &str) -> Option<Rc<BTreeMap<String, String>>> {
        if let Some(v) = self.exprs_memo.borrow().get(uid) {
            return v.clone();
        }
        let out = self.compute_expr_map(dag, uid).map(Rc::new);
        self.exprs_memo
            .borrow_mut()
            .insert(uid.to_string(), out.clone());
        out
    }

    /// Map each named output column of the node's outermost SELECT to its
    /// canonical expression text. Jinja is rendered like `compute_analyzed`
    /// does, then `expr.to_string()` canonicalizes whitespace/quoting and
    /// `fold_expr_text` folds unquoted identifier case — formatting- and
    /// comment-only edits can never reach the comparison.
    ///
    /// Only `SetExpr::Select` bodies contribute per-column expressions: a
    /// set-operation arm's projection can't be attributed to output columns
    /// without positional/name merge logic, so UNION-shaped models yield an
    /// empty map (no expression-diff flags — the model-level "SQL changed"
    /// reason still catches the edit).
    fn compute_expr_map(&self, dag: &Dag, uid: &str) -> Option<BTreeMap<String, String>> {
        let (text, _) = resolution_sql(dag, uid)?;
        let sql = render_jinja(text, self.dialect);
        let dialect = self.dialect.sqlparser();
        let stmts = Parser::parse_sql(&*dialect, &sql).ok()?;
        let query = stmts.iter().find_map(|s| match s {
            Statement::Query(q) => Some(&**q),
            _ => None,
        })?;
        let SetExpr::Select(sel) = &*query.body else {
            return Some(BTreeMap::new());
        };
        let mut out = BTreeMap::new();
        for item in &sel.projection {
            // Wildcards carry no per-column expression — the expanded names
            // live in `resolve`/`columns`, not here.
            let (name, expr) = match item {
                SelectItem::ExprWithAlias { expr, alias } => (norm_ident(alias), expr),
                SelectItem::UnnamedExpr(expr) => match derive_name(expr) {
                    Some(n) => (n, expr),
                    None => continue,
                },
                _ => continue,
            };
            out.insert(name, fold_expr_text(&expr.to_string()));
        }
        Some(out)
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
            let rendered = render_jinja_mapped(text, JinjaMark::Plain, self.dialect);
            let dialect = self.dialect.sqlparser();
            let stmts = Parser::parse_sql(&*dialect, &rendered.text).ok()?;
            // A render producing no statements — whitespace, comments, or
            // config-only (`{{ config() }}` renders to whitespace) — is
            // judged-and-empty, not unparsable: the SQL rules find nothing,
            // the model doesn't land on `unparsed`.
            if stmts.is_empty() {
                return Some(Rc::new(Analyzed::default()));
            }
            let query = stmts.iter().find_map(|s| match s {
                Statement::Query(q) => Some(&**q),
                _ => None,
            })?;
            let parents = Self::build_parents(dag, node);
            let ctx = Ctx {
                dag,
                parents: &parents,
                depth: Cell::new(0),
                unresolved: RefCell::new(ColSources::new()),
            };
            let ctes = HashMap::new();
            let env = Env {
                outer: None,
                ctes: &ctes,
                locals: &NO_LOCALS,
            };
            let (res, scope) = self.resolve_query_full(&ctx, env, query)?;
            let scan = scan_statements(&stmts, Some(&rendered));
            let wildcards = query_wildcard_items(query);
            // The resolved output list IS the output-name truth: `*`/`t.*`
            // already expanded where bindings know their columns (an
            // unexpandable wildcard contributes nothing, so only the
            // explicit names compare — the documented "can't establish"
            // direction). Synthetic `__colN` placeholders aren't written
            // names and can't collide. Neither can expression-shape names
            // (`sum`/`case`/`a::int`'s `a`) outside the Postgres family —
            // Postgres + Redshift really do assign them and then fail on a
            // collision; every other dialect invents a positional or
            // expression-text name no second expression shares.
            let names_exprs = matches!(self.dialect, SqlDialect::Postgres | SqlDialect::Redshift);
            let mut dup_order: Vec<String> = Vec::new();
            let mut dup_counts: Vec<usize> = Vec::new();
            for o in &res.out {
                if o.synthetic || (o.derived && !names_exprs) {
                    continue;
                }
                match dup_order.iter().position(|n| *n == o.name) {
                    Some(i) => dup_counts[i] += 1,
                    None => {
                        dup_order.push(o.name.clone());
                        dup_counts.push(1);
                    }
                }
            }
            Some(Rc::new(Analyzed {
                scope,
                out: res.out,
                foj: res.foj,
                unbounded_wildcard: !wildcards.is_empty(),
                wildcard_count: wildcards.len(),
                cartesian: scan.cartesian.iter().cloned().collect(),
                cartesian_count: scan.cartesian_spans.len(),
                unresolved: ctx.unresolved.into_inner(),
                duplicate_outputs: dup_order
                    .into_iter()
                    .zip(dup_counts)
                    .filter(|(_, c)| *c > 1)
                    .collect(),
                unused_ctes: scan.unread_ctes().map(|(name, _)| name.clone()).collect(),
                unlimited_order_by: outer_order_by_span(query).is_some(),
                star_over_single_explicit_cte: star_over_single_explicit_cte(query),
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
            unresolved: RefCell::new(ColSources::new()),
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

    /// Same catalog → declared priority as `columns` (shared with the
    /// name-match fallback, which owns the same catalog handle).
    fn column_type(&self, dag: &Dag, node: &str, column: &str) -> Option<String> {
        self.fallback.column_type(dag, node, column)
    }

    /// Per-column normalized SELECT expressions — `None` (not an empty map)
    /// when the node's SQL is missing or unparsable, so the diff can tell
    /// "no expression data" apart from "no named expressions".
    fn column_exprs(&self, dag: &Dag, node: &str) -> Option<BTreeMap<String, String>> {
        self.expr_map(dag, node).map(|rc| (*rc).clone())
    }

    /// Dead references the analysis provably recorded — sorted for
    /// deterministic downstream iteration. `None`/unparsable SQL → empty
    /// (nothing proven), same contract as `resolve`.
    fn unresolved_refs(&self, dag: &Dag, node: &str) -> Vec<(String, String)> {
        self.analyzed(dag, node)
            .map(|a| a.unresolved.iter().cloned().collect())
            .unwrap_or_default()
    }
}
