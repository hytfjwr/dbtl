// ---------------------------------------------------------------------------
// Relation/join scan — the lint seam
// ---------------------------------------------------------------------------
//
// A pure-AST walk over a parsed `Query` that records two facts the lint
// SQL rules read:
//
// - `hard_coded`: every relation name written literally in the source. A
//   `Table` factor's name counts ONLY when it carries no `args` (a
//   `Table{args: Some}` is a function call like `generate_series(3)`),
//   isn't a CTE name visible at that point, and isn't a `__dbtl_`-marked
//   token — under `JinjaMark::Marked` renders, `{{ ref }}`/`{{ source }}`/
//   other `{{ }}` all carry the mark, so only genuinely hand-written names
//   land in the set. Names are dotted (`analytics.orders`), sorted.
// - `cartesian`: the display text of every JOIN arm carrying no join
//   condition — a `JoinConstraint::None` arm (`JOIN t`, or `CROSS JOIN t`
//   — the spelling of a product) — plus an `ON` that can never reject a
//   row (`ON TRUE`, `ON 1 = 1`). The constraint is evaluated wherever the
//   grammar carries one: MySQL parses `CROSS JOIN t ON cond` (its INNER
//   JOIN spelling), so the operator alone never decides. `NATURAL`,
//   real `ON`/`USING` conditions, ASOF's match condition, and the
//   apply/array-join operators (lateral invocations, like UNNEST) are
//   never Cartesian.
//
// Subqueries everywhere count — derived tables, nested joins, and
// expression positions (`IN (SELECT …)`, `EXISTS …`, scalar subqueries) —
// so `scan_expr` walks every expression shape `expr_refs` walks, matching
// that exhaustive structure one-for-one.

use std::collections::{BTreeMap, BTreeSet};

use sqlparser::ast::{
    AccessExpr, Distinct, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentClause,
    FunctionArguments, GroupByExpr, InTarget, Join, JoinConstraint, JoinOperator, LimitClause,
    NamedWindowExpr, ObjectNamePart, OrderByKind, PivotValueSource, Query, Select, SelectItem,
    SetExpr, Spanned, Statement, Subscript, TableFactor, TableWithJoins, TopQuantity, WindowSpec,
    WindowType,
};
use sqlparser::tokenizer::Span;

use super::ast::{join_constraint, norm_ident, object_name_parts};
use super::jinja::JINJA_MARK_PREFIX;

/// The relation/join facts a query's FROM/JOIN clauses carry — see the
/// module doc above. `hard_coded` is only meaningful on a
/// `JinjaMark::Marked` parse (an unmarked render maps `{{ ref('x') }}` to
/// a plain name, indistinguishable from a written one).
///
/// The `*_spans`/`column_refs` lists are the lint rules' position data:
/// same walk, one AST span per fact occurrence, in source order. Column
/// refs collect only under [`scan_statements_positions`] — every
/// identifier qualifies, so the facts scan doesn't pay for them.
///
/// `cte_defs`/`cte_uses` are the `unused_cte` bookkeeping: every `WITH`
/// entry the walk declares (index = id) plus the ids a FROM-visible
/// single-part name resolved to — the complement is the unread set.
#[derive(Debug, Default)]
pub(super) struct RelationScan {
    /// Hand-written relation names (`sch.t` dotted), sorted.
    pub hard_coded: BTreeSet<String>,
    /// `JOIN`-arm display text with no join condition, sorted.
    pub cartesian: BTreeSet<String>,
    /// The joined relation's span for each conditionless JOIN arm, in
    /// occurrence order — `cartesian` dedups display text, a model can
    /// hit the same join text twice.
    pub cartesian_spans: Vec<Span>,
    /// Every recorded relation name + its name span, in source order —
    /// the positions `hard_coded_references` points at.
    pub hard_coded_spans: Vec<(String, Span)>,
    /// `(normalized last part, span)` of every column reference, in scan
    /// order — `Identifier` and `CompoundIdentifier` tails, the pool
    /// `unknown_column_reference` matches names against.
    pub column_refs: Vec<(String, Span)>,
    /// `(normalized alias name, alias-name span)` of every `WITH` CTE the
    /// walk declares, in traversal order — an entry's index is its id.
    pub cte_defs: Vec<(String, Span)>,
    /// Ids (indices into `cte_defs`) a relation reference resolved to —
    /// a CTE never named by a FROM position is unread.
    pub cte_uses: BTreeSet<usize>,
    /// Whether `column_refs` fills — set by the positions scan only.
    collect_column_refs: bool,
    /// The CTE name→id frames visible at the current walk point, one per
    /// enclosing query's `WITH` — `find_map` from the innermost frame
    /// out resolves a reference to the CTE it binds. Traversal state,
    /// not output: `scan_query` pushes/pops around each query level.
    cte_frames: Vec<BTreeMap<String, usize>>,
}

