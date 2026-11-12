// ---------------------------------------------------------------------------
// Jinja render — compiled-level text from raw model SQL
// ---------------------------------------------------------------------------

use std::ops::Range;

use sqlparser::tokenizer::Span;

/// What `{{ this }}` renders to: parses as a normal identifier and never
/// matches a parent, so self-references contribute no edges instead of
/// killing the whole parse.
const THIS_RELATION: &str = "\"__dbtl_this__\"";

/// The prefix stamped on names rendered under [`JinjaMark::Marked`]:
/// `ref('x')` → `"__dbtl_ref__x"`, `source('a','b')` →
/// `"__dbtl_src__a"."__dbtl_src__b"`, `{{ this }}` → `"__dbtl_this__"`,
/// any other `{{ }}` → `"__dbtl_jinja__"`. No real relation name starts
/// with it, so the lint relation scan treats every marked segment as
/// generated — never a hand-written reference.
pub(super) const JINJA_MARK_PREFIX: &str = "__dbtl_";

/// How `{{ ref }}`/`{{ source }}`/other `{{ }}` render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JinjaMark {
    /// The CLL render: `ref`/`source` become their relation names, other
    /// `{{ }}` become `NULL`.
    Plain,
    /// The lint render: every Jinja-derived token becomes a
    /// [`JINJA_MARK_PREFIX`]-tagged identifier so a parse tree can tell
    /// generated relation names (`{{ ref('x') }}`, `{{ target.schema }}`)
    /// from hand-written ones.
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
pub(super) struct RenderedSql {
    /// The rendered text the parser reads.
    pub(super) text: String,
    /// Segments tiling `text` in emission order (the `src` ranges tile
    /// the input the same way).
    segs: Vec<Seg>,
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

/// Turn raw dbt SQL into something a plain-SQL parser can read:
/// `ref()`/`source()` render to quoted relation names, other `{{ }}` render
/// as `NULL`, `{% %}`/`{# #}` drop. Compiled SQL needs nothing (dbt already
/// rendered it) — this only runs on the raw fallback text, where the
/// approximation is documented.
///
/// Markers are only recognized OUTSIDE SQL string literals and quoted
/// identifiers: `'{{ x' AND y = 1 AND 'z }}'` would otherwise pair the
/// in-literal `{{` with the in-literal `}}` and eat the predicate between
/// them (real Jinja *would* render inside a SQL string, but a `{{`/`}}`
/// pair spanning two separate literals is always our scanner misreading —
/// shielding them keeps the surrounding SQL intact).
pub(super) fn render_jinja(sql: &str) -> String {
    render_jinja_as(sql, JinjaMark::Plain)
}

/// [`render_jinja`] with an explicit marker mode — see [`JinjaMark`]. The
/// `Plain` mode is byte-identical to the historical renderer.
pub(super) fn render_jinja_as(sql: &str, mark: JinjaMark) -> String {
    render_jinja_mapped(sql, mark).text
}

/// [`render_jinja_as`], plus the rendered→raw byte map the lint rules
/// convert parsed spans through to point at the written file.
pub(super) fn render_jinja_mapped(sql: &str, mark: JinjaMark) -> RenderedSql {
    let b = sql.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + 16);
    let mut segs: Vec<Seg> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            // `-- …` line comments and `/* … */` block comments pass
            // through: a `{{` inside one must not pair with a `}}` in later
            // real code (the same cross-region mispairing as literals).
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                emit_copy(&mut out, &mut segs, i..end, &b[i..end]);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2);
                emit_copy(&mut out, &mut segs, i..end, &b[i..end]);
                i = end;
            }
            // `'…'` string literals and `"…"`/`` `…` `` quoted identifiers
            // pass through verbatim (doubled-quote escapes keep them open)
            // — a `{`-pair inside is literal text, never a marker.
            q @ (b'\'' | b'"' | b'`') => {
                let start = i;
                out.push(q);
                i += 1;
                while i < b.len() {
                    out.push(b[i]);
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            out.push(q); // '' escape stays inside the literal
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                // Pushed byte-wise above (the escape check reads `out`);
                // record the whole literal as one copy segment.
                segs.push(Seg {
                    out: out.len() - (i - start)..out.len(),
                    src: start..i,
                    copy: true,
                });
            }
            // Dollar-quoted strings (`$$…$$` / `$tag$…$tag$`) pass through —
            // the same marker-shielding rule as other literals.
            b'$' => match dollar_quote_open(b, i) {
                Some(dlen) => {
                    let from = i + dlen;
                    let end =
                        dollar_quote_close(b, from, &b[i..i + dlen]).map_or(b.len(), |c| c + dlen);
                    emit_copy(&mut out, &mut segs, i..end, &b[i..end]);
                    i = end;
                }
                None => {
                    emit_copy(&mut out, &mut segs, i..i + 1, &b[i..i + 1]);
                    i += 1;
                }
            },
            b'{' if matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) => {
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
                match end {
                    Some(e) => {
                        match kind {
                            b'{' => {
                                let inner = &sql[i + 2..e];
                                emit_subst(
                                    &mut out,
                                    &mut segs,
                                    i..e + 2,
                                    render_jinja_expr(inner, mark).as_bytes(),
                                );
                            }
                            // `{% … %}` tags and `{# … #}` comments drop
                            // entirely — the tag's body (if/for branches)
                            // keeps flowing into the output. A
                            // `{% else %}`/`{% elif %}` branch split becomes
                            // a `,` instead of a space: inside a select list
                            // `a , b` covers BOTH branches as separate
                            // items (the documented branch union), where
                            // `a b` would parse as `a AS b` — the false
                            // branch's name stealing the true branch's
                            // value. An adjacent literal comma (the
                            // `{% if %}, b{% else %}, c{% endif %}`
                            // conditional-column idiom) already separates
                            // the items, so a space there — and clause-level
                            // splits (`select 1 , select 2`) just fail to
                            // parse → the safe per-model fallback.
                            b'%' => {
                                let split = jinja_branch_split(&sql[i + 2..e]);
                                let literal_comma = split
                                    && (out.iter().rev().find(|c| !c.is_ascii_whitespace())
                                        == Some(&b',')
                                        || b[e + 2..].iter().find(|c| !c.is_ascii_whitespace())
                                            == Some(&b','));
                                emit_subst(
                                    &mut out,
                                    &mut segs,
                                    i..e + 2,
                                    &[if split && !literal_comma { b',' } else { b' ' }],
                                );
                            }
                            _ => emit_subst(&mut out, &mut segs, i..e + 2, b" "),
                        }
                        i = e + 2;
                    }
                    // An unterminated marker is malformed Jinja: emit the
                    // remainder verbatim rather than eating it — inside a
                    // SQL comment it stays invisible to the parser; in real
                    // SQL it fails the parse and the model falls back to
                    // name-match. Swallowing the tail would fabricate a
                    // shorter query that parses to wrong edges.
                    None => {
                        emit_copy(&mut out, &mut segs, i..b.len(), &b[i..]);
                        i = b.len();
                    }
                }
            }
            _ => {
                emit_copy(&mut out, &mut segs, i..i + 1, &b[i..i + 1]);
                i += 1;
            }
        }
    }
    // Jinja interiors are ASCII for anything we render; copied bytes keep
    // the original UTF-8 untouched.
    RenderedSql {
        text: String::from_utf8(out).unwrap_or_default(),
        segs,
    }
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

