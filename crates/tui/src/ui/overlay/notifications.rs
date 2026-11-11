use super::*;

// ---- notification centre --------------------------------------

/// The centre's content lines: `HH:MM:SS  level  text` newest-first — the
/// stamp dim, the level `warn`-coloured (bold) or dim (`info`), the text
/// plain. [`clamp_notifications_scroll`] counts the same list (the empty log
/// still shows one placeholder line).
fn notifications_display_lines(nv: &NotificationsView, t: &theme::Theme) -> Vec<Line<'static>> {
    if nv.rows.is_empty() {
        return vec![Line::styled(
            "  no notifications yet".to_string(),
            Style::default().fg(t.text_dim),
        )];
    }
    nv.rows
        .iter()
        .map(|r| {
            let level_style = match r.level {
                NoticeLevel::Info => Style::default().fg(t.text_dim),
                NoticeLevel::Warning => Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            };
            Line::from(vec![
                Span::styled(format!("{}  ", r.stamp), Style::default().fg(t.text_dim)),
                Span::styled(format!("{}  ", r.level.label()), level_style),
                Span::styled(r.text.clone(), Style::default().fg(t.text_bright)),
            ])
        })
        .collect()
}

/// Clamp the centre's scroll offset to its content/viewport (size-aware; the
/// event loop calls this on the stored scroll, mirroring `clamp_lineage`).
/// The content count is the rows (1 for the empty placeholder), so the
/// scrollbar never overflows the line list.
pub fn clamp_notifications_scroll(area: Rect, nv: &NotificationsView) -> usize {
    clamp_modal_scroll(area, nv.rows.len().max(1), nv.scroll)
}

/// Draw the notification-centre modal. `pub(crate)` for `render::draw`. The
/// rows ride inside [`NotificationsView`] (snapshotted at open), so the
/// render layer never needs a `Dag`.
pub(crate) fn draw_notifications(
    frame: &mut Frame,
    area: Rect,
    nv: &NotificationsView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        " notifications  (j/k scroll, Esc/q close) ",
        notifications_display_lines(nv, t),
        nv.scroll,
        glyphs,
        t,
    );
}

/// Draw the transient toast stack: bordered one-line boxes floating at the
/// TOP-RIGHT of the screen, acknowledging the latest copy / bookmark / export
/// / reload / warnings. `render::draw` calls it LAST, so it layers above
/// panes AND modals. The renderer is stateless — the event loop owns each
/// toast's per-level lifetime (info ~2.5s, warning ~5s) and simply stops
/// passing it on expiry.
///
/// The stack holds up to 3 boxes, `toasts[0]` (the newest) on TOP — each the
/// same bordered one-liner the single toast always was, sized to its own text
/// and right-aligned; a warning toast wears the `t.warn` border (info keeps
/// `t.accent`). Chrome-correct (the glyph-mode border set) and ASCII-safe by
/// construction: overflow truncation appends a plain `..`, never an ellipsis
/// glyph. On a short terminal only the boxes that fit above the status row
/// are drawn — the stack never lands on the protected status core (see
/// `draw_status`).
pub(crate) fn draw_toasts(
    frame: &mut Frame,
    area: Rect,
    toasts: &[ToastLine],
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    // Borders (2) + one-space padding each side (2) + a 2-col right inset.
    const FRAME_W: usize = 4;
    const INSET: u16 = 2;
    // Each box is 3 rows at a 1-row top inset: boxes 0..k occupy rows
    // y+1 .. y+1+3k, and the status row (area.height-1) stays free, so the
    // stack needs 1 + 3k + 1 <= area.height → k <= (h-2)/3.
    if (area.width as usize) < FRAME_W + INSET as usize + 4 {
        return;
    }
    let fit = ((area.height.saturating_sub(2)) / 3).min(toasts.len() as u16) as usize;
    if fit == 0 {
        return;
    }
    let budget = area.width as usize - FRAME_W - INSET as usize;
    for (i, toast) in toasts.iter().take(fit).enumerate() {
        let text = toast.text;
        let (text, text_w) = if text.width() <= budget {
            (text.to_string(), text.width())
        } else {
            // Truncate by DISPLAY width (the text may carry wide CJK names).
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
        let rect = Rect::new(
            area.x + area.width - box_w - INSET,
            area.y + 1 + i as u16 * 3,
            box_w,
            3,
        );
        frame.render_widget(Clear, rect);
        let border_color = match toast.level {
            NoticeLevel::Info => t.accent,
            NoticeLevel::Warning => t.warn,
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(super::chrome(glyphs).border)
            .border_style(
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            )
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
}
