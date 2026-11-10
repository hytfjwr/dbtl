use super::*;

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
    let thumb_top = (offset.min(max_off) * max_thumb_top)
        .checked_div(max_off)
        .unwrap_or(0);
    Some((track_y0 + thumb_top as u16, thumb_len as u16))
}
