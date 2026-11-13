// ---------------------------------------------------------------------------
// Jinja render — compiled-level text from raw model SQL
// ---------------------------------------------------------------------------

use std::collections::BTreeSet;
use std::ops::Range;

use sqlparser::ast::Statement;
use sqlparser::tokenizer::Span;

use super::depth::parse_statements;
use super::SqlDialect;
use crate::source::{
    find_subslice, jinja_branches, jinja_set_assigns, jinja_tag_close, mask_jinja_comments,
    tag_word, ArmKind, ArmStep, JinjaBranches,
};

/// The name `{{ this }}` renders to — marked like every generated token,
/// so it parses as a normal identifier, never matches a parent, and the
/// lint scan can't mistake it for a written reference.
const THIS_NAME: &str = "__dbtl_this__";

/// The prefix stamped on names the render generated — `ref('x')` →
/// `"__dbtl_ref__x"` under [`JinjaMark::Marked`], any unclassified `{{ }}`
/// → `__dbtl_jinja_<n>` under BOTH marks (the `<n>` keeps every generated
/// identifier unique, so two rendered `{{ }}` can never collide into a
/// duplicate). No real relation or column name starts with it, so a name
/// carrying the mark anywhere — `total_{{ x }}` glues it mid-word — is
/// generated, never a hand-written reference.
pub(super) const JINJA_MARK_PREFIX: &str = "__dbtl_";

/// Does `name` carry the generated-identifier mark? `contains`, not
/// `starts_with`: a `{{ }}` glued to an identifier prefix
/// (`total_{{ x }}` → `total___dbtl_jinja_0__`) buries the mark inside
/// the name.
pub(super) fn generated_name(name: &str) -> bool {
    name.contains(JINJA_MARK_PREFIX)
}

/// How `{{ ref }}`/`{{ source }}`/other `{{ }}` render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JinjaMark {
    /// The CLL render: `ref`/`source` become their relation names, other
    /// `{{ }}` take a position-aware shape (`NULL` for a value, a marked
    /// identifier where a name is required).
    Plain,
    /// The lint render: `ref`/`source` render `__dbtl_`-prefixed names and
    /// remaining `{{ }}` in value positions become a quoted marked
    /// identifier, so a parse tree can tell generated relation names
    /// (`{{ ref('x') }}`, `{{ target.schema }}`) from hand-written ones.
    Marked,
}

/// One emitted segment of a [`RenderedSql`]: `out` is the byte range in
/// the rendered text, `src` the raw-SQL range it came from.
#[derive(Debug)]
struct Seg {
    out: Range<usize>,
    src: Range<usize>,
    /// `true`: `src` copied verbatim — offsets map 1:1. `false`: a Jinja
    /// tag's render — every byte of `out` stands for all of `src`.
    copy: bool,
}

/// Raw model SQL rendered for the parser, plus the map back to source:
/// every byte range of `text` knows which raw range produced it — copied
/// regions map 1:1, a rendered `{{ }}`/`{% %}`/`{# #}` maps to its whole
/// tag. `text` is byte-identical to [`render_jinja_as`]'s output.
#[derive(Debug)]
pub(super) struct RenderedSql {
    /// The rendered text the parser reads.
    pub(super) text: String,
    /// Segments tiling `text` in emission order (the `src` ranges tile
    /// the input the same way).
    segs: Vec<Seg>,
    /// `text` byte offsets where a `{% elif %}`/`{% else %}` arm split
    /// emitted `,` — a comma no template line wrote. FROM items
    /// separated only by these are a branch union (the arms never reach
    /// one render), not a written comma join — `cartesian_join` reads
    /// the distinction through [`RenderedSql::only_branch_commas`].
    branch_commas: BTreeSet<usize>,
    /// The decoded string literals inside `{{ }}` expressions the render
    /// couldn't classify (a macro call's arguments —
    /// `{{ join_customers('customers', …) }}`, `{{ var('t') }}`…). A
    /// `WITH` CTE sharing a name with one may be read through the join
    /// the parse can't see — `unused_cte` counts it as read.
    pub(super) jinja_literals: Vec<String>,
    /// The `{% if %}`/`{% for %}` arm index over the raw source — spans
    /// mapped back through `segs` become the arm paths the
    /// exclusive-arm fact checks ([`crate::source::arms_exclusive`])
    /// read.
    branches: JinjaBranches,
}

impl RenderedSql {
    /// The source byte range `rendered` came from. A range inside a
    /// tag's render maps to the tag's whole source range; a range
    /// spanning segments takes the union of their sources — the segments
    /// tile the input, so the union is one contiguous range. `None` for
    /// an empty or out-of-`text` range.
    pub(super) fn source_range(&self, rendered: Range<usize>) -> Option<Range<usize>> {
        if rendered.is_empty() || rendered.end > self.text.len() {
            return None;
        }
        let mut out: Option<Range<usize>> = None;
        for s in &self.segs {
            let os = rendered.start.max(s.out.start);
            let oe = rendered.end.min(s.out.end);
            if os >= oe {
                continue;
            }
            let r = if s.copy {
                s.src.start + (os - s.out.start)..s.src.start + (oe - s.out.start)
            } else {
                s.src.clone()
            };
            out = Some(match out {
                None => r,
                Some(prev) => prev.start.min(r.start)..prev.end.max(r.end),
            });
        }
        out
    }

    /// The source's `{% if %}`/`{% for %}` arm index — arm paths of
    /// mapped positions decide which occurrences can share a render.
    pub(super) fn branches(&self) -> &JinjaBranches {
        &self.branches
    }

    /// The source byte range a parsed [`Span`] points at — `None` for an
    /// empty span or one whose (line, column) math doesn't land on the
    /// text: a position the renderer can't place is no position.
    pub(super) fn span_arm_path(&self, span: Span) -> Option<Vec<ArmStep>> {
        self.span_source(span)
            .map(|r| self.branches.arm_path(r.start).to_vec())
    }

    /// The source byte range a parsed [`Span`] points at — `None` for an
    /// empty span or one whose (line, column) math doesn't land on the
    /// text: a position the renderer can't place is no position.
    pub(super) fn span_source(&self, span: Span) -> Option<Range<usize>> {
        if span.start.line == 0 || span.end.line == 0 {
            return None;
        }
        let start = position_offset(&self.text, span.start.line, span.start.column)?;
        let end = position_offset(&self.text, span.end.line, span.end.column)?;
        self.source_range(start..end)
    }

    /// Is EVERY `,` inside rendered-text `gap` one an arm split emitted —
    /// i.e., no written `,` separates the two FROM items flanking it?
    /// Then the items are `{% if %}`/`{% elif %}`/`{% else %}` arms of a
    /// branch union (dbt renders exactly one per chain — they can never
    /// form a product), not a written comma join. `false` when the gap
    /// holds a literal `,` — or no comma at all.
    pub(super) fn only_branch_commas(&self, gap: Span) -> bool {
        let (Some(start), Some(end)) = (
            position_offset(&self.text, gap.start.line, gap.start.column),
            position_offset(&self.text, gap.end.line, gap.end.column),
        ) else {
            return false;
        };
        if start >= end {
            return false;
        }
        let mut found = false;
        for (i, c) in self.text.as_bytes()[start..end].iter().enumerate() {
            if *c == b',' {
                if !self.branch_commas.contains(&(start + i)) {
                    return false;
                }
                found = true;
            }
        }
        found
    }

    /// Does `span` (a rendered-text span) touch a Jinja tag's render — a
    /// generated, not written, stretch? The relation scan only counts
    /// names the author WROTE as CTE reads: a `{{ ref('x') }}` render
    /// shares its name with a CTE without reading it — the rendered
    /// relation the tag stands for is a model, not the in-file `WITH`.
    pub(super) fn is_generated(&self, span: Span) -> bool {
        let (Some(start), Some(end)) = (
            position_offset(&self.text, span.start.line, span.start.column),
            position_offset(&self.text, span.end.line, span.end.column),
        ) else {
            return false;
        };
        self.segs
            .iter()
            .any(|s| !s.copy && s.out.start < end && start < s.out.end)
    }
}

/// The byte offset of 1-based `(line, column)` in `text` — the inverse
/// of sqlparser's tokenizer walk (columns count `char`s; `\r` is a column
/// char, `\n` the line break, and a span's end sits ON the last char's
/// successor). `None` when the position runs past the text — except the
/// position right after the last char, which is `text.len()`.
fn position_offset(text: &str, line: u64, column: u64) -> Option<usize> {
    if line == 0 || column == 0 {
        return None;
    }
    let (mut l, mut c) = (1u64, 1u64);
    for (i, ch) in text.char_indices() {
        if l == line && c == column {
            return Some(i);
        }
        match ch {
            '\n' => {
                l += 1;
                c = 1;
            }
            _ => c += 1,
        }
    }
    (l == line && c == column).then_some(text.len())
}

/// Emit `bytes` as a verbatim copy of raw range `src`, merging into the
/// previous segment when it's a contiguous copy — plain SQL runs (the
/// byte-at-a-time `_` arm included) stay one segment per stretch.
fn emit_copy(out: &mut Vec<u8>, segs: &mut Vec<Seg>, src: Range<usize>, bytes: &[u8]) {
    if let Some(s) = segs.last_mut() {
        if s.copy && s.src.end == src.start && s.out.end == out.len() {
            out.extend_from_slice(bytes);
            s.src.end = src.end;
            s.out.end = out.len();
            return;
        }
    }
    let from = out.len();
    out.extend_from_slice(bytes);
    segs.push(Seg {
        out: from..out.len(),
        src,
        copy: true,
    });
}

