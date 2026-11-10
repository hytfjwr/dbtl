//! Modal overlays drawn on top of the base panes. Each is a plain function (no
//! trait/stack apparatus) selected by `render::draw` from the current `Mode`.
//! Here: the `?` help overlay and the structure (detail) modal. Search has no
//! modal — its query renders inline in the list / lineage pane titles.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::action::{
    palette_candidates, ColumnView, DetailView, DiffView, PaletteState, SqlView, StatsView,
};

use super::geom::centered_rect;
use super::{header_style, selected_style, theme};

// ---- shared scrollable-modal scaffolding ----------------------------------
//
// All four overlays (help / detail / sql / stats) ARE the same widget: a
// centered, bordered, scrollable box. Only their TITLE and their content
// (`*_display_lines`) differ — those stay separate. The Clear+Block+pagination
// +scrollbar scaffolding, the overlay rect, and the scroll clamp are shared
// here so a change to any of them lands in ONE place.

/// Every modal overlay's outer rect (80% × 80%, centered).
fn modal_rect(area: Rect) -> Rect {
    centered_rect(80, 80, area)
}

/// Clamp a modal's scroll offset to its content/viewport for `area` (size-aware;
/// the event loop calls this on the stored scroll, mirroring `clamp_lineage`).
/// Takes the line COUNT rather than a view type, so it is decoupled from which
/// overlay is open.
fn clamp_modal_scroll(area: Rect, line_count: usize, scroll: usize) -> usize {
    let inner_h = modal_rect(area).height.saturating_sub(2) as usize; // borders
    scroll.min(line_count.saturating_sub(inner_h))
}

/// Draw a centered, bordered, scrollable modal: `Clear` + accent-bold `Block` +
/// paginated `lines` (scrolled by `scroll`, clamped to the content) + the
/// right-border scrollbar thumb. The only per-overlay inputs are `title`, the
/// `lines`, and the `scroll`.
fn draw_scrollable_modal(
    frame: &mut Frame,
    area: Rect,
    title: impl Into<String>,
    lines: Vec<Line<'static>>,
    scroll: usize,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    let rect = modal_rect(area);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(super::chrome(glyphs).border)
        .border_style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD))
        .title(Line::styled(
            title.into(),
            Style::default()
                .fg(t.text_bright)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let total = lines.len();
    let view_h = inner.height as usize;
    let off = scroll.min(total.saturating_sub(view_h));
    let visible: Vec<Line> = lines.into_iter().skip(off).take(view_h).collect();
    frame.render_widget(Paragraph::new(visible), inner);
    super::stamp_scrollbar(frame, rect, total, off, view_h, glyphs, t);
}

/// The help overlay content as display lines, GENERATED from the keymap's
/// [`help_lines`](crate::action::help_lines) — the single source of truth — so
/// the `?` overlay can never drift from what the keys actually do. Grouped by
/// mode with section headers.
fn help_display_lines(t: &theme::Theme) -> Vec<Line<'static>> {
    use crate::action::{help_lines, ModeKind};
    let sections = [
        (ModeKind::Selection, "Normal"),
        (ModeKind::Search, "Search / filter"),
        (ModeKind::Detail, "Structure modal"),
        (ModeKind::Sql, "SQL preview"),
        (ModeKind::Stats, "Stats dashboard"),
        (ModeKind::Diff, "Diff summary"),
        (ModeKind::Columns, "Column lineage modal"),
        (ModeKind::Palette, "Command palette"),
        (ModeKind::Yank, "Yank (y + key)"),
        (ModeKind::Help, "Help"),
    ];
    let all = help_lines();
    let mut out: Vec<Line> = Vec::new();
    for (mk, title) in sections {
        out.push(Line::styled(format!("{title}:"), header_style(t)));
        for hl in all.iter().filter(|l| l.mode == mk) {
            // The key column in the accent, the description default — same text.
            out.push(Line::from(vec![
                Span::styled(format!("  {:<10} ", hl.keys), Style::default().fg(t.accent)),
                Span::raw(hl.desc.to_string()),
            ]));
        }
        out.push(Line::from(""));
    }
    out
}

/// Clamp a help-overlay scroll offset to the valid range for `area` (size-aware;
/// the event loop calls this on the stored scroll, mirroring `clamp_lineage`).
/// Counts with the DEFAULT theme — the line COUNT is theme-independent (a theme
/// only recolours), so the clamp never needs the active one.
pub fn clamp_help_scroll(area: Rect, scroll: usize) -> usize {
    clamp_modal_scroll(area, help_display_lines(&theme::DEFAULT).len(), scroll)
}

/// Draw the `?` help overlay: a centered, scrollable box listing every binding
/// (from the keymap). `scroll` is clamped here for display; the loop also clamps
/// the stored value so up/down stay responsive. `pub(crate)` for `render::draw`.
pub(crate) fn draw_help(
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        concat!(
            " Keybindings - dbtl v",
            env!("CARGO_PKG_VERSION"),
            "  (?/Esc/q to close, j/k to scroll) "
        ),
        help_display_lines(t),
        scroll,
        glyphs,
        t,
    );
}

// ---- structure (detail) modal --------------------------------------------

