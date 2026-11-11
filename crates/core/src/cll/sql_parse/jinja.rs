// ---------------------------------------------------------------------------
// Jinja render — compiled-level text from raw model SQL
// ---------------------------------------------------------------------------

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
    let b = sql.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + 16);
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
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2);
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            // `'…'` string literals and `"…"`/`` `…` `` quoted identifiers
            // pass through verbatim (doubled-quote escapes keep them open)
            // — a `{`-pair inside is literal text, never a marker.
            q @ (b'\'' | b'"' | b'`') => {
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
            }
            // Dollar-quoted strings (`$$…$$` / `$tag$…$tag$`) pass through —
            // the same marker-shielding rule as other literals.
            b'$' => match dollar_quote_open(b, i) {
                Some(dlen) => {
                    let from = i + dlen;
                    let end =
                        dollar_quote_close(b, from, &b[i..i + dlen]).map_or(b.len(), |c| c + dlen);
                    out.extend_from_slice(&b[i..end]);
                    i = end;
                }
                None => {
                    out.push(b[i]);
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
                                out.extend_from_slice(render_jinja_expr(inner, mark).as_bytes());
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
                                out.push(if split && !literal_comma { b',' } else { b' ' });
                            }
                            _ => out.push(b' '),
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
                        out.extend_from_slice(&b[i..]);
                        i = b.len();
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    // Jinja interiors are ASCII for anything we render; copied bytes keep
    // the original UTF-8 untouched.
    String::from_utf8(out).unwrap_or_default()
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
    let e = inner
        .trim()
        .trim_start_matches(['-', '+', '~'])
        .trim_end_matches(['-', '+', '~'])
        .trim();
    if e.starts_with("config") && e[6..].trim_start().starts_with('(') {
        return " ".to_string();
    }
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
