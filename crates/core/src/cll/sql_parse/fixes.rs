//! Raw-SQL byte surgery for `dbtl lint --fix`: pure text scans that
//! answer "which bytes can go" for a finding — a literal trailing `;`,
//! an unread `WITH` item — never the rewrite itself. Everything here is
//! conservative by design: a byte inside a Jinja tag or a paired `{%`
//! block is never cut, a region the walk can't shape exactly produces
//! no edit, and the caller re-renders+parses the result before any of
//! it lands. No I/O anywhere — the CLI reads and writes the files.

use std::ops::Range;

use super::dialect::SqlDialect;
use super::jinja::{
    block_comment_end, dollar_quote_close, dollar_quote_open, jinja_parse, line_comment_end,
    q_quote_end, quoted_end, trailing_semicolon, word_byte, JinjaMark, Lex,
};
use crate::source::{jinja_tag_close, tag_word};

/// What the byte walk found at `i` — the unit every higher-level scan
/// (`skip_insignificant`, `match_paren`, `inside_jinja`, the `WITH`
/// parser) shares so a comment, literal, or tag can never be read two
/// different ways.
enum Step {
    /// Insignificant bytes (whitespace or a comment) ended before `i` —
    /// resume the walk at the contained position.
    Skip(usize),
    /// A quoted literal (`'…'`/`"…"`/`` `…` ``/`q'[x]'`/`$tag$…$tag$`)
    /// spans `i..n` — resume at `n`.
    Literal(usize),
    /// A Jinja marker (`{{`/`{%`/`{#`) spans `i..n` — `n` is just past
    /// its close. Its content never reaches the byte walk.
    Jinja(usize),
    /// An unterminated `{{`/`{%`/`{#` — the render copies the rest
    /// verbatim, so no later `{` opens a marker.
    Verbatim,
    /// Anything else — the caller reads `b[i]` itself.
    Byte,
}

/// Classify `b[i]` — the same dispatch [`trailing_semicolon`] drives,
/// minus its tail bookkeeping: dialect comments first (they can hold
/// `{` bytes that are NOT markers), then literals, then Jinja markers.
fn step(b: &[u8], i: usize, lex: &Lex) -> Step {
    match b[i] {
        c if c.is_ascii_whitespace() => Step::Skip(i + 1),
        b'-' if lex.dash_comment_at(b, i) => Step::Skip(line_comment_end(b, i)),
        b'/' if lex.slash_comment_at(b, i) => Step::Skip(line_comment_end(b, i)),
        b'/' if b.get(i + 1) == Some(&b'*') => {
            Step::Skip(block_comment_end(b, i, lex.nested_comments))
        }
        b'#' if lex.hash_comment_at(b, i) => Step::Skip(line_comment_end(b, i)),
        b'\'' if lex.q_quote_at(b, i) => Step::Literal(q_quote_end(b, i)),
        q @ (b'\'' | b'"' | b'`') => Step::Literal(quoted_end(b, i, q, lex.escape_for(b, i))),
        b'$' if lex.dollar_quote => match dollar_quote_open(b, i) {
            Some(dlen) => Step::Literal(
                dollar_quote_close(b, i + dlen, &b[i..i + dlen]).map_or(b.len(), |c| c + dlen),
            ),
            None => Step::Byte,
        },
        b'{' if matches!(b.get(i + 1), Some(b'{') | Some(b'%') | Some(b'#')) => {
            match jinja_tag_close(b, i) {
                Some(e) => Step::Jinja(e + 2),
                None => Step::Verbatim,
            }
        }
        _ => Step::Byte,
    }
}

/// Advance over insignificant bytes — whitespace, comments, and Jinja
/// markers — from `i`. A literal or anything else stops the walk (the
/// caller reads it).
fn skip_insignificant(b: &[u8], mut i: usize, lex: &Lex) -> usize {
    while i < b.len() {
        match step(b, i, lex) {
            Step::Skip(n) | Step::Jinja(n) => i = n,
            Step::Verbatim => return b.len(),
            Step::Literal(_) | Step::Byte => break,
        }
    }
    i
}

/// Does `b[i]` open a comment — a SQL comment (`--`, `//`, `/*`, `#`
/// per the dialect) or a `{# #}` Jinja comment? Comments survive the
/// deletions [`unread_cte_deletes`] plans: they were written for the
/// reader, not the linter, and dropping a neighbour's note with the
/// dead CTE would eat documentation the fix was never asked to judge.
fn comment_at(b: &[u8], i: usize, lex: &Lex) -> bool {
    match b[i] {
        b'-' => lex.dash_comment_at(b, i),
        b'/' => lex.slash_comment_at(b, i) || b.get(i + 1) == Some(&b'*'),
        b'#' => lex.hash_comment_at(b, i),
        b'{' => b.get(i + 1) == Some(&b'#'),
        _ => false,
    }
}

