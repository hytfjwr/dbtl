// ---------------------------------------------------------------------------
// Raw-SQL positions for the lint SQL rules
// ---------------------------------------------------------------------------
//
// The SQL rules judge FACTS from whichever SQL the resolver picked
// (compiled first, raw fallback); a finding's POSITION always comes from
// the node's `raw_code` — the only text carrying the file's real line
// numbers. The functions here render raw once (with the Jinja source
// map), parse it, and map every interesting AST span back to raw byte
// ranges. They run ONLY for models a rule already flagged — the shared
// `analyzed` parse never pays for them, and a finding whose position
// can't be pinned reports file-only rather than a guessed line/column.

use std::ops::Range;

use sqlparser::ast::{Spanned, Statement};

use super::ast::query_wildcard_items;
use super::dialect::SqlDialect;
use super::facts::{outer_order_by_span, output_name_occurrences};
use super::jinja::{jinja_parse, JinjaMark};
use super::scan::{scan_statements, scan_statements_positions};
use crate::source::{arms_exclusive, ArmStep};

/// Positions inside a model's `raw_code` that the SQL lint rules attach
/// to findings — byte ranges into the raw text (the finding converts them
/// to line/column). Each list keeps source order: the first range becomes
/// the finding's primary position, the rest its secondary labels.
#[derive(Debug, Default)]
pub struct SqlPositions {
    /// Output-producing `*`/`t.*` projections (UNION arms included) —
    /// the `select_star` positions.
    pub wildcards: Vec<Range<usize>>,
    /// Conditionless JOIN arms, the joined relation's range (the JOIN
    /// keyword itself isn't in the span) — `cartesian_join`.
    pub cartesian: Vec<Range<usize>>,
    /// `(normalized last part, range)` of every column reference — the
    /// pool `unknown_column_reference` matches names against.
    pub columns: Vec<(String, Range<usize>)>,
    /// `(normalized name, occurrence ranges)` per named output in the
    /// raw render's output-defining SELECT — `duplicate_output_columns`
    /// pairs a judged name only when the raw parse counted it exactly
    /// once or the same number of times (the shared name/count key).
    pub output_names: Vec<(String, Vec<Range<usize>>)>,
    /// `(normalized name, alias-name range)` per `WITH` CTE the raw
    /// render declares but never reads — `unused_cte`'s declarations.
    pub unused_ctes: Vec<(String, Range<usize>)>,
    /// The outermost `ORDER BY`'s expression range when the raw render's
    /// final query also sorts unbounded — `order_by_without_limit` (the
    /// `ORDER BY` keyword itself isn't in the span).
    pub order_by: Option<Range<usize>>,
}

/// The `Marked`-render scan `hard_coded_references` reads: the
/// hand-written relation names (the fact it judges) plus every recorded
/// occurrence's raw range (the positions it reports).
#[derive(Debug)]
pub struct MarkedRelations {
    /// Dotted relation names, sorted + deduplicated (the lint fact).
    pub names: Vec<String>,
    /// `(name, raw byte range)` per occurrence, in source order.
    pub spans: Vec<(String, Range<usize>)>,
}

/// Render `raw` (`Plain`) and extract the positions — `None` when the
/// text doesn't parse: a positionless finding, never a guessed one.
pub(super) fn plain_positions(raw: &str, dialect: SqlDialect) -> Option<SqlPositions> {
    let (rendered, stmts) = jinja_parse(raw, JinjaMark::Plain, dialect)?;
    let mut out = SqlPositions::default();
    for s in &stmts {
        if let Statement::Query(q) = s {
            for item in query_wildcard_items(q) {
                if let Some(r) = rendered.span_source(item.span()) {
                    out.wildcards.push(r);
                }
            }
        }
    }
    // The judged-query extractors re-run on the raw render's first Query
    // — the same statement `compute_analyzed` picks for its facts — so a
    // span can't be invented for a name the raw text never duplicated.
    if let Some(q) = stmts.iter().find_map(|s| match s {
        Statement::Query(q) => Some(&**q),
        _ => None,
    }) {
        for (name, spans) in output_name_occurrences(q) {
            // The fact count only counts a name's largest co-runnable
            // occurrence set (exclusive `{% if %}`/`{% else %}` arms
            // never render together) — keep the same occurrences here,
            // so a name the facts don't count can't get a position the
            // count doesn't back.
            let mut kept: Vec<Vec<ArmStep>> = Vec::new();
            let mut ranges: Vec<Range<usize>> = Vec::with_capacity(spans.len());
            for s in &spans {
                let Some(src) = rendered.span_source(*s) else {
                    continue;
                };
                let path = rendered.branches().arm_path(src.start).to_vec();
                if kept.iter().all(|k| !arms_exclusive(k, &path)) {
                    kept.push(path);
                    ranges.push(src);
                }
            }
            if !ranges.is_empty() {
                out.output_names.push((name, ranges));
            }
        }
        out.order_by = outer_order_by_span(q).and_then(|s| rendered.span_source(s));
    }
    let scan = scan_statements_positions(&stmts, Some(&rendered));
    for span in &scan.cartesian_spans {
        if let Some(r) = rendered.span_source(*span) {
            out.cartesian.push(r);
        }
    }
    for (name, span) in &scan.column_refs {
        if let Some(r) = rendered.span_source(*span) {
            out.columns.push((name.clone(), r));
        }
    }
    for (name, span) in scan.unread_ctes() {
        if let Some(r) = rendered.span_source(*span) {
            out.unused_ctes.push((name.clone(), r));
        }
    }
    Some(out)
}

/// Render `raw` (`Marked`) and return the hand-coded relation scan —
/// `None` like [`plain_positions`].
pub(super) fn marked_relations(raw: &str, dialect: SqlDialect) -> Option<MarkedRelations> {
    let (rendered, stmts) = jinja_parse(raw, JinjaMark::Marked, dialect)?;
    let scan = scan_statements(&stmts, Some(&rendered));
    let spans = scan
        .hard_coded_spans
        .iter()
        .filter_map(|(n, s)| rendered.span_source(*s).map(|r| (n.clone(), r)))
        .collect();
    Some(MarkedRelations {
        names: scan.hard_coded.iter().cloned().collect(),
        spans,
    })
}
