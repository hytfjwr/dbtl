use super::*;

// ---------------------------------------------------------------------------
// Naïve SQL scanning (deliberately scanner-grade; a real parser replaces it)
// ---------------------------------------------------------------------------

/// One outermost-select-list item: how it names its output column (`alias`)
/// and what it draws from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ItemKind {
    /// `*` or `t.*` — every output column it contributes is a passthrough of
    /// the same-named parent column.
    Wildcard,
    /// A single (possibly qualified) identifier — the normalized source name
    /// (last dotted segment).
    Ident(String),
    /// Any other expression: the normalized identifiers inside it (potential
    /// transform sources).
    Expr(BTreeSet<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectItem {
    /// The output column this item produces (normalized): `x AS c` → `c`,
    /// a bare `x` / `t.x` → `x`. `None` only on a wildcard item.
    pub(super) alias: Option<String>,
    pub(super) kind: ItemKind,
}

/// The masked-SQL parse result: outermost select items + identifier sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedSql {
    /// Outermost select-list items in order.
    pub(super) items: Vec<SelectItem>,
    /// Identifiers appearing after the outermost FROM keyword — the
    /// JOIN/ON/WHERE/GROUP BY/HAVING/ORDER tail (name-match can't tell a join
    /// key from a filter; both are FilterOrJoin).
    pub(super) filter_idents: BTreeSet<String>,
    /// Every identifier in the masked SQL — the fallback presence test.
    pub(super) all_idents: BTreeSet<String>,
}

pub(super) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

pub(super) fn trim_ascii(b: &[u8]) -> &[u8] {
    let mut s = b;
    while !s.is_empty() && s[0].is_ascii_whitespace() {
        s = &s[1..];
    }
    while !s.is_empty() && s[s.len() - 1].is_ascii_whitespace() {
        s = &s[..s.len() - 1];
    }
    s
}

/// Is `kw` at byte offset `i` of `b`, on an identifier boundary, ASCII
/// case-insensitive? (`select` inside `myselect` or `selection` does not
/// count.) A `.` immediately before the word disqualifies it too: the `from`
/// of `t.from` is a qualified identifier segment, not the clause keyword.
/// (A `.` separated by whitespace — `t. from` — still slips through; that
/// spacing is vanishingly rare in generated SQL.)
pub(super) fn keyword_at(b: &[u8], i: usize, kw: &[u8]) -> bool {
    let Some(end) = i.checked_add(kw.len()) else {
        return false;
    };
    end <= b.len()
        && b[i..end].eq_ignore_ascii_case(kw)
        && (i == 0 || (!is_ident_byte(b[i - 1]) && b[i - 1] != b'.'))
        && (end == b.len() || !is_ident_byte(b[end]))
}

/// A set-operator that closes the outermost query for v0 purposes: a second
/// UNION/INTERSECT/EXCEPT arm's columns must not leak into the first arm's
/// select list nor its filter region (v0 answers from the first arm only).
pub(super) fn is_set_operator(b: &[u8], i: usize) -> bool {
    keyword_at(b, i, b"union") || keyword_at(b, i, b"intersect") || keyword_at(b, i, b"except")
}