/// Emit `bytes` as the render of Jinja range `src` — every emitted byte
/// stands for the whole tag.
fn emit_subst(out: &mut Vec<u8>, segs: &mut Vec<Seg>, src: Range<usize>, bytes: &[u8]) {
    let from = out.len();
    out.extend_from_slice(bytes);
    segs.push(Seg {
        out: from..out.len(),
        src,
        copy: false,
    });
}

// ---------------------------------------------------------------------------
// The dialect's lexical rules — derived once from the parser's own
// predicates so the scan and the parse can never disagree about where a
// literal or a comment ends
// ---------------------------------------------------------------------------

/// Byte-level identifier membership for the word tracker — ASCII letters,
/// digits, `_`, `$`. Deliberately NOT dialect-shaped: the tracker only
/// needs to tell a clause keyword from a column's name.
pub(super) fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// The lexical judgments the byte walk needs — every one read off the
/// same `sqlparser` dialect the parser runs under, so a `\'`, an
/// `E'…'`/`x'…'`/`r'…'`/`b'…'`/`u&'…'` prefix, a `q'[x]'` quoted string,
/// a `"` that is a string rather than a quoted identifier, `#`-/`//`-/
/// `--`-to-EOL comments, and `/* /* */ */` nesting all close where the
/// tokenizer closes them, never where a hand-rolled scanner guessed.
///
/// Approximations that remain (all cheap and safe-direction): `'''…'''`
/// triple-quoted strings scan as a `''`-escaped `'…'` — the same byte
/// span, and an interior `{{ }}` still renders inside a literal;
/// `N'…'` national strings scan as a plain `'` literal — the tokenizer
/// reads them with the same quote and the dialect's own escape rule;
/// `x`/`u&` prefixes only count at word start (`xu&'` stays a plain
/// literal, matching the tokenizer's word-first read).
pub(super) struct Lex {
    /// `\'`/`\"` escape inside a `'…'`/`"…"` string —
    /// `Dialect::supports_string_literal_backslash_escape` (Snowflake,
    /// BigQuery, MySQL, Redshift, ClickHouse, Databricks — not Generic).
    string_escape: bool,
    /// `E'…'` escape constants — `Dialect::supports_string_escape_constant`
    /// (Postgres, Redshift, Generic). An unsupported `e` stays an
    /// identifier, so the following `'` is a plain literal either way.
    e_string: bool,
    /// `"` opens a string literal, not a quoted identifier —
    /// `!is_delimited_identifier_start('"') && !is_identifier_start('"')`
    /// (BigQuery and a few others; under Generic `"` is an identifier
    /// quote and `\` never escapes inside it).
    dquote_string: bool,
    /// `r'…'`/`r"…"` raw strings — BigQuery and Generic.
    raw_prefix: bool,
    /// `b'…'`/`b"…"` byte strings — BigQuery, Postgres, MySQL, Generic.
    byte_prefix: bool,
    /// `#` opens a line comment — `Dialect::supports_hash_comment` +
    /// `requires_hash_comment_space` (Snowflake, BigQuery, MySQL, Hive,
    /// ClickHouse — the last asking for a following space or `!`).
    hash_comment: bool,
    hash_comment_space: bool,
    /// `/* /* */ */` nesting — `Dialect::supports_nested_comments`
    /// (Generic, Postgres, ClickHouse, Databricks, DuckDb, MsSql, Spark).
    pub(super) nested_comments: bool,
    /// `$$…$$`/`$tag$…$tag$` ARE strings — inverted
    /// `Dialect::supports_dollar_placeholder` (SQLite, where a `$x$`
    /// is a parameter, never a quoted string).
    pub(super) dollar_quote: bool,
    /// `--` opens a comment only before whitespace —
    /// `Dialect::requires_single_line_comment_whitespace` (MySQL: `--x`
    /// is an expression, not a comment).
    dash_comment_space: bool,
    /// `//` opens a line comment — Snowflake only (the tokenizer keys
    /// off the concrete dialect via `dialect_of!`; no predicate).
    slash_comment: bool,
    /// `q'[x]'`/`nq'[x]'` quote-delimited strings —
    /// `Dialect::supports_quote_delimited_string` (Oracle, Generic).
    q_quote: bool,
    /// `U&'…'` unicode literals — `Dialect::supports_unicode_string_literal`
    /// (Postgres, Generic). `\` begins a codepoint escape there — it
    /// consumes the following bytes, like a quote escape for this scan.
    unicode_prefix: bool,
}

impl Lex {
    pub(super) fn new(dialect: SqlDialect) -> Lex {
        let d = dialect.sqlparser();
        Lex {
            string_escape: d.supports_string_literal_backslash_escape(),
            e_string: d.supports_string_escape_constant(),
            dquote_string: !(d.is_delimited_identifier_start('"') || d.is_identifier_start('"')),
            raw_prefix: matches!(dialect, SqlDialect::BigQuery | SqlDialect::Generic),
            byte_prefix: matches!(
                dialect,
                SqlDialect::BigQuery
                    | SqlDialect::Postgres
                    | SqlDialect::MySql
                    | SqlDialect::Generic
            ),
            hash_comment: d.supports_hash_comment(),
            hash_comment_space: d.requires_hash_comment_space(),
            nested_comments: d.supports_nested_comments(),
            dollar_quote: !d.supports_dollar_placeholder(),
            dash_comment_space: d.requires_single_line_comment_whitespace(),
            slash_comment: matches!(dialect, SqlDialect::Snowflake),
            q_quote: d.supports_quote_delimited_string(),
            unicode_prefix: d.supports_unicode_string_literal(),
        }
    }

    /// Does `\` + any byte escape inside the literal opened at `b[i]` —
    /// the dialect's flag plus the prefix quirks the tokenizer applies:
    /// `x'…'`/`u&'…'` always treat `\` as an escape (a codepoint escape
    /// for `u&`), `E'…'` escapes under escape-constant dialects,
    /// `r'…'`/`b'…'` never do, and an identifier quote (`"` on most
    /// dialects, `` ` `` everywhere) never escapes at all.
    pub(super) fn escape_for(&self, b: &[u8], i: usize) -> bool {
        let prefixed = |c: u8| -> bool {
            i >= 1 && b[i - 1].eq_ignore_ascii_case(&c) && (i < 2 || !word_byte(b[i - 2]))
        };
        match b[i] {
            b'`' => false,
            b'"' => {
                self.dquote_string
                    && self.string_escape
                    && !(self.raw_prefix && prefixed(b'r'))
                    && !(self.byte_prefix && prefixed(b'b'))
            }
            _ => {
                // `u&'…'`/`U&'…'`: the `u` must START a word (`xu&'` is
                // `xu` `&` `'…'` — a plain literal, as the tokenizer
                // reads the word first).
                let unicode = self.unicode_prefix
                    && i >= 2
                    && b[i - 1] == b'&'
                    && b[i - 2].eq_ignore_ascii_case(&b'u')
                    && (i < 3 || !word_byte(b[i - 3]));
                if (self.raw_prefix && prefixed(b'r')) || (self.byte_prefix && prefixed(b'b')) {
                    false
                } else if unicode || prefixed(b'x') || (self.e_string && prefixed(b'e')) {
                    true
                } else {
                    self.string_escape
                }
            }
        }
    }

    /// Does `--` at `b[i]` open a line comment — always, except where the
    /// dialect requires whitespace after the dashes (MySQL: `--x` is an
    /// expression, `-- x` a comment; a trailing `--` at EOF is not one).
    pub(super) fn dash_comment_at(&self, b: &[u8], i: usize) -> bool {
        b.get(i + 1) == Some(&b'-')
            && (!self.dash_comment_space || b.get(i + 2).is_some_and(|c| c.is_ascii_whitespace()))
    }

    /// Does `//` at `b[i]` open a line comment (Snowflake)?
    pub(super) fn slash_comment_at(&self, b: &[u8], i: usize) -> bool {
        self.slash_comment && b.get(i + 1) == Some(&b'/')
    }

    /// Does `#` at `b[i]` open a line comment — the dialect's hash-comment
    /// predicate, including the following-space-or-`!` requirement where
    /// the dialect asks for it.
    pub(super) fn hash_comment_at(&self, b: &[u8], i: usize) -> bool {
        self.hash_comment
            && (!self.hash_comment_space || matches!(b.get(i + 1), Some(b' ') | Some(b'!')))
    }

    /// Does the `'` at `b[i]` open a `q'[x]'`/`nq'[x]'` quote-delimited
    /// string — only where the dialect supports them AND the `'` follows
    /// a bare `q`/`Q` (optionally `n`-prefixed). An identifier ending in
    /// `q` (`sq'…'`) or `nq` (`xnq'…'`) stays a plain `'` literal, as
    /// the tokenizer reads the whole word first.
    pub(super) fn q_quote_at(&self, b: &[u8], i: usize) -> bool {
        if !self.q_quote || i == 0 || !b[i - 1].eq_ignore_ascii_case(&b'q') {
            return false;
        }
        if i == 1 {
            return true;
        }
        if !word_byte(b[i - 2]) {
            return true;
        }
        b[i - 2].eq_ignore_ascii_case(&b'n') && (i < 3 || !word_byte(b[i - 3]))
    }
}

// ---------------------------------------------------------------------------
// The byte walk
// ---------------------------------------------------------------------------