/// The structure modal's lines BEFORE the columns section: type / location /
/// tags / description, ending with the blank separator. The section split
/// exists so [`detail_columns_line0`] and the renderer agree on where the
/// cursor-addressable rows sit.
fn detail_head_lines(dv: &DetailView, t: &theme::Theme) -> Vec<Line<'static>> {
    let d = &dv.detail;
    let mut out: Vec<Line> = Vec::new();

    let kind = d
        .materialized
        .clone()
        .unwrap_or_else(|| "source".to_string());
    out.push(Line::styled(format!("type:   {kind}"), header_style(t)));
    let location = match (&d.database, &d.schema) {
        (Some(db), Some(sc)) => format!("{db}.{sc}"),
        (None, Some(sc)) => sc.clone(),
        _ => "-".to_string(),
    };
    out.push(Line::from(format!("location: {location}")));
    if let Some(pkg) = &d.package_name {
        out.push(Line::from(format!("package: {pkg}")));
    }
    // Governance (dbt access controls): shown only when declared.
    if let Some(access) = &d.access {
        out.push(Line::from(format!("access:  {access}")));
    }
    if let Some(group) = &d.group {
        out.push(Line::from(format!("group:   {group}")));
    }
    if d.contract_enforced {
        out.push(Line::from("contract: enforced".to_string()));
    }
    match (&d.version, &d.latest_version) {
        (Some(v), Some(latest)) if v != latest => {
            out.push(Line::from(format!("version: {v} (latest {latest})")));
        }
        (Some(v), _) => out.push(Line::from(format!("version: {v}"))),
        _ => {}
    }
    if let Some(p) = &d.original_file_path {
        out.push(Line::from(format!("path:   {p}")));
    }
    out.push(Line::from(format!(
        "impact: {} downstream / {} upstream",
        dv.downstream_count, dv.upstream_count
    )));
    // catalog.json warehouse truth (real types / rows / size / drift) —
    // pre-rendered lines snapshotted when the modal opened.
    out.extend(dv.catalog.iter().map(|l| Line::from(l.clone())));
    if !d.tags.is_empty() {
        out.push(Line::from(format!("tags:   {}", d.tags.join(", "))));
    }
    // The `meta` dict as a key-value block (`BTreeMap` → sorted keys); nested
    // values show their compact-JSON form (`deep: {"x":1}`). Key padding is
    // counted in CHARS (rust's format width is) so a multibyte key still
    // aligns; a `\n`/`\r` in a value would tear the one-row-per-entry layout
    // — flattened to spaces like the docs table's md_cell.
    if !d.meta.is_empty() {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("meta ({}):", d.meta.len()),
            header_style(t),
        ));
        let w = d.meta.keys().map(|k| k.chars().count()).max().unwrap_or(0);
        for (k, v) in &d.meta {
            // CRLF is ONE break (single space), then lone \n / \r.
            let v = v.replace("\r\n", " ").replace(['\n', '\r'], " ");
            out.push(Line::from(format!("  {k:<w$}: {v}")));
        }
    }
    if let Some(desc) = &d.description {
        out.push(Line::from(""));
        out.push(Line::styled("description:", header_style(t)));
        for line in desc.lines() {
            out.push(Line::from(format!("  {line}")));
        }
    }
    out.push(Line::from(""));
    out
}

/// The (name, type, description) rows the columns section renders: the
/// declared columns when the node documents them, else the resolver's
/// inferred `col_names` (so undocumented models show SOMETHING cursorable).
/// The returned bool marks the inferred fallback (the header flags it).
fn detail_column_rows(dv: &DetailView) -> (Vec<(String, String, String)>, bool) {
    let d = &dv.detail;
    if d.columns.is_empty() {
        (
            dv.col_names
                .iter()
                .map(|n| (n.clone(), String::new(), String::new()))
                .collect(),
            true,
        )
    } else {
        (
            d.columns
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        c.data_type.clone().unwrap_or_default(),
                        c.description.clone().unwrap_or_default(),
                    )
                })
                .collect(),
            false,
        )
    }
}

/// The fixed-width ASCII badge for a test kind — pure ASCII in BOTH glyph
/// modes per the Issue 11 spec (no ambiguous-width glyphs on this surface):
/// `NN` not_null, `U` unique, `R` relationships, `AV` accepted_values,
/// `AR` accepted_range, `*` anything else. `ends_with` catches
/// package-namespaced spellings (`dbt_utils.accepted_range`, a package's
/// `pkg.relationships`) the way `register_test`'s `to:` resolution does —
/// a namespaced relationships test is jumpable, so it must badge `R`, not
/// `*`. `unique_combination_of_columns` has NO badge: it's a model-level
/// test (`column_name` always `None`), so it can never appear on a column
/// row — it shows in the tests list and drives the PrimaryKey lens instead.
fn test_badge(kind: &str) -> &'static str {
    if kind == "not_null" {
        "NN"
    } else if kind == "unique" {
        "U"
    } else if kind.ends_with("relationships") {
        "R"
    } else if kind.ends_with("accepted_values") {
        "AV"
    } else if kind.ends_with("accepted_range") {
        "AR"
    } else {
        "*"
    }
}

/// A column's badge list: the distinct [`test_badge`]s of the tests attached
/// to it (normalized-name match — the same key space `col_counts` uses), in
/// the FIXED display order below so the row is deterministic.
fn column_badges<'a>(dv: &'a DetailView, column: &str) -> Vec<&'a str> {
    const ORDER: &[&str] = &["NN", "U", "R", "AV", "AR", "*"];
    let norm = crate::normalize_column(column);
    let mut seen = std::collections::BTreeSet::new();
    for test in &dv.tests {
        if test
            .column_name
            .as_deref()
            .is_some_and(|c| crate::normalize_column(c) == norm)
        {
            seen.insert(test_badge(&test.kind));
        }
    }
    ORDER.iter().copied().filter(|b| seen.contains(b)).collect()
}

/// The columns section: header + one row per [`detail_column_rows`] entry,
/// each with the h/l cursor marker (`> `, the shared `SELECTED_MARKER`), the
/// per-column test-kind badges (Issue 11), and the one-hop column-level
/// up/down edge counts in the chrome badge glyphs.
fn detail_column_lines(
    dv: &DetailView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) -> Vec<Line<'static>> {
    let chrome = super::chrome(glyphs);
    let (rows, inferred) = detail_column_rows(dv);
    // ASCII-safe header tags (the `ascii_guard` renders scan this surface in
    // GlyphMode::Ascii — no `·`).
    let mut header = format!("columns ({})", rows.len());
    if inferred && !rows.is_empty() {
        header.push_str(" [from SQL]");
    }
    if dv.stale {
        header.push_str(" [stale]");
    }
    if dv.name_match {
        header.push_str(" [name-match]");
    }
    header.push(':');
    let mut out = vec![Line::styled(header, header_style(t))];
    for (i, (name, ty, desc)) in rows.iter().enumerate() {
        // The counts look up the NORMALIZED name — the resolver's key space —
        // while the row prints the declared spelling.
        let counts = dv
            .col_counts
            .get(&crate::normalize_column(name))
            .map(|(u, d)| format!("{}{} {}{}", chrome.badge_up, u, chrome.badge_down, d))
            .unwrap_or_default();
        // The test-kind badge trail for THIS column (`[NN U R]`); untested
        // columns get nothing so the gap stays visible.
        let badges = column_badges(dv, name);
        let badge_str = if badges.is_empty() {
            String::new()
        } else {
            format!(" [{}]", badges.join(" "))
        };
        let selected = i == dv.col_index && dv.test_index == usize::MAX;
        let mark = if selected {
            super::SELECTED_MARKER
        } else {
            "  "
        };
        let text = format!("  {mark}{name:<26} {ty:<14}{badge_str} {desc} {counts}");
        out.push(if selected {
            Line::styled(text, selected_style(t))
        } else {
            Line::from(text)
        });
    }
    out
}

