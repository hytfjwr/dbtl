use super::*;

/// The live pane drag: which surface owns the tracking (splitter,
/// lineage pan / minimap scrub). A single slot covers every drag surface —
/// the press cell picks it once, and mid-drag motion never re-hit-tests, so
/// a pointer dragged past a bound keeps the grabbed surface's tracking.
#[derive(Clone, Copy)]
pub enum PaneDrag {
    /// A pane-boundary resize.
    Splitter(SplitterDrag),
    /// A lineage pan: the press cell plus the scroll offset at press
    /// time, replayed through [`pan_offset`] so the diagram tracks the
    /// pointer 1:1 in both axes. Begins on ANY interior cell — blank or
    /// node — because the router only reports it once the 1-cell threshold
    /// already ruled the gesture a drag, never a click.
    LineagePan {
        /// The press cell.
        origin: ratatui::layout::Position,
        /// `UiState`'s scroll_x when the drag began.
        scroll_x: usize,
        /// `UiState`'s scroll_y when the drag began.
        scroll_y: usize,
    },
    /// A minimap scrub: every tracked cell centres the viewport through the
    /// same [`minimap_scroll_to`] mapping a click uses — no press base.
    Minimap,
    /// A tab-strip drag: the CURRENT index of the grabbed tab — the strip
    /// re-hit-tests each tracked cell, so the tab slides under the pointer
    /// (the index mutates as it moves). The press cell picks the tab; the
    /// home tab (index 0) never starts one.
    TabMove(usize),
}

/// The live splitter drag: which boundary was grabbed plus the pointer's
/// offset inside its 2-column strip at press time (`0` = the left pane's
/// right-border cell, `1` = the right pane's left-border cell). Carrying the
/// offset keeps the grabbed border under the pointer instead of snapping the
/// pane edge to it.
#[derive(Clone, Copy)]
pub struct SplitterDrag {
    pub which: Splitter,
    pub grab_off: u16,
}

/// Resolve a drag's press cell to the [`PaneDrag`] it starts — `None` on a
/// surface with no drag meaning (the list, the status bar, pane chrome, the
/// home tab's cell). The same resolution runs for `DragStart` and the `DragEnd`
/// flick-fallback (a terminal that delivered no Drag events still reports
/// the release's displacement).
pub(super) fn begin_pane_drag(
    app: &App,
    lineage: Option<&Layout>,
    area: Rect,
    origin: ratatui::layout::Position,
    chrome: &ChromeGeom,
) -> Option<PaneDrag> {
    let g = chrome.hit_geometry(app, lineage, area, None);
    match hit_target(&g, origin.x, origin.y)? {
        HitTarget::PaneBorder(which) => {
            let strip = g.panes.splitter(which)?;
            Some(PaneDrag::Splitter(SplitterDrag {
                which,
                grab_off: origin.x.saturating_sub(strip.x),
            }))
        }
        // A drag that began anywhere inside the drawn lineage — blank
        // space or a node box alike — pans the diagram. `None` while the
        // pane holds no layout (an empty pane has nothing to pan).
        HitTarget::LineageNode(_) | HitTarget::LineageBlank if lineage.is_some() => {
            Some(PaneDrag::LineagePan {
                origin,
                scroll_x: app.ui_state.lineage_scroll_x(),
                scroll_y: app.ui_state.lineage_scroll_y(),
            })
        }
        HitTarget::Minimap => Some(PaneDrag::Minimap),
        // A press on a tab cell grabs it for reordering — the `×` column
        // included (the whole cell is the tab). The home tab can't be
        // dragged, so index 0 starts nothing (a flick off it stays inert).
        HitTarget::Tab(i) | HitTarget::TabClose(i) if i > 0 => Some(PaneDrag::TabMove(i)),
        _ => None,
    }
}

/// Apply one tracked step of a live [`PaneDrag`]: the splitter writes its
/// width, the pan replays `pan_offset` 1:1, the minimap centres on the
/// scrubbed cell, and the tab drag re-hit-tests the pointer's cell — a hit
/// on another tab slides the grabbed one there (the stored index tracks
/// where it now sits). The stored offsets then take the loop's per-frame
/// `clamp_lineage` like every other scroll writer.
pub(super) fn apply_pane_drag(
    app: &mut App,
    drag: &mut PaneDrag,
    chrome: &ChromeGeom,
    lineage: Option<&Layout>,
    area: Rect,
    at: ratatui::layout::Position,
) {
    match drag {
        PaneDrag::Splitter(d) => apply_splitter_drag(app, *d, at.x, area),
        PaneDrag::LineagePan {
            origin,
            scroll_x,
            scroll_y,
        } => app.ui_state.pan_lineage(
            pan_offset(*scroll_x, origin.x, at.x),
            pan_offset(*scroll_y, origin.y, at.y),
        ),
        PaneDrag::Minimap => minimap_scroll_at(app, lineage, area, at.x, at.y),
        PaneDrag::TabMove(from) => {
            // Re-hit-test each tracked cell against the strip the frame
            // drew: the grabbed tab takes the hovered tab's slot (hovering
            // the home cell lands at index 1 — the leftmost movable slot —
            // and off-strip cells just hold the last landing).
            let g = chrome.hit_geometry(app, lineage, area, None);
            if let Some(HitTarget::Tab(to) | HitTarget::TabClose(to)) = hit_target(&g, at.x, at.y) {
                let to = to.max(1);
                if to != *from {
                    app.move_tab(*from, to);
                    *from = to;
                }
            }
        }
    }
}