/// Where a Jinja tag sits — top-level SQL, or inside a literal (the
/// quote byte distinguishes strings `'`/`$$` from quoted identifiers
/// `"`/`` ` ``).
#[derive(Clone, Copy)]
enum TagCtx {
    Top,
    Literal(u8),
}

impl TagCtx {
    /// Top-level emissions are SQL tokens — the word/clause tracker
    /// reads them; literal interiors are just text.
    fn is_top(self) -> bool {
        matches!(self, TagCtx::Top)
    }
}

/// Words that END a `SELECT` item list — true clause and statement
/// boundaries (`from`/`where`/`group`/`union`/join-type prefixes/DDL
/// starts…). Expression-internal words are deliberately absent: `as`,
/// `and`, `by`, `order`, `desc`, `then`… all occur INSIDE items, and
/// dropping the flag on them would render a following `{{ }}` as `NULL`
/// (`o.id as other NULL` — broken) where `, <name>` parses cleanly
/// (`o.id as other, <name>` — a new item, exactly what a column-macro
/// `{{ }}` line means). `select`/`distinct` re-enter.
const CLAUSE_WORDS: &[&[u8]] = &[
    b"all",
    b"alter",
    b"analyze",
    b"apply",
    b"begin",
    b"call",
    b"comment",
    b"constraint",
    b"copy",
    b"create",
    b"cross",
    b"declare",
    b"default",
    b"delete",
    b"describe",
    b"do",
    b"drop",
    b"elif",
    b"elseif",
    b"exec",
    b"execute",
    b"except",
    b"explain",
    b"fetch",
    b"for",
    b"foreign",
    b"from",
    b"full",
    b"grant",
    b"group",
    b"having",
    b"if",
    b"inner",
    b"insert",
    b"intersect",
    b"into",
    b"join",
    b"key",
    b"lateral",
    b"left",
    b"limit",
    b"loop",
    b"match_recognize",
    b"merge",
    b"minus",
    b"natural",
    b"no",
    b"of",
    b"offset",
    b"on",
    b"outer",
    b"pragma",
    b"primary",
    b"qualify",
    b"recursive",
    b"references",
    b"rename",
    b"repeat",
    b"replace",
    b"return",
    b"returning",
    b"revoke",
    b"right",
    b"sample",
    b"seek",
    b"set",
    b"show",
    b"table",
    b"tablesample",
    b"truncate",
    b"union",
    b"unique",
    b"unnest",
    b"unpivot",
    b"until",
    b"update",
    b"use",
    b"using",
    b"vacuum",
    b"values",
    b"where",
    b"while",
    b"window",
    b"with",
];

/// Words after which a bare identifier always parses — name positions
/// (`as`/`from`/`join`/`with` give the alias, relation, or CTE name —
/// `FROM NULL` fails outright on some dialects) and value positions
/// (`then`/`else`/`and`/`or`/`by`/`on`/`collate` take an expression a
/// marked column name satisfies). A `{{ }}` after one of these emits a
/// generated identifier, never `NULL` or a leading `,`.
const NAME_WORDS: &[&[u8]] = &[
    b"and", b"as", b"between", b"by", b"case", b"collate", b"else", b"end", b"from", b"join",
    b"like", b"ilike", b"rlike", b"similar", b"not", b"on", b"or", b"then", b"when", b"with",
];

/// Which `{% if %}`/`{% for %}` arms a render emits — [`jinja_parse`]
/// judges the union view first and falls to the collapse view when the
/// union text can't parse.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderArms {
    /// Every arm's content — an `{% if %}` chain's `{% elif %}`/`{% else %}`
    /// splits emit `,` where the position allows, unioning the arms as
    /// list items: the coverage view the resolver and lint judge first.
    Union,
    /// Only each chain's arm 0 — one render's worth of SQL, so facts can
    /// never combine mutually exclusive arms. Every arm split renders
    /// whitespace.
    Collapse,
}

/// The byte-walk state [`render_jinja_mapped`] carries — the emitted
/// text + segment map plus the little context a `{{ }}` render needs:
/// the last completed word (`as`/`from`/`join`/`select` matter), whether
/// a `SELECT` item list is open, the last significant emitted byte (a
/// `,` or an expression ender), and the per-tag counter keeping every
/// generated identifier unique.
struct Scan<'a> {
    sql: &'a str,
    lex: Lex,
    mark: JinjaMark,
    /// The arm mode this render runs under.
    arms: RenderArms,
    /// The source's `{% if %}`/`{% for %}` arm index — arm paths decide
    /// collapse-mode drops and whether a `{% else %}` split can emit
    /// `,`.
    branches: JinjaBranches,
    out: Vec<u8>,
    segs: Vec<Seg>,
    branch_commas: BTreeSet<usize>,
    jinja_literals: Vec<String>,
    /// Identifier bytes accumulating — flushed to `last_word` by the
    /// first non-identifier byte.
    word: Vec<u8>,
    /// The last completed identifier, lowercased — the `as`/`from`/`join`
    /// position checks read it.
    last_word: Vec<u8>,
    /// The last non-whitespace token byte — comments and literal
    /// interiors don't update it.
    last_sig: Option<u8>,
    /// Inside a `SELECT` item list — `select`/`distinct` set it, a
    /// [`CLAUSE_WORDS`] keyword clears it.
    select: bool,
    /// The `(last_word, select)` frames `(` pushed — a parenthesized
    /// expression is a FRESH context: the `select`/`from` an inner
    /// subquery holds are its own, and letting them mutate the outer
    /// state would render a following `{{ }}` from the wrong position
    /// (`select a, (select y from t) {{ m() }}` — the inner `from` must
    /// not close the outer item list). `)` restores the frame.
    paren: Vec<(Vec<u8>, bool)>,
    /// Per-render counter stamped on generated names so two `{{ }}`
    /// placeholders can never collide into one name.
    seq: usize,
}

