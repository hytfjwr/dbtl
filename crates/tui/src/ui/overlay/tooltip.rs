use super::*;

// ---- hover tooltip ----------------------------------------------

/// The settled-hover tooltip's content, snapshotted Dag-side by
/// [`App::tooltip_view`](crate::App::tooltip_view) — the render layer stays
/// Dag-free like every other overlay. `at` is the pointer cell the frame rect
/// anchors to (screen coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooltipView {
    /// The node's display name (the box title).
    pub name: String,
    /// Its one-line kind tag — `MaterializationClass::tag` (`"View model"`,
    /// `"Source"`, …), the same label the node's own box draws.
    pub tag: String,
    /// Up to two description lines (empty when the node documents nothing).
    pub desc: Vec<String>,
    /// Attached test count.
    pub tests: usize,
    /// Direct upstream / downstream node counts (the stats row's badges).
    pub up: usize,
    /// Direct downstream count.
    pub down: usize,
    /// The pointer cell the frame rect anchors to.
    pub at: (u16, u16),
}

/// Draw the settled-hover tooltip: a small bordered box floating beside the
/// pointer carrying the node's name, kind tag, first description lines, and
/// the `tests:N ↑u ↓d` stats — the pointer summary (always shown on a
/// settled hover, whether or not the inspector is up). Frame-buffer
/// chrome like the toasts: it writes only cells, never CharGrid, so the
/// layout cache and the golden fixtures are untouched; it draws over the
/// panes but under the transient toasts.
pub(crate) fn draw_tooltip(
    frame: &mut Frame,
    area: Rect,
    tip: &TooltipView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    use unicode_width::UnicodeWidthStr;
    let chrome = super::chrome(glyphs);
    // The stats row reuses the list badges' glyph pair (`↑`/`↓` vs ASCII
    // `^`/`v`) — no new glyph enters the repertoire.
    let stats = format!(
        "tests:{}  {}{} {}{}",
        tip.tests, chrome.badge_up, tip.up, chrome.badge_down, tip.down
    );
    let rows: Vec<Line> = std::iter::once(tip.tag.as_str())
        .chain(tip.desc.iter().take(2).map(String::as_str))
        .map(|s| Line::styled(s.to_string(), Style::default().fg(t.text_dim)))
        .chain(std::iter::once(Line::styled(
            stats.clone(),
            Style::default().fg(t.text_faint),
        )))
        .collect();
    // Border (2) + a one-space inner pad each side (2); wide enough for the
    // ` {name} ` title too, capped at the chrome limit.
    let content_w = tip
        .tag
        .width()
        .max(
            tip.desc
                .iter()
                .take(2)
                .map(|s| s.width())
                .max()
                .unwrap_or(0),
        )
        .max(stats.width())
        .max(tip.name.width() + 2);
    let w = (content_w as u16 + 4).min(super::geom::TOOLTIP_MAX_W);
    let h = rows.len() as u16 + 2;
    let rect = super::geom::tooltip_rect(
        Position {
            x: tip.at.0,
            y: tip.at.1,
        },
        w,
        h,
        area,
    );
    let inner = super::draw_accent_modal(frame, rect, format!(" {} ", tip.name), glyphs, t);
    if inner.width > 0 && inner.height > 0 {
        frame.render_widget(Paragraph::new(rows), inner);
    }
}