/// Scan every statement `stmts` parsed into — models are single statements,
/// but a multi-statement text scans each `Query` (other statement kinds
/// aren't a model's SELECT — the resolver ignores them too).
pub(super) fn scan_statements(stmts: &[Statement]) -> RelationScan {
    scan_stmts(stmts, false)
}

/// [`scan_statements`] with column references collected — the lint
/// positions pass; the facts path leaves them off (dense: one entry per
/// identifier expression anywhere in the query).
pub(super) fn scan_statements_positions(stmts: &[Statement]) -> RelationScan {
    scan_stmts(stmts, true)
}

fn scan_stmts(stmts: &[Statement], collect_column_refs: bool) -> RelationScan {
    let mut out = RelationScan {
        collect_column_refs,
        ..RelationScan::default()
    };
    for s in stmts {
        if let Statement::Query(q) = s {
            scan_query(q, &BTreeSet::new(), &mut out);
        }
    }
    out
}

/// Scan one `Query`: its own CTEs enter scope for its body and every nested
/// query below it (outer CTE names stay visible — the SQL standard), CTE
/// bodies are scanned too (a CTE can read a hand-written table), and the
/// ORDER/LIMIT/FETCH/pipe tail walks.
fn scan_query(q: &Query, ctes: &BTreeSet<String>, out: &mut RelationScan) {
    // A CTE's own body sees the env's names plus the earlier ones — AND its
    // own name (a recursive CTE's self-reference is never a relation).
    // Insert BEFORE scanning the body, then the name stays visible for the
    // rest of the WITH list and the body below.
    let mut scope = ctes.clone();
    // The name→id frame this query's WITH adds, filled one entry per CTE
    // as the walk reaches it so a later sibling resolves earlier ones.
    // Pushed around the body scan only — CTE bodies get a frame holding
    // just the earlier siblings, so a CTE referencing its own name finds
    // nothing (a recursive self-read isn't a use).
    let mut with_frame: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            let name = norm_ident(&cte.alias.name);
            let id = out.cte_defs.len();
            out.cte_defs.push((name.clone(), cte.alias.name.span));
            scope.insert(name.clone());
            let body_scope = scope.clone();
            out.cte_frames.push(with_frame.clone());
            scan_query(&cte.query, &body_scope, out);
            out.cte_frames.pop();
            with_frame.insert(name, id);
        }
    }
    out.cte_frames.push(with_frame);
    scan_set_expr(&q.body, &scope, out);
    if let Some(ob) = &q.order_by {
        if let OrderByKind::Expressions(exprs) = &ob.kind {
            for obe in exprs {
                scan_expr(&obe.expr, &scope, out);
                if let Some(wf) = &obe.with_fill {
                    for e in [&wf.from, &wf.to, &wf.step].into_iter().flatten() {
                        scan_expr(e, &scope, out);
                    }
                }
            }
        }
    }
    if let Some(lc) = &q.limit_clause {
        match lc {
            LimitClause::LimitOffset {
                limit,
                offset,
                limit_by,
            } => {
                for e in limit.iter().chain(limit_by.iter()) {
                    scan_expr(e, &scope, out);
                }
                if let Some(off) = offset {
                    scan_expr(&off.value, &scope, out);
                }
            }
            LimitClause::OffsetCommaLimit { offset, limit } => {
                for e in [offset, limit] {
                    scan_expr(e, &scope, out);
                }
            }
        }
    }
    if let Some(f) = &q.fetch {
        if let Some(qe) = &f.quantity {
            scan_expr(qe, &scope, out);
        }
    }
    for po in &q.pipe_operators {
        scan_pipe(po, &scope, out);
    }
    out.cte_frames.pop();
}

