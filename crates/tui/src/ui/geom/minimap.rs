use super::*;

/// Minimap inset dimensions (cells). A fixed small sketch in the top-right of
/// the lineage interior — the SAME constants [`draw_minimap`] stamps with, so
/// the drawn rect and the hit-tested rect can never drift apart.
///
/// [`draw_minimap`]: super::lineage
pub(crate) const MM_W: u16 = 16;
pub(crate) const MM_H: u16 = 6;

/// The screen rect the minimap occupies inside the lineage `interior`, or
/// `None` when it is not drawn: disabled, the diagram fully fits (a minimap of
/// a visible-all diagram is noise), or the interior is too small to host the
/// inset without smothering the content. This is the single source of the
/// draw guard — `draw_lineage_pane` and [`hit_target`] share it, so clicks can
/// never disagree with what is on screen.
pub fn minimap_rect(
    interior: Rect,
    grid_w: usize,
    grid_h: usize,
    scroll_x: usize,
    scroll_y: usize,
    enabled: bool,
) -> Option<Rect> {
    if !enabled {
        return None;
    }
    let content = lineage_content_rect(interior, grid_w, grid_h);
    let edges = clip_edges(
        grid_w,
        grid_h,
        scroll_x,
        scroll_y,
        content.width as usize,
        content.height as usize,
    );
    if !edges.any() || interior.width < MM_W + 2 || interior.height < MM_H + 2 {
        return None;
    }
    Some(Rect {
        x: interior.x + interior.width - MM_W,
        y: interior.y,
        width: MM_W,
        height: MM_H,
    })
}

/// The scroll offsets a minimap cell `(col, row)` asks for — click-to-centre
/// and drag-scrub share it (a click moves the viewport, a drag
/// pans). Each minimap cell covers a `sx`×`sy` block of grid cells (the same
/// division [`draw_minimap`] stamps with — ceil so the whole grid maps inside
/// the inset), so the cell maps back to its block's CENTRE in grid space and
/// the returned offsets put the content window's centre there. Saturating;
/// the caller's `clamp_lineage` pass bounds the far edge.
///
/// `mm` is the [`minimap_rect`] screen rect; `view_w`/`view_h` are the
/// CONTENT window dims (`min(grid, interior)` — the same value the minimap
/// draws its viewport sketch from), so a click on the sketched viewport's own
/// cell is a near-no-op rather than a jump.
///
/// [`draw_minimap`]: super::lineage
pub fn minimap_scroll_to(
    mm: Rect,
    col: u16,
    row: u16,
    grid_w: usize,
    grid_h: usize,
    view_w: usize,
    view_h: usize,
) -> (usize, usize) {
    let sx = grid_w.div_ceil(mm.width.max(1) as usize).max(1);
    let sy = grid_h.div_ceil(mm.height.max(1) as usize).max(1);
    // Cells past the rect's right/bottom edge (a drag scrubbing outside)
    // clamp to the grid's far edge — `mx * sx` can only overshoot grid_w by
    // less than one block, and `clamp_lineage` lands it on the max offset.
    let mx = usize::from(col.saturating_sub(mm.x));
    let my = usize::from(row.saturating_sub(mm.y));
    let scroll_x = (mx * sx + sx / 2).saturating_sub(view_w / 2);
    let scroll_y = (my * sy + sy / 2).saturating_sub(view_h / 2);
    (scroll_x, scroll_y)
}

/// The scroll offset a lineage pan drag asks for: the press-time offset
/// `base` replayed against the pointer's displacement from its press cell
/// `origin` to `at` — the diagram tracks the pointer 1:1 in both axes (grab
/// the picture: drag left → content moves left → the scroll offset grows).
/// Saturates at 0; the loop's `clamp_lineage` bounds the far edge, the same
/// contract every scroll writer (`wheel_lineage`, `anchor_lineage`) keeps.
pub fn pan_offset(base: usize, origin: u16, at: u16) -> usize {
    (base as i64 + i64::from(origin) - i64::from(at)).max(0) as usize
}