/// The tests section: header + one row per attached test — the plain
/// `kind(col)` for schema tests, a `-> target.col` jump hint for resolved
/// `relationships` targets, and the `in:N given:M expect:K` shape for unit
/// tests (Issue 11). The tests-section h/l cursor (`test_index`, owned via
/// `Tab`) lands only on resolved relationships targets — the rows Enter can
/// re-root to — so it renders the same `> ` marker the columns side does.
fn detail_test_lines(dv: &DetailView, t: &theme::Theme) -> Vec<Line<'static>> {
    let mut out = vec![Line::styled(
        format!("tests ({}):", dv.tests.len()),
        header_style(t),
    )];
    for (i, test) in dv.tests.iter().enumerate() {
        let col = test
            .column_name
            .as_deref()
            .map(|c| format!("({c})"))
            .unwrap_or_default();
        // The relationships target: the resolved display name when the node
        // survived pruning, else the raw uid (still informative); `->` is
        // pure ASCII in both glyph modes.
        let target = if test.ref_uid.is_some() {
            let name = test
                .ref_name
                .as_deref()
                .or(test.ref_uid.as_deref())
                .unwrap_or("?");
            let rcol = test
                .ref_column
                .as_deref()
                .map(|c| format!(".{c}"))
                .unwrap_or_default();
            format!(" -> {name}{rcol}")
        } else {
            String::new()
        };
        // The unit-test shape: the test's own name (a unit test's identity —
        // the `kind` alone is just `unit`), the input-table count, and the
        // inline row counts the manifest carries (a `fixture:`/CSV side
        // shows no number).
        let unit = test.unit.as_ref().map_or(String::new(), |u| {
            let mut s = format!(" {} in:{}", test.name, u.inputs);
            if let Some(g) = u.given_rows {
                s.push_str(&format!(" given:{g}"));
            }
            if let Some(e) = u.expect_rows {
                s.push_str(&format!(" expect:{e}"));
            }
            s
        });
        let selected = i == dv.test_index;
        let mark = if selected {
            super::SELECTED_MARKER
        } else {
            "  "
        };
        let text = format!("  {mark}{}{}{}{}", test.kind, col, target, unit);
        out.push(if selected {
            Line::styled(text, selected_style(t))
        } else {
            Line::from(text)
        });
    }
    out
}

/// The line index of the FIRST column row inside the modal body (head + the
/// `columns (N):` header) — the base the cursor's ensure-visible math needs.
/// Line-count only, so the theme/glyph args don't matter. `pub(crate)`:
/// consumed by `clamp_detail_scroll` and the renderer only — not part of the
/// crate's API surface.
pub(crate) fn detail_columns_line0(dv: &DetailView) -> usize {
    detail_head_lines(dv, &theme::DEFAULT).len() + 1
}

/// The line index of the FIRST test row inside the modal body (head + the
/// columns header + column rows + the blank separator + the `tests (N):`
/// header) — the same ensure-visible base for the tests-section cursor.
pub(crate) fn detail_tests_line0(dv: &DetailView) -> usize {
    detail_columns_line0(dv) + detail_column_rows(dv).0.len() + 2
}

/// The structure modal content as display lines: type / location / tags /
/// description / columns table / tests list. Built once and shared by the clamp
/// and the renderer so scroll bounds match the content.
fn detail_display_lines(
    dv: &DetailView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) -> Vec<Line<'static>> {
    let mut out = detail_head_lines(dv, t);
    out.extend(detail_column_lines(dv, glyphs, t));

    out.push(Line::from(""));
    out.extend(detail_test_lines(dv, t));

    out
}

/// Clamp the detail modal's scroll offset to its content/viewport (size-aware),
/// then ensure the ACTIVE section cursor's row is inside the window — the
/// minimal-pan follow the lineage cursor already uses. Counts with the
/// DEFAULT theme — the line count is theme- and glyph-independent.
pub fn clamp_detail_scroll(area: Rect, dv: &DetailView) -> usize {
    let total = detail_display_lines(dv, crate::GlyphMode::Ascii, &theme::DEFAULT).len();
    let view_h = modal_rect(area).height.saturating_sub(2) as usize;
    let mut scroll = clamp_modal_scroll(area, total, dv.scroll);
    // Ensure-visible for the h/l cursor: pan just enough to keep its row
    // inside [scroll, scroll + view_h) — never a re-center. The tests-section
    // cursor (Tab-owned) gets the same follow at its own line base.
    if dv.test_index != usize::MAX && dv.test_index < dv.tests.len() {
        let row = detail_tests_line0(dv) + dv.test_index;
        if row < scroll {
            scroll = row;
        } else if view_h > 0 && row >= scroll + view_h {
            scroll = row + 1 - view_h;
        }
    } else if dv.col_index != usize::MAX && dv.col_index < dv.col_names.len() {
        let row = detail_columns_line0(dv) + dv.col_index;
        if row < scroll {
            scroll = row;
        } else if view_h > 0 && row >= scroll + view_h {
            scroll = row + 1 - view_h;
        }
    }
    scroll.min(total.saturating_sub(view_h))
}

/// Draw the structure (detail) modal for the selected node. `pub(crate)` for
/// `render::draw`. Data comes from the [`DetailView`] payload (cloned at open),
/// so the render layer never needs a `Dag`.
pub(crate) fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    dv: &DetailView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    // The gesture hints follow the ACTIVE cursor section: the columns side
    // traces a column on Enter, the tests side jumps to the relationships
    // `to:` target. `Tab` is only advertised when the tests section HAS a
    // jumpable row — otherwise the key notices and stays on columns.
    let (sect_hint, tab_hint) = if dv.test_index != usize::MAX {
        (", h/l test, Enter/c jump", ", Tab cols")
    } else {
        let col_hint = if dv.col_index == usize::MAX {
            ""
        } else {
            ", h/l column, Enter/c trace"
        };
        let tab_hint = if dv.tests.iter().any(|t| t.ref_uid.is_some()) {
            ", Tab tests"
        } else {
            ""
        };
        (col_hint, tab_hint)
    };
    draw_scrollable_modal(
        frame,
        area,
        format!(
            " {}  (Esc/q to close, j/k scroll{sect_hint}{tab_hint}) ",
            dv.name
        ),
        detail_display_lines(dv, glyphs, t),
        dv.scroll,
        glyphs,
        t,
    );
}

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

