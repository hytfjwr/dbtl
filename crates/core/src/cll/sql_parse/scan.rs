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
//   condition — `CROSS JOIN` (a cross product by definition) and any
//   constrained-operator join whose constraint is `None` (`JOIN t` with no
//   ON/USING/NATURAL). `NATURAL`, `ON`, `USING`, ASOF's match condition,
//   and the apply/array-join operators (lateral invocations, like UNNEST)
//   are never Cartesian.
//
// Subqueries everywhere count — derived tables, nested joins, and
// expression positions (`IN (SELECT …)`, `EXISTS …`, scalar subqueries) —
// so `scan_expr` walks every expression shape `expr_refs` walks, matching
// that exhaustive structure one-for-one.

use std::collections::BTreeSet;

use sqlparser::ast::{
    AccessExpr, Distinct, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentClause,
    FunctionArguments, GroupByExpr, InTarget, Join, JoinConstraint, JoinOperator, LimitClause,
    NamedWindowExpr, OrderByKind, PivotValueSource, Query, Select, SelectItem, SetExpr, Statement,
    Subscript, TableFactor, TableWithJoins, TopQuantity, WindowSpec, WindowType,
};

use super::ast::{join_constraint, norm_ident, object_name_parts};
use super::jinja::JINJA_MARK_PREFIX;

/// The relation/join facts a query's FROM/JOIN clauses carry — see the
/// module doc above. `hard_coded` is only meaningful on a
/// `JinjaMark::Marked` parse (an unmarked render maps `{{ ref('x') }}` to
/// a plain name, indistinguishable from a written one).
#[derive(Debug, Default)]
pub(super) struct RelationScan {
    /// Hand-written relation names (`sch.t` dotted), sorted.
    pub hard_coded: BTreeSet<String>,
    /// `JOIN`-arm display text with no join condition, sorted.
    pub cartesian: BTreeSet<String>,
}

/// Scan every statement `stmts` parsed into — models are single statements,
/// but a multi-statement text scans each `Query` (other statement kinds
/// aren't a model's SELECT — the resolver ignores them too).
pub(super) fn scan_statements(stmts: &[Statement]) -> RelationScan {
    let mut out = RelationScan::default();
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
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            let name = norm_ident(&cte.alias.name);
            scope.insert(name);
            let body_scope = scope.clone();
            scan_query(&cte.query, &body_scope, out);
        }
    }
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
            record_name(&parts, scope, out);
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

fn scan_join(j: &Join, scope: &BTreeSet<String>, out: &mut RelationScan) {
    scan_factor(&j.relation, scope, out);
    // A `CROSS JOIN` is a cross product by definition; any other
    // constrained operator with `JoinConstraint::None` joins every row.
    // `NATURAL`/`ON`/`USING` conditions and the apply/ASOF/array operators
    // (a lateral invocation or a conditioned match) never count — and
    // neither does a join TARGET that isn't a relation at all (UNNEST /
    // table functions / lateral applies are per-row, not products).
    let functionish = matches!(
        j.relation,
        TableFactor::UNNEST { .. }
            | TableFactor::Function { .. }
            | TableFactor::TableFunction { .. }
            | TableFactor::Table { args: Some(_), .. }
            | TableFactor::Derived { lateral: true, .. }
    );
    let unconditional = !functionish
        && !matches!(j.join_operator, JoinOperator::AsOf { .. })
        && (matches!(j.join_operator, JoinOperator::CrossJoin(_))
            || matches!(join_constraint(j), Some(JoinConstraint::None)));
    if unconditional {
        out.cartesian.insert(j.to_string());
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
                record_name(&parts, scope, out);
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
                    record_name(&object_name_parts(name), scope, out);
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
        // Identifiers, compound identifiers, `MATCH(...) COLUMNS(...)` names,
        // literals, typed strings and wildcards carry no subquery.
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
/// `__dbtl_`-marked Jinja render (generated, not written) or a visible CTE
/// name (CTE references are unqualified-only: `sch.c` is always a real
/// relation, matching the binder's rule).
fn record_name(parts: &[String], scope: &BTreeSet<String>, out: &mut RelationScan) {
    if parts.is_empty()
        || parts.iter().any(|p| p.starts_with(JINJA_MARK_PREFIX))
        || (parts.len() == 1 && scope.contains(parts[0].as_str()))
    {
        return;
    }
    out.hard_coded.insert(parts.join("."));
}