/// Centre the lineage viewport on the grid block minimap cell `(col, row)`
/// covers — the click-to-centre and each step of a scrub share it. The
/// pure [`minimap_scroll_to`] maps through the same block division the draw
/// stamps; `pan_lineage` stores the intent and the loop's `clamp_lineage`
/// bounds the far edge, the same contract `wheel_lineage` keeps.
pub(super) fn minimap_scroll_at(
    app: &mut App,
    lineage: Option<&Layout>,
    area: Rect,
    col: u16,
    row: u16,
) {
    let Some(lay) = lineage else {
        return;
    };
    let panes = pane_layout(area, &app.ui_state.pane_config());
    let inner = pane_interior(panes.lineage);
    let Some(mm) = minimap_rect(
        inner,
        lay.grid.width(),
        lay.grid.height(),
        app.ui_state.lineage_scroll_x(),
        app.ui_state.lineage_scroll_y(),
        app.ui_state.minimap_visible(),
    ) else {
        return;
    };
    let content = lineage_content_rect(inner, lay.grid.width(), lay.grid.height());
    let (x, y) = minimap_scroll_to(
        mm,
        col,
        row,
        lay.grid.width(),
        lay.grid.height(),
        content.width as usize,
        content.height as usize,
    );
    app.ui_state.pan_lineage(x, y);
}

/// Write the width a live drag asks for: the pointer's column resolved
/// through [`splitter_drag_width`] (grab-offset honoured) against the
/// dragged pane's FIXED outer edge — the list's left `x`, the inspector's
/// right edge — then clamped to the pane min/max for this terminal inside
/// `set_pane_width`, so a dragged-out-of-range value clamps identically to
/// the `<`/`>` keys.
pub(super) fn apply_splitter_drag(app: &mut App, drag: SplitterDrag, pointer_x: u16, area: Rect) {
    let panes = pane_layout(area, &app.ui_state.pane_config());
    let anchor = match drag.which {
        Splitter::List => panes.list.x,
        // The inspector hugs the body's right edge, so its outer edge stays
        // put while the width moves (`x + width` is stable mid-drag).
        Splitter::Inspector => panes.inspector.x.saturating_add(panes.inspector.width),
    };
    let w = splitter_drag_width(drag.which, pointer_x, drag.grab_off, anchor);
    app.ui_state.set_pane_width(drag.which, w, area.width);
}

/// The `DragStart` arm: a left-press drag's press cell picks its surface
/// once and the stored drag tracks the pointer from then on. The
/// assignment also clears any drag a superseding press left (a press on a
/// non-drag surface begins nothing). The first tracked step applies
/// immediately — the press cell is already the drag's first point.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_drag_start(
    app: &mut App,
    pane_drag: &mut Option<PaneDrag>,
    button: MouseButton,
    origin: ratatui::layout::Position,
    at: ratatui::layout::Position,
    chrome: &ChromeGeom,
    lineage: Option<&Layout>,
    area: Rect,
) {
    if button == MouseButton::Left {
        *pane_drag = begin_pane_drag(app, lineage, area, origin, chrome);
        if let Some(d) = pane_drag.as_mut() {
            apply_pane_drag(app, d, chrome, lineage, area, at);
        }
    }
}

/// The `DragMove` arm: a stored drag tracks the pointer — mid-drag motion
/// never re-hit-tests the surface, so a pointer dragged past a bound keeps
/// the grabbed surface's tracking.
pub(super) fn handle_drag_move(
    app: &mut App,
    pane_drag: &mut Option<PaneDrag>,
    at: ratatui::layout::Position,
    chrome: &ChromeGeom,
    lineage: Option<&Layout>,
    area: Rect,
) {
    if let Some(d) = pane_drag.as_mut() {
        apply_pane_drag(app, d, chrome, lineage, area, at);
    }
}

/// The `DragEnd` arm: finish the live drag — or, for a flick that
/// delivered no Drag events, resolve the press cell once before clearing.
pub(super) fn handle_drag_end(
    app: &mut App,
    pane_drag: &mut Option<PaneDrag>,
    origin: ratatui::layout::Position,
    at: ratatui::layout::Position,
    chrome: &ChromeGeom,
    lineage: Option<&Layout>,
    area: Rect,
) {
    if pane_drag.is_none() {
        // A terminal can deliver no Drag events for a fast flick (press →
        // release elsewhere): the router still reports DragEnd, so resolve
        // the press cell like DragStart before finishing.
        let mut d = begin_pane_drag(app, lineage, area, origin, chrome);
        if let Some(d) = d.as_mut() {
            apply_pane_drag(app, d, chrome, lineage, area, at);
        }
    }
    // A tracked drag needs NO release apply: its last DragMove already
    // committed the release cell's slot — re-resolving would hit-test a
    // surface the last slide just rearranged (the dragged tab's own cell
    // may no longer cover the pointer).
    *pane_drag = None;
}
