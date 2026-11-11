use super::*;

// ---- SQL preview modal ----------------------------------------------------

/// Common SQL keywords, uppercased for case-insensitive recognition.
const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "ON",
    "USING",
    "GROUP",
    "ORDER",
    "BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "WITH",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IN",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "UNION",
    "ALL",
    "DISTINCT",
    "INSERT",
    "INTO",
    "UPDATE",
    "DELETE",
    "CREATE",
    "TABLE",
    "VIEW",
    "NULL",
    "IS",
    "BETWEEN",
    "LIKE",
    "EXISTS",
    "OVER",
    "PARTITION",
    "QUALIFY",
];

/// One token class in the SQL preview's syntax colouring. Per-CHAR (a parallel
/// `kinds` array rides alongside the chars, then groups into spans), so the
/// text bytes are preserved verbatim — the only change is ever colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlKind {
    Plain,
    Keyword,
    Str,
    Comment,
    Jinja,
}

impl SqlKind {
    /// The theme role for a token class (`Plain` = the default style).
    fn style(self, t: &theme::Theme) -> Style {
        match self {
            SqlKind::Plain => Style::default(),
            SqlKind::Keyword => Style::default().fg(t.sql_keyword),
            SqlKind::Str => Style::default().fg(t.sql_string),
            SqlKind::Comment => Style::default().fg(t.sql_comment),
            SqlKind::Jinja => Style::default().fg(t.sql_jinja),
        }
    }
}

/// Tokenizer state that survives a line break: inside a `/* */` block comment,
/// a `{{ }}`/`{% %}` Jinja region, or a `{# #}` Jinja comment. (Strings are
/// line-terminated on purpose — a runaway unclosed quote must not swallow the
/// rest of the file.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlState {
    Normal,
    BlockComment,
    /// Inside `{{ … }}` or `{% … %}`; the payload is the closer's FIRST char
    /// (`'}'` or `'%'`), so each opener only matches its own closer — `{{ a %}`
    /// stays Jinja through the `%}` instead of closing on the wrong delimiter.
    Jinja(char),
    JinjaComment,
}

/// Tokenize one line under the carried `state`, returning the per-char kinds
/// and the state the NEXT line starts in. Two-char openers/closers (`--`,
/// `/*`, `*/`, `{{`, `{%`, `{#`, `}}`, `%}`, `#}`) are matched with one char of
/// lookahead; keywords are recognized afterwards over maximal Plain word runs.
fn sql_line_kinds(chars: &[char], mut state: SqlState) -> (Vec<SqlKind>, SqlState) {
    let mut kinds: Vec<SqlKind> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let next = chars.get(i + 1).copied();
        match state {
            SqlState::BlockComment => {
                if chars[i] == '*' && next == Some('/') {
                    kinds.extend([SqlKind::Comment; 2]);
                    i += 2;
                    state = SqlState::Normal;
                } else {
                    kinds.push(SqlKind::Comment);
                    i += 1;
                }
            }
            SqlState::Jinja(closer) => {
                if chars[i] == closer && next == Some('}') {
                    kinds.extend([SqlKind::Jinja; 2]);
                    i += 2;
                    state = SqlState::Normal;
                } else {
                    kinds.push(SqlKind::Jinja);
                    i += 1;
                }
            }
            SqlState::JinjaComment => {
                if chars[i] == '#' && next == Some('}') {
                    kinds.extend([SqlKind::Comment; 2]);
                    i += 2;
                    state = SqlState::Normal;
                } else {
                    kinds.push(SqlKind::Comment);
                    i += 1;
                }
            }
            SqlState::Normal => match (chars[i], next) {
                ('-', Some('-')) => {
                    // Line comment: the rest of the line, state resets at EOL.
                    kinds.extend(std::iter::repeat_n(SqlKind::Comment, chars.len() - i));
                    i = chars.len();
                }
                ('/', Some('*')) => {
                    kinds.extend([SqlKind::Comment; 2]);
                    i += 2;
                    state = SqlState::BlockComment;
                }
                ('{', Some(open @ ('{' | '%'))) => {
                    kinds.extend([SqlKind::Jinja; 2]);
                    i += 2;
                    // `{{` closes on `}}`, `{%` on `%}` — each opener gets its
                    // own closer, never the other's.
                    state = SqlState::Jinja(if open == '{' { '}' } else { '%' });
                }
                ('{', Some('#')) => {
                    kinds.extend([SqlKind::Comment; 2]);
                    i += 2;
                    state = SqlState::JinjaComment;
                }
                ('\'', _) => {
                    // String literal: opening quote to the closing quote (or
                    // EOL when unterminated). A doubled '' reads as two
                    // adjacent strings — visually identical, semantically fine.
                    let close = chars[i + 1..].iter().position(|&c| c == '\'');
                    let end = match close {
                        Some(off) => i + 1 + off + 1, // one past the closing quote
                        None => chars.len(),
                    };
                    kinds.extend(std::iter::repeat_n(SqlKind::Str, end - i));
                    i = end;
                }
                _ => {
                    kinds.push(SqlKind::Plain);
                    i += 1;
                }
            },
        }
    }
    debug_assert_eq!(kinds.len(), chars.len());

    // Keyword pass: maximal word runs (`is_alphanumeric || '_'`) that stayed
    // Plain — so a keyword inside a string/comment/Jinja region never matches.
    let mut start = 0;
    while start < chars.len() {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if !(is_word(chars[start]) && kinds[start] == SqlKind::Plain) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < chars.len() && is_word(chars[end]) && kinds[end] == SqlKind::Plain {
            end += 1;
        }
        let word: String = chars[start..end].iter().collect();
        if SQL_KEYWORDS.contains(&word.to_ascii_uppercase().as_str()) {
            for k in &mut kinds[start..end] {
                *k = SqlKind::Keyword;
            }
        }
        start = end;
    }
    (kinds, state)
}

