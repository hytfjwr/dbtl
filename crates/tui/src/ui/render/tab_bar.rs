use super::*;

/// Paint the one-row tab strip: ` {n} {name} × ` cells over a
/// `surface` band — the active tab `surface_hi`+`accent` bold, the others dim —
/// `‹`/`›` overflow arrows on whichever side hides tabs, ` + ` pinned at the
/// strip's right end, and the project name dim at the bar's right edge. The
/// hover cues: a hovered tab emphasises its `×`, a hovered `×`
/// turns `danger`, and `+`/arrows take the accent.
///
/// Everything positional arrives resolved in `view.layout` (the same
/// `tab_bar_layout` the hit-test ran — no re-measuring here), and every text
/// arrives pre-truncated in `view.labels`, so this function is pure painting.
pub(super) fn draw_tab_bar(
    frame: &mut Frame,
    area: Rect,
    view: &TabBarView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    if area.height == 0 {
        return;
    }
    let chrome = super::chrome(glyphs);
    let hover = view.hover;
    // The whole strip is a `surface` band — the panes' `bg` would read as one
    // more content row instead of chrome.
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.surface)),
        area,
    );

    for cell in &view.layout.tabs {
        let active = cell.index == view.active;
        let base = if active {
            Style::default()
                .bg(t.surface_hi)
                .fg(t.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(t.surface).fg(t.text_dim)
        };
        // The `×` restyle ladder: hovered close → danger; hovered tab
        // body → accent emphasis; otherwise the quiet `text_faint`.
        let close_st = if matches!(hover, Some(HitTarget::TabClose(i)) if *i == cell.index) {
            base.patch(Style::default().fg(t.danger))
        } else if matches!(hover, Some(HitTarget::Tab(i)) if *i == cell.index) {
            base.patch(Style::default().fg(t.accent))
        } else {
            base.patch(Style::default().fg(t.text_faint))
        };
        // The `×` only exists when the cell carries a close slot (the home
        // tab doesn't — `close_x: None`); the cell still ends in a space so
        // both shapes share the band rhythm.
        let text = Line::from(vec![
            Span::styled(format!(" {} ", view.labels[cell.index]), base),
            Span::styled(
                if cell.close_x.is_some() {
                    chrome.tab_close
                } else {
                    " "
                },
                close_st,
            ),
            Span::styled(" ", base),
        ]);
        frame.render_widget(Paragraph::new(text), cell.rect);
    }

    let dim = Style::default().bg(t.surface).fg(t.text_dim);
    let hot = Style::default()
        .bg(t.surface)
        .fg(t.accent)
        .add_modifier(Modifier::BOLD);
    if let Some(r) = view.layout.left_arrow {
        let st = if matches!(hover, Some(HitTarget::TabScrollLeft)) {
            hot
        } else {
            dim
        };
        frame.render_widget(Paragraph::new(chrome.tab_prev).style(st), r);
    }
    if let Some(r) = view.layout.right_arrow {
        let st = if matches!(hover, Some(HitTarget::TabScrollRight)) {
            hot
        } else {
            dim
        };
        frame.render_widget(Paragraph::new(chrome.tab_next).style(st), r);
    }
    {
        let st = if matches!(hover, Some(HitTarget::TabNew)) {
            hot
        } else {
            dim
        };
        frame.render_widget(Paragraph::new(" + ").style(st), view.layout.plus);
    }
    if let (Some(name), Some(r)) = (view.project, view.layout.project) {
        frame.render_widget(
            Paragraph::new(format!(" {name}"))
                .style(Style::default().bg(t.surface).fg(t.text_faint))
                .alignment(ratatui::layout::Alignment::Right),
            r,
        );
    }
}
