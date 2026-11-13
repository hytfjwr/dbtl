// Output-shape lint facts over the judged parse's `Query`.
//
// Two of the lint SQL rules read facts the relation scan (`scan.rs`)
// doesn't carry: repeated output column names (`duplicate_output_columns`)
// and a final `ORDER BY` with no row bound (`order_by_without_limit`).
// Both are small one-query walks — the facts side runs them on
// `compute_analyzed`'s (compiled-preferred) `Query`, and the positions
// pass re-runs them on the raw render so a span can never point at a name
// the raw text didn't write. `unused_cte` needs the relation walk's FROM
// coverage instead — it rides the scan's own CTE bookkeeping
// (`RelationScan::cte_defs`/`cte_uses`).

use sqlparser::ast::{
    AccessExpr, Expr, LimitClause, ObjectNamePart, Query, Select, SelectItem, SetExpr, Spanned,
    TableFactor,
};
use sqlparser::tokenizer::Span;

use super::ast::{norm_ident, object_name_parts, query_wildcard_items};

/// The `Select` whose projection names the output — the leftmost arm of a
/// set-operation chain (`UNION`/`EXCEPT`/`INTERSECT` name their columns
/// from the first arm's projection, a duplicate confined to a later arm
/// isn't the model's output name). `None` when the shape can't establish
/// an output projection: non-SELECT bodies, and any `|>`-piped query —
/// a `|> SELECT`/`EXTEND`/`DROP` step can rename or re-project past the
/// body's projection, so only a pipe-free query's first arm is provably
/// the output.
fn leftmost_select(e: &SetExpr) -> Option<&Select> {
    match e {
        SetExpr::Select(sel) => Some(sel),
        SetExpr::Query(q) if q.pipe_operators.is_empty() => leftmost_select(&q.body),
        SetExpr::SetOperation { left, .. } => leftmost_select(left),
        _ => None,
    }
}

/// The `(normalized name, name span)` an un-aliased projection expr
/// contributes — the same name `derive_name` would give it, paired with
/// the span the name part covers so a duplicate can point at its
/// occurrence. `None` when the expr contributes no name (a positional
/// literal `1` is named by position, `*` contributes nothing concrete).
fn name_span(e: &Expr) -> Option<(String, Span)> {
    match e {
        Expr::Identifier(i) => Some((norm_ident(i), i.span)),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| (norm_ident(i), i.span)),
        Expr::CompoundFieldAccess {
            root, access_chain, ..
        } => access_chain
            .iter()
            .rev()
            .find_map(|a| match a {
                AccessExpr::Dot(e) => name_span(e),
                _ => None,
            })
            .or_else(|| name_span(root)),
        Expr::Nested(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Named { expr: inner, .. } => name_span(inner),
        Expr::Prefixed { value, .. } => name_span(value),
        Expr::Function(f) => f
            .name
            .0
            .iter()
            .filter_map(|p| match p {
                ObjectNamePart::Identifier(i) => Some((norm_ident(i), i.span)),
                _ => None,
            })
            .next_back(),
        // `CASE`'s derived name is literally `case`; the keyword span is
        // the occurrence's underline (empty when the token wasn't kept).
        Expr::Case { case_token, .. } => Some(("case".to_string(), case_token.0.span)),
        _ => None,
    }
}

/// `(normalized name, occurrence spans)` for every named output the
/// model's output-defining SELECT emits — `duplicate_output_columns`'s
/// raw-side occurrences. Wildcards never contribute: a `*` can't be
/// enumerated here, so `select *, a, a` still catches the explicit `a`s
/// while `select *, a` claims nothing (the wildcard-carried `a` may or
/// may not collide — "can't establish" stays silent). Names compare by
/// [`norm_ident`] — unquoted folds case, quoted keeps it: `A`/`a`
/// collide, `"A"`/`a` don't. Entries keep first-occurrence order; the
/// positions pass pairs a judged duplicate only when raw counted it
/// exactly once or the same number of times (the shared name/count key).
pub(super) fn output_name_occurrences(q: &Query) -> Vec<(String, Vec<Span>)> {
    if !q.pipe_operators.is_empty() {
        return Vec::new();
    }
    let Some(sel) = leftmost_select(&q.body) else {
        return Vec::new();
    };
    let mut order: Vec<String> = Vec::new();
    let mut spans: Vec<Vec<Span>> = Vec::new();
    for item in &sel.projection {
        match item {
            SelectItem::ExprWithAlias { alias, .. } => {
                push_name(&mut order, &mut spans, norm_ident(alias), alias.span);
            }
            SelectItem::ExprWithAliases { aliases, .. } => {
                for a in aliases {
                    push_name(&mut order, &mut spans, norm_ident(a), a.span);
                }
            }
            SelectItem::UnnamedExpr(e) => {
                if let Some((name, span)) = name_span(e) {
                    push_name(&mut order, &mut spans, name, span);
                }
            }
            _ => {}
        }
    }
    order.into_iter().zip(spans).collect()
}