fn scan_set_expr(e: &SetExpr, scope: &BTreeSet<String>, out: &mut RelationScan) {
    match e {
        SetExpr::Select(sel) => scan_select(sel, scope, out),
        SetExpr::Query(q) => scan_query(q, scope, out),
        SetExpr::SetOperation { left, right, .. } => {
            scan_set_expr(left, scope, out);
            scan_set_expr(right, scope, out);
        }
        SetExpr::Values(v) => {
            for row in &v.rows {
                for e in &row.content {
                    scan_expr(e, scope, out);
                }
            }
        }
        // `TABLE [sch.]name` reads the whole relation — a written name like
        // any FROM item (CTE names are unqualified and stay in scope).
        SetExpr::Table(t) => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(s) = &t.schema_name {
                parts.push(norm_ident(s));
            }
            if let Some(n) = &t.table_name {
                parts.push(norm_ident(n));
            }
            let span = Span::union_iter(t.schema_name.iter().chain(&t.table_name).map(|i| i.span));
            record_name(span, &parts, scope, out);
        }
        // Insert/Update/Delete/Merge bodies aren't a model's SELECT — the
        // resolver ignores them too.
        _ => {}
    }
}

fn scan_select(sel: &Select, scope: &BTreeSet<String>, out: &mut RelationScan) {
    for twj in &sel.from {
        scan_twj(twj, scope, out);
    }
    // Comma-separated `FROM a, b` with no WHERE is an unconditional cross
    // product — every relation after the first is joined conditionlessly,
    // so each counts exactly like a conditionless JOIN arm (and carries
    // its own span). A `WHERE` clause, even one that never relates the
    // tables, suppresses the flag — filtered products aren't the
    // foot-gun shape the rule targets. Lateral/UNNEST factors are
    // per-row expansions, not products, same as the JOIN-arm rule.
    if sel.from.len() > 1 && sel.selection.is_none() {
        for twj in sel.from.iter().skip(1) {
            if !functionish_factor(&twj.relation) {
                out.cartesian.insert(twj.relation.to_string());
                out.cartesian_spans.push(twj.relation.span());
            }
        }
    }
    for def in &sel.named_window {
        if let NamedWindowExpr::WindowSpec(spec) = &def.1 {
            scan_window_spec(spec, scope, out);
        }
    }
    for lv in &sel.lateral_views {
        scan_expr(&lv.lateral_view, scope, out);
    }
    if let Some(top) = &sel.top {
        if let Some(TopQuantity::Expr(e)) = &top.quantity {
            scan_expr(e, scope, out);
        }
    }
    for e in [&sel.prewhere, &sel.selection, &sel.having, &sel.qualify]
        .into_iter()
        .flatten()
    {
        scan_expr(e, scope, out);
    }
    if let GroupByExpr::Expressions(exprs, _) = &sel.group_by {
        for e in exprs {
            scan_expr(e, scope, out);
        }
    }
    for cb in &sel.connect_by {
        match cb {
            sqlparser::ast::ConnectByKind::ConnectBy { relationships, .. } => {
                for e in relationships {
                    scan_expr(e, scope, out);
                }
            }
            sqlparser::ast::ConnectByKind::StartWith { condition, .. } => {
                scan_expr(condition, scope, out);
            }
        }
    }
    for e in sel
        .cluster_by
        .iter()
        .chain(&sel.distribute_by)
        .chain(sel.sort_by.iter().map(|o| &o.expr))
    {
        scan_expr(e, scope, out);
    }
    if let Some(Distinct::On(exprs)) = &sel.distinct {
        for e in exprs {
            scan_expr(e, scope, out);
        }
    }
    for item in &sel.projection {
        match item {
            SelectItem::UnnamedExpr(e)
            | SelectItem::ExprWithAlias { expr: e, .. }
            | SelectItem::ExprWithAliases { expr: e, .. } => {
                scan_expr(e, scope, out);
            }
            SelectItem::Wildcard(w) | SelectItem::QualifiedWildcard(_, w) => {
                // `SELECT * REPLACE (expr AS c)` — the only wildcard option
                // carrying expressions (EXCLUDE/EXCEPT/ILIKE are names).
                if let Some(rep) = &w.opt_replace {
                    for item in &rep.items {
                        scan_expr(&item.expr, scope, out);
                    }
                }
            }
        }
    }
}