/// Group one line's `(char, kind)` pairs into styled spans. Adjacent same-kind
/// chars merge; the concatenated span text equals the input line verbatim.
fn sql_line_spans(chars: &[char], kinds: &[SqlKind], t: &theme::Theme) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_kind = SqlKind::Plain;
    for (i, &ch) in chars.iter().enumerate() {
        if kinds[i] != run_kind && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_kind.style(t)));
        }
        run_kind = kinds[i];
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_kind.style(t)));
    }
    Line::from(spans)
}

/// The SQL-preview content as syntax-coloured display lines: keywords, string
/// literals, comments (`--` / `/* */` spanning lines / Jinja `{# #}`), and
/// Jinja `{{ }}` / `{% %}` regions, all from [`theme`] roles. NO wrapping (it
/// would desync the line-count scroll clamp); long lines truncate at the
/// width. Text bytes are preserved verbatim — only colour ever changes.
/// Highlights the ACTIVE variant (`sv.active_sql()`): raw or compiled.
fn sql_display_lines(sv: &SqlView, t: &theme::Theme) -> Vec<Line<'static>> {
    let mut state = SqlState::Normal;
    sv.active_sql()
        .lines()
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            let (kinds, next) = sql_line_kinds(&chars, state);
            state = next;
            sql_line_spans(&chars, &kinds, t)
        })
        .collect()
}

/// Clamp the SQL modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_detail_scroll`]. Counts the active variant's `.lines()`
/// directly instead of building [`sql_display_lines`]: the highlighter maps
/// input lines 1:1 (never splits or injects — asserted by `sql_display_lines_
/// count_matches_input_lines`), and the clamp runs EVERY FRAME while the
/// modal is open, so it must not re-run the tokenizer.
pub fn clamp_sql_scroll(area: Rect, sv: &SqlView) -> usize {
    clamp_modal_scroll(area, sv.active_sql().lines().count(), sv.scroll)
}

