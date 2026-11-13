//! The right inspector pane: a continuously
//! updated summary of the node under the lineage cursor (or the rooted
//! selection). All domain data arrives as an [`InspectorView`] snapshot —
//! the same `detail_view_for`-built payload the structure modal renders —
//! so this module stays `Dag`-free like every other render surface.
//!
//! Layout contract: head line + owner/tags/meta, Description, Tests,
//! Columns, Impact, Last run, Lint, Diff — every section omitted (header
//! included) when it has no data, so the pane never shows dead chrome.
//! `wrap_prefixed`/`test_row_text` are shared with the structure modal via
//! `overlay` — one presentation pass, never two.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::InspectorView;

use super::render::RenderCtx;
use super::theme;
use super::{chrome, focus_border, header_style, selected_style, stamp_scrollbar, title_style};
use super::{overlay, Focus};

/// The fixed scroll step `d`/`u`/PageDown/PageUp apply while the inspector
/// is focused — the same 10-line step the modal `DetailScrollPage` uses.
/// The reducer writes the intent unclamped; the event loop bounds it.
pub const INSPECTOR_PAGE_STEP: usize = 10;

/// The pane's full content as display lines, wrapped to `width` (the
/// interior column count). `col_cursor` is the inspector's column-row
/// cursor (`usize::MAX` = none — the scroll-clamp and hit-test callers
/// build with no cursor since the line GEOMETRY is cursor-independent).
/// Shared by the renderer and the event loop's scroll clamp / column-row
/// hit mapping so bounds always match the rendered content.
pub fn inspector_lines(
    view: &InspectorView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
    width: usize,
    col_cursor: usize,
) -> Vec<Line<'static>> {
    let chr = chrome(glyphs);
    let d = &view.detail.detail;
    let section = header_style(t);
    let dim = Style::default().fg(t.text_dim);
    let bright = Style::default().fg(t.text_bright);
    let mut out: Vec<Line<'static>> = Vec::new();

    // ---- head: materialization tag + project path -------------------------
    let mut head = view.tag.to_string();
    if let Some(path) = &d.original_file_path {
        head.push_str(&format!("  {path}"));
    }
    out.push(Line::styled(head, bright));
    if let Some(owner) = &view.owner {
        out.push(Line::from(format!("owner: {owner}")));
    }
    if !d.tags.is_empty() {
        out.push(Line::from(format!("tags:  {}", d.tags.join(", "))));
    }
    // The `meta` dict as a `key=value, …` joined line — the compact form
    // (the modal's one-row-per-entry block would eat the narrow pane).
    if !d.meta.is_empty() {
        let joined = d
            .meta
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        for line in overlay::wrap_prefixed("meta:  ", &joined, width) {
            out.push(Line::styled(line, dim));
        }
    }

    // ---- Description -------------------------------------------------------
    if let Some(desc) = &d.description {
        out.push(Line::from(""));
        out.push(Line::styled("Description", section));
        for line in desc.lines() {
            for w in overlay::wrap_prefixed("", line, width) {
                out.push(Line::from(w));
            }
        }
    }

    // ---- Tests -------------------------------------------------------------
    if !view.detail.tests.is_empty() {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("Tests ({})", view.detail.tests.len()),
            section,
        ));
        for test in &view.detail.tests {
            out.push(Line::from(format!(
                "  {} {}",
                chr.run_ok,
                overlay::test_row_text(test)
            )));
        }
    }

    // ---- Columns -----------------------------------------------------------
    // One row per cursor-addressable column (`col_names` — the SAME index
    // space `inspector_col` / `InspectorColumnPick` address). Type = the
    // declared `data_type`, else the catalog's warehouse type, else blank.
    if !view.detail.col_names.is_empty() {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("Columns ({})", view.detail.col_names.len()),
            section,
        ));
        for (i, name) in view.detail.col_names.iter().enumerate() {
            let declared_ty = d.columns.get(i).and_then(|c| c.data_type.as_deref());
            let ty = declared_ty
                .or_else(|| view.col_types.get(name).map(String::as_str))
                .unwrap_or_default();
            // The counts look up the NORMALIZED name — the resolver's key
            // space — while the row prints the declared spelling.
            let counts = view
                .detail
                .col_counts
                .get(&crate::normalize_column(name))
                .map(|(u, dn)| format!("{}{} {}{}", chr.badge_up, u, chr.badge_down, dn))
                .unwrap_or_default();
            let text = format!("  {name:<18} {ty:<12} {counts}");
            out.push(if i == col_cursor {
                Line::styled(text, selected_style(t))
            } else {
                Line::from(text)
            });
        }
    }

    // ---- Impact ------------------------------------------------------------
    // Transitive up/down + exposure reach — the `impact_breakdown_for`
    // numbers (the modal drops the exposure component for its two-number
    // blast radius; the pane wants all three).
    out.push(Line::from(""));
    out.push(Line::styled("Impact", section));
    out.push(Line::from(format!(
        "  {} transitive: {} nodes",
        chr.badge_down, view.detail.downstream_count
    )));
    out.push(Line::from(format!(
        "  {} transitive: {} nodes",
        chr.badge_up, view.detail.upstream_count
    )));
    if view.exposures > 0 {
        out.push(Line::from(format!("  exposures: {}", view.exposures)));
    }

    // ---- Last run ----------------------------------------------------------
    if let Some((word, secs)) = &view.run {
        out.push(Line::from(""));
        out.push(Line::styled("Last run", section));
        let (glyph, style) = match word.as_str() {
            "error" | "fail" => (chr.run_fail, Style::default().fg(t.danger)),
            "warn" | "skipped" => (chr.run_warn, Style::default().fg(t.warn)),
            _ => (chr.run_ok, Style::default().fg(t.ok)),
        };
        let mut text = format!("  {glyph} {word}");
        if let Some(s) = secs {
            text.push_str(&format!("  {s:.1}s"));
        }
        out.push(Line::styled(text, style));
    }

    // ---- Lint --------------------------------------------------------------
    // A lint config is always in effect (defaults lint too), so an empty
    // per-node finding list reads as a real verdict, not "no data" — the
    // `✓ all checks pass` readout. Each finding renders `mark rule: summary`
    // (severity mark + colour — the same `x`/`!` run glyphs the Last-run
    // section uses), head and message both wrapped on the indented lines.
    out.push(Line::from(""));
    out.push(Line::styled("Lint", section));
    if view.detail.lint.is_empty() {
        out.push(Line::styled(
            format!("  {} all checks pass", chr.run_ok),
            Style::default().fg(t.ok),
        ));
    } else {
        for finding in &view.detail.lint {
            let (mark, style) = match finding.severity {
                crate::Severity::Error => (chr.run_fail, Style::default().fg(t.danger)),
                crate::Severity::Warn => (chr.run_warn, Style::default().fg(t.warn)),
            };
            for w in overlay::wrap_prefixed(
                &format!("  {mark} "),
                &format!("{}: {}", finding.rule, finding.summary),
                width,
            ) {
                out.push(Line::styled(w, style));
            }
            for w in overlay::wrap_prefixed("    ", &finding.message, width) {
                out.push(Line::styled(w, style));
            }
        }
    }

    // ---- Diff --------------------------------------------------------------
    if let Some(diff) = &view.diff {
        out.push(Line::from(""));
        out.push(Line::styled("Diff", section));
        let style = if diff.starts_with('+') {
            Style::default().fg(t.diff_add)
        } else {
            Style::default().fg(t.diff_mod)
        };
        for w in overlay::wrap_prefixed("  ", diff, width) {
            out.push(Line::styled(w, style));
        }
    }

    out
}

