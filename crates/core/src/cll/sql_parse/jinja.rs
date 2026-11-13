// ---------------------------------------------------------------------------
// Jinja render — compiled-level text from raw model SQL
// ---------------------------------------------------------------------------

use std::collections::BTreeSet;
use std::ops::Range;

use sqlparser::tokenizer::Span;

use super::SqlDialect;

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
fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// The lexical judgments the byte walk needs — every one read off the
/// same `sqlparser` dialect the parser runs under, so a `\'`, an
/// `E'…'`/`x'…'`/`r'…'`/`b'…'` prefix, a `"` that is a string rather
/// than a quoted identifier, and `#`-to-EOL comments all close where the
/// tokenizer closes them, never where a hand-rolled scanner guessed.
struct Lex {
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
}

impl Lex {
    fn new(dialect: SqlDialect) -> Lex {
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
        }
    }

    /// Does `\` + any byte escape inside the literal opened at `b[i]` —
    /// the dialect's flag plus the prefix quirks the tokenizer applies:
    /// `x'…'` always escapes, `E'…'` escapes under escape-constant
    /// dialects, `r'…'`/`b'…'` never do, and an identifier quote (`"` on
    /// most dialects, `` ` `` everywhere) never escapes at all.
    fn escape_for(&self, b: &[u8], i: usize) -> bool {
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
                if (self.raw_prefix && prefixed(b'r')) || (self.byte_prefix && prefixed(b'b')) {
                    false
                } else if prefixed(b'x') || (self.e_string && prefixed(b'e')) {
                    true
                } else {
                    self.string_escape
                }
            }
        }
    }

    /// Does `#` at `b[i]` open a line comment — the dialect's hash-comment
    /// predicate, including the following-space-or-`!` requirement where
    /// the dialect asks for it.
    fn hash_comment_at(&self, b: &[u8], i: usize) -> bool {
        self.hash_comment
            && (!self.hash_comment_space || matches!(b.get(i + 1), Some(b' ') | Some(b'!')))
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
    /// Per-render counter stamped on generated names so two `{{ }}`
    /// placeholders can never collide into one name.
    seq: usize,
}