/// Scan one `FROM`/`JOIN` item: the base relation, each join arm's relation
/// + its (missing) condition, and any `ON`/ASOF expressions.
fn scan_twj(twj: &TableWithJoins, scope: &BTreeSet<String>, out: &mut RelationScan) {
    scan_factor(&twj.relation, scope, out);
    for j in &twj.joins {
        scan_join(j, scope, out);
    }
}

/// A join TARGET that isn't a relation at all — UNNEST, table functions,
/// lateral applies are per-row invocations, not cross products.
fn functionish_factor(f: &TableFactor) -> bool {
    matches!(
        f,
        TableFactor::UNNEST { .. }
            | TableFactor::Function { .. }
            | TableFactor::TableFunction { .. }
            | TableFactor::Table { args: Some(_), .. }
            | TableFactor::Derived { lateral: true, .. }
    )
}

/// Whether an `ON` condition can never reject a row: `ON TRUE` (parens
/// unwrapped) or a self-comparison of equal literals (`ON 1 = 1`,
/// `ON 'x' = 'x'`). Only LITERAL equality counts — `a.id = a.id` is
/// column-shaped and stays a normal (if pointless) condition.
fn tautological_condition(e: &Expr) -> bool {
    match e {
        Expr::Nested(e) => tautological_condition(e),
        Expr::Value(v) => matches!(&v.value, sqlparser::ast::Value::Boolean(true)),
        Expr::BinaryOp {
            left,
            op: sqlparser::ast::BinaryOperator::Eq,
            right,
        } => {
            matches!(&**left, Expr::Value(_))
                && matches!(&**right, Expr::Value(_))
                && left.to_string() == right.to_string()
        }
        _ => false,
    }
}

fn scan_join(j: &Join, scope: &BTreeSet<String>, out: &mut RelationScan) {
    scan_factor(&j.relation, scope, out);
    // A conditionless arm joins every row: `JoinConstraint::None` is a
    // cross product (`CROSS JOIN` spells it intentionally), and an `ON`
    // that can never reject a row (`ON TRUE`, `ON 1 = 1`) joins them all
    // the same. The constraint is evaluated wherever one exists — MySQL
    // parses `CROSS JOIN t ON cond` (its INNER JOIN spelling), so the
    // operator never decides alone. `NATURAL`/real `ON`/`USING`
    // conditions and the apply/ASOF/array operators (a lateral
    // invocation or a conditioned match) never count — and neither does
    // a join TARGET that isn't a relation at all (UNNEST / table
    // functions / lateral applies are per-row, not products).
    let unconditional = !functionish_factor(&j.relation)
        && !matches!(j.join_operator, JoinOperator::AsOf { .. })
        && match join_constraint(j) {
            Some(JoinConstraint::None) => true,
            Some(JoinConstraint::On(e)) => tautological_condition(e),
            _ => false,
        };
    if unconditional {
        out.cartesian.insert(j.to_string());
        // The relation's own span — the `JOIN` keyword stays outside it,
        // so the finding underlines the joined table, not the keyword.
        out.cartesian_spans.push(j.relation.span());
    }
    if let JoinOperator::AsOf {
        match_condition, ..
    } = &j.join_operator
    {
        scan_expr(match_condition, scope, out);
    }
    if let Some(JoinConstraint::On(e)) = join_constraint(j) {
        scan_expr(e, scope, out);
    }
    // `USING`/`USING…AS` lists are column names, never relations.
}