impl<'a> Scan<'a> {
    fn new(
        sql: &'a str,
        mark: JinjaMark,
        lex: Lex,
        branches: JinjaBranches,
        arms: RenderArms,
    ) -> Scan<'a> {
        Scan {
            sql,
            lex,
            mark,
            arms,
            branches,
            out: Vec::with_capacity(sql.len() + 16),
            segs: Vec::new(),
            branch_commas: BTreeSet::new(),
            jinja_literals: Vec::new(),
            word: Vec::new(),
            last_word: Vec::new(),
            last_sig: None,
            select: false,
            paren: Vec::new(),
            seq: 0,
        }
    }

    /// Feed emitted TOKEN bytes into the word/clause context — identifier
    /// bytes accumulate into `word`, other bytes complete it (updating
    /// `last_word`/`select`), every non-whitespace byte updates
    /// `last_sig`, and `(`/`)` push/pop the [`Scan::paren`] frames so a
    /// subquery's keywords stay inside the subquery. Literal interiors
    /// and comments are never fed.
    fn note(&mut self, bytes: &[u8]) {
        for &c in bytes {
            if !c.is_ascii_whitespace() {
                self.last_sig = Some(c);
            }
            if word_byte(c) {
                self.word.push(c.to_ascii_lowercase());
                continue;
            }
            if !self.word.is_empty() {
                let w = std::mem::take(&mut self.word);
                if w == b"select" || w == b"distinct" {
                    self.select = true;
                } else if CLAUSE_WORDS.contains(&w.as_slice()) {
                    self.select = false;
                }
                self.last_word = w;
            }
            match c {
                b'(' => {
                    self.paren
                        .push((std::mem::take(&mut self.last_word), self.select));
                    self.select = false;
                }
                b')' => {
                    if let Some((w, s)) = self.paren.pop() {
                        self.last_word = w;
                        self.select = s;
                    }
                }
                _ => {}
            }
        }
    }

    /// Emit raw range `src` verbatim; `note` when the bytes are tokens
    /// (code regions) rather than literal text or comments.
    fn copy(&mut self, src: Range<usize>, note: bool) {
        if note {
            self.note(&self.sql.as_bytes()[src.clone()]);
        }
        emit_copy(
            &mut self.out,
            &mut self.segs,
            src.clone(),
            &self.sql.as_bytes()[src],
        );
    }

    /// Emit `bytes` as a tag's render; `note` when they're tokens.
    fn subst(&mut self, src: Range<usize>, bytes: &[u8], note: bool) {
        if note {
            self.note(bytes);
        }
        emit_subst(&mut self.out, &mut self.segs, src, bytes);
    }

    fn finish(self) -> RenderedSql {
        // Invariant: emitted text is always valid UTF-8 — renders are
        // ASCII and every copied range is a contiguous run of the source
        // `&str`, emitted in order.
        RenderedSql {
            text: String::from_utf8(self.out).expect("the render only emits valid UTF-8"),
            segs: self.segs,
            branch_commas: self.branch_commas,
            jinja_literals: self.jinja_literals,
            branches: self.branches,
        }
    }

    /// Is raw offset `off` inside a `>0` arm the collapse render keeps
    /// out — the `{% elif %}`/`{% else %}` bodies dbt wouldn't emit on
    /// the arm-0 render this text approximates?
    fn dropped(&self, off: usize) -> bool {
        self.arms == RenderArms::Collapse && self.branches.arm_path(off).iter().any(|s| s.arm > 0)
    }

    fn run(&mut self) {
        let b = self.sql.as_bytes();
        let mut i = 0usize;
        while i < b.len() {
            // Collapse mode: a stretch sitting in a `>0` arm emits one
            // space for the whole run — the chain's arm 0 keeps its
            // text.
            if self.dropped(i) {
                let start = i;
                i += 1;
                while i < b.len() && self.dropped(i) {
                    i += 1;
                }
                self.subst(start..i, b" ", true);
                continue;
            }
            match b[i] {
                // Comments pass through verbatim — a `{{` inside one is
                // comment text, never a marker. `--` needs whitespace
                // after it only where the dialect asks (MySQL's `--x`
                // is an expression); `//` is Snowflake's own; `/*`
                // nests where the dialect says it does.
                b'-' if self.lex.dash_comment_at(b, i) => {
                    let end = line_comment_end(b, i);
                    self.copy(i..end, false);
                    i = end;
                }
                b'/' if self.lex.slash_comment_at(b, i) => {
                    let end = line_comment_end(b, i);
                    self.copy(i..end, false);
                    i = end;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    let end = block_comment_end(b, i, self.lex.nested_comments);
                    self.copy(i..end, false);
                    i = end;
                }
                // `# …` — a comment only where the dialect makes it one
                // (`# don't` on Snowflake/BigQuery/MySQL/Hive; elsewhere
                // `#` is just a byte and the line's contents stay live).
                b'#' if self.lex.hash_comment_at(b, i) => {
                    let end = line_comment_end(b, i);
                    self.copy(i..end, false);
                    i = end;
                }
                // `q'[x]'`/`nq'[x]'` — a `'…'` scan would stop at the
                // first interior `'`, so it gets its own close rule.
                b'\'' if self.lex.q_quote_at(b, i) => {
                    i = self.q_literal(i);
                }
                q @ (b'\'' | b'"' | b'`') => {
                    i = self.literal(i, q);
                }
                // Dollar-quoted strings — `$$…$$`/`$tag$…$tag$`, only
                // where `$` isn't a placeholder (SQLite).
                b'$' if self.lex.dollar_quote => match dollar_quote_open(b, i) {
                    Some(dlen) => {
                        i = self.dollar_literal(i, dlen);
                    }
                    None => {
                        self.copy(i..i + 1, true);
                        i += 1;
                    }
                },
                b'{' if matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) => {
                    i = self.tag(i, TagCtx::Top);
                }
                _ => {
                    self.copy(i..i + 1, true);
                    i += 1;
                }
            }
        }
    }

    /// Scan a `'…'`/`"…"`/`` `…` `` literal opened at `i`: the dialect's
    /// escape rule decides whether `\'` keeps it open, `''`-doubling
    /// always does, and Jinja tags inside render like anywhere else —
    /// real Jinja substitutes inside SQL strings too, so `'{{ var('x') }}'`
    /// is a literal with a tag inside it, not two literals. Returns the
    /// index after the closing quote — `b.len()` when unterminated.
    fn literal(&mut self, i: usize, q: u8) -> usize {
        let b = self.sql.as_bytes();
        let escape = self.lex.escape_for(b, i);
        self.copy(i..i + 1, false);
        let mut j = i + 1;
        while j < b.len() {
            if self.dropped(j) {
                self.subst(j..j + 1, b" ", false);
                j += 1;
            } else if escape && b[j] == b'\\' && j + 1 < b.len() {
                self.copy(j..j + 2, false);
                j += 2;
            } else if b[j] == q {
                if b.get(j + 1) == Some(&q) {
                    self.copy(j..j + 2, false);
                    j += 2;
                } else {
                    self.copy(j..j + 1, false);
                    // The closing quote ends a token — feed a boundary
                    // byte so a following `{{ }}` sees an expression end.
                    self.note(b"\"");
                    return j + 1;
                }
            } else if b[j] == b'{'
                && matches!(b.get(j + 1), Some(&b'{') | Some(&b'%') | Some(&b'#'))
            {
                j = self.tag(j, TagCtx::Literal(q));
            } else {
                self.copy(j..j + 1, false);
                j += 1;
            }
        }
        j
    }

    /// Scan a `q'[x]'` quote-delimited literal opened at `i` — no
    /// escapes: the byte after `'` picks the delimiter (`(`/`<`/`{`
    /// pair, `[`→`]`, else the byte itself) and the string closes at
    /// `enddelim'`. Jinja tags inside render like anywhere else.
    /// Returns the index after the close — `b.len()` when unterminated.
    fn q_literal(&mut self, i: usize) -> usize {
        let b = self.sql.as_bytes();
        self.copy(i..i + 1, false);
        let Some(&open) = b.get(i + 1) else {
            return b.len();
        };
        self.copy(i + 1..i + 2, false);
        let close = match open {
            b'(' => b')',
            b'[' => b']',
            b'<' => b'>',
            b'{' => b'}',
            c => c,
        };
        let mut j = i + 2;
        while j < b.len() {
            if self.dropped(j) {
                self.subst(j..j + 1, b" ", false);
                j += 1;
            } else if b[j] == b'{'
                && matches!(b.get(j + 1), Some(&b'{') | Some(&b'%') | Some(&b'#'))
            {
                j = self.tag(j, TagCtx::Literal(b'\''));
            } else if b[j] == close && b.get(j + 1) == Some(&b'\'') {
                self.copy(j..j + 2, false);
                self.note(b"\"");
                return j + 2;
            } else {
                self.copy(j..j + 1, false);
                j += 1;
            }
        }
        j
    }

    /// Scan a `$$…$$`/`$tag$…$tag$` literal — no escapes, Jinja tags
    /// inside render like anywhere else.
    fn dollar_literal(&mut self, i: usize, dlen: usize) -> usize {
        let b = self.sql.as_bytes();
        let delim = &b[i..i + dlen];
        self.copy(i..i + dlen, false);
        let mut j = i + dlen;
        while j < b.len() {
            if self.dropped(j) {
                self.subst(j..j + 1, b" ", false);
                j += 1;
            } else if b[j] == b'{'
                && matches!(b.get(j + 1), Some(&b'{') | Some(&b'%') | Some(&b'#'))
            {
                j = self.tag(j, TagCtx::Literal(b'$'));
            } else if b[j..].starts_with(delim) {
                self.copy(j..j + dlen, false);
                self.note(b"\"");
                return j + dlen;
            } else {
                self.copy(j..j + 1, false);
                j += 1;
            }
        }
        j
    }

    /// Handle the `{{`/`{%`/`{#` tag opened at `i` — unterminated markers
    /// copy the rest verbatim (a later `{{` in the remainder is text, and
    /// swallowing the tail would fabricate a shorter query that parses to
    /// wrong edges). Returns the index after the tag — or after the whole
    /// paired span for the template-body `{% %}` forms
    /// ([`Scan::block_tag`]).
    fn tag(&mut self, i: usize, ctx: TagCtx) -> usize {
        let b = self.sql.as_bytes();
        let kind = b[i + 1];
        match tag_close(b, i, kind) {
            Some(e) => {
                match kind {
                    b'{' => {
                        self.expr_tag(i, e, ctx);
                        e + 2
                    }
                    b'%' => self.block_tag(i, e, ctx),
                    _ => {
                        self.subst(i..e + 2, b" ", ctx.is_top()); // `{# … #}`
                        e + 2
                    }
                }
            }
            None => {
                self.copy(i..b.len(), ctx.is_top());
                b.len()
            }
        }
    }

    /// Render a `{{ … }}` expression tag — `config(…)` emits nothing,
    /// `ref`/`source`/`this` emit their relation name(s), everything else
    /// takes the position-aware shape [`Scan::other_text`] picks.
    fn expr_tag(&mut self, i: usize, e: usize, ctx: TagCtx) {
        let inner = &self.sql[i + 2..e];
        let src = i..e + 2;
        let after = self.sql.as_bytes().get(e + 2).copied();
        match classify_jinja_expr(inner) {
            JinjaExpr::Config => self.subst(src, b" ", ctx.is_top()),
            // A classified relation name GLUED to identifier text
            // (`{{ ref('events') }}_2024`, `o.{{ ref('x') }}`) can't keep
            // the quoted render — `"events"_2024` isn't SQL — and it
            // can't render the raw name into the glued identifier either:
            // `events_2024` could bind a same-named CTE/relation the tag
            // never read. A marked generated name parses as one
            // identifier AND stays under the generated-name protections.
            JinjaExpr::Ref(_) | JinjaExpr::Source(..) | JinjaExpr::This
                if ctx.is_top() && self.adjacent(after) =>
            {
                let n = self.jinja_name();
                self.subst(src, n.as_bytes(), true);
            }
            JinjaExpr::Ref(name) => {
                let n = match self.mark {
                    JinjaMark::Plain => name,
                    JinjaMark::Marked => format!("{JINJA_MARK_PREFIX}ref__{name}"),
                };
                let t = self.name_text(&[n], ctx);
                self.subst(src, t.as_bytes(), ctx.is_top());
            }
            JinjaExpr::Source(a, b) => {
                let parts = match self.mark {
                    JinjaMark::Plain => [a, b],
                    JinjaMark::Marked => [
                        format!("{JINJA_MARK_PREFIX}src__{a}"),
                        format!("{JINJA_MARK_PREFIX}src__{b}"),
                    ],
                };
                let t = self.name_text(&parts, ctx);
                self.subst(src, t.as_bytes(), ctx.is_top());
            }
            JinjaExpr::This => {
                let t = self.name_text(&[THIS_NAME.to_string()], ctx);
                self.subst(src, t.as_bytes(), ctx.is_top());
            }
            JinjaExpr::Other(literals) => {
                self.jinja_literals.extend(literals);
                let t = self.other_text(ctx, after);
                self.subst(src, t.as_bytes(), ctx.is_top());
            }
        }
    }

    /// `{% … %}` handling, with two body-aware exceptions:
    ///
    /// - A `{% set x %}…{% endset %}`/`{% macro %}…{% endmacro %}`/
    ///   `{% call %}…{% endcall %}` block is template-side text — dbt
    ///   evaluates the body into a variable, macro, or call result and
    ///   never emits its SQL at this position — so the whole paired span
    ///   renders as whitespace ([`block_body_end`] applies the same tag
    ///   rules [`jinja_branches`] walks by; an unmatched tag falls back
    ///   to tag-only dropping, body text and all).
    /// - An `{% else %}`/`{% elif %}` arm split — only inside an
    ///   `{% if %}` chain (the arm index's innermost step says which
    ///   block kind the tag sits in; a `{% for %}`'s `{% else %}` is its
    ///   empty-iteration fallback, never a union member) — becomes `,`
    ///   in the union render where the position allows: inside a select
    ///   list `a , b` covers BOTH branches as separate items (the
    ///   documented branch union), where `a b` would parse as `a AS b` —
    ///   the false branch's name stealing the true branch's value. An
    ///   adjacent literal comma (the `{% if %}, b{% else %}, c{% endif %}`
    ///   idiom) already separates the items, so a space there. An arm
    ///   opening with `from` keeps the `,` and blanks the keyword —
    ///   `from a ,  b` reads as a FROM-item union. An arm opening with
    ///   any other clause keyword (`{% else %} limit 100`,
    ///   `{% else %} where …`) can't follow `,` without the keyword
    ///   posing as a name (`FROM a , FROM b` reads `from` as a
    ///   relation) — it emits whitespace and the [`jinja_parse`] retry
    ///   keeps one arm instead. Inside a literal a comma is just a
    ///   character — the tag always drops to a space.
    ///
    /// Returns the index after the tag — after the `{% end… %}` for the
    /// blanked-body forms.
    fn block_tag(&mut self, i: usize, e: usize, ctx: TagCtx) -> usize {
        let (word, rest) = tag_word(&self.sql[i + 2..e]);
        if let Some(end) = block_body_end(self.sql, e + 2, word, rest) {
            self.subst(i..end, b" ", ctx.is_top());
            return end;
        }
        if let TagCtx::Literal(_) = ctx {
            self.subst(i..e + 2, b" ", false);
            return e + 2;
        }
        let if_split = self.arms == RenderArms::Union
            && matches!(word, "elif" | "elseif" | "else")
            && matches!(self.branches.arm_path(i).last(), Some(s) if s.kind == ArmKind::If);
        let comma_ready = if_split
            && self.last_sig != Some(b',')
            && next_sig_byte(self.sql.as_bytes(), e + 2) != Some(b',');
        if comma_ready {
            match arm_start(self.sql.as_bytes(), e + 2, &self.lex) {
                ArmStart::From(w) => {
                    // The offset is `out`'s current end — the byte `,`
                    // lands at.
                    self.branch_commas.insert(self.out.len());
                    self.subst(i..e + 2, b",", true);
                    self.copy(e + 2..w.start, false);
                    let end = w.end;
                    self.subst(w, b" ", true);
                    // The `from` still opens the clause for what the
                    // arm emits — a following `{{ }}` reads FROM
                    // position.
                    self.note(b"from ");
                    return end;
                }
                ArmStart::Boundary => {}
                ArmStart::Other => {
                    self.branch_commas.insert(self.out.len());
                    self.subst(i..e + 2, b",", true);
                    return e + 2;
                }
            }
        }
        self.subst(i..e + 2, b" ", true);
        e + 2
    }

    /// The render for a classified relation name — the quoted
    /// `"a"."b"`/`"__dbtl_ref__x"` shape at top level, the bare
    /// `a.b`/`__dbtl_ref__x` inside a literal (the literal's own quote
    /// already delimits it — `'"x"'`/`""x""` would mangle the text).
    fn name_text(&self, parts: &[String], ctx: TagCtx) -> String {
        match ctx {
            TagCtx::Top => parts
                .iter()
                .map(|p| quoted_ident(p))
                .collect::<Vec<_>>()
                .join("."),
            TagCtx::Literal(_) => parts.join("."),
        }
    }

    /// The next generated identifier — `__dbtl_jinja_0__`, `_1__`, … —
    /// unique per tag and marked, so every scan that understands the
    /// prefix treats it as generated.
    fn jinja_name(&mut self) -> String {
        let n = self.seq;
        self.seq += 1;
        format!("{JINJA_MARK_PREFIX}jinja_{n}__")
    }

    /// The render for an unclassifiable `{{ }}` — `var`, macro calls,
    /// column expressions. The SQL position decides the shape:
    ///
    /// - glued to identifier text or `.` (`{{ c }}_sum`, `o.{{ q('x') }}`,
    ///   `{{ var('s') }}_raw.t`): the tag is a name PART — a marked
    ///   fragment with no surrounding whitespace, so the pieces read as
    ///   one identifier the way real Jinja concatenation produces.
    /// - inside a quoted identifier `"{{ x }}"`/`` `{{ x }}` ``: the same
    ///   fragment — the quote is the delimiter.
    /// - inside a string `'{{ x }}'`/`$$ {{ x }} $$`: `NULL` under
    ///   `Plain` (an unknown value inside the text), a fragment under
    ///   `Marked`.
    /// - right after a [`NAME_WORDS`] keyword (`as`/`from`/`join`/`with`
    ///   but also `then`/`and`/`by`/`on`…): a name or value identifier —
    ///   `FROM NULL` fails outright on some dialects, and two `{{ }}`
    ///   aliases must not fold to the same `null`
    ///   (`duplicate_output_columns`'s false positive).
    /// - inside a `SELECT` item list: an item — when the previous token
    ///   could end an expression (`o.status` then a bare `{{ m() }}` line)
    ///   a leading `,` keeps it from posing as that item's alias; after a
    ///   `,`/`(`/keyword it just becomes the item.
    /// - anywhere else: a value — `NULL` under `Plain`, a marked quoted
    ///   identifier under `Marked`.
    fn other_text(&mut self, ctx: TagCtx, after: Option<u8>) -> String {
        let name = self.jinja_name();
        match ctx {
            TagCtx::Literal(q) if q == b'"' || q == b'`' => name,
            TagCtx::Literal(_) => match self.mark {
                JinjaMark::Plain => " NULL ".to_string(),
                JinjaMark::Marked => name,
            },
            TagCtx::Top => {
                if self.adjacent(after)
                    || self.last_sig == Some(b'.')
                    || NAME_WORDS.contains(&self.last_word.as_slice())
                {
                    return name;
                }
                if self.select {
                    return if self.expr_ender() {
                        format!(", {name}")
                    } else {
                        name
                    };
                }
                match self.mark {
                    JinjaMark::Plain => " NULL ".to_string(),
                    JinjaMark::Marked => format!("\"{name}\""),
                }
            }
        }
    }

    /// Is the `{{ }}` glued to identifier text — an identifier byte or a
    /// `.` immediately before the tag in the emitted text, or immediately
    /// after `}}` in the input? `total_{{ c }}`, `{{ c }}_sum`, and
    /// `o.{{ q('x') }}` all concatenate the value into a name.
    fn adjacent(&self, after: Option<u8>) -> bool {
        let touches = |c: u8| word_byte(c) || c == b'.' || c >= 0x80;
        self.out.last().copied().is_some_and(touches) || after.is_some_and(touches)
    }

    /// Could an expression have ended just before this `{{ }}` — making
    /// it a NEW select item rather than part of the previous one?
    /// Identifiers (but not the `select`/`distinct` openers), literals,
    /// `)`, `]`, `*`, `?` end expressions; keywords, operators, `(`, `,`
    /// don't. `'`/`` ` `` can't appear as `last_sig` — a literal's
    /// closing quote is fed as the `"` boundary byte (see
    /// [`Scan::literal`]), so `"` stands in for every closed literal.
    fn expr_ender(&self) -> bool {
        match self.last_sig {
            Some(b')' | b']' | b'*' | b'?' | b'"') => true,
            Some(c) if word_byte(c) || c >= 0x80 => {
                // A word — under `select` it's an identifier or a literal:
                // clause keywords already flipped `select` off, and
                // `select`/`distinct` are openers, not expression ends.
                !matches!(&self.last_word[..], b"select" | b"distinct")
            }
            _ => false,
        }
    }
}