/// Mask comment / string / Jinja regions of `sql` to spaces (length preserved,
/// newlines kept). `--` / `/* */` comments, `'…'` string literals (with `''`
/// escapes), and `{{ }}` / `{% %}` / `{# #}` Jinja all blank out — what remains
/// is the SQL the scanner actually parses, so a column name inside a comment
/// or a quoted literal can never masquerade as a reference. Double-quoted and
/// backticked identifiers are NOT masked (they are identifiers).
pub(super) fn mask_sql(sql: &str) -> Vec<u8> {
    let b = sql.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        let from = i;
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'\'' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2; // '' escape stays inside the literal
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'{' if matches!(b.get(i + 1), Some(&b'{') | Some(&b'%') | Some(&b'#')) => {
                let close = match b[i + 1] {
                    b'{' => b"}}",
                    b'%' => b"%}",
                    _ => b"#}",
                };
                // `{{ }}` tracks `(`/`[`/`{` depth: a `}}` inside a nested
                // literal — `config(meta={'a': {'b': 1}})` — does NOT close
                // the expression. `{%`/`{#` blocks close at the first `%}`/`#}`.
                let tracked = b[i + 1] == b'{';
                let mut depth = 0usize;
                i += 2;
                while i + 1 < b.len() {
                    match b[i] {
                        _ if b[i] == close[0]
                            && b[i + 1] == close[1]
                            && (!tracked || depth == 0) =>
                        {
                            break
                        }
                        b'(' | b'[' | b'{' if tracked => depth += 1,
                        b')' | b']' | b'}' if tracked => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => {
                i += 1;
                continue;
            }
        }
        for c in out.iter_mut().take(i).skip(from) {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    }
    out
}

/// A second masking layer over [`mask_sql`]'s output: the interiors of
/// double-quoted / backticked identifiers (quotes included) blank to spaces.
/// Clause-keyword scans and select-item splitting run on THIS view, so
/// `"union"` or `` `except` `` can never be mistaken for a clause marker, a
/// set-operator boundary, or an item separator — and a `,`/`(` inside quotes
/// can neither split an item nor skew paren depth. [`idents`] still reads the
/// [`mask_sql`] view: quoted identifiers ARE identifiers to name-match, they
/// just can't carry keywords.
pub(super) fn keyword_mask(text: &[u8]) -> Vec<u8> {
    let mut out = text.to_vec();
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        if b != b'"' && b != b'`' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < text.len() && text[i] != b && text[i] != b'\n' {
            i += 1;
        }
        i = (i + 1).min(text.len()); // consume the closing quote when present
        for c in out.iter_mut().take(i).skip(start) {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    }
    out
}

/// SQL words that are never honest column references when they appear
/// UNQUOTED: clause markers, operators, join/aggregate/window function names,
/// and type names — the unambiguous ones only. `idents()` drops them so a
/// keyword can never name-match a parent column that coincidentally shares
/// its spelling (`cast(x as date)` must not draw a transform edge to a
/// `date` parent column, nor `left join` a `filter_or_join` edge to `left`).
/// SORTED — `binary_search`ed per word; keep it that way (a test pins it).
pub(super) const SQL_WORDS: &[&str] = &[
    "abs",
    "all",
    "alter",
    "and",
    "any",
    "approx_count_distinct",
    "array",
    "array_agg",
    "as",
    "asc",
    "avg",
    "between",
    "bigint",
    "bigserial",
    "binary",
    "blob",
    "bool",
    "boolean",
    "by",
    "bytes",
    "case",
    "cast",
    "ceil",
    "ceiling",
    "char",
    "char_length",
    "character",
    "character_length",
    "check",
    "clob",
    "coalesce",
    "collate",
    "commit",
    "concat",
    "concat_ws",
    "constraint",
    "convert",
    "corr",
    "count",
    "covar_pop",
    "covar_samp",
    "create",
    "cross",
    "cume_dist",
    "curdate",
    "current",
    "current_date",
    "current_time",
    "current_timestamp",
    "date",
    "date_add",
    "date_diff",
    "date_part",
    "date_sub",
    "date_trunc",
    "dateadd",
    "datediff",
    "datetime",
    "datetime2",
    "day",
    "decade",
    "decimal",
    "decode",
    "default",
    "delete",
    "dense_rank",
    "desc",
    "describe",
    "distinct",
    "double",
    "dow",
    "doy",
    "drop",
    "else",
    "end",
    "epoch",
    "escape",
    "except",
    "exists",
    "exp",
    "explain",
    "extract",
    "false",
    "fetch",
    "first",
    "first_value",
    "flatten",
    "float",
    "float4",
    "float64",
    "float8",
    "floor",
    "following",
    "for",
    "foreign",
    "from",
    "full",
    "generate_series",
    "getdate",
    "grant",
    "greatest",
    "group",
    "group_concat",
    "having",
    "hour",
    "if",
    "iff",
    "ilike",
    "in",
    "index",
    "initcap",
    "inner",
    "insert",
    "instr",
    "int",
    "int2",
    "int4",
    "int64",
    "int8",
    "integer",
    "intersect",
    "interval",
    "into",
    "is",
    "isnull",
    "isodow",
    "isoyear",
    "join",
    "json",
    "jsonb",
    "lag",
    "last_value",
    "lateral",
    "lead",
    "least",
    "left",
    "len",
    "length",
    "like",
    "limit",
    "listagg",
    "ln",
    "locate",
    "log",
    "log10",
    "lower",
    "lpad",
    "ltrim",
    "map",
    "max",
    "md5",
    "median",
    "millennium",
    "min",
    "minus",
    "minute",
    "mod",
    "month",
    "natural",
    "nchar",
    "next",
    "not",
    "now",
    "nth_value",
    "ntile",
    "null",
    "nullif",
    "nulls",
    "number",
    "numeric",
    "nvarchar",
    "nvl",
    "nvl2",
    "object",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "percent_rank",
    "percentile_cont",
    "percentile_disc",
    "pivot",
    "position",
    "pow",
    "power",
    "preceding",
    "primary",
    "qualify",
    "quarter",
    "rand",
    "random",
    "range",
    "rank",
    "real",
    "recursive",
    "references",
    "regexp",
    "regexp_extract",
    "regexp_like",
    "regexp_replace",
    "repeat",
    "replace",
    "reverse",
    "revoke",
    "right",
    "rlike",
    "rollback",
    "round",
    "row_number",
    "rows",
    "rpad",
    "rtrim",
    "safe_cast",
    "sample",
    "second",
    "select",
    "serial",
    "set",
    "sha1",
    "sha2",
    "show",
    "sign",
    "similar",
    "smallint",
    "smallserial",
    "some",
    "split",
    "split_part",
    "sqrt",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "string",
    "string_agg",
    "strpos",
    "struct",
    "substring",
    "sum",
    "sysdate",
    "table",
    "tablesample",
    "text",
    "then",
    "time",
    "timestamp",
    "timestamp_ltz",
    "timestamp_ntz",
    "timestamp_tz",
    "timestamptz",
    "timetz",
    "tinyint",
    "to_char",
    "to_date",
    "to_number",
    "to_timestamp",
    "top",
    "transaction",
    "translate",
    "trim",
    "true",
    "trunc",
    "try_cast",
    "unbounded",
    "union",
    "unique",
    "unnest",
    "unpivot",
    "update",
    "upper",
    "uuid",
    "values",
    "var_pop",
    "var_samp",
    "varbinary",
    "varchar",
    "variance",
    "variant",
    "view",
    "week",
    "when",
    "where",
    "with",
    "xml",
    "year",
];

/// Is `w` (already-lowercased) an unambiguous SQL keyword / function name /
/// type name? See [`SQL_WORDS`] — binary search over the sorted list.
pub(super) fn is_sql_keyword(w: &str) -> bool {
    debug_assert!(SQL_WORDS.windows(2).all(|p| p[0] < p[1]));
    SQL_WORDS.binary_search(&w).is_ok()
}

/// All identifiers in `text`, normalized: unquoted words fold lowercase;
/// `"…"` / `` `…` `` keep their case (sans quotes). `[…]` bracket text is NOT
/// scanned — in dbt-targeted SQL it is overwhelmingly array indexing, not a
/// quoted identifier.
///
/// Two word classes are skipped as candidates: bare [`is_sql_keyword`] words
/// (a keyword can't honestly name-match a parent column — `cast(x as date)`
/// is a TYPE usage, not a `date` reference), and `[…]` regions. A keyword
/// spelling still counts when it is *marked as an identifier*: `.`-qualified
/// (`t.order` — keywords can't be qualified) or quoted (`"left"`). That keeps
/// the escape hatch for real columns that happen to share a keyword's name.
pub(super) fn idents(text: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < text.len() {
        let b = text[i];
        if b == b'"' || b == b'`' {
            let start = i;
            i += 1;
            while i < text.len() && text[i] != b && text[i] != b'\n' {
                i += 1;
            }
            if i < text.len() {
                i += 1; // consume the closing quote
                let tok = std::str::from_utf8(&text[start..i]).unwrap_or("");
                out.insert(normalize_column(tok));
            }
        } else if b == b'[' {
            // Array indexing, not a quoted identifier — skip the bracketed
            // region wholesale (nested brackets count depth).
            let mut depth = 1i32;
            i += 1;
            while i < text.len() && depth > 0 {
                match text[i] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < text.len() && is_ident_byte(text[i]) {
                i += 1;
            }
            let tok = String::from_utf8_lossy(&text[start..i]).to_lowercase();
            // A `.`-qualified segment is definitively a column reference —
            // the stoplist only prunes BARE words.
            if (start > 0 && text[start - 1] == b'.') || !is_sql_keyword(&tok) {
                out.insert(tok);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The last depth-0 (outside parens) position of `kw` in `b`, on word
/// boundaries. `x AS c` is the ONLY alias syntax v0 honours (a bare `expr c`
/// alias is too ambiguous to name-match safely); the LAST depth-0 `as` splits
/// `cast(x as int) as c` correctly (the inner `as` nests inside parens).
pub(super) fn last_top_keyword(b: &[u8], kw: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut found = None;
    for (i, &ch) in b.iter().enumerate() {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if depth == 0 && keyword_at(b, i, kw) => found = Some(i),
            _ => {}
        }
    }
    found
}

/// A whole-token identifier: `a`, `a.b.c`, `"Quoted"`, `` `quoted` `` —
/// returned as the normalized LAST segment (the column-name position).
pub(super) fn ident_last_segment(t: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(t).ok()?.trim();
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'`' && b[b.len() - 1] == b'`')
            || (b[0] == b'[' && b[b.len() - 1] == b']'))
    {
        return (!s[1..s.len() - 1].is_empty()).then(|| normalize_column(s));
    }
    let mut last = "";
    for seg in s.split('.') {
        let sb = seg.as_bytes();
        if sb.is_empty()
            || !(sb[0].is_ascii_alphabetic() || sb[0] == b'_')
            || !sb.iter().all(|&c| is_ident_byte(c))
        {
            return None;
        }
        last = seg;
    }
    Some(normalize_column(last))
}

/// Parse one select-list item (already masked): wildcards, `x AS c`, bare
/// identifiers, and named expressions. Unnamed non-identifier expressions
/// (`count(*)` with no alias) are not nameable output columns — skipped.
pub(super) fn parse_item(item: &[u8]) -> Option<SelectItem> {
    let t = trim_ascii(item);
    if t.is_empty() {
        return None;
    }
    if t == b"*" || (t.ends_with(b".*") && ident_last_segment(&t[..t.len() - 2]).is_some()) {
        return Some(SelectItem {
            alias: None,
            kind: ItemKind::Wildcard,
        });
    }
    // The `as` split scans the keyword-masked copy (same length, positions
    // aligned): an `as` inside a quoted identifier — `x as "a as b"` — can
    // never split the item, and parens inside quotes can't skew its depth.
    let kw = keyword_mask(t);
    if let Some(pos) = last_top_keyword(&kw, b"as") {
        if let Some(alias) = ident_last_segment(trim_ascii(&t[pos + 2..])) {
            let expr = trim_ascii(&t[..pos]);
            let kind = match ident_last_segment(expr) {
                Some(src) => ItemKind::Ident(src),
                None => ItemKind::Expr(idents(expr)),
            };
            return Some(SelectItem {
                alias: Some(alias),
                kind,
            });
        }
        // An `as` without a clean alias — fall through to whole-item tests.
    }
    if let Some(name) = ident_last_segment(t) {
        return Some(SelectItem {
            alias: Some(name.clone()),
            kind: ItemKind::Ident(name),
        });
    }
    None
}

/// Parse the masked SQL: the OUTERMOST select list (the model's output
/// columns) plus the identifier sets for the passthrough/fallback and
/// filter-or-join tests.
///
/// "Outermost" = the first `select` keyword at paren depth 0, which is the
/// model's own list — CTE bodies and subqueries nest inside parens and never
/// claim the top slot. The list runs to the first depth-0 `from`; the
/// filter/join region is everything after it up to a depth-0
/// `union`/`intersect`/`except` (a second UNION arm's columns would otherwise
/// leak false FilterOrJoin hits — v0 answers from the first arm only).
///
/// Every structural scan (clause keywords, set operators, depth, item
/// commas) runs on [`keyword_mask`]'s view — quoted-identifier interiors are
/// spaces there, so `"union"`/`t.from`/a comma inside `"a,b"` can't fake a
/// boundary. Identifier EXTRACTION still reads `masked`, so the quoted
/// identifier itself remains a name-match candidate.
pub(super) fn parse_select(masked: &[u8]) -> ParsedSql {
    let all_idents = idents(masked);
    let empty = || ParsedSql {
        items: Vec::new(),
        filter_idents: BTreeSet::new(),
        all_idents: all_idents.clone(),
    };
    let kw = keyword_mask(masked);
    // First depth-0 `select`.
    let mut depth = 0i32;
    let mut select_end = None;
    for (i, &ch) in kw.iter().enumerate() {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if depth == 0 && keyword_at(&kw, i, b"select") => {
                select_end = Some(i + 6);
                break;
            }
            _ => {}
        }
    }
    let Some(mut list_start) = select_end else {
        return empty();
    };
    // A leading DISTINCT / ALL keyword is list decoration, not part of an item.
    {
        let ws = kw[list_start..]
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
        let w = list_start + ws;
        for word in [b"distinct".as_slice(), b"all".as_slice()] {
            if keyword_at(&kw, w, word) {
                list_start = w + word.len();
                break;
            }
        }
    }
    // List end = first depth-0 `from`, else the first depth-0 set-operator,
    // else end of text.
    let mut depth = 0i32;
    let mut from_pos = None;
    let mut boundary = masked.len();
    for (i, &ch) in kw.iter().enumerate().skip(list_start) {
        match ch {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if depth == 0 && keyword_at(&kw, i, b"from") => {
                from_pos = Some(i);
                break;
            }
            _ if depth == 0 && is_set_operator(&kw, i) => {
                boundary = i;
                break;
            }
            _ => {}
        }
    }
    let list_end = from_pos.unwrap_or(boundary);
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut start = list_start;
    for i in list_start..list_end {
        match kw[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                if let Some(item) = parse_item(&masked[start..i]) {
                    items.push(item);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if let Some(item) = parse_item(&masked[start..list_end]) {
        items.push(item);
    }
    // The filter region: after `from`, up to the first depth-0 set-operator.
    let filter_idents = match from_pos {
        Some(from) => {
            let mut depth = 0i32;
            let mut end = masked.len();
            for (i, &ch) in kw.iter().enumerate().skip(from + 4) {
                match ch {
                    b'(' | b'[' => depth += 1,
                    b')' | b']' => depth -= 1,
                    _ if depth == 0 && is_set_operator(&kw, i) => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            idents(&masked[from..end])
        }
        None => BTreeSet::new(),
    };
    ParsedSql {
        items,
        filter_idents,
        all_idents,
    }
}