/// Scan one table factor — the only place a hand-written relation name can
/// appear — and every nested factor/query it carries.
fn scan_factor(f: &TableFactor, scope: &BTreeSet<String>, out: &mut RelationScan) {
    match f {
        TableFactor::Table {
            name,
            args,
            with_hints,
            ..
        } => {
            // `t(1, 2)` is a table FUNCTION call (`generate_series(3)`), not
            // a relation name — record nothing for the name itself.
            if args.is_none() {
                let parts = object_name_parts(name);
                record_name(name.span(), &parts, scope, out);
            } else if let Some(a) = args {
                for a in &a.args {
                    scan_function_arg(a, scope, out);
                }
            }
            for e in with_hints {
                scan_expr(e, scope, out);
            }
        }
        TableFactor::Derived { subquery, .. } => scan_query(subquery, scope, out),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => scan_twj(table_with_joins, scope, out),
        TableFactor::Pivot {
            table,
            aggregate_functions,
            value_column,
            value_source,
            default_on_null,
            ..
        } => {
            scan_factor(table, scope, out);
            for ef in aggregate_functions {
                scan_expr(&ef.expr, scope, out);
            }
            for e in value_column {
                scan_expr(e, scope, out);
            }
            scan_pivot_source(value_source, scope, out);
            if let Some(e) = default_on_null {
                scan_expr(e, scope, out);
            }
        }
        TableFactor::Unpivot {
            table,
            value,
            columns,
            ..
        } => {
            scan_factor(table, scope, out);
            scan_expr(value, scope, out);
            for c in columns {
                scan_expr(&c.expr, scope, out);
            }
        }
        TableFactor::UnpivotExpr { expression, .. } => scan_expr(expression, scope, out),
        TableFactor::Function { args, .. } => {
            for a in args {
                scan_function_arg(a, scope, out);
            }
        }
        TableFactor::TableFunction { expr, .. } => scan_expr(expr, scope, out),
        TableFactor::UNNEST { array_exprs, .. } => {
            for e in array_exprs {
                scan_expr(e, scope, out);
            }
        }
        TableFactor::JsonTable { json_expr, .. } | TableFactor::OpenJsonTable { json_expr, .. } => {
            scan_expr(json_expr, scope, out);
        }
        TableFactor::MatchRecognize {
            table,
            partition_by,
            order_by,
            measures,
            symbols,
            ..
        } => {
            scan_factor(table, scope, out);
            for e in partition_by {
                scan_expr(e, scope, out);
            }
            for o in order_by {
                scan_expr(&o.expr, scope, out);
            }
            for m in measures {
                scan_expr(&m.expr, scope, out);
            }
            for sd in symbols {
                scan_expr(&sd.definition, scope, out);
            }
        }
        TableFactor::XmlTable {
            row_expression,
            passing,
            namespaces,
            ..
        } => {
            scan_expr(row_expression, scope, out);
            for ns in namespaces {
                scan_expr(&ns.uri, scope, out);
            }
            for a in &passing.arguments {
                scan_expr(&a.expr, scope, out);
            }
        }
        TableFactor::SemanticView {
            dimensions,
            metrics,
            facts,
            where_clause,
            ..
        } => {
            for e in dimensions.iter().chain(metrics).chain(facts) {
                scan_expr(e, scope, out);
            }
            if let Some(e) = where_clause {
                scan_expr(e, scope, out);
            }
        }
    }
}

/// `PIVOT … IN (…)` — a literal list or a subquery carrying relations.
fn scan_pivot_source(vs: &PivotValueSource, scope: &BTreeSet<String>, out: &mut RelationScan) {
    match vs {
        PivotValueSource::Any(order_by) => {
            for ob in order_by {
                scan_expr(&ob.expr, scope, out);
            }
        }
        PivotValueSource::List(values) => {
            for ewa in values {
                scan_expr(&ewa.expr, scope, out);
            }
        }
        PivotValueSource::Subquery(q) => scan_query(q, scope, out),
    }
}