// ---- stats dashboard modal ------------------------------------------------

/// Cap used for every worklist listing (orphans / violations); the overflow is
/// rolled into an explicit "+K more" line so the modal never floods.
const WORKLIST_CAP: usize = 10;

/// A bg-space mini-bar `Span`: `n` spaces styled with `color` as the background,
/// width-proportional to `value`/`max` over `width` cells (at least 1 cell when
/// `value > 0`). Pure spaces → ASCII in both glyph modes (ascii_guard-safe by
/// construction; no new glyph). `max == 0` yields an empty span.
fn mini_bar(value: usize, max: usize, width: usize, color: Color) -> Span<'static> {
    if max == 0 || value == 0 {
        return Span::raw(String::new());
    }
    let filled = ((value * width) / max).max(1).min(width);
    Span::styled(" ".repeat(filled), Style::default().bg(color))
}

/// The coverage gauge as a two-span run: a graded-colour FILL of spaces over a
/// dark track of spaces (the theme's `surface_hi`), `width` cells total, fill
/// width-proportional to `pct` (integer math). Grade: danger <40, warn <70,
/// ok ≥70. Spaces only — ASCII in both modes.
fn coverage_gauge(pct: usize, width: usize, t: &theme::Theme) -> Vec<Span<'static>> {
    let grade = if pct < 40 {
        t.danger
    } else if pct < 70 {
        t.warn
    } else {
        t.ok
    };
    let filled = (pct * width / 100).min(width);
    let empty = width - filled;
    let mut spans = Vec::new();
    if filled > 0 {
        spans.push(Span::styled(" ".repeat(filled), Style::default().bg(grade)));
    }
    if empty > 0 {
        spans.push(Span::styled(
            " ".repeat(empty),
            Style::default().bg(t.surface_hi),
        ));
    }
    spans
}

/// The stats-dashboard content as display lines, grouped into sections via
/// [`header_style`]. Visual bars are bg-styled SPACES (a gauge for coverage,
/// proportional mini-bars after counts) — never block / shade glyphs, which are
/// East-Asian-Ambiguous and would fail ascii_guard in BOTH modes. Percentages
/// are integer math (`StatsView` is `Eq`, no f64). This is the SINGLE source of
/// the modal's line list, so [`clamp_stats_scroll`] stays correct automatically.
fn stats_display_lines(sv: &StatsView, t: &theme::Theme) -> Vec<Line<'static>> {
    const BAR_W: usize = 16;
    let mut out: Vec<Line> = Vec::new();
    out.push(Line::from(format!("project: {}", sv.project)));

    out.push(Line::from(""));
    out.push(Line::styled("Resource counts:", header_style(t)));
    let rt_max = sv
        .by_resource_type
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (rt, count) in &sv.by_resource_type {
        out.push(Line::from(vec![
            Span::raw(format!("  {rt:<12} {count:<4} ")),
            mini_bar(*count, rt_max, BAR_W, t.bar_resource),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled("Materialization (models):", header_style(t)));
    let mat_max = sv
        .by_materialization
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (mat, count) in &sv.by_materialization {
        out.push(Line::from(vec![
            Span::raw(format!("  {mat:<12} {count:<4} ")),
            mini_bar(*count, mat_max, BAR_W, t.bar_materialization),
        ]));
    }

    out.push(Line::from(""));
    // Same coverage_gap base as the `t` lens and the status `cov` segment.
    out.push(Line::styled(
        "Test coverage (models / seeds / snapshots):",
        header_style(t),
    ));
    let pct = (sv.testable_tested * 100)
        .checked_div(sv.testable_total)
        .unwrap_or(0);
    let mut cov_spans = vec![Span::raw("  ")];
    cov_spans.extend(coverage_gauge(pct, 20, t));
    cov_spans.push(Span::raw(format!(
        " {}/{} tested ({pct}%)",
        sv.testable_tested, sv.testable_total
    )));
    out.push(Line::from(cov_spans));

    // Column-level coverage (Issue 11): the SAME `column_coverage_counts`
    // numbers `dbtl docs`' README quotes — declared columns carrying ≥1 test.
    out.push(Line::styled(
        "Column coverage (declared columns):",
        header_style(t),
    ));
    let cpct = (sv.col_tested * 100)
        .checked_div(sv.col_declared)
        .unwrap_or(0);
    let mut col_spans = vec![Span::raw("  ")];
    col_spans.extend(coverage_gauge(cpct, 20, t));
    col_spans.push(Span::raw(format!(
        " {}/{} columns tested ({cpct}%)",
        sv.col_tested, sv.col_declared
    )));
    out.push(Line::from(col_spans));

    out.push(Line::from(""));
    out.push(Line::styled("Top hubs (degree):", header_style(t)));
    let deg_max = sv.top_hubs.iter().map(|(_, _, d)| *d).max().unwrap_or(0);
    for (_, name, degree) in &sv.top_hubs {
        out.push(Line::from(vec![
            Span::raw(format!("  {name:<28} {degree:<4} ")),
            mini_bar(*degree, deg_max, BAR_W, t.bar_degree),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        "Hubs (transitive downstream):",
        header_style(t),
    ));
    let tr_max = sv
        .transitive_hubs
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (name, count) in &sv.transitive_hubs {
        out.push(Line::from(vec![
            Span::raw(format!("  {name:<28} {count:<4} ")),
            mini_bar(*count, tr_max, BAR_W, t.bar_transitive),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Critical path (depth {}):", sv.critical_path.len()),
        header_style(t),
    ));
    for (i, name) in sv.critical_path.iter().take(WORKLIST_CAP).enumerate() {
        // Indent each hop one step further: the staircase reads as the chain
        // without any new glyph (pure spaces — ascii_guard-safe).
        out.push(Line::from(format!("  {}{name}", " ".repeat(i))));
    }
    if sv.critical_path.len() > WORKLIST_CAP {
        out.push(Line::from(format!(
            "  +{} more",
            sv.critical_path.len() - WORKLIST_CAP
        )));
    }

    // The runtime-weighted sibling: which chain costs wall-clock time, not
    // just depth. Only present when run_results.json loaded.
    if let Some((path, total)) = &sv.critical_path_runtime {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("Critical path by runtime ({total}):"),
            header_style(t),
        ));
        for (i, name) in path.iter().take(WORKLIST_CAP).enumerate() {
            out.push(Line::from(format!("  {}{name}", " ".repeat(i))));
        }
        if path.len() > WORKLIST_CAP {
            out.push(Line::from(format!("  +{} more", path.len() - WORKLIST_CAP)));
        }
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Orphans ({}):", sv.orphan_models.len()),
        header_style(t),
    ));
    for name in sv.orphan_models.iter().take(WORKLIST_CAP) {
        out.push(Line::from(format!("  {name}")));
    }
    if sv.orphan_models.len() > WORKLIST_CAP {
        out.push(Line::from(format!(
            "  +{} more",
            sv.orphan_models.len() - WORKLIST_CAP
        )));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Layer violations ({}):", sv.layer_violations.len()),
        header_style(t),
    ));
    for (parent, child) in sv.layer_violations.iter().take(WORKLIST_CAP) {
        out.push(Line::from(format!("  {parent} -> {child}")));
    }
    if sv.layer_violations.len() > WORKLIST_CAP {
        out.push(Line::from(format!(
            "  +{} more",
            sv.layer_violations.len() - WORKLIST_CAP
        )));
    }

    // The static-lint worklist: per-rule node names from the same `run_rules`
    // findings `dbtl check --lint` gates on — so the dashboard can never
    // disagree with the CI gate. Each rule's names cap like the other lists.
    let lint_total: usize = sv.lint_findings.iter().map(|(_, names)| names.len()).sum();
    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Lint findings ({lint_total}):"),
        header_style(t),
    ));
    if lint_total == 0 {
        out.push(Line::from("  none"));
    }
    for (rule, names) in &sv.lint_findings {
        out.push(Line::from(format!("  {rule} ({}):", names.len())));
        for name in names.iter().take(WORKLIST_CAP) {
            out.push(Line::from(format!("    {name}")));
        }
        if names.len() > WORKLIST_CAP {
            out.push(Line::from(format!(
                "    +{} more",
                names.len() - WORKLIST_CAP
            )));
        }
    }

    out.push(Line::from(""));
    out.push(Line::styled("Warnings:", header_style(t)));
    out.push(Line::from(format!(
        "  untested (model/seed/snapshot): {}",
        sv.untested_testable
    )));
    out.push(Line::from(format!(
        "  models with no downstream:      {}",
        sv.zero_downstream_models
    )));
    out.push(Line::from(format!(
        "  models with no description:     {}",
        sv.no_description_models
    )));
    // Same number `dbtl columns` reports on stderr; the detail modal flags the
    // individual models with `[name-match]`.
    out.push(Line::from(format!(
        "  models resolved by name-match:  {}",
        sv.cll_fallbacks
    )));

    out
}