/// The index of the line comment's end (the `\n`, or EOF) for the `--`
/// or `#` opened at `i`.
pub(super) fn line_comment_end(b: &[u8], i: usize) -> usize {
    b[i..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| i + p)
}

/// The index after `*/` for the block comment opened at `i` — `b.len()`
/// when unterminated. `nested` (the dialect's
/// `supports_nested_comments`) counts inner `/*`/`*/` pairs so
/// `/* a /* b */ c */` ends at the OUTER close where the dialect sees
/// one; a flat dialect's first `*/` wins either way.
pub(super) fn block_comment_end(b: &[u8], i: usize, nested: bool) -> usize {
    let mut depth = 1usize;
    let mut j = i + 2;
    while j + 1 < b.len() {
        if b[j] == b'*' && b[j + 1] == b'/' {
            depth -= 1;
            if depth == 0 {
                return j + 2;
            }
            j += 2;
        } else if nested && b[j] == b'/' && b[j + 1] == b'*' {
            depth += 1;
            j += 2;
        } else {
            j += 1;
        }
    }
    b.len()
}

/// The index after a `q'[x]'` quote-delimited string opened at `i` —
/// the byte after `'` picks the delimiter (`(`/`[`/`<`/`{` pair, else
/// the byte itself) and the string closes at `enddelim'`. `b.len()`
/// when unterminated.
pub(super) fn q_quote_end(b: &[u8], i: usize) -> usize {
    let Some(&open) = b.get(i + 1) else {
        return b.len();
    };
    let close = match open {
        b'(' => b')',
        b'[' => b']',
        b'<' => b'>',
        b'{' => b'}',
        c => c,
    };
    let mut j = i + 2;
    while j + 1 < b.len() {
        if b[j] == close && b[j + 1] == b'\'' {
            return j + 2;
        }
        j += 1;
    }
    b.len()
}

