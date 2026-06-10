//! Modal overlays drawn on top of the base panes. Each is a plain function (no
//! trait/stack apparatus) selected by `render::draw` from the current `Mode`.
//! Here: the `?` help overlay and the structure (detail) modal. Search has no
//! modal — its query renders inline in the list / lineage pane titles.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::action::{palette_candidates, DetailView, PaletteState, SqlView, StatsView};

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
) {
    let rect = modal_rect(area);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(super::chrome(glyphs).border)
        .border_style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::styled(
            title.into(),
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let total = lines.len();
    let view_h = inner.height as usize;
    let off = scroll.min(total.saturating_sub(view_h));
    let visible: Vec<Line> = lines.into_iter().skip(off).take(view_h).collect();
    frame.render_widget(Paragraph::new(visible), inner);
    super::stamp_scrollbar(frame, rect, total, off, view_h, glyphs);
}

/// The help overlay content as display lines, GENERATED from the keymap's
/// [`help_lines`](crate::action::help_lines) — the single source of truth — so
/// the `?` overlay can never drift from what the keys actually do. Grouped by
/// mode with section headers.
fn help_display_lines() -> Vec<Line<'static>> {
    use crate::action::{help_lines, ModeKind};
    let sections = [
        (ModeKind::Selection, "Normal"),
        (ModeKind::Search, "Search / filter"),
        (ModeKind::Detail, "Structure modal"),
        (ModeKind::Sql, "SQL preview"),
        (ModeKind::Stats, "Stats dashboard"),
        (ModeKind::Palette, "Command palette"),
        (ModeKind::Help, "Help"),
    ];
    let all = help_lines();
    let mut out: Vec<Line> = Vec::new();
    for (mk, title) in sections {
        out.push(Line::styled(format!("{title}:"), header_style()));
        for hl in all.iter().filter(|l| l.mode == mk) {
            // The key column in the accent, the description default — same text.
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {:<10} ", hl.keys),
                    Style::default().fg(theme::ACCENT),
                ),
                Span::raw(hl.desc.to_string()),
            ]));
        }
        out.push(Line::from(""));
    }
    out
}

/// Clamp a help-overlay scroll offset to the valid range for `area` (size-aware;
/// the event loop calls this on the stored scroll, mirroring `clamp_lineage`).
pub fn clamp_help_scroll(area: Rect, scroll: usize) -> usize {
    clamp_modal_scroll(area, help_display_lines().len(), scroll)
}

/// Draw the `?` help overlay: a centered, scrollable box listing every binding
/// (from the keymap). `scroll` is clamped here for display; the loop also clamps
/// the stored value so up/down stay responsive. `pub(crate)` for `render::draw`.
pub(crate) fn draw_help(frame: &mut Frame, area: Rect, scroll: usize, glyphs: crate::GlyphMode) {
    draw_scrollable_modal(
        frame,
        area,
        concat!(
            " Keybindings - dbtl v",
            env!("CARGO_PKG_VERSION"),
            "  (?/Esc/q to close, j/k to scroll) "
        ),
        help_display_lines(),
        scroll,
        glyphs,
    );
}

// ---- structure (detail) modal --------------------------------------------

/// The structure modal content as display lines: type / location / tags /
/// description / columns table / tests list. Built once and shared by the clamp
/// and the renderer so scroll bounds match the content.
fn detail_display_lines(dv: &DetailView) -> Vec<Line<'static>> {
    let d = &dv.detail;
    let mut out: Vec<Line> = Vec::new();

    let kind = d
        .materialized
        .clone()
        .unwrap_or_else(|| "source".to_string());
    out.push(Line::styled(format!("type:   {kind}"), header_style()));
    let location = match (&d.database, &d.schema) {
        (Some(db), Some(sc)) => format!("{db}.{sc}"),
        (None, Some(sc)) => sc.clone(),
        _ => "-".to_string(),
    };
    out.push(Line::from(format!("location: {location}")));
    if let Some(p) = &d.original_file_path {
        out.push(Line::from(format!("path:   {p}")));
    }
    out.push(Line::from(format!(
        "impact: {} downstream / {} upstream",
        dv.downstream_count, dv.upstream_count
    )));
    if !d.tags.is_empty() {
        out.push(Line::from(format!("tags:   {}", d.tags.join(", "))));
    }
    if let Some(desc) = &d.description {
        out.push(Line::from(""));
        out.push(Line::styled("description:", header_style()));
        for line in desc.lines() {
            out.push(Line::from(format!("  {line}")));
        }
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("columns ({}):", d.columns.len()),
        header_style(),
    ));
    for c in &d.columns {
        let ty = c.data_type.as_deref().unwrap_or("");
        let desc = c.description.as_deref().unwrap_or("");
        out.push(Line::from(format!("  {:<28} {:<14} {}", c.name, ty, desc)));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("tests ({}):", dv.tests.len()),
        header_style(),
    ));
    for t in &dv.tests {
        let col = t
            .column_name
            .as_deref()
            .map(|c| format!("({c})"))
            .unwrap_or_default();
        out.push(Line::from(format!("  {}{}", t.kind, col)));
    }

    out
}