/// What a `{{ … }}` expression renders to. `config(...)` disappears (it
/// emits no SQL); `ref()`/`source()` emit their quoted relation name(s)
/// ([`JinjaMark::Marked`]: `__dbtl_`-prefixed names instead); `this` a fake
/// relation; everything else (`{{ var(...) }}`, macro calls, `{{ col }}`…)
/// → `NULL` under `Plain`, a `__dbtl_jinja__` placeholder under `Marked` —
/// an expression that yields no column refs either way, but that a lint
/// scan can recognise as generated.
pub(super) fn render_jinja_expr(inner: &str, mark: JinjaMark) -> String {
    if jinja_expr_emits_nothing(inner) {
        return " ".to_string();
    }
    let e = inner
        .trim()
        .trim_start_matches(['-', '+', '~'])
        .trim_end_matches(['-', '+', '~'])
        .trim();
    if let Some(args) = jinja_call_args(e, "ref") {
        // ref('model') / ref("pkg","model") — the LAST literal arg is the
        // model name (a two-arg ref's first arg is the package).
        if let Some(name) = args.last() {
            return match mark {
                JinjaMark::Plain => quoted_ident(name),
                JinjaMark::Marked => quoted_ident(&format!("{JINJA_MARK_PREFIX}ref__{name}")),
            };
        }
    }
    if let Some(args) = jinja_call_args(e, "source") {
        // source('src','tbl') → "src"."tbl" — the two-arg form dbt renders.
        if args.len() >= 2 {
            let (a, b) = (&args[args.len() - 2], &args[args.len() - 1]);
            return match mark {
                JinjaMark::Plain => format!("{}.{}", quoted_ident(a), quoted_ident(b)),
                JinjaMark::Marked => format!(
                    "{}.{}",
                    quoted_ident(&format!("{JINJA_MARK_PREFIX}src__{a}")),
                    quoted_ident(&format!("{JINJA_MARK_PREFIX}src__{b}"))
                ),
            };
        }
    }
    if e == "this" || e.starts_with("this.") {
        return THIS_RELATION.to_string();
    }
    match mark {
        JinjaMark::Plain => " NULL ".to_string(),
        // A Jinja expression in a relation position (`FROM {{ var('t') }}`,
        // `FROM {{ target.schema }}.t`) is unclassifiable — the marked token
        // keeps the parse valid and tells the scan "generated, not written".
        JinjaMark::Marked => quoted_ident(&format!("{JINJA_MARK_PREFIX}jinja__")),
    }
}

/// Does a `{{ … }}` body render to nothing? Only `config(…)` emits
/// whitespace — every other expression yields text. Shared by
/// [`render_jinja_expr`] and the trailing-semicolon scan, which asks the
/// same question without paying for the rendered string.
pub(super) fn jinja_expr_emits_nothing(inner: &str) -> bool {
    let e = inner
        .trim()
        .trim_start_matches(['-', '+', '~'])
        .trim_end_matches(['-', '+', '~'])
        .trim();
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
/// the same regions [`render_jinja_mapped`] does — literals, comments,
/// dollar-quoted strings, and Jinja markers — so a `;` inside one never
/// counts, a literal AFTER the `;` still counts as content (`select 1;
/// 'x'` is mid-file, not a tail), and only the LAST meaningful byte can
/// qualify.
pub(crate) fn trailing_semicolon(sql: &str) -> Option<Range<usize>> {
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
                i = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2);
            }
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                last = Some((i - 1, b[i - 1]));
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
                let close: &[u8] = match kind {
                    b'{' => b"}}",
                    b'%' => b"%}",
                    _ => b"#}",
                };
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
                match end {
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