/// The index just past the closing quote of the `q`-quoted span opened
/// at `i` — `\<byte>` counts as escaped only under `escape`, `''`/`""`
/// doubling always keeps it open. `b.len()` when unterminated.
pub(super) fn quoted_end(b: &[u8], i: usize, q: u8, escape: bool) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if escape && b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// The index of the close marker's first byte for the tag opened at `i`
/// (`kind` = `b'{'`/`b'%'`/`b'#'`) — `None` when unterminated. `{{ }}`
/// tracks bracket depth so `{{ a({'x':1}) }}` doesn't close at the inner
/// `}}`; `'…'`/`"…"` spans inside `{{`/`{%` are skipped so a quoted `}}`
/// or `%}` — or a bracket inside a string — can't fake the close
/// (`{{ f('}') }}`, `{{ var('x', 'a') }}`…). A `{# #}` comment holds no
/// string syntax, so its quotes never skip. Shared by the render and
/// the trailing-semicolon scan so both read the same tag boundaries.
fn tag_close(b: &[u8], i: usize, kind: u8) -> Option<usize> {
    let close: &[u8] = match kind {
        b'{' => b"}}",
        b'%' => b"%}",
        _ => b"#}",
    };
    let tracked = kind == b'{';
    let mut depth = 0usize;
    let mut j = i + 2;
    while j + 1 < b.len() {
        if b[j] == close[0] && b[j + 1] == close[1] && (!tracked || depth == 0) {
            return Some(j);
        }
        match b[j] {
            // A `{# #}` comment holds NO string syntax — a `'`/`"` inside
            // is plain comment text (`{# don't run #}`), and skipping a
            // quoted span would either hunt a closing quote to EOF
            // (returning `None` → the `{#` leaks into the render) or eat
            // the SQL between two `#}`s. Only `{{`/`{%` bodies hold
            // Jinja strings whose quotes can hide a close marker.
            q @ (b'\'' | b'"') if kind != b'#' => {
                // Jinja strings backslash-escape — `\'` stays inside.
                j = quoted_end(b, j, q, true);
                continue;
            }
            b'(' | b'[' | b'{' if tracked => depth += 1,
            b')' | b']' | b'}' if tracked => depth = depth.saturating_sub(1),
            _ => {}
        }
        j += 1;
    }
    None
}

/// Turn raw dbt SQL into something a plain-SQL parser can read:
/// `ref()`/`source()` render to quoted relation names, other `{{ }}` take
/// the shape their position needs, `{% %}`/`{# #}` drop. Compiled SQL
/// needs nothing (dbt already rendered it) — this only runs on the raw
/// fallback text, where the approximation is documented. `dialect` is the
/// same [`SqlDialect`] the parse runs under — the scan derives its
/// literal/comment boundaries from it (`\'` escapes, `#` comments,
/// `E'…'`/`x'…'`/`r'…'`/`b'…'` prefixes, `"` as string vs identifier
/// quote) so the two never disagree about where a literal ends.
///
/// Jinja markers render inside SQL string literals and quoted
/// identifiers too — real Jinja substitutes `'{{ var('x') }}'` — while
/// markers inside SQL comments stay text. A `{{`/`}}` pair that can't be
/// matched (an unterminated tag, or one whose `}}` only exists inside a
/// later literal) leaves the remainder verbatim rather than eating it.
#[cfg(test)]
pub(super) fn render_jinja(sql: &str, dialect: SqlDialect) -> String {
    render_jinja_as(sql, JinjaMark::Plain, dialect)
}

/// [`render_jinja`] with an explicit marker mode — see [`JinjaMark`].
#[cfg(test)]
pub(super) fn render_jinja_as(sql: &str, mark: JinjaMark, dialect: SqlDialect) -> String {
    render_jinja_mapped(sql, mark, dialect).text
}

/// [`render_jinja_as`], plus the rendered→raw byte map the lint rules
/// convert parsed spans through to point at the written file — the union
/// arm render ([`RenderArms::Union`]).
pub(super) fn render_jinja_mapped(sql: &str, mark: JinjaMark, dialect: SqlDialect) -> RenderedSql {
    render_arms(sql, mark, dialect, RenderArms::Union)
}

/// [`render_jinja_mapped`] with an explicit arm mode — see
/// [`RenderArms`]. The `{% if %}`/`{% for %}` arm index is built on the
/// comment-masked text — `mask_jinja_comments`, the same masking the
/// call-site scanner indexes — so a `{# {% else %} #}` can't fake an arm
/// and every arm path lines up with what `CallSiteResolver` computes.
pub(super) fn render_arms(
    sql: &str,
    mark: JinjaMark,
    dialect: SqlDialect,
    arms: RenderArms,
) -> RenderedSql {
    let branches = jinja_branches(&mask_jinja_comments(sql));
    let mut scan = Scan::new(sql, mark, Lex::new(dialect), branches, arms);
    scan.run();
    scan.finish()
}

/// Render `sql` and return the first parse that succeeds, trying:
///
/// 1. the union render as-is,
/// 2. the union render with dangling loop separators blanked
///    ([`strip_dangling`]) — a `{% for %}` body's
///    `{% if not loop.last %}SEP{% endif %}` keeps the separator on the
///    last iteration because `loop.last` can't be evaluated here,
/// 3. the collapse render — arm 0 of every `{% if %}`/`{% for %}` chain,
///    one render's worth of SQL, so mutually exclusive arms (`where`,
///    `on`, `limit`, whole-statement splits) can't combine into an
///    unparsable or phantom-fact union, and
/// 4. the collapse render stripped the same way.
///
/// `None` when all four fail — the caller's unparsed-model path.
pub(super) fn jinja_parse(
    sql: &str,
    mark: JinjaMark,
    dialect: SqlDialect,
) -> Option<(RenderedSql, Vec<Statement>)> {
    let lex = Lex::new(dialect);
    let mut rendered = render_jinja_mapped(sql, mark, dialect);
    if let Some(stmts) = parse_statements(dialect, &rendered.text) {
        return Some((rendered, stmts));
    }
    strip_dangling(&mut rendered, &lex);
    if let Some(stmts) = parse_statements(dialect, &rendered.text) {
        return Some((rendered, stmts));
    }
    let mut rendered = render_arms(sql, mark, dialect, RenderArms::Collapse);
    if let Some(stmts) = parse_statements(dialect, &rendered.text) {
        return Some((rendered, stmts));
    }
    strip_dangling(&mut rendered, &lex);
    parse_statements(dialect, &rendered.text).map(|stmts| (rendered, stmts))
}

// ---------------------------------------------------------------------------
// Parse-failure retry helpers — dangling separators and arm starts
// ---------------------------------------------------------------------------

/// Words an arm can't open with when `,` precedes it — clause starters
/// and clause-internal boundary words (`limit`/`where`/`on`/`union`/
/// `and`/`or`/`then`/`end`…): after a comma they'd parse as a name
/// (`FROM a , FROM b` reads `from` as a relation) or simply fail.
/// Deliberately absent: words a `,`-separated item legitimately starts
/// with — `not`/`case`/function names/`over`, and the comma-joinable
/// FROM items `lateral`/`unnest`. `select` IS included — `, select`
/// can't union the statements — while [`dangling_at`] treats it as an
/// operand because `union all select` is a real arm.
const ARM_BOUNDARY_WORDS: &[&[u8]] = &[
    b"all",
    b"and",
    b"anti",
    b"apply",
    b"as",
    b"asof",
    b"between",
    b"by",
    b"collate",
    b"cross",
    b"distinct",
    b"else",
    b"end",
    b"escape",
    b"except",
    b"fetch",
    b"for",
    b"from",
    b"full",
    b"group",
    b"having",
    b"ilike",
    b"in",
    b"inner",
    b"intersect",
    b"into",
    b"is",
    b"join",
    b"left",
    b"like",
    b"limit",
    b"match_recognize",
    b"minus",
    b"natural",
    b"offset",
    b"on",
    b"or",
    b"order",
    b"outer",
    b"pivot",
    b"prewhere",
    b"qualify",
    b"right",
    b"rlike",
    b"select",
    b"semi",
    b"set",
    b"similar",
    b"tablesample",
    b"then",
    b"top",
    b"union",
    b"unpivot",
    b"using",
    b"values",
    b"when",
    b"where",
    b"window",
    b"with",
];

/// What the raw text after an arm split opens with — decides whether
/// the union render can put `,` between the arms.
enum ArmStart {
    /// `from` — the `Range` covers the keyword: `from a , <blank> b`
    /// unions the keyword-per-arm idiom's relations.
    From(Range<usize>),
    /// A word that can't follow `,` — whitespace is the honest render;
    /// the collapse retry then keeps one arm.
    Boundary,
    /// Anything else — an expression, a name, a `{{`/`{%` marker: `,`
    /// unions the arms like today.
    Other,
}

