use super::*;

// ---- node context menu (Issue 06) ------------------------------------------

/// Draw the node context menu into `rect` (precomputed by
/// [`super::geom::menu_rect`] — the same rect the hit-test routes into):
/// the target's name as the title, one row per [`MenuEntry`] (labels and key
/// columns come from the [`MenuState`] snapshot, which was built off
/// [`BINDINGS`](crate::action::BINDINGS) at open — nothing is re-resolved
/// here). The selected item row is reversed; separators draw a full-width
/// chrome rule (the `─`/`-` glyph pair from screen-spec §15).
pub(crate) fn draw_menu(
    frame: &mut Frame,
    rect: Rect,
    ms: &MenuState,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    let chrome = super::chrome(glyphs);
    let inner = super::draw_accent_modal(frame, rect, format!(" {} ", ms.name), glyphs, t);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Row layout: `  label<pad>key` — the key column is right-aligned to the
    // widest key label and dim (like the palette's); the separator is a
    // rule of chrome glyphs across the inner width.
    let label_w = ms
        .items
        .iter()
        .map(|e| match e {
            MenuEntry::Item { label, .. } => label.chars().count(),
            MenuEntry::Separator => 0,
        })
        .max()
        .unwrap_or(0);
    let key_w = ms
        .items
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item { binding, .. } => Some(
                crate::action::BINDINGS[*binding]
                    .key_label()
                    .chars()
                    .count(),
            ),
            MenuEntry::Separator => None,
        })
        .max()
        .unwrap_or(0);
    let rows: Vec<Line> = ms
        .items
        .iter()
        .enumerate()
        .map(|(i, e)| match e {
            MenuEntry::Separator => Line::styled(
                chrome.rule.repeat(inner.width as usize),
                Style::default().fg(t.text_faint),
            ),
            MenuEntry::Item { binding, label } => {
                let selected = i == ms.selected;
                let base = if selected {
                    selected_style(t)
                } else {
                    Style::default()
                };
                let key_base = if selected {
                    base
                } else {
                    Style::default().fg(t.text_dim)
                };
                Line::from(vec![
                    Span::styled(format!("  {label:<label_w$}"), base),
                    Span::styled("  ", base),
                    Span::styled(
                        format!("{:>key_w$}", crate::action::BINDINGS[*binding].key_label()),
                        key_base,
                    ),
                ])
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}