/// Clamp the stats modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_detail_scroll`]. Counts with the DEFAULT theme — the line
/// count is theme-independent.
pub fn clamp_stats_scroll(area: Rect, sv: &StatsView) -> usize {
    clamp_modal_scroll(
        area,
        stats_display_lines(sv, &theme::DEFAULT).len(),
        sv.scroll,
    )
}

/// Draw the stats-dashboard modal. `pub(crate)` for `render::draw`. The stats
/// ride inside the [`StatsView`] payload (computed at open), so the render layer
/// never needs a `Dag`.
pub(crate) fn draw_stats(
    frame: &mut Frame,
    area: Rect,
    sv: &StatsView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        format!(" Stats - {}  (Esc/q to close, j/k to scroll) ", sv.project),
        stats_display_lines(sv, t),
        sv.scroll,
        glyphs,
        t,
    );
}

// ---- baseline-diff summary modal -------------------------------------------

/// The diff modal's content as display lines: one section per change kind
/// (added / modified / removed nodes, then edge changes), each header counting
/// its list. UNCAPPED on purpose — unlike the stats worklists (top-N teasers
/// over data visible elsewhere), this modal is the only COMPLETE listing
/// surface (removed nodes exist nowhere else in the app), and it scrolls.
/// Line marks are plain ASCII (`+` / `~` / `-` / `->`), coloured by the diff
/// roles so the modal agrees with the Diff lens tints. The SINGLE source of
/// the modal's line list, so [`clamp_diff_scroll`] stays correct automatically.
fn diff_display_lines(dv: &DiffView, t: &theme::Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    out.push(Line::from(format!("baseline: {}", dv.baseline)));
    // Same `- scope:` line the export and `dbtl impact` print: the numbers
    // below describe the ROOT project under either scope, and the modal must
    // say so or a screenshot reads differently from the pasted report.
    out.push(Line::from(format!("scope: {}", dv.scope.describe())));

    let total = dv.added.len()
        + dv.modified.len()
        + dv.removed.len()
        + dv.edges_added.len()
        + dv.edges_removed.len();
    if total == 0 {
        out.push(Line::from(""));
        out.push(Line::from("no changes vs the baseline"));
        return out;
    }

    // One section: a counting header + every row, pre-marked and tinted.
    // Shared so the five sections can't drift in shape.
    let section = |title: &str, rows: Vec<String>, color: Color, out: &mut Vec<Line>| {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("{title} ({}):", rows.len()),
            header_style(t),
        ));
        for row in rows {
            out.push(Line::styled(row, Style::default().fg(color)));
        }
    };

    let node_row = |mark: char, name: &str, kind: &str| {
        if kind.is_empty() {
            format!("  {mark} {name}")
        } else {
            format!("  {mark} {name} ({kind})")
        }
    };
    section(
        "Added",
        dv.added.iter().map(|(n, k)| node_row('+', n, k)).collect(),
        t.diff_add,
        &mut out,
    );
    section(
        "Modified",
        dv.modified
            .iter()
            .map(|(n, reasons)| format!("  ~ {n}: {reasons}"))
            .collect(),
        t.diff_mod,
        &mut out,
    );
    section(
        "Removed",
        dv.removed
            .iter()
            .map(|(n, k)| node_row('-', n, k))
            .collect(),
        t.danger,
        &mut out,
    );
    section(
        "Edges added",
        dv.edges_added
            .iter()
            .map(|(p, c)| format!("  + {p} -> {c}"))
            .collect(),
        t.diff_add,
        &mut out,
    );
    section(
        "Edges removed",
        dv.edges_removed
            .iter()
            .map(|(p, c)| format!("  - {p} -> {c}"))
            .collect(),
        t.danger,
        &mut out,
    );

    // ---- Column changes: the CLL refinement of the diff -------------------
    // One tinted head row per classified change plus dim detail rows for the
    // downstream columns (the "via" list) and exposures it reaches — the
    // same data the `e` export / `dbtl impact` "Column changes" section emits.
    if !dv.column_changes.is_empty() {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("Column changes ({}):", dv.column_changes.len()),
            header_style(t),
        ));
        for ch in &dv.column_changes {
            let (mark, color) = match ch.class {
                crate::ColumnChangeClass::Breaking => ('x', t.danger),
                crate::ColumnChangeClass::Partial => ('~', t.warn),
                crate::ColumnChangeClass::NonBreaking => ('+', t.diff_add),
            };
            let name = match &ch.renamed_from {
                Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                None => format!("{}.{}", ch.node_name, ch.column),
            };
            let what = match ch.kind {
                crate::ColumnChangeKind::Removed => "removed".to_string(),
                crate::ColumnChangeKind::Renamed => "renamed".to_string(),
                crate::ColumnChangeKind::TypeChanged => format!(
                    "type {} -> {}",
                    ch.type_from.as_deref().unwrap_or("?"),
                    ch.type_to.as_deref().unwrap_or("?")
                ),
                crate::ColumnChangeKind::ExprChanged => "expression changed".to_string(),
                crate::ColumnChangeKind::Added => "added".to_string(),
            };
            out.push(Line::styled(
                format!("  {mark} {name}: {what} [{}]", ch.class.label()),
                Style::default().fg(color),
            ));
            if !ch.affected_columns.is_empty() {
                out.push(Line::styled(
                    format!("      cols: {}", ch.affected_columns.join(", ")),
                    Style::default().fg(t.text_dim),
                ));
            }
            if !ch.affected_exposures.is_empty() {
                out.push(Line::styled(
                    format!("      exposures: {}", ch.affected_exposures.join(", ")),
                    Style::default().fg(t.text_dim),
                ));
            }
        }
    }

    // ---- PR impact pack: blast radius / exposures / CI command / risks ----
    // Appended after the diff sections so the modal IS the reviewer's report.
    // Reached only when there ARE changes (the no-change case returned above).
    let pr = &dv.pr;
    out.push(Line::from(""));
    out.push(Line::styled("PR impact pack", header_style(t)));
    out.push(Line::from(format!(
        "  affected models: {}",
        pr.affected_models
    )));
    if pr.column_affected_models > 0 {
        out.push(Line::from(format!(
            "  column-affected models: {}",
            pr.column_affected_models
        )));
    }
    if !pr.affected_marts.is_empty() {
        out.push(Line::from(format!(
            "  affected marts: {}",
            pr.affected_marts.join(", ")
        )));
    }
    // The "who to page" line — the meta[owner_key] values of the affected
    // downstream models (same set the export's Affected owners section lists).
    if !pr.affected_owners.is_empty() {
        out.push(Line::from(format!(
            "  affected owners: {}",
            pr.affected_owners.join(", ")
        )));
    }
    match &pr.ci_command {
        Some(cmd) => out.push(Line::styled(
            format!("  CI: {cmd}"),
            Style::default().fg(t.accent),
        )),
        None => out.push(Line::from("  CI: no buildable changes")),
    }
    // The column-narrowed sibling of `CI:` — the changed nodes plus exactly
    // the models their changed columns reach (no `+` fan-out).
    if let Some(cmd) = &pr.ci_command_columns {
        out.push(Line::styled(
            format!("  CI columns: {cmd}"),
            Style::default().fg(t.accent),
        ));
    }

    if !pr.affected_exposures.is_empty() {
        section(
            "Affected exposures",
            pr.affected_exposures
                .iter()
                .map(|e| format!("  {e}"))
                .collect(),
            t.accent,
            &mut out,
        );
    }

    if !pr.untested_changes.is_empty()
        || !pr.changed_hubs.is_empty()
        || !pr.new_layer_violations.is_empty()
    {
        out.push(Line::from(""));
        out.push(Line::styled("Risk flags", header_style(t)));
        for name in &pr.untested_changes {
            out.push(Line::styled(
                format!("  untested change: {name}"),
                Style::default().fg(t.warn),
            ));
        }
        for (name, count) in &pr.changed_hubs {
            out.push(Line::styled(
                format!("  hub change: {name} ({count} downstream)"),
                Style::default().fg(t.warn),
            ));
        }
        for (p, c) in &pr.new_layer_violations {
            out.push(Line::styled(
                format!("  new layer violation: {p} -> {c}"),
                Style::default().fg(t.danger),
            ));
        }
    }
    out
}

