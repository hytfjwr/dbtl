//! Pure screen geometry: pane rects, pane interiors, and the centered-overlay
//! rect. Computed the same way by `render` and by the tests, so a test can
//! isolate exactly the lineage pane interior. No state here.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders};

use crate::NodeRect;

/// The three screen regions: left list pane, right lineage pane, bottom status
/// line (each with its border, except the status line).
#[derive(Debug, Clone, Copy)]
pub struct PaneRects {
    /// Left model-list pane (with border).
    pub list: Rect,
    /// Right lineage pane (with border).
    pub lineage: Rect,
    /// Bottom status/help line.
    pub status: Rect,
}

/// Split a total `area` into the three pane rects (vertical body/status, then
/// horizontal 40/60). The single source of truth for screen geometry.
///
/// With `list_visible = false` the horizontal split is skipped entirely: the
/// lineage pane takes the whole body and the list rect is zero-width (never a
/// stray solver cell), so mouse `within` tests on it are always false.
pub fn pane_rects(area: Rect, list_visible: bool) -> PaneRects {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let body = chunks[0];
    let status = chunks[1];

    if !list_visible {
        return PaneRects {
            list: Rect {
                width: 0,
                ..body
            },
            lineage: body,
            status,
        };
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(body);

    PaneRects {
        list: panes[0],
        lineage: panes[1],
        status,
    }
}

/// The interior (inside the border) of a bordered pane rect. Mirrors
/// `Block::default().borders(ALL).inner(rect)`, computed purely so tests can scan
/// exactly the lineage pane's interior without a `Block`.
pub fn pane_interior(rect: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(rect)
}

/// A rectangle centered in `area` at `pct_w`% × `pct_h`% of its size. Saturating
/// so tiny terminals never panic (the result is at least 1×1). Used for overlays.
/// The `% / 100` is done in `u32` so very wide terminals (≥820 cols) don't
/// overflow `u16` before the divide.
pub(crate) fn centered_rect(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let w = ((area.width as u32 * pct_w as u32 / 100).max(1) as u16).min(area.width.max(1));
    let h = ((area.height as u32 * pct_h as u32 / 100).max(1) as u16).min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// The centered sub-rect a `grid_w × grid_h` lineage diagram occupies inside the
/// lineage pane `interior`: as large as the diagram up to the interior, padded to
/// the centre. The SINGLE source of this geometry — both `draw_lineage_pane` and
/// the mouse `hit_test` use it, so "computed window == drawn window" holds for
/// hit-testing too (clicks in the pad margin map to no node).
pub fn lineage_content_rect(interior: Rect, grid_w: usize, grid_h: usize) -> Rect {
    let vw = interior.width as usize;
    let vh = interior.height as usize;
    let content_w = grid_w.min(vw);
    let content_h = grid_h.min(vh);
    let pad_x = (vw - content_w) / 2;
    let pad_y = (vh - content_h) / 2;
    Rect {
        x: interior.x + pad_x as u16,
        y: interior.y + pad_y as u16,
        width: content_w as u16,
        height: content_h as u16,
    }
}

/// Which edges of a scrollable diagram are clipped — i.e. more grid lies beyond
/// the viewport in that direction — at the current scroll offset. Pure, so the
/// lineage pane and a headless test agree on when to show a scroll marker.
///
/// `view_w`/`view_h` are the CONTENT window size (the blit window =
/// `min(grid, interior)`), so when the whole diagram fits, every edge is `false`
/// (the offset is clamped to 0 and `0 + grid == grid`, not `< grid`). When it
/// does not fit, an edge is clipped exactly when there are unshown cells on that
/// side. This is the affordance that turns a partially-drawn box at the pane
/// border from "broken render" into "scroll this way (`h`/`l`, `↑`/`↓`)".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClipEdges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl ClipEdges {
    /// Whether any edge is clipped (the diagram does not fully fit the pane).
    pub fn any(&self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

/// Compute the clipped edges (see [`ClipEdges`]). Saturating, so degenerate
/// sizes never panic.
pub fn clip_edges(
    grid_w: usize,
    grid_h: usize,
    scroll_x: usize,
    scroll_y: usize,
    view_w: usize,
    view_h: usize,
) -> ClipEdges {
    ClipEdges {
        left: scroll_x > 0,
        right: scroll_x + view_w < grid_w,
        top: scroll_y > 0,
        bottom: scroll_y + view_h < grid_h,
    }
}

/// Vertical scrollbar thumb geometry inside a bordered pane's right-edge track.
///
/// Returns `None` when the content fits (`total <= viewport`) or the track has
/// no room (`track_h == 0`) — in those cases no bar is drawn (the plain border
/// stays clean). Otherwise returns `Some((thumb_top_y, thumb_len))` in the SAME
/// y-space as `track_y0 .. track_y0 + track_h`: a proportional thumb (at least 1
/// cell) whose top tracks `offset / (total - viewport)` over the available
/// travel. Saturating throughout — never panics, never divides by zero.
pub fn scrollbar_thumb(
    total: usize,
    offset: usize,
    viewport: usize,
    track_y0: u16,
    track_h: u16,
) -> Option<(u16, u16)> {
    if total <= viewport || track_h == 0 {
        return None;
    }
    let th = track_h as usize;
    // Proportional thumb length, clamped to [1, track_h].
    let thumb_len = ((viewport * th) / total).max(1).min(th);
    let max_off = total - viewport; // > 0 here (total > viewport)
    let max_thumb_top = th - thumb_len; // travel room (>= 0)
    let thumb_top = if max_off == 0 {
        0
    } else {
        (offset.min(max_off) * max_thumb_top) / max_off
    };
    Some((track_y0 + thumb_top as u16, thumb_len as u16))
}

/// Pure lineage hit-test: map a screen click `(click_x, click_y)` to the
/// `unique_id` of the node whose label rect it lands on, or `None`.
///
/// The classic bug here is the coordinate spaces: the click is in SCREEN space,
/// `rects` is in CHARGRID space. We subtract the pane-interior origin and add the
/// scroll offset to convert, then test containment. Pure (no terminal), so it is
/// unit-tested headlessly.
pub fn hit_test(
    rects: &HashMap<String, NodeRect>,
    scroll_x: usize,
    scroll_y: usize,
    interior: Rect,
    click_x: u16,
    click_y: u16,
) -> Option<String> {
    if click_x < interior.x
        || click_y < interior.y
        || click_x >= interior.x + interior.width
        || click_y >= interior.y + interior.height
    {
        return None; // outside the lineage pane interior
    }
    let gx = scroll_x + (click_x - interior.x) as usize;
    let gy = scroll_y + (click_y - interior.y) as usize;
    rects
        .iter()
        .find(|(_, r)| gx >= r.x && gx < r.x + r.width && gy >= r.y && gy < r.y + r.height)
        .map(|(uid, _)| uid.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_rects_hidden_list_gives_lineage_the_full_body() {
        let area = Rect::new(0, 0, 100, 30);
        let shown = pane_rects(area, true);
        assert_eq!(shown.list.width, 40, "40% split while visible");
        assert_eq!(shown.lineage.width, 60);

        let hidden = pane_rects(area, false);
        assert_eq!(hidden.list.width, 0, "hidden list is zero-width");
        assert_eq!(
            hidden.lineage,
            Rect::new(0, 0, 100, 29),
            "lineage takes the whole body (status line excluded)"
        );
        assert_eq!(hidden.status, shown.status, "status line is unaffected");
    }

    #[test]
    fn hit_test_maps_clicks_to_nodes_across_scroll() {
        // Two single-cell-tall labels at known grid positions.
        let rects = HashMap::from([
            (
                "a".to_string(),
                NodeRect {
                    x: 0,
                    y: 0,
                    width: 3,
                    height: 1,
                },
            ),
            (
                "b".to_string(),
                NodeRect {
                    x: 10,
                    y: 4,
                    width: 5,
                    height: 1,
                },
            ),
        ]);
        let interior = Rect::new(2, 1, 40, 20); // pane interior offset from screen origin

        // Click on 'a' at grid (0,0) with no scroll: screen = interior origin.
        assert_eq!(hit_test(&rects, 0, 0, interior, 2, 1).as_deref(), Some("a"));
        // A blank grid cell → None.
        assert_eq!(hit_test(&rects, 0, 0, interior, 6, 1), None);
        // Click outside the interior → None.
        assert_eq!(hit_test(&rects, 0, 0, interior, 0, 0), None);
        // 'b' at grid (10,4): with scroll (8,3), it sits at screen
        // (interior.x + 10 - 8, interior.y + 4 - 3) = (4, 2). Round-trips.
        assert_eq!(hit_test(&rects, 8, 3, interior, 4, 2).as_deref(), Some("b"));
        // Same screen cell without scroll lands on empty grid → None.
        assert_eq!(hit_test(&rects, 0, 0, interior, 4, 2), None);
    }

    #[test]
    fn clip_edges_flags_only_real_overflow() {
        // Grid fits the viewport (content == grid, offset 0): nothing clipped.
        assert_eq!(clip_edges(50, 10, 0, 0, 50, 10), ClipEdges::default());
        assert!(!clip_edges(50, 10, 0, 0, 50, 10).any());

        // Wider grid, scrolled into the middle: both horizontal edges clipped,
        // vertical fits.
        let e = clip_edges(100, 10, 20, 0, 60, 10);
        assert_eq!(
            (e.left, e.right, e.top, e.bottom),
            (true, true, false, false)
        );

        // Scrolled hard left: left edge flush (no left clip), right still cut.
        let e = clip_edges(100, 10, 0, 0, 60, 10);
        assert_eq!((e.left, e.right), (false, true));

        // Scrolled to the far right edge: right flush, left cut.
        let e = clip_edges(100, 10, 40, 0, 60, 10);
        assert_eq!((e.left, e.right), (true, false));

        // Taller grid scrolled down: top and bottom both clipped.
        let e = clip_edges(50, 100, 0, 30, 50, 20);
        assert_eq!((e.top, e.bottom), (true, true));
    }

    #[test]
    fn scrollbar_thumb_none_when_content_fits() {
        // Exactly fits → no bar.
        assert_eq!(scrollbar_thumb(10, 0, 10, 0, 10), None);
        // Viewport larger than content → no bar.
        assert_eq!(scrollbar_thumb(5, 0, 10, 0, 10), None);
        // Degenerate zero-height track → no bar even when overflowing.
        assert_eq!(scrollbar_thumb(100, 0, 10, 0, 0), None);
    }

    #[test]
    fn scrollbar_thumb_at_top_when_offset_zero() {
        let track_y0 = 1;
        let track_h = 10;
        let (top, len) = scrollbar_thumb(100, 0, 10, track_y0, track_h).expect("overflow → bar");
        assert_eq!(top, track_y0, "offset 0 → thumb flush at the track top");
        assert!(len >= 1, "thumb length is always at least 1");
        assert!(
            top + len <= track_y0 + track_h,
            "thumb stays inside the track"
        );
    }

    #[test]
    fn scrollbar_thumb_flush_bottom_at_max_offset() {
        let track_y0 = 1;
        let track_h = 10;
        let total = 100;
        let viewport = 10;
        let max_off = total - viewport;
        let (top, len) =
            scrollbar_thumb(total, max_off, viewport, track_y0, track_h).expect("overflow → bar");
        assert_eq!(
            top + len,
            track_y0 + track_h,
            "at max offset the thumb is flush against the track bottom"
        );
    }

    #[test]
    fn scrollbar_thumb_len_always_at_least_one() {
        // A huge total vs a 1-cell viewport would round the proportional length
        // to 0 — it must clamp up to 1 so the thumb is never invisible.
        let (_, len) = scrollbar_thumb(10_000, 0, 1, 0, 5).expect("overflow → bar");
        assert_eq!(len, 1, "tiny proportional thumb clamps to >= 1");
    }

    #[test]
    fn scrollbar_thumb_offset_past_max_clamps() {
        let track_y0 = 0;
        let track_h = 8;
        let total = 50;
        let viewport = 10;
        // An offset beyond max_off must not push the thumb past the track bottom.
        let (top, len) =
            scrollbar_thumb(total, 9999, viewport, track_y0, track_h).expect("overflow → bar");
        assert_eq!(
            top + len,
            track_y0 + track_h,
            "out-of-range offset clamps to a flush-bottom thumb"
        );
    }
}