/// The first token an arm opens with — whitespace, SQL comments, and
/// `{# #}` Jinja comments skipped.
fn arm_start(b: &[u8], from: usize, lex: &Lex) -> ArmStart {
    let mut j = from;
    loop {
        match b.get(j) {
            Some(c) if c.is_ascii_whitespace() => j += 1,
            Some(b'-') if lex.dash_comment_at(b, j) => j = line_comment_end(b, j),
            Some(b'/') if lex.slash_comment_at(b, j) => j = line_comment_end(b, j),
            Some(b'/') if b.get(j + 1) == Some(&b'*') => {
                j = block_comment_end(b, j, lex.nested_comments)
            }
            Some(b'#') if lex.hash_comment_at(b, j) => j = line_comment_end(b, j),
            Some(b'{') if b.get(j + 1) == Some(&b'#') => match jinja_tag_close(b, j) {
                Some(e) => j = e + 2,
                None => return ArmStart::Other,
            },
            Some(&c) if word_byte(c) => {
                let start = j;
                while j < b.len() && word_byte(b[j]) {
                    j += 1;
                }
                let mut w = b[start..j].to_vec();
                w.make_ascii_lowercase();
                return if w == b"from" {
                    ArmStart::From(start..j)
                } else if ARM_BOUNDARY_WORDS.contains(&w.as_slice()) {
                    ArmStart::Boundary
                } else {
                    ArmStart::Other
                };
            }
            // Punctuation or a `{{`/`{%` marker — the arm's own render
            // decides the shape; `,` is the honest union.
            _ => return ArmStart::Other,
        }
    }
}

/// The first non-whitespace byte at or after `from` — the `,`-adjacency
/// check's lookahead.
fn next_sig_byte(b: &[u8], from: usize) -> Option<u8> {
    b[from..].iter().find(|c| !c.is_ascii_whitespace()).copied()
}