/// May the bytes of `kill` be deleted? A kill that removes a WHOLE Jinja
/// construct is safe — a `{{ ref() }}` inside a deleted `WITH` item
/// renders away with it; what must never happen is a cut that splits a
/// construct:
///
/// - a tag `{{ }}`/`{% %}`/`{# #}` whose bytes straddle the kill
///   boundary (its open inside and close out, or vice versa),
/// - a paired `{% open %}` inside the kill whose `{% endopen %}` lands
///   outside — or the mirror, an `{% end… %}` inside the kill pairing
///   with an opener outside — either leaves a dangling half behind,
/// - an unterminated marker, whose verbatim tail can't be scoped.
///
/// Tags wholly inside the kill are fine, and a kill sitting inside a
/// paired block's body is fine — the item's own `inside_jinja` check
/// already refused a def that lives inside an arm, so reaching here
/// means the cut only deletes body text the render produces or
/// doesn't, cleanly.
fn jinja_safe_cut(sql: &str, kill: Range<usize>, lex: &Lex) -> bool {
    let b = sql.as_bytes();
    let (ks, ke) = (kill.start, kill.end);
    if ks >= ke || ks >= b.len() {
        return false;
    }
    let inside = |p: usize| p >= ks && p < ke;
    // Open paired-block frames — true when the opener sits inside the
    // kill, so an `end…` can flag an edge that crosses the boundary.
    let mut stack: Vec<bool> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let tag_end = match step(b, i, lex) {
            Step::Skip(n) | Step::Literal(n) => {
                i = n;
                continue;
            }
            // An unterminated marker: reached before the kill → the kill
            // is all verbatim tail, nothing to split → safe; inside it
            // → the tag's extent can't be proven → refuse.
            Step::Verbatim => return !inside(i),
            Step::Jinja(n) => n,
            Step::Byte => {
                i += 1;
                continue;
            }
        };
        // The tag's bytes span `i..tag_end - 1` — first and last inside
        // different sides of the kill means a boundary split.
        if inside(i) != inside(tag_end - 1) {
            return false;
        }
        if b[i + 1] == b'%' {
            let (word, rest) = tag_word(&sql[i + 2..tag_end - 2]);
            if paired_end(word, rest).is_some() {
                stack.push(inside(i));
            } else if word.starts_with("end") {
                // Pair with the innermost open frame — position is all
                // the check needs (a mismatched end word collapses the
                // same edge). An opener inside the kill closed outside,
                // or vice versa, is a split block edge.
                if let Some(p) = stack.iter().rposition(|_| true) {
                    if stack[p] != inside(i) {
                        return false;
                    }
                    stack.truncate(p);
                }
            }
        }
        i = tag_end;
    }
    // Frames opened inside the kill must have closed inside it — a
    // surviving inside-frame means the kill took `{% if %}` and left
    // `{% endif %}` behind.
    stack.iter().all(|in_kill| !*in_kill)
}

/// `{% word %}` → the `{% end… %}` word it pairs with, when the tag
/// opens a block whose body must stay off-limits to edits: the
/// conditional arms (`if`/`for`), the bodies that render as dead or
/// verbatim text (`set` blocks, `macro`, `call`, `raw`, `comment`,
/// `docs`), and every other paired Jinja block — the same set
/// `jinja_branches`/`block_body_end` recognize, kept in sync so a `;`
/// sitting inside any of them counts as "inside Jinja" and is left
/// alone. Single-statement tags (`do`, `include`, `{% set x = … %}`,
/// `elif`/`else`/`end…` themselves) open nothing.
fn paired_end(word: &str, rest: &str) -> Option<&'static str> {
    let end = match word {
        "if" => "endif",
        "for" => "endfor",
        "set" if !crate::source::jinja_set_assigns(rest) => "endset",
        "macro" => "endmacro",
        "call" => "endcall",
        "raw" => "endraw",
        "comment" => "endcomment",
        "docs" => "enddocs",
        "block" => "endblock",
        "filter" => "endfilter",
        "with" => "endwith",
        "trans" => "endtrans",
        "autoescape" => "endautoescape",
        "snapshot" => "endsnapshot",
        "test" => "endtest",
        "materialization" => "endmaterialization",
        "verification" => "endverification",
        "embed" => "endembed",
        _ => return None,
    };
    Some(end)
}