/// The line index of the FIRST column row — the base the event loop's
/// ensure-visible and the column-row hit mapping need. Counts with the SAME
/// width as the draw, like `clamp_detail_scroll` counts with the modal's.
pub fn inspector_columns_line0(
    view: &InspectorView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
    width: usize,
) -> usize {
    let lines = inspector_lines(view, glyphs, t, width, usize::MAX);
    let header = format!("Columns ({})", view.detail.col_names.len());
    lines
        .iter()
        .position(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                == header
        })
        .map(|p| p + 1)
        .unwrap_or(lines.len())
}

/// Clamp the inspector's scroll to its content/viewport and — when
/// `reveal_col` names a column row — scroll it inside the window, the
/// pane-side analogue of [`clamp_detail_scroll`](super::clamp_detail_scroll).
/// `reveal_col` is a ONE-SHOT intent (the mouse pick's reveal, drained via
/// [`UiState::take_inspector_reveal`](crate::UiState::take_inspector_reveal));
/// `usize::MAX` means "clamp only". It is deliberately NOT the every-frame
/// `inspector_col` — re-pinning each frame would cage the scroll keys inside
/// `inner_h` of the last pick forever. `inner_w`/`inner_h` are the interior
/// cell counts the loop measured.
pub fn clamp_inspector_scroll(
    view: &InspectorView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
    inner_w: usize,
    inner_h: usize,
    scroll: usize,
    reveal_col: usize,
) -> usize {
    let total = inspector_lines(view, glyphs, t, inner_w, usize::MAX).len();
    let mut scroll = scroll.min(total.saturating_sub(inner_h.max(1)));
    if reveal_col != usize::MAX && reveal_col < view.detail.col_names.len() {
        let row = inspector_columns_line0(view, glyphs, t, inner_w) + reveal_col;
        if row < scroll {
            scroll = row;
        } else if row >= scroll + inner_h.max(1) {
            scroll = row + 1 - inner_h.max(1);
        }
    }
    scroll.min(total.saturating_sub(inner_h.max(1)))
}

/// The column index under interior row `y` (0-based inside the pane's inner
/// rect), given `scroll` — the screen→row mapping the click handler needs
/// for `InspectorColumnPick`. `None` off the rows.
pub fn inspector_column_at(
    view: &InspectorView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
    inner_w: usize,
    scroll: usize,
    y: u16,
) -> Option<usize> {
    let base = inspector_columns_line0(view, glyphs, t, inner_w);
    let line = scroll + y as usize;
    line.checked_sub(base)
        .filter(|i| *i < view.detail.col_names.len())
}

/// Draw the inspector pane into `area` — a bordered box titled with the
/// node name, its content the scrolled `inspector_lines`, a scrollbar
/// thumb on overflow. `ctx.state` supplies focus (border/title accent) and
/// the scroll + column cursor (UiState slots, like every scrollable pane).
pub fn draw_inspector_pane(frame: &mut Frame, area: Rect, ctx: &RenderCtx, view: &InspectorView) {
    let glyphs = ctx.glyphs;
    let t = ctx.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome(glyphs).border)
        .border_style(focus_border(ctx.state, Focus::Inspector, t))
        .title(Line::styled(
            format!(" {} ", view.detail.name),
            title_style(ctx.state, Focus::Inspector, t),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    let lines = inspector_lines(view, glyphs, t, inner_w, ctx.state.inspector_col());
    let scroll = ctx
        .state
        .inspector_scroll()
        .min(lines.len().saturating_sub(inner_h));
    let visible: Vec<Line> = lines.iter().skip(scroll).take(inner_h).cloned().collect();
    frame.render_widget(Paragraph::new(visible), inner);
    stamp_scrollbar(frame, area, lines.len(), scroll, inner_h, glyphs, t);
}