/// Clamp the diff modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_stats_scroll`]. Counts with the DEFAULT theme — the line
/// count is theme-independent.
pub fn clamp_diff_scroll(area: Rect, dv: &DiffView) -> usize {
    clamp_modal_scroll(
        area,
        diff_display_lines(dv, &theme::DEFAULT).len(),
        dv.scroll,
    )
}

/// Draw the baseline-diff summary modal. `pub(crate)` for `render::draw`. The
/// diff rides inside the [`DiffView`] payload (snapshotted at open), so the
/// render layer never needs a `Dag`.
pub(crate) fn draw_diff(
    frame: &mut Frame,
    area: Rect,
    dv: &DiffView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        " Diff vs baseline - PR impact  (e: export, y: copy CI cmd, Esc/q: close) ",
        diff_display_lines(dv, t),
        dv.scroll,
        glyphs,
        t,
    );
}

// ---- column-lineage modal (Issue 09) ---------------------------------------

/// The columns modal's content as display lines: the [`ColumnView`] rows,
/// each styled by its flags — the cursor row reversed ([`selected_style`]),
/// the traced column accented, inferred rows dim, and section headers in the
/// shared header style. The SINGLE source of the modal's line list, so
/// [`clamp_columns_scroll`] stays correct automatically.
fn columns_display_lines(cv: &ColumnView, t: &theme::Theme) -> Vec<Line<'static>> {
    cv.rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let style = if i == cv.cursor {
                selected_style(t)
            } else if r.traced {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else if r.faint {
                Style::default().fg(t.text_dim)
            } else if r.uid.is_none() {
                header_style(t)
            } else {
                Style::default()
            };
            Line::styled(r.label.clone(), style)
        })
        .collect()
}