/// Is byte `at` inside a Jinja construct — inside a `{{ }}`/`{% %}`/
/// `{# #}` tag, or inside the body of any paired `{% … %}`/`{% end… %}`
/// block (`{% if %}` arm, `{% for %}` body, `{% set %}`/`{% macro %}`/
/// `{% raw %}`/`{% comment %}` bodies, `{% block %}`/`{% filter %}` and
/// the rest — any depth)? `true` whenever the structure can't be proven
/// to leave `at` in plain rendered text — the `--fix` guards treat that
/// as inside, so a `;` under a conditional or a CTE inside a macro can
/// never be cut by accident. SQL comments and literals are skipped
/// under the dialect's rules, so a `{` inside one never opens a block.
fn inside_jinja(sql: &str, at: usize, lex: &Lex) -> bool {
    let b = sql.as_bytes();
    if at >= b.len() {
        return false;
    }
    // Open paired blocks — `(body start, close word)`. A position is
    // inside Jinja when it sits inside a tag or inside a still-open
    // block body; the outermost open frame's body start is the
    // smallest, so `at >= stack[0].0` proves containment.
    let mut stack: Vec<(usize, &'static str)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() && i <= at {
        let tag = match step(b, i, lex) {
            Step::Skip(n) | Step::Literal(n) => {
                i = n;
                continue;
            }
            // An unterminated marker: `at` at/after its open sits in
            // template-verbatim text — inside Jinja by construction.
            Step::Verbatim => return true,
            Step::Jinja(close) => close,
            Step::Byte => {
                i += 1;
                continue;
            }
        };
        // The tag spans `i..tag`. `at` inside a tag is inside Jinja.
        if at >= i && at < tag {
            return true;
        }
        if b[i + 1] == b'%' {
            let (word, rest) = tag_word(&sql[i + 2..tag - 2]);
            if let Some(end) = paired_end(word, rest) {
                stack.push((tag, end));
            } else if word.starts_with("end") {
                // Pop ONLY the frame expecting this word — a mismatched
                // end (`{% if %}{% for %}{% endif %}`) must leave the
                // still-unclosed `for` body open, the conservative read.
                if let Some(p) = stack.iter().rposition(|(_, w)| *w == word) {
                    stack.remove(p);
                }
            }
        }
        i = tag;
    }
    // `i` passed `at` (or the file ended): `at` is inside a body when
    // some frame is still open and `at` is at/past its start.
    stack.first().is_some_and(|(s, _)| at >= *s)
}

/// The `;` `--fix` may delete: [`trailing_semicolon`]'s raw byte range,
/// refused when the byte can't be pinned or sits inside Jinja — a tag,
/// an `{% if %}`/`{% for %}` arm, or any paired-block body (a `;`
/// inside `{% raw %}` renders literally, but deleting it edits the
/// template, not the finding). `Err` carries the reason — the finding
/// stays for the report and the run lists the refusal.
pub(crate) fn semicolon_delete(
    sql: &str,
    dialect: SqlDialect,
) -> Result<Range<usize>, &'static str> {
    let Some(range) = trailing_semicolon(sql, dialect) else {
        return Err("the `;` could not be located in the raw SQL");
    };
    if inside_jinja(sql, range.start, &Lex::new(dialect)) {
        return Err("the `;` sits inside a Jinja tag or block");
    }
    Ok(range)
}

/// The raw extents of one `WITH` item — `name` is the alias-name byte
/// range a [`crate::SqlPositions::unused_ctes`] entry points at;
/// `end` is just past the body's `)`; `comma_before` is the `,`
/// separating this item from the previous one (`None` on the first).
struct WithItem {
    name: Range<usize>,
    end: usize,
    comma_before: Option<usize>,
}

/// A parsed `WITH` clause — the `with`/`with recursive` keyword plus
/// its items. Only cleanly-shaped clauses register: a `,` no item
/// follows, a missing `as`, or an unbalanced `(` drops the whole
/// clause, so no delete is ever computed on a structure the walk only
/// half saw.
struct WithClause {
    /// The `with` keyword's start byte — the kill start when every
    /// item of the clause is being deleted.
    kw_start: usize,
    items: Vec<WithItem>,
}

/// The identifier run starting at `i` as `(start, end)` — `None` when
/// `b[i]` can't start an ASCII word (keyword tests only ever ask about
/// `with`/`as`/`recursive`).
fn word_at(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let c = *b.get(i)?;
    if !(c.is_ascii_alphabetic() || c == b'_') {
        return None;
    }
    let mut end = i + 1;
    while b.get(end).is_some_and(|c| word_byte(*c)) {
        end += 1;
    }
    Some((i, end))
}

/// Is `b[i..]` a case-insensitive `word` on identifier boundaries?
fn word_is(b: &[u8], i: usize, word: &str) -> bool {
    let Some((s, e)) = word_at(b, i) else {
        return false;
    };
    e - s == word.len() && b[s..e].eq_ignore_ascii_case(word.as_bytes())
}