fn push_name(order: &mut Vec<String>, spans: &mut Vec<Vec<Span>>, name: String, span: Span) {
    match order.iter().position(|n| *n == name) {
        Some(i) => spans[i].push(span),
        None => {
            order.push(name);
            spans.push(vec![span]);
        }
    }
}

/// The `Select` the model's final query IS — a bare `SELECT` (possibly
/// wrapped in `SetExpr::Query` parens), nothing else. Unlike
/// [`leftmost_select`], set operations don't unwrap: the
/// `select * from final` exemption proves the WHOLE output comes from one
/// explicit CTE, and a `UNION` arm's own `*`/`FROM` can't be accounted
/// for by that shape.
fn bare_final_select(e: &SetExpr) -> Option<&Select> {
    match e {
        SetExpr::Select(sel) => Some(sel),
        SetExpr::Query(q) if q.pipe_operators.is_empty() => bare_final_select(&q.body),
        _ => None,
    }
}

/// The `select * from <one CTE>` closing shape many dbt projects end a
/// model on: the final query's FROM reads exactly one `WITH` CTE — no
/// joins, no other relations — and that CTE's own query names every
/// column explicitly (no `*`/`t.*` in any of its projections). The
/// wildcard's columns are then provably enumerated inside the CTE, so
/// `select_star`'s `allow_final_star_over_single_cte` param can exempt
/// exactly this shape. Everything else — a `*` inside the CTE, a join or
/// second FROM entry, a subquery/table FROM, `|>` steps — keeps flagging:
/// "can't prove" stays a violation.
pub(super) fn star_over_single_explicit_cte(q: &Query) -> bool {
    if !q.pipe_operators.is_empty() {
        return false;
    }
    let Some(sel) = bare_final_select(&q.body) else {
        return false;
    };
    let [from] = &sel.from[..] else {
        return false;
    };
    if !from.joins.is_empty() {
        return false;
    }
    let TableFactor::Table { name, .. } = &from.relation else {
        return false;
    };
    // A CTE reference is one identifier — a qualified `schema.final` is a
    // real relation, never a `WITH` name.
    let parts = object_name_parts(name);
    let [from_name] = &parts[..] else {
        return false;
    };
    let Some(with) = &q.with else {
        return false;
    };
    let Some(cte) = with
        .cte_tables
        .iter()
        .find(|c| norm_ident(&c.alias.name) == *from_name)
    else {
        return false;
    };
    query_wildcard_items(&cte.query).is_empty()
}

/// The outermost `ORDER BY`'s expression range when the model's final
/// query sorts rows nothing bounds — `order_by_without_limit`. `None`
/// when nothing flags: no `ORDER BY` at the final-query level, or
/// `LIMIT`/`FETCH`/`TOP` bounds it. `SetExpr::Query` bodies are
/// transparent — a parenthesized final query's own tail is the model's
/// tail — while `LIMIT`/`FETCH`/`TOP` found on any level between the
/// `ORDER BY` and the file's outermost query bounds it (a `LIMIT 3`
/// wrapping `… order by b` makes the inner sort meaningful). A
/// `|>`-piped level stops the walk entirely: the steps can sort or bound
/// past what `query.order_by` alone shows. The span covers the ORDER BY
/// EXPRESSIONS (sqlparser's span starts after the keyword), and is empty
/// for `ORDER BY ALL` — flagged without a position.
pub(super) fn outer_order_by_span(q: &Query) -> Option<Span> {
    let mut q = q;
    loop {
        if !q.pipe_operators.is_empty() {
            return None;
        }
        let bounded = row_bound(q);
        // `ORDER BY ALL` carries no expression span (`Span::empty`) — the
        // finding still fires, the underline just can't be computed.
        if let Some(ob) = &q.order_by {
            return (!bounded).then(|| ob.span());
        }
        if bounded {
            return None;
        }
        match &*q.body {
            SetExpr::Query(inner) => q = inner,
            _ => return None,
        }
    }
}

/// `q`'s own row bound — a LIMIT count (`LIMIT n`, `LIMIT … BY`, the
/// comma form), `FETCH`, or a bare-`SELECT` body's `TOP`. An `OFFSET`
/// alone is NOT a bound: `order by a offset 2` still sorts an unbounded
/// relation downstream can't rely on, so it flags like the un-offset
/// shape.
fn row_bound(q: &Query) -> bool {
    let limited = match &q.limit_clause {
        Some(LimitClause::LimitOffset {
            limit, limit_by, ..
        }) => limit.is_some() || !limit_by.is_empty(),
        Some(LimitClause::OffsetCommaLimit { .. }) => true,
        None => false,
    };
    limited || q.fetch.is_some() || matches!(&*q.body, SetExpr::Select(s) if s.top.is_some())
}