/// Clamp the columns modal's scroll offset to its content/viewport AND keep
/// the row cursor inside the viewport (the ensure-visible follow for j/k —
/// the cursor is a selection, not the pan position). `inner_h == 0` (a
/// 2-row-tall modal rect) degenerates safely to a fixed scroll of 0.
pub fn clamp_columns_scroll(area: Rect, cv: &ColumnView) -> usize {
    let inner_h = modal_rect(area).height.saturating_sub(2) as usize;
    let max_scroll = cv.rows.len().saturating_sub(inner_h);
    let mut scroll = cv.scroll.min(max_scroll);
    if cv.cursor != usize::MAX {
        if cv.cursor < scroll {
            scroll = cv.cursor;
        } else if inner_h > 0 && cv.cursor >= scroll + inner_h {
            scroll = cv.cursor.saturating_add(1).saturating_sub(inner_h);
        }
    }
    scroll.min(max_scroll)
}

/// Draw the column-lineage modal. `pub(crate)` for `render::draw`. The tree
/// rides inside the [`ColumnView`] payload (snapshotted at open), so the
/// render layer never needs a `Dag`.
pub(crate) fn draw_columns(
    frame: &mut Frame,
    area: Rect,
    cv: &ColumnView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        format!(
            " Column lineage - {}  (j/k rows, Enter jump, Esc/q close) ",
            cv.title
        ),
        columns_display_lines(cv, t),
        cv.scroll,
        glyphs,
        t,
    );
}

// ---- command palette ------------------------------------------------------

/// Draw the command palette: a centered fuzzy-finder over every Selection-mode
/// command. `pub(crate)` for `render::draw`. The candidate list is derived from
/// the keymap ([`palette_candidates`]) at draw time — keyed off `BINDINGS`, so
/// the palette can never drift from the keys. No `Dag` is consulted (the keymap
/// is static), so `RenderCtx` stays Dag-free.
///
/// Layout: a query line (with the [`Chrome`](super::Chrome) caret) on the first
/// interior row, then one row per candidate below. The selected row is reversed
/// via [`selected_style`]; the matched query chars are highlighted Yellow+bold
/// (via [`match_indices`](crate::model_list::match_indices)); the key label is
/// right-aligned and dim. The scroll window is DERIVED from `selected` (no stored
/// scroll, so nothing for the loop to clamp), and the right-border scrollbar thumb
/// is stamped on overflow. Chrome glyphs only — ASCII-safe in both modes.
pub(crate) fn draw_palette(
    frame: &mut Frame,
    area: Rect,
    state: &PaletteState,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    // ~60% × ~60%, min-clamped so the box never collapses on a tiny terminal.
    let rect = centered_rect(60, 60, area).intersection(area);
    let chrome = super::chrome(glyphs);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome.border)
        .border_style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD))
        .title(Line::styled(
            " commands  (Esc to close, Enter to run) ",
            Style::default()
                .fg(t.text_bright)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The query line (row 0 of the interior), with the caret after the text.
    let query_line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.query.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(chrome.caret.to_string(), Style::default().fg(t.accent)),
    ]);
    let query_rect = Rect { height: 1, ..inner };
    frame.render_widget(Paragraph::new(query_line), query_rect);

    // The candidate list fills the rows below the query line.
    let list_rect = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let view_h = list_rect.height as usize;
    let candidates = palette_candidates(&state.query);
    let total = candidates.len();
    if view_h == 0 {
        return;
    }
    // Derive the scroll window from `selected`: keep it visible, never past the end.
    let off = state
        .selected
        .saturating_sub(view_h - 1)
        .min(total.saturating_sub(view_h));

    let label_w = list_rect.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(view_h);
    for (i, b) in candidates.iter().enumerate().skip(off).take(view_h) {
        let selected = i == state.selected;
        let base = if selected {
            selected_style(t)
        } else {
            Style::default()
        };
        // The help text, with matched query chars highlighted (yellow+bold). The
        // highlight wins over the base except for the reversed selected row, which
        // keeps the reverse (selection cue beats the match cue).
        let hits = crate::model_list::match_indices(b.help, &state.query);
        let hi = Style::default().fg(t.gold).add_modifier(Modifier::BOLD);
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled("  ", base));
        for (ci, ch) in b.help.chars().enumerate() {
            let st = if hits.contains(&ci) && !selected {
                base.patch(hi)
            } else {
                base
            };
            spans.push(Span::styled(ch.to_string(), st));
        }
        // Right-align the dim key label within the row width (best-effort: pad
        // between help and key when there is room; ratatui clips an overlong row).
        let key = b.key_label();
        let used = 2 + b.help.chars().count() + key.chars().count();
        if label_w > used {
            spans.push(Span::styled(" ".repeat(label_w - used), base));
        } else {
            spans.push(Span::styled(" ", base));
        }
        let key_base = if selected {
            base
        } else {
            Style::default().fg(t.text_dim)
        };
        // A row can be admitted by a KEY-LABEL match (e.g. "tab" matches the "Tab"
        // label, not the help text — see `palette_candidates`). When the help has
        // NO hits but the query is non-empty, highlight the matched key-label chars
        // so that row's match cue is visible too. Match the EXACT `key_label()`
        // string `palette_candidates` matched (the "/"-joined form). The selected
        // (reversed) row keeps its reverse, like the help highlight above.
        let key_hits = if hits.is_empty() && !state.query.trim().is_empty() {
            crate::model_list::match_indices(&key, &state.query)
        } else {
            Vec::new()
        };
        if key_hits.is_empty() {
            spans.push(Span::styled(key, key_base));
        } else {
            for (ci, ch) in key.chars().enumerate() {
                let st = if key_hits.contains(&ci) && !selected {
                    key_base.patch(hi)
                } else {
                    key_base
                };
                spans.push(Span::styled(ch.to_string(), st));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), list_rect);

    // Right-border scrollbar thumb on overflow (candidate-list units).
    super::stamp_scrollbar(frame, rect, total, off, view_h, glyphs, t);
}