/// The `|>` pipe tail — joins carry the same conditionless check as a
/// `FROM` join arm; `|> UNION/INTERSECT/EXCEPT` queries and expression
/// arguments all walk.
fn scan_pipe(po: &sqlparser::ast::PipeOperator, scope: &BTreeSet<String>, out: &mut RelationScan) {
    use sqlparser::ast::PipeOperator as P;
    match po {
        P::Where { expr } => scan_expr(expr, scope, out),
        P::OrderBy { exprs } => {
            for e in exprs {
                scan_expr(&e.expr, scope, out);
            }
        }
        P::Limit { expr, offset } => {
            for e in [expr].into_iter().chain(offset.iter()) {
                scan_expr(e, scope, out);
            }
        }
        P::Select { exprs } | P::Extend { exprs } => {
            for item in exprs {
                match item {
                    SelectItem::UnnamedExpr(e)
                    | SelectItem::ExprWithAlias { expr: e, .. }
                    | SelectItem::ExprWithAliases { expr: e, .. } => {
                        scan_expr(e, scope, out);
                    }
                    SelectItem::Wildcard(w) | SelectItem::QualifiedWildcard(_, w) => {
                        if let Some(rep) = &w.opt_replace {
                            for item in &rep.items {
                                scan_expr(&item.expr, scope, out);
                            }
                        }
                    }
                }
            }
        }
        P::Set { assignments } => {
            for a in assignments {
                scan_expr(&a.value, scope, out);
            }
        }
        P::Aggregate {
            full_table_exprs,
            group_by_expr,
        } => {
            for ewa in full_table_exprs.iter().chain(group_by_expr.iter()) {
                scan_expr(&ewa.expr.expr, scope, out);
            }
        }
        P::Union { queries, .. } | P::Intersect { queries, .. } | P::Except { queries, .. } => {
            for q in queries {
                scan_query(q, scope, out);
            }
        }
        P::Join(j) => scan_join(j, scope, out),
        P::Call { function, .. } => scan_function(function, scope, out),
        P::Pivot {
            aggregate_functions,
            value_source,
            ..
        } => {
            for ef in aggregate_functions {
                scan_expr(&ef.expr, scope, out);
            }
            scan_pivot_source(value_source, scope, out);
        }
        // Drop/As/Rename/TableSample/Unpivot carry no expressions that can
        // name a relation.
        _ => {}
    }
}