/// Clamp the detail modal's scroll offset to its content/viewport (size-aware).
pub fn clamp_detail_scroll(area: Rect, dv: &DetailView) -> usize {
    clamp_modal_scroll(area, detail_display_lines(dv).len(), dv.scroll)
}

/// Draw the structure (detail) modal for the selected node. `pub(crate)` for
/// `render::draw`. Data comes from the [`DetailView`] payload (cloned at open),
/// so the render layer never needs a `Dag`.
pub(crate) fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    dv: &DetailView,
    glyphs: crate::GlyphMode,
) {
    draw_scrollable_modal(
        frame,
        area,
        format!(" {}  (Esc/q to close, j/k to scroll) ", dv.name),
        detail_display_lines(dv),
        dv.scroll,
        glyphs,
    );
}

// ---- SQL preview modal ----------------------------------------------------

/// Common SQL keywords, uppercased for case-insensitive recognition.
const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON",
    "USING", "GROUP", "ORDER", "BY", "HAVING", "LIMIT", "OFFSET", "WITH", "AS", "AND", "OR", "NOT",
    "IN", "CASE", "WHEN", "THEN", "ELSE", "END", "UNION", "ALL", "DISTINCT", "INSERT", "INTO",
    "UPDATE", "DELETE", "CREATE", "TABLE", "VIEW", "NULL", "IS", "BETWEEN", "LIKE", "EXISTS",
    "OVER", "PARTITION", "QUALIFY",
];

/// Recolour SQL keywords (case-insensitive) cyan, leaving every other byte
/// untouched. Splits the line into maximal word / non-word runs (`is_alphanumeric
/// || '_'`), so separators are preserved verbatim — the only change is colour,
/// never the text. CJK in comments is user DATA (ascii_guard permits width-2),
/// and a non-keyword word is simply left at the default style.
fn highlight_sql_line(line: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_is_word: Option<bool> = None;
    let flush = |spans: &mut Vec<Span<'static>>, run: &str, is_word: bool| {
        if run.is_empty() {
            return;
        }
        if is_word && SQL_KEYWORDS.contains(&run.to_ascii_uppercase().as_str()) {
            spans.push(Span::styled(
                run.to_string(),
                Style::default().fg(theme::SQL_KEYWORD),
            ));
        } else {
            spans.push(Span::raw(run.to_string()));
        }
    };
    for ch in line.chars() {
        let is_word = ch.is_alphanumeric() || ch == '_';
        match run_is_word {
            Some(prev) if prev == is_word => run.push(ch),
            Some(prev) => {
                flush(&mut spans, &run, prev);
                run.clear();
                run.push(ch);
                run_is_word = Some(is_word);
            }
            None => {
                run.push(ch);
                run_is_word = Some(is_word);
            }
        }
    }
    if let Some(is_word) = run_is_word {
        flush(&mut spans, &run, is_word);
    }
    Line::from(spans)
}

/// The SQL-preview content as syntax-coloured display lines. NO wrapping (it
/// would desync the line-count scroll clamp); long lines truncate at the width.
fn sql_display_lines(sv: &SqlView) -> Vec<Line<'static>> {
    sv.sql.lines().map(highlight_sql_line).collect()
}

/// Clamp the SQL modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_detail_scroll`].
pub fn clamp_sql_scroll(area: Rect, sv: &SqlView) -> usize {
    clamp_modal_scroll(area, sql_display_lines(sv).len(), sv.scroll)
}