/// Draw the SQL-preview modal. `pub(crate)` for `render::draw`. The SQL text
/// rides inside the [`SqlView`] payload (snapshotted at open), so the render
/// layer never needs a `Dag`. The title's `[raw]`/`[compiled]`/`[compiled,
/// stale]` tag names the displayed variant (`c` toggles when a compiled text
/// exists — `sv.variant_label()` is the single naming home).
pub(crate) fn draw_sql(
    frame: &mut Frame,
    area: Rect,
    sv: &SqlView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    // The path tells the user WHICH file `o` would open; absent for sources /
    // seeds and manifests that omit it.
    let path = sv
        .path
        .as_deref()
        .map(|p| format!(" - {p}"))
        .unwrap_or_default();
    // The `c` hint only appears when there IS a compiled variant to toggle to
    // — advertising it on a raw-only node would promise a dead key.
    let toggle = if sv.compiled.is_some() {
        ", c: raw/compiled"
    } else {
        ""
    };
    draw_scrollable_modal(
        frame,
        area,
        format!(
            " {}{path} [{}] (Esc/q to close, j/k to scroll{}) ",
            sv.name,
            sv.variant_label(),
            toggle
        ),
        sql_display_lines(sv, t),
        sv.scroll,
        glyphs,
        t,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Tokenize a multi-line SQL snippet into per-line `(text, kind)` span
    /// pairs, mirroring `sql_display_lines`'s state threading.
    fn kinds_of(sql: &str) -> Vec<Vec<(String, SqlKind)>> {
        let mut state = SqlState::Normal;
        sql.lines()
            .map(|line| {
                let chars: Vec<char> = line.chars().collect();
                let (kinds, next) = sql_line_kinds(&chars, state);
                state = next;
                // Group like sql_line_spans, but keep the kind for asserting.
                let mut out: Vec<(String, SqlKind)> = Vec::new();
                for (i, &ch) in chars.iter().enumerate() {
                    match out.last_mut() {
                        Some((run, k)) if *k == kinds[i] => run.push(ch),
                        _ => out.push((ch.to_string(), kinds[i])),
                    }
                }
                out
            })
            .collect()
    }

    /// The kind covering a given substring of a line (panics when the substring
    /// spans kinds — that IS the failure being asserted).
    fn kind_at(line: &[(String, SqlKind)], needle: &str) -> SqlKind {
        let joined: String = line.iter().map(|(s, _)| s.as_str()).collect();
        let start = joined.find(needle).expect("needle present");
        let mut pos = 0;
        for (run, k) in line {
            let end = pos + run.chars().count();
            if start >= pos && start + needle.chars().count() <= end {
                return *k;
            }
            pos = end;
        }
        panic!("needle {needle:?} spans kind boundaries in {joined:?}");
    }

    #[test]
    fn sql_tokenizer_classifies_keywords_strings_comments_and_jinja() {
        let sql = "select 'from' as x -- from here\nfrom {{ ref('stg_orders') }} tbl";
        let lines = kinds_of(sql);
        assert_eq!(kind_at(&lines[0], "select"), SqlKind::Keyword);
        assert_eq!(
            kind_at(&lines[0], "'from'"),
            SqlKind::Str,
            "a keyword inside a string is a string"
        );
        assert_eq!(kind_at(&lines[0], "as"), SqlKind::Keyword);
        assert_eq!(
            kind_at(&lines[0], "-- from here"),
            SqlKind::Comment,
            "line comment runs to EOL, keyword inside it never matches"
        );
        assert_eq!(kind_at(&lines[1], "from"), SqlKind::Keyword);
        assert_eq!(
            kind_at(&lines[1], "{{ ref('stg_orders') }}"),
            SqlKind::Jinja,
            "the whole Jinja expression is one region (string quoting inside included)"
        );
        assert_eq!(kind_at(&lines[1], "tbl"), SqlKind::Plain);
    }

    #[test]
    fn sql_tokenizer_carries_block_comments_and_jinja_across_lines() {
        let sql = "select 1 /* select\nstill comment */ from t\n{% if a %}\nx{# note\nmore #}y";
        let lines = kinds_of(sql);
        assert_eq!(kind_at(&lines[0], "/* select"), SqlKind::Comment);
        assert_eq!(
            kind_at(&lines[1], "still comment */"),
            SqlKind::Comment,
            "block comment state survives the line break"
        );
        assert_eq!(
            kind_at(&lines[1], "from"),
            SqlKind::Keyword,
            "tokenizing resumes after the closer"
        );
        assert_eq!(kind_at(&lines[2], "{% if a %}"), SqlKind::Jinja);
        // Distinct closers: `{{ a %}` must NOT close on `%}` — it stays Jinja
        // until a real `}}` arrives (here: never, so the rest of the line).
        let mixed = kinds_of("{{ a %} still jinja");
        assert_eq!(kind_at(&mixed[0], "still jinja"), SqlKind::Jinja);
        let block = kinds_of("{% set x %} plain");
        assert_eq!(kind_at(&block[0], "plain"), SqlKind::Plain);
        assert_eq!(kind_at(&lines[3], "{# note"), SqlKind::Comment);
        assert_eq!(kind_at(&lines[4], "more #}"), SqlKind::Comment);
        assert_eq!(kind_at(&lines[4], "y"), SqlKind::Plain);
    }

    #[test]
    fn sql_display_lines_count_matches_input_lines() {
        // The contract `clamp_sql_scroll` relies on: the highlighter maps input
        // lines 1:1, so counting `sql.lines()` (cheap, per-frame) always equals
        // the rendered line count (tokenized, per-open).
        let sql = "select 1 /* a
    b */

    -- c
    {{ ref('x') }}";
        let sv = SqlView {
            model_id: String::new(),
            name: String::new(),
            sql: sql.to_string(),
            compiled: None,
            compiled_stale: false,
            show_compiled: false,
            path: None,
            scroll: 0,
        };
        assert_eq!(
            sql_display_lines(&sv, &theme::DEFAULT).len(),
            sql.lines().count()
        );
    }

    #[test]
    fn sql_highlight_preserves_text_bytes_exactly() {
        // The spans concatenate back to the input line verbatim — colour only,
        // never text (the modal-content contract). Includes an unterminated
        // string (swallows to EOL, resets next line) and CJK user data.
        let sql = "select '日本語 unterminated\nselect 1";
        let mut state = SqlState::Normal;
        for line in sql.lines() {
            let chars: Vec<char> = line.chars().collect();
            let (kinds, next) = sql_line_kinds(&chars, state);
            let rendered = sql_line_spans(&chars, &kinds, &theme::DEFAULT);
            let joined: String = rendered.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(joined, line, "span text is byte-identical to the input");
            state = next;
        }
        let lines = kinds_of(sql);
        assert_eq!(
            kind_at(&lines[0], "'日本語 unterminated"),
            SqlKind::Str,
            "unterminated string swallows to EOL only"
        );
        assert_eq!(
            kind_at(&lines[1], "select"),
            SqlKind::Keyword,
            "string state does NOT leak across the line break"
        );
    }
}