/// The identifier token at `i` — a bare word or a `"…"`/`` `…` ``
/// quoted name (what `WITH` aliases can be). `None` on anything else —
/// a Jinja marker, a number, punctuation, a non-ASCII lead.
fn ident_at(b: &[u8], i: usize, lex: &Lex) -> Option<Range<usize>> {
    match b.get(i) {
        Some(q @ (b'"' | b'`')) => {
            let e = quoted_end(b, i, *q, lex.escape_for(b, i));
            (e > i + 1).then_some(i..e)
        }
        Some(&c) if c.is_ascii_alphabetic() || c == b'_' => word_at(b, i).map(|(s, e)| s..e),
        _ => None,
    }
}

/// Match the `(` opened at `i` — returns the `)` index: depth counting
/// over the byte walk, so strings, comments, and Jinja markers inside
/// the parens can never fake a close. `None` when the parens never
/// balance.
fn match_paren(b: &[u8], i: usize, lex: &Lex) -> Option<usize> {
    let mut depth = 1usize;
    let mut j = i + 1;
    while j < b.len() {
        match step(b, j, lex) {
            Step::Skip(n) | Step::Literal(n) | Step::Jinja(n) => j = n,
            Step::Verbatim => return None,
            Step::Byte => {
                match b[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
    }
    None
}

/// Parse the `WITH` clause whose `with` keyword opens at `kw` — items
/// are `name [(col,…)] as ( … )` separated by `,` — and return it with
/// the resume position (just past the last item's `)`). `None` on any
/// shape the grammar doesn't accept: a missing `as`, an unbalanced
/// paren, or a `,` that no item follows — a clause the walk only
/// half-understood yields no edits at all.
fn parse_with(sql: &str, kw: usize, lex: &Lex) -> Option<(WithClause, usize)> {
    let b = sql.as_bytes();
    let mut j = skip_insignificant(b, kw + 4, lex);
    // `WITH RECURSIVE` — the extra keyword folds into the clause.
    if word_is(b, j, "recursive") {
        let (_, e) = word_at(b, j)?;
        j = skip_insignificant(b, e, lex);
    }
    let mut items: Vec<WithItem> = Vec::new();
    let mut comma_before: Option<usize> = None;
    loop {
        j = skip_insignificant(b, j, lex);
        let Some(name) = ident_at(b, j, lex) else {
            // A promised item that never arrives (`… ,` then a
            // keyword/other token) makes the clause unparsable —
            // but a `,`-free stop is just the clause's end when it
            // follows a completed item.
            if comma_before.is_some() {
                return None;
            }
            break;
        };
        let mut k = skip_insignificant(b, name.end, lex);
        // Optional `name(col, col)` column list before `as`.
        if b.get(k) == Some(&b'(') {
            k = skip_insignificant(b, match_paren(b, k, lex)? + 1, lex);
        }
        if !word_is(b, k, "as") {
            return None;
        }
        let k = skip_insignificant(b, k + 2, lex);
        if b.get(k) != Some(&b'(') {
            return None;
        }
        let end = match_paren(b, k, lex)? + 1;
        items.push(WithItem {
            name,
            end,
            comma_before: comma_before.take(),
        });
        let after = skip_insignificant(b, end, lex);
        if b.get(after) == Some(&b',') {
            comma_before = Some(after);
            j = after + 1;
        } else {
            return Some((
                WithClause {
                    kw_start: kw,
                    items,
                },
                after,
            ));
        }
    }
    // The loop broke at the clause's first item position — nothing
    // parsed, so `kw` wasn't a real clause (or was an empty one,
    // which is invalid SQL either way).
    None
}

/// Every `WITH` clause in `sql`, top-level and nested — the walk sees
/// `with` keywords at any paren depth so a `WITH` inside a subquery or
/// a CTE body maps the same way. Clauses nest disjointly: an inner
/// `with` lives inside an outer item's parens, never inside an outer
/// item's name/separator span — kills from different clauses can still
/// overlap (an outer kill covers the inner clause's region), and the
/// apply step refuses those edits per its overlap rule.
fn with_clauses(sql: &str, lex: &Lex) -> Vec<WithClause> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match step(b, i, lex) {
            Step::Skip(n) | Step::Literal(n) | Step::Jinja(n) => i = n,
            Step::Verbatim => break,
            Step::Byte => {
                if word_is(b, i, "with") && (i == 0 || !(word_byte(b[i - 1]) || b[i - 1] == b'.')) {
                    if let Some((clause, _)) = parse_with(sql, i, lex) {
                        out.push(clause);
                    }
                }
                i += 1;
            }
        }
    }
    out
}

/// [`unread_cte_deletes`]'s full verdict for the flagged names: the
/// byte ranges to delete plus, per name no delete was emitted for,
/// the reason.
pub(crate) struct CteDeletes {
    /// The delete ranges — non-overlapping, in scan order. One doomed
    /// run can emit several: the comments it preserves split its kill
    /// into the gaps between them.
    pub kills: Vec<Range<usize>>,
    /// Flagged name spans that produced no kill, each paired with why —
    /// the findings they belong to stay for the report.
    pub refused: Vec<(Range<usize>, &'static str)>,
}

/// Is `a`'s span the same bytes as `b`'s — exact or contained either
/// way (quoting differences)? The flagged-name matching predicate.
fn spans_match(a: &Range<usize>, b: &Range<usize>) -> bool {
    (a.start >= b.start && a.end <= b.end) || (b.start >= a.start && b.end <= a.end)
}

/// The `kill` span minus the comments worth keeping — the ranges that
/// actually delete. A maximal run of insignificant bytes (whitespace,
/// SQL comments, Jinja markers) inside `kill` survives WHOLE when it
/// carries a comment AND does not sit inside a `guarded` span — the
/// deleted items' own `name … as (…)` extents, whose comments go with
/// the definition they annotated. A comment in a separator seam
/// (`u as (…), /* note */` before the next item) or beside the `with`
/// keyword was written for the reader, not the linter: it stays, its
/// whitespace intact, so the surviving neighbours keep their spacing.
/// Everything else in `kill` — the doomed defs, the separator `,`s,
/// the `with` keyword, comment-free runs — deletes.
///
/// The caller runs [`jinja_safe_cut`] first, so no kept run can hide
/// a construct the delete would split.
fn cuts_preserving_comments(
    sql: &str,
    kill: &Range<usize>,
    guarded: &[Range<usize>],
    lex: &Lex,
) -> Vec<Range<usize>> {
    let b = sql.as_bytes();
    let mut cuts = Vec::new();
    // `from` — the first byte not yet emitted; `run` — the open
    // insignificant run `(start, saw_a_comment)`. A significant step
    // (or the kill's end) closes the run: kept runs punch a hole in
    // the delete; comment-free ones delete with their span.
    let mut from = kill.start;
    let mut run: Option<(usize, bool)> = None;
    let mut i = kill.start;
    while i < kill.end {
        match step(b, i, lex) {
            Step::Skip(n) | Step::Jinja(n) => {
                let run = run.get_or_insert((i, false));
                run.1 |= comment_at(b, i, lex);
                i = n;
            }
            Step::Literal(n) => {
                close_run(&mut cuts, &mut from, &mut run, i, guarded);
                i = n;
            }
            Step::Byte => {
                close_run(&mut cuts, &mut from, &mut run, i, guarded);
                i += 1;
            }
            // Unreachable — `jinja_safe_cut` refused the kill first —
            // but the verbatim tail can't be scoped, so the scan stops
            // and the rest deletes with the tail segment.
            Step::Verbatim => break,
        }
    }
    close_run(&mut cuts, &mut from, &mut run, kill.end, guarded);
    if from < kill.end {
        cuts.push(from..kill.end);
    }
    cuts
}

/// Close the insignificant run `run` at `end`: a comment-carrying run
/// outside every `guarded` span survives — the delete before it emits
/// and the next one starts at `end` — while a comment-free or guarded
/// run just joins the pending delete.
fn close_run(
    cuts: &mut Vec<Range<usize>>,
    from: &mut usize,
    run: &mut Option<(usize, bool)>,
    end: usize,
    guarded: &[Range<usize>],
) {
    let Some((start, saw_comment)) = run.take() else {
        return;
    };
    let guarded_hit = guarded.iter().any(|g| start < g.end && g.start < end);
    if saw_comment && !guarded_hit {
        if *from < start {
            cuts.push(*from..start);
        }
        *from = end;
    }
}

/// The byte ranges to delete for the unread `WITH` items `names`
/// identifies — each `Range` is an alias-name span, the same anchors
/// `SqlPositions::unused_ctes` carries. Runs of flagged items compute
/// against the clause's whole item list so separators land right:
///
/// - kept items after the run → the run's start to the next kept
///   item's start (the deleted items + their trailing `,`s go; the
///   `,` before the run stays to separate the survivors),
/// - a run at the clause tail → from the `,` before the run through
///   the last item (the separator goes with the deleted tail),
/// - every item gone → the whole `with`/`with recursive` clause.
///
/// Comments ride along differently: a comment inside a deleted item's
/// own span goes with the item, but a comment in a separator seam or
/// beside the `with` keyword is kept whole — [`cuts_preserving_comments`]
/// splits each kill around them.
///
/// An item inside a Jinja block stays — it's treated as kept so its
/// neighbours' kills still compute around it — and a kill spanning a
/// Jinja marker is refused: the finding then stands for the report
/// rather than cutting template the render owns. Both kinds name their
/// spans on `CteDeletes::refused`.
pub(crate) fn unread_cte_deletes(
    sql: &str,
    names: &[Range<usize>],
    dialect: SqlDialect,
) -> CteDeletes {
    let lex = Lex::new(dialect);
    let b = sql.as_bytes();
    let mut out = CteDeletes {
        kills: Vec::new(),
        refused: Vec::new(),
    };
    // Every clause item's name span (for the "unpinned" check) and the
    // refused ones with their reasons — filled while kills compute.
    let mut item_spans: Vec<Range<usize>> = Vec::new();
    let mut refused_items: Vec<(Range<usize>, &'static str)> = Vec::new();
    for clause in with_clauses(sql, &lex) {
        for it in &clause.items {
            item_spans.push(it.name.clone());
        }
        // Match flagged name spans to this clause's items — the span
        // and the item's name range describe the same bytes, exact or
        // contained either way (quoting differences).
        let flagged: Vec<bool> = clause
            .items
            .iter()
            .map(|it| names.iter().any(|n| spans_match(n, &it.name)))
            .collect();
        if !flagged.iter().any(|f| *f) {
            continue;
        }
        // A flagged item that stays gets its refusal reason; a doomed
        // one's stays `None` unless its kill is refused below.
        let mut verdict: Vec<Option<&'static str>> = vec![None; clause.items.len()];
        let doomed: Vec<bool> = clause
            .items
            .iter()
            .zip(&flagged)
            .enumerate()
            .map(|(i, (it, f))| {
                let inside = *f && inside_jinja(sql, it.name.start, &lex);
                if *f && inside {
                    verdict[i] = Some("its definition sits inside a Jinja tag or block");
                }
                *f && !inside
            })
            .collect();
        let mut i = 0usize;
        while i < clause.items.len() {
            if !doomed[i] {
                i += 1;
                continue;
            }
            let mut j = i;
            while j + 1 < clause.items.len() && doomed[j + 1] {
                j += 1;
            }
            let kill = if j + 1 < clause.items.len() {
                clause.items[i].name.start..clause.items[j + 1].name.start
            } else if i > 0 {
                clause.items[i]
                    .comma_before
                    .unwrap_or(clause.items[i].name.start)..clause.items[j].end
            } else {
                // The whole clause goes — take its line's residue with
                // it, or `with … )\nselect` would fix to a leading
                // blank line (`\nselect`). Only when the clause owns
                // its line though: a `;`-separated statement before
                // `with` keeps its spacing.
                let mut s = clause.kw_start;
                let mut e = clause.items[j].end;
                let mut head = s;
                while head > 0 && matches!(b[head - 1], b' ' | b'\t') {
                    head -= 1;
                }
                if head == 0 || b[head - 1] == b'\n' {
                    s = head;
                    if b[e..].starts_with(b"\r\n") {
                        e += 2;
                    } else if b.get(e) == Some(&b'\n') {
                        e += 1;
                    }
                }
                s..e
            };
            if jinja_safe_cut(sql, kill.clone(), &lex) {
                // Comments inside a doomed item's own `name … as (…)`
                // go with the item; seam and `with`-keyword comments
                // survive whole.
                let defs: Vec<Range<usize>> = (i..=j)
                    .map(|k| clause.items[k].name.start..clause.items[k].end)
                    .collect();
                out.kills
                    .extend(cuts_preserving_comments(sql, &kill, &defs, &lex));
            } else {
                for v in &mut verdict[i..=j] {
                    *v = Some("deleting it would split a Jinja tag or a paired block");
                }
            }
            i = j + 1;
        }
        for (it, reason) in clause.items.iter().zip(verdict) {
            if let Some(reason) = reason {
                refused_items.push((it.name.clone(), reason));
            }
        }
    }
    // Hand each input span its verdict: a refused item's reason, the
    // "unpinned" refusal when no clause item claimed it at all — a
    // matched-and-killed span is simply not refused.
    for n in names {
        if let Some((_, reason)) = refused_items.iter().find(|(s, _)| spans_match(n, s)) {
            out.refused.push((n.clone(), *reason));
        } else if !item_spans.iter().any(|s| spans_match(n, s)) {
            out.refused.push((
                n.clone(),
                "its definition could not be pinned to a WITH item",
            ));
        }
    }
    out
}

/// The post-edit guard `unused_cte`'s fix requires: the new raw text
/// must still render under Jinja and parse — the same `Plain` render
/// the positions pass judges, with the same macro expansion. `node`
/// (`dag` + uid + the batch's resolver) reuses the resolver's memoized
/// macro table instead of rebuilding one per call.
pub(crate) fn sql_still_parses(
    sql: &str,
    dialect: SqlDialect,
    node: Option<(&crate::dag::Dag, &str, &super::SqlParseResolver)>,
) -> bool {
    let expand = node.and_then(|(dag, uid, res)| res.expansion(dag, uid));
    jinja_parse(sql, JinjaMark::Plain, dialect, expand).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flagged-name span — `unread_cte_deletes` takes `&[Range]`; a
    /// bare `[s..e]` array reads as a one-range vec to clippy.
    fn r(s: usize, e: usize) -> Range<usize> {
        s..e
    }

    /// `sql` after every kill `unread_cte_deletes` emits is deleted.
    fn killed(sql: &str, names: &[Range<usize>]) -> (String, CteDeletes) {
        let plan = unread_cte_deletes(sql, names, SqlDialect::Generic);
        let mut t = sql.to_string();
        let mut ranges: Vec<Range<usize>> = plan.kills.clone();
        ranges.sort_by_key(|r| (r.start, r.end));
        for r in ranges.iter().rev() {
            t.replace_range(r.clone(), "");
        }
        (t, plan)
    }

    #[test]
    fn semicolon_delete_plain_tail() {
        assert_eq!(semicolon_delete("select 1;", SqlDialect::Generic), Ok(8..9));
        assert!(semicolon_delete("select 1", SqlDialect::Generic).is_err());
        // A `;` inside a string/comment is never the tail.
        assert!(semicolon_delete("select 'a;'", SqlDialect::Generic).is_err());
        assert!(semicolon_delete("select 1 -- ;\n", SqlDialect::Generic).is_err());
    }

    #[test]
    fn semicolon_inside_jinja_blocks_is_refused() {
        // `;` inside an `{% if %}` arm — conditional, not a tail.
        assert_eq!(
            semicolon_delete("select 1\n{% if x %};{% endif %}", SqlDialect::Generic),
            Err("the `;` sits inside a Jinja tag or block")
        );
        // `;` inside `{% raw %}` — template text, not a tail.
        assert_eq!(
            semicolon_delete("select 1\n{% raw %};{% endraw %}", SqlDialect::Generic),
            Err("the `;` sits inside a Jinja tag or block")
        );
        // `;` after a closed block is plain text — the fix is real.
        assert_eq!(
            semicolon_delete(
                "select 1 {% if x %}and 1=1{% endif %};",
                SqlDialect::Generic
            ),
            Ok(37..38)
        );
        // `{% set %}` bodies never reach the tail either.
        assert!(
            semicolon_delete("select 1\n{% set s %};{% endset %}", SqlDialect::Generic).is_err()
        );
        // A mismatched end (`{% if %}{% for %}{% endif %}`) pops only
        // the `if` — the `for` body stays open, so its `;` is refused.
        assert!(semicolon_delete(
            "select 1\n{% if x %}{% for y in z %}{% endif %};{% endfor %}",
            SqlDialect::Generic
        )
        .is_err());
    }

    #[test]
    fn unread_cte_deletes_single_and_tail() {
        let sql = "with a as (select 1), b as (select 2)\nselect * from b";
        // `a` unread → keep `b`, delete `a as (…), `.
        let (t, plan) = killed(sql, &[r(5, 6)]);
        assert_eq!(plan.kills.len(), 1);
        assert_eq!(t, "with b as (select 2)\nselect * from b");
        // `b` unread → tail delete keeps `a`'s item and its own `,`.
        let (t, _) = killed(sql, &[r(22, 23)]);
        assert_eq!(t, "with a as (select 1)\nselect * from b");
    }

    #[test]
    fn unread_cte_deletes_whole_clause() {
        let (t, _) = killed("with a as (select 1)\nselect * from a", &[r(5, 6)]);
        assert_eq!(t, "select * from a", "no blank line is left behind");
        // …and CRLF — the whole pair goes, not half of it.
        let (t, _) = killed("with a as (select 1)\r\nselect * from a", &[r(5, 6)]);
        assert_eq!(t, "select * from a");
    }

    #[test]
    fn unread_cte_deletes_middle_of_three() {
        let (t, _) = killed(
            "with a as (select 1), b as (select 2), c as (select 3)\nselect * from a join c on true",
            &[r(22, 23)],
        );
        assert_eq!(
            t,
            "with a as (select 1), c as (select 3)\nselect * from a join c on true"
        );
    }

    #[test]
    fn unread_cte_two_of_three_deleted() {
        // `a` and `b` unread → one kill covering both, `with c` stays.
        let (t, _) = killed(
            "with a as (select 1), b as (select 2), c as (select 3)\nselect * from c",
            &[r(5, 6), r(22, 23)],
        );
        assert_eq!(t, "with c as (select 3)\nselect * from c");
    }

    #[test]
    fn unread_cte_jinja_refusals() {
        // A def inside `{% if %}` stays — and says why.
        let sql = "with {% if x %}a as (select 1), {% endif %}b as (select 2)\nselect * from b";
        let plan = unread_cte_deletes(sql, &[r(15, 16)], SqlDialect::Generic);
        assert!(plan.kills.is_empty());
        assert_eq!(plan.refused.len(), 1);
        // A `{% if %}` inside `a`'s def whose `{% endif %}` lives in
        // `b`'s — the kill would split the block edge: refused.
        let sql = "with a as (select 1 {% if x %}), b as (select 2 {% endif %})\nselect * from b";
        let plan = unread_cte_deletes(sql, &[r(5, 6)], SqlDialect::Generic);
        assert!(plan.kills.is_empty());
        assert_eq!(plan.refused.len(), 1);
        // An unterminated `{%` inside the clause — extent unprovable.
        let sql = "with a as (select 1 {% if x), b as (select 2)\nselect * from b";
        let plan = unread_cte_deletes(sql, &[r(5, 6)], SqlDialect::Generic);
        assert!(plan.kills.is_empty());
        assert_eq!(plan.refused.len(), 1);
    }

    #[test]
    fn unread_cte_jinja_inside_def_is_cut() {
        // A `{{ }}` wholly inside the deleted def renders away with it —
        // the common `ref()`-in-CTE-body case must fix.
        let (t, _) = killed(
            "with a as (select * from {{ ref('x') }}), b as (select 2)\nselect * from b",
            &[r(5, 6)],
        );
        assert_eq!(t, "with b as (select 2)\nselect * from b");
    }

    #[test]
    fn unread_cte_keeps_the_following_comment() {
        // The seam comment between the deleted middle item and the
        // survivor is not part of `unused_mid`'s def — it stays whole.
        let sql = "with x as (select 1 as id), -- keep\nunused_mid as (select id from x), /* c */\ny as (select id from x)\nselect id from y";
        let (t, _) = killed(sql, &[r(36, 46)]);
        assert_eq!(
            t,
            "with x as (select 1 as id), -- keep\n /* c */\ny as (select id from x)\nselect id from y"
        );
        // A `--` line comment in the same spot survives too.
        let sql = "with x as (select 1 as id), -- keep\nunused_mid as (select id from x), -- c\ny as (select id from x)\nselect id from y";
        let (t, _) = killed(sql, &[r(36, 46)]);
        assert_eq!(
            t,
            "with x as (select 1 as id), -- keep\n -- c\ny as (select id from x)\nselect id from y"
        );
        // Same placement on a TAIL delete — the `,` still goes, the
        // comment stays behind the surviving item.
        let sql = "with x as (select 1 as id), -- note\ny as (select id from x)\nselect id from y";
        let (t, _) = killed(sql, &[r(36, 37)]);
        assert_eq!(t, "with x as (select 1 as id) -- note\n\nselect id from y");
    }

    #[test]
    fn unread_cte_comment_placement() {
        // A comment between `)` and `,` — the comma still goes, the
        // comment keeps its spot after the surviving item's `,`.
        let (t, _) = killed(
            "with x as (select 1), u as (select id from x) /* c */, y as (select 2)\nselect * from x",
            &[r(22, 23)],
        );
        assert_eq!(
            t,
            "with x as (select 1),  /* c */y as (select 2)\nselect * from x"
        );
        // A comment INSIDE a deleted def goes with it — it documented
        // the dead CTE, not its neighbours.
        let (t, _) = killed(
            "with x as (select 1), u /* gone */ as (select id from x), y as (select 2)\nselect * from x",
            &[r(22, 23)],
        );
        assert_eq!(t, "with x as (select 1), y as (select 2)\nselect * from x");
        // A comment beside `with` survives a whole-clause delete.
        let (t, _) = killed(
            "with /* note */ a as (select 1)\nselect * from a",
            &[r(16, 17)],
        );
        assert_eq!(t, " /* note */ select * from a");
    }

    #[test]
    fn sql_still_parses_smoke() {
        assert!(sql_still_parses("select 1", SqlDialect::Generic, None));
        assert!(sql_still_parses(
            "with a as (select 1) select * from a",
            SqlDialect::Generic,
            None
        ));
        assert!(!sql_still_parses("with a as (", SqlDialect::Generic, None));
    }
}