/// A scalar/EXISTS/ANY-ALL/`IN` subquery hides a whole query — and thus a
/// hand-written relation — inside any expression position. Walk every
/// shape `expr_refs` walks so a NEW sqlparser variant doesn't silently
//  under-scan.
fn scan_expr(e: &Expr, scope: &BTreeSet<String>, out: &mut RelationScan) {
    match e {
        Expr::Subquery(q) | Expr::Exists { subquery: q, .. } => scan_query(q, scope, out),
        Expr::In { expr, target, .. } => {
            scan_expr(expr, scope, out);
            match target {
                InTarget::List(exprs) => {
                    for e in exprs {
                        scan_expr(e, scope, out);
                    }
                }
                InTarget::Subquery(q) => scan_query(q, scope, out),
                InTarget::Unnest(e) => scan_expr(e, scope, out),
                InTarget::Table { name, args } => {
                    // `x IN table_name` (SQLite) reads the named relation.
                    for a in args.iter().flatten() {
                        scan_function_arg(a, scope, out);
                    }
                    record_name(name.span(), &object_name_parts(name), scope, out);
                }
            }
        }
        Expr::CompoundFieldAccess {
            root, access_chain, ..
        } => {
            scan_expr(root, scope, out);
            for a in access_chain {
                match a {
                    AccessExpr::Subscript(sub) => scan_subscript(sub, scope, out),
                    AccessExpr::Dot(e) => scan_expr(e, scope, out),
                }
            }
        }
        Expr::JsonAccess { value, .. } => scan_expr(value, scope, out),
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
        | Expr::Extract { expr: e, .. } => scan_expr(e, scope, out),
        Expr::AtTimeZone {
            timestamp,
            time_zone,
            ..
        } => {
            scan_expr(timestamp, scope, out);
            scan_expr(time_zone, scope, out);
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
            scan_expr(a, scope, out);
            scan_expr(b, scope, out);
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            for e in [expr, low, high] {
                scan_expr(e, scope, out);
            }
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => {
            scan_expr(expr, scope, out);
            scan_expr(pattern, scope, out);
        }
        Expr::Convert { expr, styles, .. } => {
            scan_expr(expr, scope, out);
            for st in styles {
                scan_expr(st, scope, out);
            }
        }
        Expr::Cast { expr, .. } => scan_expr(expr, scope, out),
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            scan_expr(expr, scope, out);
            for e in [substring_from.as_ref(), substring_for.as_ref()]
                .into_iter()
                .flatten()
            {
                scan_expr(e, scope, out);
            }
        }
        Expr::Trim {
            expr,
            trim_what,
            trim_characters,
            ..
        } => {
            scan_expr(expr, scope, out);
            if let Some(e) = trim_what {
                scan_expr(e, scope, out);
            }
            for e in trim_characters.iter().flatten() {
                scan_expr(e, scope, out);
            }
        }
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            for e in [expr, overlay_what, overlay_from]
                .into_iter()
                .chain(overlay_for)
            {
                scan_expr(e, scope, out);
            }
        }
        Expr::Function(f) => scan_function(f, scope, out),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(o) = operand {
                scan_expr(o, scope, out);
            }
            for cw in conditions {
                scan_expr(&cw.condition, scope, out);
                scan_expr(&cw.result, scope, out);
            }
            if let Some(e) = else_result {
                scan_expr(e, scope, out);
            }
        }
        Expr::GroupingSets(sets) | Expr::Cube(sets) | Expr::Rollup(sets) => {
            for set in sets {
                for e in set {
                    scan_expr(e, scope, out);
                }
            }
        }
        Expr::Tuple(exprs) | Expr::Struct { values: exprs, .. } => {
            for e in exprs {
                scan_expr(e, scope, out);
            }
        }
        Expr::Dictionary(fields) => {
            for f in fields {
                scan_expr(&f.value, scope, out);
            }
        }
        Expr::Map(m) => {
            for e in &m.entries {
                scan_expr(&e.key, scope, out);
                scan_expr(&e.value, scope, out);
            }
        }
        Expr::Array(a) => {
            for e in &a.elem {
                scan_expr(e, scope, out);
            }
        }
        Expr::Interval(i) => scan_expr(&i.value, scope, out),
        Expr::MemberOf(m) => {
            scan_expr(&m.value, scope, out);
            scan_expr(&m.array, scope, out);
        }
        Expr::Lambda(l) => scan_expr(&l.body, scope, out),
        // Column references — collected only on the positions scan, as
        // `(last part normalized, span)` pairs. A qualified `t.c` counts
        // once at `c` (the parts are the qualifier).
        Expr::Identifier(i) => {
            if out.collect_column_refs {
                out.column_refs.push((norm_ident(i), i.span));
            }
        }
        Expr::CompoundIdentifier(parts) => {
            if let (true, Some(last)) = (out.collect_column_refs, parts.last()) {
                out.column_refs.push((norm_ident(last), last.span));
            }
        }
        // `MATCH (a, t.b) AGAINST ('…')` — its column list is real column
        // references too (the match value is a literal). Without this a
        // name written once here and once elsewhere looks "unique" to the
        // positional pairing.
        Expr::MatchAgainst { columns, .. } if out.collect_column_refs => {
            for c in columns {
                if let Some(ObjectNamePart::Identifier(i)) = c.0.last() {
                    out.column_refs.push((norm_ident(i), i.span));
                }
            }
        }
        // Literals, typed strings and wildcards carry no subquery.
        _ => {}
    }
}