/// The next `{` opening a `{{`/`{%`/`{#` marker at or after `from` —
/// `None` when the rest of the text holds none.
fn next_tag_open(b: &[u8], from: usize) -> Option<usize> {
    (from..b.len().saturating_sub(1))
        .find(|&i| b[i] == b'{' && matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')))
}

/// The offset just past the `{% end… %}` closing the template-body
/// block `{% <word> … %}` opened — `{% set x %}` (the block form ONLY:
/// `{% set x = … %}` is the statement, no body), `{% macro %}`,
/// `{% call %}`. `None` for other tags and unmatched pairs — the tag
/// then renders like any other, body text and all. Same-word blocks
/// nest; `{% raw %}` bodies are inert; `{{`/`{#` markers are skipped
/// whole — the same rules [`jinja_branches`] walks by.
fn block_body_end(sql: &str, from: usize, word: &str, rest: &str) -> Option<usize> {
    let close = match word {
        "set" if !jinja_set_assigns(rest) => "endset",
        "macro" => "endmacro",
        "call" => "endcall",
        _ => return None,
    };
    let b = sql.as_bytes();
    let mut depth = 1usize;
    let mut j = from;
    loop {
        let open = next_tag_open(b, j)?;
        let e = jinja_tag_close(b, open)?;
        if b[open + 1] == b'%' {
            let (w, r) = tag_word(&sql[open + 2..e]);
            if w == close {
                depth -= 1;
                if depth == 0 {
                    return Some(e + 2);
                }
            } else if w == "raw" {
                // A `{% raw %}` body is inert — scan `{%` markers for
                // `{% endraw %}` like `jinja_branches` does.
                let mut k = e + 2;
                loop {
                    let t = find_subslice(b, k, b"{%")?;
                    let te = jinja_tag_close(b, t)?;
                    k = te + 2;
                    if tag_word(&sql[t + 2..te]).0 == "endraw" {
                        break;
                    }
                }
                j = k;
                continue;
            } else if w == word && (word != "set" || !jinja_set_assigns(r)) {
                depth += 1;
            }
        }
        j = e + 2;
    }
}

/// Blank the `,`/`and`/`or`/`union [all|distinct]`/`intersect`/`except`/
/// `minus` separators the rendered text leaves dangling — a `{% for %}`
/// body's `{% if not loop.last %} SEP {% endif %}` keeps the separator
/// on the last iteration because `loop.last` can't be evaluated here,
/// so `a, b, from t` or `x union all y union all` reaches the parser
/// with a separator missing its right operand. A separator is dangling
/// when the token after it can't be an operand — `)`, `;`, another
/// separator, the text's end, or an [`ARM_BOUNDARY_WORDS`] keyword
/// (`select` excepted: `union all select` is a real arm). Blanked
/// bytes become spaces — every rendered offset and segment stays
/// valid. A separator with a real operand after it (`a , b`,
/// `x union all y`) is never touched, so the strip only fires on text
/// that couldn't parse anyway.
fn strip_dangling(rendered: &mut RenderedSql, lex: &Lex) {
    let toks = sep_tokens(rendered.text.as_bytes(), lex);
    let mut blanks: Vec<Range<usize>> = Vec::new();
    let mut i = 0usize;
    while i < toks.len() {
        let (r, t) = toks[i].clone();
        let mut sep_end = None;
        match t {
            Tok::Comma => sep_end = Some(r.end),
            Tok::Word => match lower(&rendered.text, &r).as_str() {
                "and" | "or" => sep_end = Some(r.end),
                "union" | "intersect" | "except" | "minus" => {
                    sep_end = Some(r.end);
                    // `union all`/`union distinct` are one separator.
                    if let Some((r2, Tok::Word)) = toks.get(i + 1) {
                        let w2 = lower(&rendered.text, r2);
                        if w2 == "all" || w2 == "distinct" {
                            sep_end = Some(r2.end);
                            i += 1;
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        if let Some(end) = sep_end {
            if dangling_at(&toks, i + 1, &rendered.text) {
                blanks.push(r.start..end);
            }
        }
        i += 1;
    }
    if blanks.is_empty() {
        return;
    }
    let mut text = std::mem::take(&mut rendered.text).into_bytes();
    for r in blanks {
        for c in &mut text[r] {
            *c = b' ';
        }
    }
    rendered.text = String::from_utf8(text).expect("blanked bytes stay UTF-8");
}

/// The token stream [`strip_dangling`] reads — word runs and the
/// single-byte punctuation a separator check cares about; comments and
/// literals (every dialect form) are skipped like the parser reads
/// them.
#[derive(Clone, PartialEq)]
enum Tok {
    Word,
    Comma,
    RParen,
    Semi,
    Other,
}

fn sep_tokens(b: &[u8], lex: &Lex) -> Vec<(Range<usize>, Tok)> {
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'-' if lex.dash_comment_at(b, i) => i = line_comment_end(b, i),
            b'/' if lex.slash_comment_at(b, i) => i = line_comment_end(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => i = block_comment_end(b, i, lex.nested_comments),
            b'#' if lex.hash_comment_at(b, i) => i = line_comment_end(b, i),
            b'\'' if lex.q_quote_at(b, i) => i = q_quote_end(b, i),
            q @ (b'\'' | b'"' | b'`') => i = quoted_end(b, i, q, lex.escape_for(b, i)),
            b'$' if lex.dollar_quote => match dollar_quote_open(b, i) {
                Some(dlen) => {
                    let from = i + dlen;
                    i = dollar_quote_close(b, from, &b[i..i + dlen]).map_or(b.len(), |c| c + dlen);
                }
                None => {
                    toks.push((i..i + 1, Tok::Other));
                    i += 1;
                }
            },
            c if word_byte(c) => {
                let start = i;
                while i < b.len() && word_byte(b[i]) {
                    i += 1;
                }
                toks.push((start..i, Tok::Word));
            }
            b',' => {
                toks.push((i..i + 1, Tok::Comma));
                i += 1;
            }
            b')' => {
                toks.push((i..i + 1, Tok::RParen));
                i += 1;
            }
            b';' => {
                toks.push((i..i + 1, Tok::Semi));
                i += 1;
            }
            _ => {
                toks.push((i..i + 1, Tok::Other));
                i += 1;
            }
        }
    }
    toks
}

/// Can the token at `i` (or the text's end) take a separator's operand?
/// `)`, `;`, another separator, a clause keyword — `select` excepted:
/// `union all select` is a real arm — and EOF can't, so the separator
/// before them is dangling.
fn dangling_at(toks: &[(Range<usize>, Tok)], i: usize, text: &str) -> bool {
    match toks.get(i) {
        None => true,
        Some((_, Tok::Comma | Tok::RParen | Tok::Semi)) => true,
        Some((r, Tok::Word)) => {
            let w = lower(text, r);
            w != "select" && ARM_BOUNDARY_WORDS.contains(&w.as_bytes())
        }
        Some((_, Tok::Other)) => false,
    }
}

/// The token's bytes lowercased — separator and boundary matching is
/// case-insensitive.
fn lower(text: &str, r: &Range<usize>) -> String {
    text[r.clone()].to_ascii_lowercase()
}

/// If `b[i]` opens a dollar-quoted string, the opening delimiter's byte
/// length (`$$` → 2, `$tag$` → tag length + 2). Tags follow identifier
/// rules (no leading digit) so a Postgres `$1` positional parameter or a
/// `$`-identifier never opens one.
pub(super) fn dollar_quote_open(b: &[u8], i: usize) -> Option<usize> {
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
pub(super) fn dollar_quote_close(b: &[u8], from: usize, delim: &[u8]) -> Option<usize> {
    b[from.min(b.len())..]
        .windows(delim.len())
        .position(|w| w == delim)
        .map(|p| from + p)
}

/// What a `{{ … }}` expression IS for the render — the classification
/// that picks its text. `config(…)` emits nothing; `ref`/`source` emit
/// their relation name(s) (the LAST literal argument of `ref` is the
/// model name; the two-arg form of `source` is what dbt renders);
/// `{{ this }}` is the self relation; everything else (`{{ var(...) }}`,
/// macro calls, `{{ col }}`…) is [`JinjaExpr::Other`] and takes the
/// position-aware shape [`Scan::other_text`] picks. `Other` carries the
/// decoded string literals the expression holds — a macro taking a CTE
/// by string argument (`{{ join_customers('customers', …) }}`) may read
/// it through text the parse can't see, so `unused_cte` counts them.
enum JinjaExpr {
    /// `config(…)` — dbt emits nothing, the render emits whitespace.
    Config,
    /// `ref('x')`/`ref('p','x')` — the model name.
    Ref(String),
    /// `source('a','b')` — (source name, table name).
    Source(String, String),
    /// `{{ this }}`/`{{ this.col }}` — the self relation.
    This,
    /// Unclassified — with the decoded `'…'`/`"…"` literals it carries.
    Other(Vec<String>),
}

/// The `{{ … }}` body stripped of surrounding whitespace and Jinja's
/// whitespace-control/sigil characters (`{{- … -}}`, `{{+ … +}}`,
/// `{{~ … ~}}`) — the text `ref`/`source`/`this` matching reads.
fn jinja_inner(inner: &str) -> &str {
    inner
        .trim()
        .trim_start_matches(['-', '+', '~'])
        .trim_end_matches(['-', '+', '~'])
        .trim()
}

fn classify_jinja_expr(inner: &str) -> JinjaExpr {
    if jinja_expr_emits_nothing(inner) {
        return JinjaExpr::Config;
    }
    let e = jinja_inner(inner);
    if let Some(args) = jinja_call_args(e, "ref") {
        // ref('model') / ref("pkg","model") — the LAST literal arg is the
        // model name (a two-arg ref's first arg is the package).
        if let Some(name) = args.last() {
            return JinjaExpr::Ref(name.clone());
        }
    }
    if let Some(args) = jinja_call_args(e, "source") {
        // source('src','tbl') → "src"."tbl" — the two-arg form dbt renders.
        if args.len() >= 2 {
            return JinjaExpr::Source(args[args.len() - 2].clone(), args[args.len() - 1].clone());
        }
    }
    if e == "this" || e.starts_with("this.") {
        return JinjaExpr::This;
    }
    JinjaExpr::Other(jinja_string_literals(inner))
}

/// The decoded `'…'`/`"…"` string literals in a `{{ }}` expression body —
/// every quoted span, wherever it sits. `\` escape decoding follows
/// Jinja: `\n`/`\t`/`\r` produce the control chars, any other escaped
/// char lands verbatim. An unterminated literal is dropped — it was
/// never a complete string.
fn jinja_string_literals(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(q) = chars.next() {
        if q != '\'' && q != '"' {
            continue;
        }
        let mut lit = String::new();
        let mut closed = false;
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(e) = chars.next() {
                        lit.push(match e {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            other => other,
                        });
                    }
                }
                _ if c == q => {
                    closed = true;
                    break;
                }
                _ => lit.push(c),
            }
        }
        if closed {
            out.push(lit);
        }
    }
    out
}

/// Does a `{{ … }}` body render to nothing? Only `config(…)` emits
/// whitespace — every other expression yields text. Shared by
/// [`classify_jinja_expr`] and the trailing-semicolon scan, which asks
/// the same question without paying for the rendered string.
pub(super) fn jinja_expr_emits_nothing(inner: &str) -> bool {
    let e = jinja_inner(inner);
    e.starts_with("config") && e[6..].trim_start().starts_with('(')
}

/// Double-quote an identifier for the rendered SQL (inner `"`s doubled).
pub(super) fn quoted_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The string-literal arguments of a `name('a','b',…)` call at the start of
/// `e` — just enough to read `ref`/`source` argument lists. Non-literal
/// args (`var('x')`, identifiers) contribute nothing.
pub(super) fn jinja_call_args(e: &str, fname: &str) -> Option<Vec<String>> {
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

/// The raw byte range of a model file's trailing `;` — `None` when the
/// SQL doesn't effectively end with one. "Effectively" is what the Jinja
/// render's tail would say — `{% %}`/`{# #}` emit nothing, `{{ config(…)
/// }}` renders blank, every other `{{ }}` emits text — but a substituted
/// mark never EMITS a `;`, so the effective tail is a copied byte: the
/// raw text answers the question with no render at all. The scan walks
/// the same regions [`render_jinja_mapped`] does — literals (`'…'`/
/// `q'[x]'`/`u&'…'` included), comments (`--`/`//`, `/* */` — nested
/// where the dialect nests — and dialect `#`), dollar-quoted strings,
/// and Jinja markers — under the same dialect-derived rules, so a `;`
/// inside one never counts, a literal AFTER the `;` still counts as
/// content
/// (`select 1; 'x'` is mid-file, not a tail), and only the LAST
/// meaningful byte can qualify.
pub(crate) fn trailing_semicolon(sql: &str, dialect: SqlDialect) -> Option<Range<usize>> {
    let lex = Lex::new(dialect);
    let b = sql.as_bytes();
    // `(position, byte)` of the last meaningful byte — a Jinja mark
    // contributes a synthetic byte (no raw byte stands for it, and marks
    // never end in `;`, so it can't be the tail). `{%` emits `,` on an
    // `{% if %}` chain's arm split unless a literal comma already
    // separates the items or the next arm opens with a clause keyword —
    // the same rule the union render applies, tracked as the byte.
    let mut last: Option<(usize, u8)> = None;
    // Set by an unterminated Jinja marker — the render copies the rest
    // verbatim, so a later `{{` in the remainder is text, not a marker.
    let mut verbatim = false;
    // The arm index — built lazily on the first `{%` tag (most SQL
    // holds none).
    let mut branches: Option<JinjaBranches> = None;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'-' if lex.dash_comment_at(b, i) => {
                i = line_comment_end(b, i);
            }
            b'/' if lex.slash_comment_at(b, i) => {
                i = line_comment_end(b, i);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(b, i, lex.nested_comments);
            }
            b'#' if lex.hash_comment_at(b, i) => {
                i = line_comment_end(b, i);
            }
            b'\'' if lex.q_quote_at(b, i) => {
                let end = q_quote_end(b, i);
                last = Some((end - 1, b[end - 1]));
                i = end;
            }
            q @ (b'\'' | b'"' | b'`') => {
                let end = quoted_end(b, i, q, lex.escape_for(b, i));
                last = Some((end - 1, b[end - 1]));
                i = end;
            }
            b'$' if lex.dollar_quote => match dollar_quote_open(b, i) {
                Some(dlen) => {
                    let from = i + dlen;
                    let end =
                        dollar_quote_close(b, from, &b[i..i + dlen]).map_or(b.len(), |c| c + dlen);
                    last = Some((end - 1, b[end - 1]));
                    i = end;
                }
                None => {
                    last = Some((i, b[i]));
                    i += 1;
                }
            },
            b'{' if !verbatim
                && matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) =>
            {
                let kind = b[i + 1];
                match tag_close(b, i, kind) {
                    Some(e) => {
                        match kind {
                            // `{{ config(…) }}` emits whitespace; every
                            // other `{{ }}` emits text — meaningful, but
                            // never a `;`, so any non-`;` byte stands in.
                            b'{' => {
                                if !jinja_expr_emits_nothing(&sql[i + 2..e]) {
                                    last = Some((i, b'"'));
                                }
                            }
                            // `{% … %}` emits `,` only at an
                            // `{% if %}` chain's `{% elif %}`/`{% else %}`
                            // split where no literal `,` is adjacent and
                            // the next arm doesn't open with a
                            // non-`from` clause keyword — the union
                            // render's exact emission condition.
                            b'%' => {
                                let branches = branches.get_or_insert_with(|| {
                                    jinja_branches(&mask_jinja_comments(sql))
                                });
                                let (word, rest) = tag_word(&sql[i + 2..e]);
                                // A `{% set %}`/`{% macro %}`/`{% call %}`
                                // body renders whitespace too — skip the
                                // whole paired span like `block_tag` does.
                                if let Some(end) = block_body_end(sql, e + 2, word, rest) {
                                    i = end;
                                    continue;
                                }
                                if matches!(word, "elif" | "elseif" | "else")
                                    && matches!(
                                        branches.arm_path(i).last(),
                                        Some(s) if s.kind == ArmKind::If
                                    )
                                    && last.map(|(_, c)| c) != Some(b',')
                                    && next_sig_byte(b, e + 2) != Some(b',')
                                    && !matches!(arm_start(b, e + 2, &lex), ArmStart::Boundary)
                                {
                                    last = Some((i, b','));
                                }
                            }
                            // `{# … #}` emits nothing.
                            _ => {}
                        }
                        i = e + 2;
                    }
                    // An unterminated marker: the render copies the rest
                    // verbatim — a later `{{` stays text, so tag
                    // detection switches off while literal/comment
                    // handling continues as for any rendered bytes.
                    None => {
                        verbatim = true;
                        last = Some((i, b[i]));
                        i += 1;
                    }
                }
            }
            c => {
                if !c.is_ascii_whitespace() {
                    last = Some((i, c));
                }
                i += 1;
            }
        }
    }
    let (start, _) = last.filter(|(_, c)| *c == b';')?;
    Some(start..start + 1)
}