/// Draw the `y` yank-prefix which-key menu: a compact box anchored bottom-center
/// listing each `y<key>` binding. Rows are GENERATED from the `M::Yank` keymap
/// entries (same source of truth as dispatch + `?` help), so the menu can't
/// drift from what the keys actually do. The second-key label is prefixed with
/// the `y` the user already pressed.
pub(crate) fn draw_yank_menu(
    frame: &mut Frame,
    area: Rect,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    use crate::action::{ModeKind, BINDINGS};
    let rows: Vec<Line> = BINDINGS
        .iter()
        .filter(|b| b.mode == ModeKind::Yank)
        .map(|b| {
            Line::from(vec![
                Span::styled(
                    format!("  y{:<3}", b.key_label()),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {}", b.help)),
            ])
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    // Size to content: borders + rows, width = longest row + padding, centered
    // horizontally and hugging the bottom edge (which-key convention).
    let width = rows
        .iter()
        .map(|l| l.width() as u16)
        .max()
        .unwrap_or(10)
        .saturating_add(4)
        .min(area.width.saturating_sub(2));
    let height = (rows.len() as u16 + 2).min(area.height);
    if width < 8 || height < 4 {
        return;
    }
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    }
    .intersection(area);
    let chrome = super::chrome(glyphs);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome.border)
        .border_style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD))
        .title(Line::styled(
            " yank + key ",
            Style::default()
                .fg(t.text_bright)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(rows), inner);
}

/// Draw the transient toast: a one-line bordered box floating at the TOP-RIGHT
/// of the screen, acknowledging the latest copy / bookmark / export / reload.
/// `render::draw` calls it LAST, so it layers above panes AND modals. The
/// renderer is stateless — the event loop owns the toast's ~2.5s lifetime and
/// simply stops passing the text when it expires.
///
/// Chrome-correct (the glyph-mode border set) and ASCII-safe by construction:
/// overflow truncation appends a plain `..`, never an ellipsis glyph. Skipped
/// entirely on a terminal too small to float a legible box.
pub(crate) fn draw_toast(
    frame: &mut Frame,
    area: Rect,
    text: &str,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    // Borders (2) + one-space padding each side (2) + a 2-col right inset.
    const FRAME_W: usize = 4;
    const INSET: u16 = 2;
    // Height 5 minimum: the 3-row box sits at y+1, so anything shorter would
    // land its bottom border on the status bar row (whose protected core is
    // never overwritten — see `draw_status`).
    if area.height < 5 || (area.width as usize) < FRAME_W + INSET as usize + 4 {
        return;
    }
    let budget = area.width as usize - FRAME_W - INSET as usize;
    let (text, text_w) = if text.width() <= budget {
        (text.to_string(), text.width())
    } else {
        // Truncate by DISPLAY width (the text may carry wide CJK model names).
        let cut = budget.saturating_sub(2); // room for the ".." marker
        let mut kept = String::new();
        let mut used = 0;
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w > cut {
                break;
            }
            used += w;
            kept.push(ch);
        }
        kept.push_str("..");
        (kept, used + 2)
    };

    let box_w = (text_w + FRAME_W) as u16;
    let rect = Rect::new(area.x + area.width - box_w - INSET, area.y + 1, box_w, 3);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(super::chrome(glyphs).border)
        .border_style(Style::default().fg(t.accent).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(t.surface));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(" {text} "),
            Style::default()
                .fg(t.text_bright)
                .add_modifier(Modifier::BOLD),
        )),
        inner,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_meta_block_is_sorted_and_key_aligned() {
        use std::collections::BTreeMap;
        let dv = DetailView {
            model_id: "model.p.m".into(),
            name: "m".into(),
            detail: crate::NodeDetail {
                meta: BTreeMap::from_iter([
                    ("zone".to_string(), "z1".to_string()),
                    ("owner".to_string(), "alice".to_string()),
                    ("deep".to_string(), r#"{"x":1}"#.to_string()),
                ]),
                ..Default::default()
            },
            tests: vec![],
            downstream_count: 0,
            upstream_count: 0,
            catalog: vec![],
            col_names: vec![],
            col_index: usize::MAX,
            test_index: usize::MAX,
            col_counts: Default::default(),
            stale: false,
            name_match: false,
            scroll: 0,
        };
        let lines = detail_display_lines(&dv, crate::GlyphMode::Unicode, &theme::DEFAULT);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let meta_at = text
            .iter()
            .position(|l| l == "meta (3):")
            .expect("the meta header");
        // Rows follow the header, BTreeMap-sorted by key (deep < owner < zone),
        // key column padded to the widest key.
        assert_eq!(
            &text[meta_at + 1..meta_at + 4],
            &[
                "  deep : {\"x\":1}".to_string(),
                "  owner: alice".to_string(),
                "  zone : z1".to_string(),
            ]
        );
        // An empty meta dict draws no block at all.
        let mut empty = dv.clone();
        empty.detail.meta = BTreeMap::new();
        let lines = detail_display_lines(&empty, crate::GlyphMode::Unicode, &theme::DEFAULT);
        assert!(lines
            .iter()
            .all(|l| !l.spans.iter().any(|s| s.content.starts_with("meta"))));
    }

    #[test]
    fn detail_meta_block_flattens_newlines_and_pads_multibyte_keys() {
        use std::collections::BTreeMap;
        // A \n inside a meta VALUE would tear the one-row-per-entry layout —
        // flattened to a space. And the key column pads by CHAR count (the
        // format width), not bytes: `équipe` is 6 chars but 7 bytes, so a
        // byte-counted pad would misalign the `:` column.
        let dv = DetailView {
            model_id: "model.p.m".into(),
            name: "m".into(),
            detail: crate::NodeDetail {
                meta: BTreeMap::from_iter([
                    ("k".to_string(), "line1\nline2\r\nline3".to_string()),
                    ("équipe".to_string(), "x".to_string()),
                ]),
                ..Default::default()
            },
            tests: vec![],
            downstream_count: 0,
            upstream_count: 0,
            catalog: vec![],
            col_names: vec![],
            col_index: usize::MAX,
            test_index: usize::MAX,
            col_counts: Default::default(),
            stale: false,
            name_match: false,
            scroll: 0,
        };
        let lines = detail_display_lines(&dv, crate::GlyphMode::Unicode, &theme::DEFAULT);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let meta_at = text
            .iter()
            .position(|l| l == "meta (2):")
            .expect("the meta header");
        assert_eq!(
            &text[meta_at + 1..meta_at + 3],
            &[
                "  k     : line1 line2 line3".to_string(),
                "  équipe: x".to_string(),
            ],
            "char-width padding + newline flattening"
        );
    }

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