fn scan_subscript(sub: &Subscript, scope: &BTreeSet<String>, out: &mut RelationScan) {
    match sub {
        Subscript::Index { index } => scan_expr(index, scope, out),
        Subscript::Slice {
            lower_bound,
            upper_bound,
            stride,
        } => {
            for e in [lower_bound, upper_bound, stride].into_iter().flatten() {
                scan_expr(e, scope, out);
            }
        }
    }
}

/// A `Function` call's walkable subquery positions — the shape
/// `function_refs` walks (params + args + clauses + WITHIN GROUP + FILTER +
/// window spec).
fn scan_function(f: &Function, scope: &BTreeSet<String>, out: &mut RelationScan) {
    for args in [&f.parameters, &f.args] {
        match args {
            FunctionArguments::List(l) => {
                for a in &l.args {
                    scan_function_arg(a, scope, out);
                }
                for c in &l.clauses {
                    match c {
                        FunctionArgumentClause::OrderBy(ob) => {
                            for e in ob {
                                scan_expr(&e.expr, scope, out);
                            }
                        }
                        FunctionArgumentClause::Limit(e) | FunctionArgumentClause::Where(e) => {
                            scan_expr(e, scope, out);
                        }
                        FunctionArgumentClause::Having(bound) => {
                            scan_expr(&bound.1, scope, out);
                        }
                        _ => {}
                    }
                }
            }
            FunctionArguments::Subquery(q) => scan_query(q, scope, out),
            _ => {}
        }
    }
    for ob in &f.within_group {
        scan_expr(&ob.expr, scope, out);
    }
    if let Some(flt) = &f.filter {
        scan_expr(flt, scope, out);
    }
    if let Some(WindowType::WindowSpec(spec)) = &f.over {
        scan_window_spec(spec, scope, out);
    }
}

fn scan_window_spec(spec: &WindowSpec, scope: &BTreeSet<String>, out: &mut RelationScan) {
    for e in &spec.partition_by {
        scan_expr(e, scope, out);
    }
    for ob in &spec.order_by {
        scan_expr(&ob.expr, scope, out);
    }
}

fn scan_function_arg(a: &FunctionArg, scope: &BTreeSet<String>, out: &mut RelationScan) {
    let e = match a {
        FunctionArg::Named { arg, .. }
        | FunctionArg::ExprNamed { arg, .. }
        | FunctionArg::Unnamed(arg) => match arg {
            FunctionArgExpr::Expr(e) => Some(e),
            _ => None,
        },
    };
    if let Some(e) = e {
        scan_expr(e, scope, out);
    }
}

/// Record a FROM/JOIN/`TABLE`/`IN` name as hand-written — unless it is a
/// `__dbtl_`-marked Jinja render (generated, not written), a visible CTE
/// name (CTE references are unqualified-only: `sch.c` is always a real
/// relation, matching the binder's rule), or an `information_schema`
/// read at ANY qualification depth (a metadata query, not a bypassed
/// dependency — `db.information_schema.t` and `information_schema.t`
/// alike). `span` is the recorded name's own AST span, kept in
/// `hard_coded_spans` per occurrence.
fn record_name(span: Span, parts: &[String], scope: &BTreeSet<String>, out: &mut RelationScan) {
    // A single-part name resolves against the visible CTE frames
    // innermost-first — a hit counts as that CTE's read (`unused_cte`),
    // independent of whether the same name is also a written relation.
    if let [name] = parts {
        if let Some(id) = out
            .cte_frames
            .iter()
            .rev()
            .find_map(|f| f.get(name.as_str()))
        {
            out.cte_uses.insert(*id);
        }
    }
    if parts.is_empty()
        || parts.iter().any(|p| p.starts_with(JINJA_MARK_PREFIX))
        || (parts.len() == 1 && scope.contains(parts[0].as_str()))
        || parts
            .iter()
            .any(|p| p.eq_ignore_ascii_case("information_schema"))
    {
        return;
    }
    out.hard_coded.insert(parts.join("."));
    out.hard_coded_spans.push((parts.join("."), span));
}
