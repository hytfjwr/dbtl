use super::*;

// ---- shared scrollable-modal scaffolding ----------------------------------
//
// All four overlays (help / detail / sql / stats) ARE the same widget: a
// centered, bordered, scrollable box. Only their TITLE and their content
// (`*_display_lines`) differ — those stay separate. The Clear+Block+pagination
// +scrollbar scaffolding is shared here so a change lands in ONE place; the
// overlay rect itself is `geom::modal_rect` (shared with the hit-test).

/// A bordered modal's inner content width for `area` — two columns for the
/// left/right borders. Line-list builders wrap to it (joined lists), and
/// the matching `clamp_*_scroll` counts with the SAME width so the scroll
/// range always covers the real line count.
pub(crate) fn modal_content_width(area: Rect) -> usize {
    modal_rect(area).width.saturating_sub(2) as usize
}

/// Word-wrap `prefix + text` to `width`: space-separated tokens pack
/// greedily, continuation lines re-indent under the first token so a
/// wrapped list still reads as one block. A token wider than the line
/// stands alone rather than splitting mid-name. `width == 0` (a
/// degenerate terminal) yields the unwrapped single line. `pub(crate)`:
/// the inspector pane wraps its Description/Lint/Diff text the same way.
pub(crate) fn wrap_prefixed(prefix: &str, text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![format!("{prefix}{text}")];
    }
    let indent = " ".repeat(prefix.chars().count());
    let mut lines: Vec<String> = Vec::new();
    let mut cur = prefix.to_string();
    for tok in text.split(' ').filter(|t| !t.is_empty()) {
        let bare = cur == prefix || cur == indent;
        let needed = tok.chars().count() + usize::from(!bare);
        if !bare && cur.chars().count() + needed > width {
            lines.push(std::mem::replace(&mut cur, indent.clone()));
        }
        if cur != prefix && cur != indent {
            cur.push(' ');
        }
        cur.push_str(tok);
    }
    lines.push(cur);
    lines
}

/// Clamp a modal's scroll offset to its content/viewport for `area` (size-aware;
/// the event loop calls this on the stored scroll, mirroring `clamp_lineage`).
/// Takes the line COUNT rather than a view type, so it is decoupled from which
/// overlay is open.
pub(crate) fn clamp_modal_scroll(area: Rect, line_count: usize, scroll: usize) -> usize {
    let inner_h = modal_rect(area).height.saturating_sub(2) as usize; // borders
    scroll.min(line_count.saturating_sub(inner_h))
}

/// Draw a centered, bordered, scrollable modal: `Clear` + accent-bold `Block` +
/// paginated `lines` (scrolled by `scroll`, clamped to the content) + the
/// right-border scrollbar thumb. The only per-overlay inputs are `title`, the
/// `lines`, and the `scroll`.
pub(crate) fn draw_scrollable_modal(
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

/// The small pop-up frame shared by the menu / tooltip / palette / yank
/// boxes: `Clear` + ALL borders + bold accent border + bold bright title,
/// drawn at `rect` verbatim (callers position/size it). Returns the inner
/// rect; the caller fills it (and handles the degenerate 0-size case).
pub(crate) fn draw_accent_modal(
    frame: &mut Frame,
    rect: Rect,
    title: impl Into<String>,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) -> Rect {
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
    inner
}
