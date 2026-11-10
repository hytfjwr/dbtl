use super::*;

/// Clamp a desired scroll offset to `[0, grid_dim - view_dim]` for one axis.
///
/// When the grid is no larger than the viewport, the only valid offset is 0.
/// All arithmetic is saturating, so degenerate sizes never panic. This is the
/// single source of truth for both axes' scroll bounds.
pub fn clamp_offset(grid_dim: usize, view_dim: usize, desired: usize) -> usize {
    let max_offset = grid_dim.saturating_sub(view_dim);
    desired.min(max_offset)
}

/// Compute the initial viewport offset `(x, y)` that centers the selected
/// node's LABEL in a `view_w x view_h` viewport over `grid`, then clamps each
/// axis so the whole label is visible.
///
/// The horizontal anchor centers the label's *midpoint* (not its start cell),
/// so a long label (e.g. `fct_subscription_process`, 24 cells) fits entirely
/// when the viewport is at least as wide as the label — centering the start
/// would clip the tail. Two cheap edge-ensures cover grid boundaries and the
/// extreme case where the label is wider than the viewport (very long source
/// names / tiny panes): show the right edge, then prefer the start (names read
/// left-to-right) without ever panicking. Labels are one row tall, so the
/// vertical axis just centers the row and clamps.
///
/// `None` (empty subgraph) yields `(0, 0)`.
pub fn anchor_offset(
    selected_rect: Option<NodeRect>,
    grid: &CharGrid,
    view_w: usize,
    view_h: usize,
) -> (usize, usize) {
    let Some(rect) = selected_rect else {
        return (0, 0);
    };
    let grid_w = grid.width();
    let grid_h = grid.height();
    let (sx, w) = (rect.x, rect.width.max(1));

    // Horizontal: center the label's midpoint in the viewport.
    let mut off_x = clamp_offset(grid_w, view_w, (sx + w / 2).saturating_sub(view_w / 2));
    // Ensure the label's right edge is visible (no tail clipping).
    if sx + w > off_x + view_w {
        off_x = (sx + w).saturating_sub(view_w);
    }
    // Ensure the label's start is visible (wins for labels wider than view_w,
    // so the human-meaningful prefix shows; names read left-to-right).
    if sx < off_x {
        off_x = sx;
    }
    off_x = clamp_offset(grid_w, view_w, off_x);

    // Vertical: labels are 1 row tall, so centering the row + clamp suffices.
    let off_y = clamp_offset(grid_h, view_h, rect.y.saturating_sub(view_h / 2));

    (off_x, off_y)
}

/// Cut a `view_w x view_h` window out of `grid` at offset `(off_x, off_y)`,
/// returning a new [`CharGrid`] of exactly that size (padding with spaces where
/// the window extends past the grid). Pure (no ratatui), so what lands in the
/// viewport is testable without a terminal.
///
/// The whole grid is built first (`layout`) and only then sliced, so the
/// "compute window == draw window" guarantee holds in 2D.
pub fn blit(grid: &CharGrid, off_x: usize, off_y: usize, view_w: usize, view_h: usize) -> CharGrid {
    let mut out = CharGrid::new(view_w.max(1), view_h.max(1));
    for vy in 0..out.height {
        for vx in 0..out.width {
            let gx = off_x + vx;
            let gy = off_y + vy;
            if gx < grid.width && gy < grid.height {
                let c = grid.char_at(gx, gy);
                let e = grid.emphasis_at(gx, gy);
                out.put_emph(vx, vy, c, e);
                out.set_attr(vx, vy, grid.attr_at(gx, gy));
            }
        }
    }
    out
}