/// Draw the SQL-preview modal. `pub(crate)` for `render::draw`. The SQL text
/// rides inside the [`SqlView`] payload (snapshotted at open), so the render
/// layer never needs a `Dag`.
pub(crate) fn draw_sql(frame: &mut Frame, area: Rect, sv: &SqlView, glyphs: crate::GlyphMode) {
    // The path tells the user WHICH file `o` would open; absent for sources /
    // seeds and manifests that omit it.
    let path = sv
        .path
        .as_deref()
        .map(|p| format!(" - {p}"))
        .unwrap_or_default();
    draw_scrollable_modal(
        frame,
        area,
        format!(" {}{path}  (Esc/q to close, j/k to scroll) ", sv.name),
        sql_display_lines(sv),
        sv.scroll,
        glyphs,
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
/// dark track of spaces ([`theme::SURFACE_HI`]), `width` cells total, fill
/// width-proportional to `pct` (integer math). Grade: danger <40, warn <70,
/// ok ≥70. Spaces only — ASCII in both modes.
fn coverage_gauge(pct: usize, width: usize) -> Vec<Span<'static>> {
    let grade = if pct < 40 {
        theme::DANGER
    } else if pct < 70 {
        theme::WARN
    } else {
        theme::OK
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
            Style::default().bg(theme::SURFACE_HI),
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
fn stats_display_lines(sv: &StatsView) -> Vec<Line<'static>> {
    const BAR_W: usize = 16;
    let mut out: Vec<Line> = Vec::new();
    out.push(Line::from(format!("project: {}", sv.project)));

    out.push(Line::from(""));
    out.push(Line::styled("Resource counts:", header_style()));
    let rt_max = sv.by_resource_type.iter().map(|(_, c)| *c).max().unwrap_or(0);
    for (rt, count) in &sv.by_resource_type {
        out.push(Line::from(vec![
            Span::raw(format!("  {rt:<12} {count:<4} ")),
            mini_bar(*count, rt_max, BAR_W, theme::BAR_RESOURCE),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled("Materialization (models):", header_style()));
    let mat_max = sv
        .by_materialization
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (mat, count) in &sv.by_materialization {
        out.push(Line::from(vec![
            Span::raw(format!("  {mat:<12} {count:<4} ")),
            mini_bar(*count, mat_max, BAR_W, theme::BAR_MATERIALIZATION),
        ]));
    }

    out.push(Line::from(""));
    // Same coverage_gap base as the `t` lens and the status `cov` segment.
    out.push(Line::styled(
        "Test coverage (models / seeds / snapshots):",
        header_style(),
    ));
    let pct = if sv.testable_total == 0 {
        0
    } else {
        sv.testable_tested * 100 / sv.testable_total
    };
    let mut cov_spans = vec![Span::raw("  ")];
    cov_spans.extend(coverage_gauge(pct, 20));
    cov_spans.push(Span::raw(format!(
        " {}/{} tested ({pct}%)",
        sv.testable_tested, sv.testable_total
    )));
    out.push(Line::from(cov_spans));

    out.push(Line::from(""));
    out.push(Line::styled("Top hubs (degree):", header_style()));
    let deg_max = sv.top_hubs.iter().map(|(_, _, d)| *d).max().unwrap_or(0);
    for (_, name, degree) in &sv.top_hubs {
        out.push(Line::from(vec![
            Span::raw(format!("  {name:<28} {degree:<4} ")),
            mini_bar(*degree, deg_max, BAR_W, theme::BAR_DEGREE),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled("Hubs (transitive downstream):", header_style()));
    let tr_max = sv.transitive_hubs.iter().map(|(_, c)| *c).max().unwrap_or(0);
    for (name, count) in &sv.transitive_hubs {
        out.push(Line::from(vec![
            Span::raw(format!("  {name:<28} {count:<4} ")),
            mini_bar(*count, tr_max, BAR_W, theme::BAR_TRANSITIVE),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Critical path (depth {}):", sv.critical_path.len()),
        header_style(),
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

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Orphans ({}):", sv.orphan_models.len()),
        header_style(),
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
        header_style(),
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

    out.push(Line::from(""));
    out.push(Line::styled("Warnings:", header_style()));
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

    out
}

/// Clamp the stats modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_detail_scroll`].
pub fn clamp_stats_scroll(area: Rect, sv: &StatsView) -> usize {
    clamp_modal_scroll(area, stats_display_lines(sv).len(), sv.scroll)
}

/// Draw the stats-dashboard modal. `pub(crate)` for `render::draw`. The stats
/// ride inside the [`StatsView`] payload (computed at open), so the render layer
/// never needs a `Dag`.
pub(crate) fn draw_stats(frame: &mut Frame, area: Rect, sv: &StatsView, glyphs: crate::GlyphMode) {
    draw_scrollable_modal(
        frame,
        area,
        format!(" Stats - {}  (Esc/q to close, j/k to scroll) ", sv.project),
        stats_display_lines(sv),
        sv.scroll,
        glyphs,
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
) {
    // ~60% × ~60%, min-clamped so the box never collapses on a tiny terminal.
    let rect = centered_rect(60, 60, area).intersection(area);
    let chrome = super::chrome(glyphs);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome.border)
        .border_style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::styled(
            " commands  (Esc to close, Enter to run) ",
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The query line (row 0 of the interior), with the caret after the text.
    let query_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(
            state.query.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(chrome.caret.to_string(), Style::default().fg(theme::ACCENT)),
    ]);
    let query_rect = Rect {
        height: 1,
        ..inner
    };
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
            selected_style()
        } else {
            Style::default()
        };
        // The help text, with matched query chars highlighted (yellow+bold). The
        // highlight wins over the base except for the reversed selected row, which
        // keeps the reverse (selection cue beats the match cue).
        let hits = crate::model_list::match_indices(b.help, &state.query);
        let hi = Style::default()
            .fg(theme::GOLD)
            .add_modifier(Modifier::BOLD);
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
            Style::default().fg(theme::TEXT_DIM)
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
    super::stamp_scrollbar(frame, rect, total, off, view_h, glyphs);
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
pub(crate) fn draw_toast(frame: &mut Frame, area: Rect, text: &str, glyphs: crate::GlyphMode) {
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
        .border_style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(theme::SURFACE));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(" {text} "),
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        inner,
    );
}
