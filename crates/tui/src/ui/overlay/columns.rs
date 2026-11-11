use super::*;

// ---- column-lineage modal ---------------------------------------

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