impl<'a> Scan<'a> {
    fn new(sql: &'a str, mark: JinjaMark, lex: Lex) -> Scan<'a> {
        Scan {
            sql,
            lex,
            mark,
            out: Vec::with_capacity(sql.len() + 16),
            segs: Vec::new(),
            branch_commas: BTreeSet::new(),
            jinja_literals: Vec::new(),
            word: Vec::new(),
            last_word: Vec::new(),
            last_sig: None,
            select: false,
            seq: 0,
        }
    }

    /// Feed emitted TOKEN bytes into the word/clause context — identifier
    /// bytes accumulate into `word`, other bytes complete it (updating
    /// `last_word`/`select`), and every non-whitespace byte updates
    /// `last_sig`. Literal interiors and comments are never fed.
    fn note(&mut self, bytes: &[u8]) {
        for &c in bytes {
            if !c.is_ascii_whitespace() {
                self.last_sig = Some(c);
            }
            if word_byte(c) {
                self.word.push(c.to_ascii_lowercase());
            } else if !self.word.is_empty() {
                let w = std::mem::take(&mut self.word);
                if w == b"select" || w == b"distinct" {
                    self.select = true;
                } else if CLAUSE_WORDS.contains(&w.as_slice()) {
                    self.select = false;
                }
                self.last_word = w;
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
        // Jinja interiors are ASCII for anything we render; copied bytes
        // keep the original UTF-8 untouched.
        RenderedSql {
            text: String::from_utf8(self.out).unwrap_or_default(),
            segs: self.segs,
            branch_commas: self.branch_commas,
            jinja_literals: self.jinja_literals,
        }
    }

    fn run(&mut self) {
        let b = self.sql.as_bytes();
        let mut i = 0usize;
        while i < b.len() {
            match b[i] {
                // Comments pass through verbatim — a `{{` inside one is
                // comment text, never a marker.
                b'-' if b.get(i + 1) == Some(&b'-') => {
                    let end = line_comment_end(b, i);
                    self.copy(i..end, false);
                    i = end;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    let end = block_comment_end(b, i);
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
                q @ (b'\'' | b'"' | b'`') => {
                    i = self.literal(i, q);
                }
                // Dollar-quoted strings — `$$…$$`/`$tag$…$tag$`.
                b'$' => match dollar_quote_open(b, i) {
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
            if escape && b[j] == b'\\' && j + 1 < b.len() {
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

    /// Scan a `$$…$$`/`$tag$…$tag$` literal — no escapes, Jinja tags
    /// inside render like anywhere else.
    fn dollar_literal(&mut self, i: usize, dlen: usize) -> usize {
        let b = self.sql.as_bytes();
        let delim = &b[i..i + dlen];
        self.copy(i..i + dlen, false);
        let mut j = i + dlen;
        while j < b.len() {
            if b[j] == b'{' && matches!(b.get(j + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) {
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
    /// wrong edges). Returns the index after the tag.
    fn tag(&mut self, i: usize, ctx: TagCtx) -> usize {
        let b = self.sql.as_bytes();
        let kind = b[i + 1];
        match tag_close(b, i, kind) {
            Some(e) => {
                match kind {
                    b'{' => self.expr_tag(i, e, ctx),
                    b'%' => self.block_tag(i, e, ctx),
                    _ => self.subst(i..e + 2, b" ", ctx.is_top()), // `{# … #}`
                }
                e + 2
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
        match classify_jinja_expr(inner) {
            JinjaExpr::Config => self.subst(src, b" ", ctx.is_top()),
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
                let after = self.sql.as_bytes().get(e + 2).copied();
                let t = self.other_text(ctx, after);
                self.subst(src, t.as_bytes(), ctx.is_top());
            }
        }
    }

    /// `{% … %}` tags drop entirely — the tag's body keeps flowing into
    /// the output. A `{% else %}`/`{% elif %}` branch split becomes a `,`
    /// instead of a space: inside a select list `a , b` covers BOTH
    /// branches as separate items (the documented branch union), where
    /// `a b` would parse as `a AS b` — the false branch's name stealing
    /// the true branch's value. An adjacent literal comma (the
    /// `{% if %}, b{% else %}, c{% endif %}` conditional-column idiom)
    /// already separates the items, so a space there — and clause-level
    /// splits (`select 1 , select 2`) just fail to parse → the safe
    /// per-model fallback. Inside a literal a comma is just a character —
    /// the tag always drops to a space.
    fn block_tag(&mut self, i: usize, e: usize, ctx: TagCtx) {
        if let TagCtx::Literal(_) = ctx {
            self.subst(i..e + 2, b" ", false);
            return;
        }
        let split = jinja_branch_split(&self.sql[i + 2..e]);
        let literal_comma = split
            && (self.last_sig == Some(b',')
                || self.sql.as_bytes()[e + 2..]
                    .iter()
                    .find(|c| !c.is_ascii_whitespace())
                    == Some(&b','));
        if split && !literal_comma {
            // The offset is `out`'s current end — the byte `,` lands at.
            self.branch_commas.insert(self.out.len());
            self.subst(i..e + 2, b",", true);
        } else {
            self.subst(i..e + 2, b" ", true);
        }
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
    /// don't.
    fn expr_ender(&self) -> bool {
        match self.last_sig {
            Some(b')' | b']' | b'*' | b'?' | b'\'' | b'"' | b'`') => true,
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
fn line_comment_end(b: &[u8], i: usize) -> usize {
    b[i..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| i + p)
}

/// The index after `*/` for the block comment opened at `i` — `b.len()`
/// when unterminated.
fn block_comment_end(b: &[u8], i: usize) -> usize {
    b[i + 2..]
        .windows(2)
        .position(|w| w == b"*/")
        .map_or(b.len(), |p| i + 2 + p + 2)
}

/// The index just past the closing quote of the `q`-quoted span opened
/// at `i` — `\<byte>` counts as escaped only under `escape`, `''`/`""`
/// doubling always keeps it open. `b.len()` when unterminated.
fn quoted_end(b: &[u8], i: usize, q: u8, escape: bool) -> usize {
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
/// `}}`; `'…'`/`"…"` spans inside ANY tag are skipped so a quoted `}}`,
/// `%}`, or `#}` — or a bracket inside a string — can't fake the close
/// (`{{ f('}') }}`, `{{ var('x', 'a') }}`…). Shared by the render and
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
            q @ (b'\'' | b'"') => {
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
pub(super) fn render_jinja(sql: &str, dialect: SqlDialect) -> String {
    render_jinja_as(sql, JinjaMark::Plain, dialect)
}

/// [`render_jinja`] with an explicit marker mode — see [`JinjaMark`].
pub(super) fn render_jinja_as(sql: &str, mark: JinjaMark, dialect: SqlDialect) -> String {
    render_jinja_mapped(sql, mark, dialect).text
}

/// [`render_jinja_as`], plus the rendered→raw byte map the lint rules
/// convert parsed spans through to point at the written file.
pub(super) fn render_jinja_mapped(sql: &str, mark: JinjaMark, dialect: SqlDialect) -> RenderedSql {
    let mut scan = Scan::new(sql, mark, Lex::new(dialect));
    scan.run();
    scan.finish()
}

/// Is `body` (the inside of a `{% … %}` tag, whitespace-control dashes and
/// spaces stripped) a Jinja branch split — `else`, `elif`, or `elseif`? The
/// keyword must be followed by nothing or a non-identifier char so
/// `{%- else -%}` matches but an identifier like `elsewhere` can't.
pub(super) fn jinja_branch_split(body: &str) -> bool {
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
/// the same regions [`render_jinja_mapped`] does — literals, comments
/// (`--`, `/* */`, and dialect `#`), dollar-quoted strings, and Jinja
/// markers — under the same dialect-derived rules, so a `;` inside one
/// never counts, a literal AFTER the `;` still counts as content
/// (`select 1; 'x'` is mid-file, not a tail), and only the LAST
/// meaningful byte can qualify.
pub(crate) fn trailing_semicolon(sql: &str, dialect: SqlDialect) -> Option<Range<usize>> {
    let lex = Lex::new(dialect);
    let b = sql.as_bytes();
    // `(position, byte)` of the last meaningful byte — a Jinja mark
    // contributes a synthetic byte (no raw byte stands for it, and marks
    // never end in `;`, so it can't be the tail). `{%` emits `,` on a
    // branch split unless a literal comma already separates the items —
    // the same adjacency rule the render applies, tracked as the byte.
    let mut last: Option<(usize, u8)> = None;
    // Set by an unterminated Jinja marker — the render copies the rest
    // verbatim, so a later `{{` in the remainder is text, not a marker.
    let mut verbatim = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = line_comment_end(b, i);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = block_comment_end(b, i);
            }
            b'#' if lex.hash_comment_at(b, i) => {
                i = line_comment_end(b, i);
            }
            q @ (b'\'' | b'"' | b'`') => {
                let end = quoted_end(b, i, q, lex.escape_for(b, i));
                last = Some((end - 1, b[end - 1]));
                i = end;
            }
            b'$' => match dollar_quote_open(b, i) {
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
                            // `{% … %}` emits `,` on a branch split
                            // (`{% else %}`) unless a literal comma
                            // already separates the items — meaningful
                            // either way only when it emits `,`.
                            b'%' if jinja_branch_split(&sql[i + 2..e])
                                && last.map(|(_, c)| c) != Some(b',')
                                && b[e + 2..].iter().find(|c| !c.is_ascii_whitespace())
                                    != Some(&b',') =>
                            {
                                last = Some((i, b','));
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
