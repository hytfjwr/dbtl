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
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;

use super::ast::query_wildcard_items;
use super::jinja::{render_jinja_mapped, JinjaMark};
use super::scan::{scan_statements, scan_statements_positions};

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
pub(super) fn plain_positions(raw: &str, dialect: &dyn Dialect) -> Option<SqlPositions> {
    let rendered = render_jinja_mapped(raw, JinjaMark::Plain);
    let stmts = Parser::parse_sql(dialect, &rendered.text).ok()?;
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
    let scan = scan_statements_positions(&stmts);
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
    Some(out)
}

/// Render `raw` (`Marked`) and return the hand-coded relation scan —
/// `None` like [`plain_positions`].
pub(super) fn marked_relations(raw: &str, dialect: &dyn Dialect) -> Option<MarkedRelations> {
    let rendered = render_jinja_mapped(raw, JinjaMark::Marked);
    let stmts = Parser::parse_sql(dialect, &rendered.text).ok()?;
    let scan = scan_statements(&stmts);
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
