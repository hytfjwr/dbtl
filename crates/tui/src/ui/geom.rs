//! Pure screen geometry: pane rects, pane interiors, and the centered-overlay
//! rect. Computed the same way by `render` and by the tests, so a test can
//! isolate exactly the lineage pane interior. No state here.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::widgets::{Block, Borders};

use crate::model_list::DisplayRow;
use crate::{ModelList, NodeRect};

/// Pane-width policy (screen-spec §1/§5), in columns — never ratios, so a
/// resized terminal can't silently change a saved width.
/// The list pane's default and minimum width in columns.
pub const DEFAULT_LIST_WIDTH: u16 = 34;
pub const MIN_LIST_WIDTH: u16 = 18;
/// The inspector pane's default and minimum width in columns (the pane itself
/// arrives in Issue 04; the width pref exists ahead of it).
pub const DEFAULT_INSPECTOR_WIDTH: u16 = 42;
pub const MIN_INSPECTOR_WIDTH: u16 = 30;
/// The lineage pane's minimum width in columns — the floor every other pane
/// gives room to.
pub const MIN_LINEAGE_WIDTH: u16 = 40;
/// The list pane's maximum width: 60% of the terminal width.
const LIST_MAX_PCT: u16 = 60;
/// The inspector pane's maximum width: 50% of the terminal width.
const INSPECTOR_MAX_PCT: u16 = 50;

/// Which pane boundary a screen point sits on — the drag target's identity
/// (which saved width the drag edits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Splitter {
    /// Between the list and lineage panes — resizes `list_width`.
    List,
    /// Between the lineage and inspector panes — resizes `inspector_width`.
    Inspector,
}

impl Splitter {
    /// The pane's status-text name (the drag hint's `"list 34 cols"`).
    pub fn label(self) -> &'static str {
        match self {
            Splitter::List => "list",
            Splitter::Inspector => "inspector",
        }
    }
}

/// The layout inputs [`pane_layout`] resolves: which panes exist and the
/// user's saved column widths (columns, not ratios — §5). One value covers
/// every pane so Issues 03 (tab bar) and 04 (inspector) extend the struct,
/// not the signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneConfig {
    /// Show the left model-list pane (the Ctrl-b toggle).
    pub list_visible: bool,
    /// The list pane's saved width in columns.
    pub list_width: u16,
    /// Show the right inspector pane (Issue 04 fills it; until then the field
    /// exists so turning it on is a flag flip, not a redesign).
    pub inspector_visible: bool,
    /// The inspector pane's saved width in columns.
    pub inspector_width: u16,
    /// Show the one-row tab bar above the body (Issue 03 fills it; until then
    /// the field exists so turning it on is a flag flip, not a redesign).
    pub tab_bar: bool,
}

impl Default for PaneConfig {
    /// The spec §1 defaults: list shown at 34, inspector hidden at a saved 42,
    /// no tab bar.
    fn default() -> Self {
        PaneConfig {
            list_visible: true,
            list_width: DEFAULT_LIST_WIDTH,
            inspector_visible: false,
            inspector_width: DEFAULT_INSPECTOR_WIDTH,
            tab_bar: false,
        }
    }
}

/// The screen regions: the tab-bar strip, the bordered list / lineage /
/// inspector panes, the bottom status line, and the draggable splitter
/// strips. The single source of truth the renderer, the mouse hit-test, and
/// the event loop's viewport math all read — they can never disagree.
#[derive(Debug, Clone, Copy)]
pub struct PaneRects {
    /// The tab-bar strip above the body (zero height until Issue 03).
    pub tab_bar: Rect,
    /// Left model-list pane (with border). Zero-width while hidden or fully
    /// squeezed on a narrow terminal (mouse `within` tests on it are then
    /// always false).
    pub list: Rect,
    /// Centre lineage pane (with border).
    pub lineage: Rect,
    /// Right inspector pane (with border; zero width until Issue 04 shows it).
    pub inspector: Rect,
    /// Bottom status/help line.
    pub status: Rect,
    /// The list|lineage splitter strip: the adjacent two border columns where
    /// the panes meet, spanning the body (either side is grabbable — §5).
    /// `None` when no boundary exists to grab (a neighbour has no border).
    pub list_splitter: Option<Rect>,
    /// The lineage|inspector splitter strip — `None` while the inspector is
    /// hidden (everywhere today; Issue 04 turns it on).
    pub inspector_splitter: Option<Rect>,
}

impl PaneRects {
    /// The strip for a boundary (`None` when that boundary isn't shown).
    pub fn splitter(&self, which: Splitter) -> Option<Rect> {
        match which {
            Splitter::List => self.list_splitter,
            Splitter::Inspector => self.inspector_splitter,
        }
    }
}

/// Clamp a requested list-pane width to the §1 floor and the terminal-width
/// cap (`terminal_w` columns). The floor wins over the cap on a degenerate
/// terminal (the squeeze below the floor is the layout's job, not the pref's).
pub fn clamp_list_width(w: u16, terminal_w: u16) -> u16 {
    // u32 math like `centered_rect`: `w * 60` overflows u16 past ~1000 cols.
    let cap = (terminal_w as u32 * LIST_MAX_PCT as u32 / 100) as u16;
    w.clamp(MIN_LIST_WIDTH, cap.max(MIN_LIST_WIDTH))
}

/// Clamp a requested inspector-pane width to the §1 floor and cap, like
/// [`clamp_list_width`].
pub fn clamp_inspector_width(w: u16, terminal_w: u16) -> u16 {
    let cap = (terminal_w as u32 * INSPECTOR_MAX_PCT as u32 / 100) as u16;
    w.clamp(MIN_INSPECTOR_WIDTH, cap.max(MIN_INSPECTOR_WIDTH))
}

/// The width a splitter drag asks for: `at_x` the pointer column, `grab_off`
/// the press's offset inside the 2-column strip (`0` = the left pane's border
/// cell was grabbed, `1` = the right pane's), and `anchor` the dragged pane's
/// FIXED outer edge (the list's left edge — its `x`; the inspector's right
/// edge — its `x + width`; both sit at a terminal edge in this layout).
/// Saturates at 0; the §1 min/max clamp is the caller's
/// (`clamp_list_width`/`clamp_inspector_width`), so a drag past a bound
/// clamps instead of sticking.
pub fn splitter_drag_width(which: Splitter, at_x: u16, grab_off: u16, anchor: u16) -> u16 {
    // The split point tracks the grabbed border cell under the pointer.
    let edge = i32::from(at_x) - i32::from(grab_off) + 1;
    let raw = match which {
        Splitter::List => edge - i32::from(anchor),
        Splitter::Inspector => i32::from(anchor) - edge,
    };
    raw.max(0) as u16
}

/// Split a total `area` into the pane rects under `cfg`. Vertical: tab bar
/// (0/1 rows) + body + one status row. Horizontal inside the body: list +
/// lineage + inspector. The single source of truth for screen geometry.
///
/// Widths are the saved column counts clamped to §1's min/max (60% / 50% of
/// the terminal). When the terminal is too narrow to honour every minimum,
/// the DISPLAYED widths shrink — inspector first, then the list, each toward
/// its floor and then below it — while the lineage's 40-column floor holds to
/// the last. The saved values in `cfg` are never touched (§5).
///
/// With `cfg.list_visible = false` the list rect is zero-width (the lineage
/// pane takes the freed space) so mouse `within` tests on it stay false.
pub fn pane_layout(area: Rect, cfg: &PaneConfig) -> PaneRects {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(u16::from(cfg.tab_bar)),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let (tab_bar, body, status) = (chunks[0], chunks[1], chunks[2]);

    // Desired widths: saved values clamped to each pane's §1 floor and
    // terminal-width cap.
    let mut list_w = if cfg.list_visible {
        clamp_list_width(cfg.list_width, body.width)
    } else {
        0
    };
    let mut inspector_w = if cfg.inspector_visible {
        clamp_inspector_width(cfg.inspector_width, body.width)
    } else {
        0
    };
    // The deficit under the lineage's 40-column floor — how much the three
    // panes over-spend the body — is absorbed in §5 order (inspector first,
    // then the list), first down to each pane's floor, then below it (a
    // width can reach 0: the pane simply isn't drawn and no splitter exists
    // to grab). u32 math: `list + inspector + floor` can exceed u16::MAX.
    let deficit = |list_w: u16, inspector_w: u16| -> u32 {
        (u32::from(list_w) + u32::from(inspector_w) + u32::from(MIN_LINEAGE_WIDTH))
            .saturating_sub(u32::from(body.width))
    };
    let mut d = deficit(list_w, inspector_w);
    if d > 0 {
        // Pass 1: honour the floors — each pane gives up only its surplus.
        let take = d.min(u32::from(inspector_w.saturating_sub(MIN_INSPECTOR_WIDTH))) as u16;
        inspector_w -= take;
        d -= u32::from(take);
        let take = d.min(u32::from(list_w.saturating_sub(MIN_LIST_WIDTH))) as u16;
        list_w -= take;
        d -= u32::from(take);
        // Pass 2: the floors themselves give way, same order.
        let take = d.min(u32::from(inspector_w)) as u16;
        inspector_w -= take;
        d -= u32::from(take);
        list_w = list_w.saturating_sub(d.min(u32::from(list_w)) as u16);
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(list_w),
            Constraint::Min(0),
            Constraint::Length(inspector_w),
        ])
        .split(body);
    let (list, lineage, inspector) = (panes[0], panes[1], panes[2]);

    // A splitter strip is the adjacent two border columns where the panes
    // meet — it exists only while both neighbours have a border to grab.
    let strip = |left: Rect, right: Rect| -> Option<Rect> {
        (left.width > 0 && right.width > 0).then(|| Rect {
            x: left.x + left.width - 1,
            y: body.y,
            width: 2,
            height: body.height,
        })
    };
    PaneRects {
        tab_bar,
        list,
        lineage,
        inspector,
        status,
        list_splitter: strip(list, lineage),
        inspector_splitter: strip(lineage, inspector),
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
    let thumb_top = (offset.min(max_off) * max_thumb_top)
        .checked_div(max_off)
        .unwrap_or(0);
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
    if !interior.contains(Position {
        x: click_x,
        y: click_y,
    }) {
        return None; // outside the lineage pane interior
    }
    let gx = scroll_x + (click_x - interior.x) as usize;
    let gy = scroll_y + (click_y - interior.y) as usize;
    rects
        .iter()
        .find(|(_, r)| gx >= r.x && gx < r.x + r.width && gy >= r.y && gy < r.y + r.height)
        .map(|(uid, _)| uid.clone())
}

/// What a screen cell sits on — the answer the mouse router routes clicks,
/// drags, and hover through. New interactive surfaces (tabs, the inspector,
/// menus, status chips) extend this enum; the event loop resolves a point to
/// a target via [`hit_target`] and then decides what a click there means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// A model row in the list pane: the model's selection-space index into
    /// `ModelList::models` (re-resolved to a `unique_id` at click time).
    ListRow(usize),
    /// A group header row in the list pane: the layer key (e.g. `"staging"`).
    ListHeader(String),
    /// List pane interior below the last display row. The lookup is per-row —
    /// a point anywhere left or right of a drawn row still reports THAT row's
    /// target, so blank space only exists under the rows.
    ListBlank,
    /// A lineage node box: its `unique_id` (or a `+M more` marker's).
    LineageNode(String),
    /// Lineage pane interior with no node under the point (includes the
    /// centering pad around a fully-fitting diagram).
    LineageBlank,
    /// The lineage minimap inset. It overlays real nodes, so a point inside it
    /// reports `Minimap` and never the node below.
    Minimap,
    /// A shared boundary between adjacent panes — the two border columns where
    /// the panes' edges meet (screen-spec §5's drag-to-resize target). The
    /// payload says WHICH boundary, so the drag/double-click edits the right
    /// saved width.
    PaneBorder(Splitter),
    /// The bottom status line.
    StatusBar,
    /// Inside a modal overlay. Reserved for Issue 06 (mouse inside modals) —
    /// nothing resolves it yet because `Mode::Selection` gating runs first.
    ModalInterior,
    /// Outside a modal overlay while it owns the screen. Reserved for Issue
    /// 06's click-outside-to-dismiss.
    ModalExterior,
}

/// The pure inputs [`hit_target`] needs, resolved once per mouse event by the
/// (only) size-aware layer, the event loop. New surfaces extend this bundle
/// rather than growing the signature — the loop already computes every field
/// for drawing, so the hit-test always sees what the frame drew.
pub struct HitGeometry<'a> {
    /// The frame's pane rects (`pane_layout(area, &pane_config)`).
    pub panes: PaneRects,
    /// The active model list (its `rows` interleave group headers).
    pub list: &'a ModelList,
    /// First visible display row (`UiState::offset`).
    pub list_offset: usize,
    /// The styled layout the lineage pane draws (`None` = empty diagram).
    pub layout: Option<&'a crate::Layout>,
    /// Lineage scroll offsets (CharGrid space).
    pub scroll_x: usize,
    /// Ditto, vertical.
    pub scroll_y: usize,
    /// Whether the minimap inset is on — its cells never reach nodes below.
    pub minimap: bool,
}

/// Map a screen cell `(col, row)` to its [`HitTarget`], or `None` when it hits
/// no interactive surface (pane chrome, or a dead zone between targets).
///
/// Every region is measured through the SAME pure geometry the renderer uses —
/// `pane_layout`/`pane_interior` for the panes, [`lineage_content_rect`] for
/// the diagram, [`minimap_rect`] for the inset — so "computed window == drawn
/// window" holds for hit-testing too. The splitters are checked BEFORE the
/// interiors (their cells overlap the panes' border columns), and the minimap
/// BEFORE the node hit-test: it is drawn over the diagram, so its cells must
/// not fall through.
pub fn hit_target(g: &HitGeometry, col: u16, row: u16) -> Option<HitTarget> {
    let p = g.panes;
    let at = Position { x: col, y: row };
    if p.status.contains(at) {
        return Some(HitTarget::StatusBar);
    }
    for which in [Splitter::List, Splitter::Inspector] {
        if p.splitter(which).is_some_and(|r| r.contains(at)) {
            return Some(HitTarget::PaneBorder(which));
        }
    }
    let list_inner = pane_interior(p.list);
    if list_inner.contains(at) {
        let r = g.list_offset + (row - list_inner.y) as usize;
        return Some(match g.list.rows.get(r) {
            Some(DisplayRow::Model { model_index, .. }) => HitTarget::ListRow(*model_index),
            Some(DisplayRow::Header { layer, .. }) => HitTarget::ListHeader(layer.clone()),
            None => HitTarget::ListBlank,
        });
    }
    let lin_inner = pane_interior(p.lineage);
    if lin_inner.contains(at) {
        if let Some(lay) = g.layout {
            if let Some(mm) = minimap_rect(
                lin_inner,
                lay.grid.width(),
                lay.grid.height(),
                g.scroll_x,
                g.scroll_y,
                g.minimap,
            ) {
                if mm.contains(at) {
                    return Some(HitTarget::Minimap);
                }
            }
            let content = lineage_content_rect(lin_inner, lay.grid.width(), lay.grid.height());
            if let Some(uid) = hit_test(&lay.rects, g.scroll_x, g.scroll_y, content, col, row) {
                return Some(HitTarget::LineageNode(uid));
            }
        }
        return Some(HitTarget::LineageBlank);
    }
    None
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PaneConfig {
        PaneConfig::default()
    }

    #[test]
    fn pane_layout_default_is_list_34_plus_lineage() {
        // §1: the list defaults to 34 columns regardless of terminal width —
        // NOT a ratio (the old 40/60 split is gone).
        for w in [80u16, 100, 160, 220] {
            let area = Rect::new(0, 0, w, 30);
            let p = pane_layout(area, &cfg());
            assert_eq!(p.list.width, DEFAULT_LIST_WIDTH, "{w}: list is 34");
            assert_eq!(p.list, Rect::new(0, 0, 34, 29), "{w}: list spans the body");
            assert_eq!(
                p.lineage,
                Rect::new(34, 0, w - 34, 29),
                "{w}: lineage takes the rest"
            );
            assert_eq!(p.inspector.width, 0, "{w}: inspector hidden by default");
            assert_eq!(p.tab_bar.height, 0, "{w}: tab bar hidden by default");
            assert_eq!(
                p.status,
                Rect::new(0, 29, w, 1),
                "{w}: status is the last row"
            );
        }
    }

    #[test]
    fn pane_layout_clamps_saved_width_to_min_and_cap() {
        // A saved width below the floor lifts to it; above the 60% cap drops
        // to it (the cap is terminal-width-relative, unlike the floor).
        let area = Rect::new(0, 0, 100, 30);
        let low = pane_layout(
            area,
            &PaneConfig {
                list_width: 4,
                ..cfg()
            },
        );
        assert_eq!(low.list.width, MIN_LIST_WIDTH, "saved 4 -> floor 18");
        let high = pane_layout(
            area,
            &PaneConfig {
                list_width: 200,
                ..cfg()
            },
        );
        assert_eq!(high.list.width, 60, "saved 200 -> cap 60% of 100");
        // A bigger terminal raises the cap with it.
        let wide = pane_layout(
            Rect::new(0, 0, 200, 30),
            &PaneConfig {
                list_width: 200,
                ..cfg()
            },
        );
        assert_eq!(wide.list.width, 120, "cap tracks the terminal");
    }

    #[test]
    fn pane_layout_squeezes_displayed_width_below_floor_when_narrow() {
        // §5: the saved value is untouched while the DISPLAYED width shrinks.
        // body 50: desired 34 + 0 leaves lineage 16 < 40 — the list gives up
        // first its surplus to the floor, then below it.
        let cfg = PaneConfig {
            list_width: 34,
            ..cfg()
        };
        let p = pane_layout(Rect::new(0, 0, 50, 30), &cfg);
        assert_eq!(p.list.width, 10, "50 - 40 = a 10-column list display");
        assert_eq!(p.lineage.width, MIN_LINEAGE_WIDTH);
        assert_eq!(cfg.list_width, 34, "the saved width never changes");
    }

    #[test]
    fn pane_layout_hidden_list_gives_lineage_the_full_body() {
        let area = Rect::new(0, 0, 100, 30);
        let hidden = pane_layout(
            area,
            &PaneConfig {
                list_visible: false,
                ..cfg()
            },
        );
        assert_eq!(hidden.list.width, 0, "hidden list is zero-width");
        assert_eq!(
            hidden.lineage,
            Rect::new(0, 0, 100, 29),
            "lineage takes the whole body (status line excluded)"
        );
        assert_eq!(hidden.list_splitter, None, "no boundary to grab");
        assert_eq!(
            hidden.status,
            Rect::new(0, 29, 100, 1),
            "status line is unaffected"
        );
    }

    #[test]
    fn pane_layout_splitter_is_the_adjacent_two_border_columns() {
        // §5's strip: the list's right border column plus the lineage's left
        // border column, spanning the whole body — either side grabs.
        let p = pane_layout(Rect::new(0, 0, 100, 30), &cfg());
        assert_eq!(
            p.list_splitter,
            Some(Rect::new(33, 0, 2, 29)),
            "the 2-column strip between the panes"
        );
        // The strip's cells resolve to the List boundary, not to a pane
        // interior — verified against hit_target in the app tests.
    }

    #[test]
    fn pane_layout_inspector_takes_the_right_columns() {
        // Issue 04's pane, driven through the same config: a 160-col terminal
        // shows list 34 + inspector 42 + the lineage in between, with a second
        // splitter on the lineage|inspector edge.
        let p = pane_layout(
            Rect::new(0, 0, 160, 30),
            &PaneConfig {
                inspector_visible: true,
                ..cfg()
            },
        );
        assert_eq!(p.list.width, 34);
        assert_eq!(p.inspector, Rect::new(118, 0, 42, 29));
        assert_eq!(p.lineage, Rect::new(34, 0, 84, 29));
        assert_eq!(p.inspector_splitter, Some(Rect::new(117, 0, 2, 29)));
        assert_eq!(p.list_splitter, Some(Rect::new(33, 0, 2, 29)));
    }

    #[test]
    fn pane_layout_tab_bar_claims_a_row_above_the_body() {
        // Issue 03's strip: with the flag on, the body starts one row lower —
        // status is untouched.
        let p = pane_layout(
            Rect::new(0, 0, 100, 30),
            &PaneConfig {
                tab_bar: true,
                ..cfg()
            },
        );
        assert_eq!(p.tab_bar, Rect::new(0, 0, 100, 1));
        assert_eq!(p.list.y, 1, "the body moves down one row");
        assert_eq!(p.list.height, 28);
        assert_eq!(p.status, Rect::new(0, 29, 100, 1));
        assert_eq!(p.list_splitter, Some(Rect::new(33, 1, 2, 28)));
    }

    #[test]
    fn pane_layout_narrow_squeeze_prefers_lineage_floor_then_order() {
        // Inspector shown at 90 cols: desired 34 + 42 leaves lineage 14.
        // Pass 1 (honour floors): inspector 42->30, list 34->20 — every floor
        // kept. The saved values stay intact.
        let cfg = PaneConfig {
            inspector_visible: true,
            ..cfg()
        };
        let p = pane_layout(Rect::new(0, 0, 90, 30), &cfg);
        assert_eq!(
            (p.list.width, p.lineage.width, p.inspector.width),
            (20, 40, 30),
            "surplus shed in inspector->list order, floors held"
        );
        // 80 cols: floors can't all hold — the inspector's own floor breaks
        // first (the least essential pane), the list keeps 18.
        let p = pane_layout(Rect::new(0, 0, 80, 30), &cfg);
        assert_eq!(
            (p.list.width, p.lineage.width, p.inspector.width),
            (18, 40, 22),
            "inspector dips below its floor before the list does"
        );
        // 55 cols: inspector is squeezed to nothing, then the list.
        let p = pane_layout(Rect::new(0, 0, 55, 30), &cfg);
        assert_eq!(p.inspector.width, 0, "inspector is fully squeezed first");
        assert_eq!(p.inspector_splitter, None, "no edge to grab on a 0 pane");
        assert_eq!((p.list.width, p.lineage.width), (15, 40));
    }

    #[test]
    fn pane_layout_max_cap_interacts_with_squeeze() {
        // A saved width past the cap on a narrow terminal: cap first, then the
        // squeeze decides (cap 60% of 60 = 36, then lineage floor shrinks it).
        let p = pane_layout(
            Rect::new(0, 0, 60, 30),
            &PaneConfig {
                list_width: 80,
                ..cfg()
            },
        );
        assert_eq!(p.list.width, 20, "60 - 40; the cap already held it to 36");
        assert_eq!(p.lineage.width, 40);
    }

    #[test]
    fn pane_layout_degenerate_sizes_never_panic() {
        for (w, h) in [(1u16, 1u16), (3, 3), (10, 2), (40, 5), (18, 10)] {
            for tab_bar in [false, true] {
                for inspector in [false, true] {
                    let p = pane_layout(
                        Rect::new(0, 0, w, h),
                        &PaneConfig {
                            tab_bar,
                            inspector_visible: inspector,
                            ..cfg()
                        },
                    );
                    // The panes tile the body without overlap or overflow.
                    assert_eq!(
                        p.list.width + p.lineage.width + p.inspector.width,
                        w,
                        "{w}x{h}: panes tile the full body width"
                    );
                    assert_eq!(
                        p.list.x + p.list.width,
                        p.lineage.x,
                        "{w}x{h}: list|lineage abut"
                    );
                    assert_eq!(
                        p.lineage.x + p.lineage.width,
                        p.inspector.x,
                        "{w}x{h}: lineage|inspector abut"
                    );
                    assert!(
                        p.inspector.x + p.inspector.width <= w,
                        "{w}x{h}: inspector stays inside the body"
                    );
                }
            }
        }
    }

    #[test]
    fn clamp_widths_floor_and_terminal_cap() {
        assert_eq!(clamp_list_width(0, 100), 18);
        assert_eq!(clamp_list_width(34, 100), 34);
        assert_eq!(clamp_list_width(80, 100), 60, "60% cap");
        assert_eq!(clamp_inspector_width(0, 100), 30);
        assert_eq!(clamp_inspector_width(80, 100), 50, "50% cap");
        // Degenerate terminal: the floor wins over a cap below it.
        assert_eq!(clamp_list_width(34, 20), 18);
        assert_eq!(clamp_inspector_width(34, 20), 30);
    }

    #[test]
    fn splitter_drag_width_tracks_the_grabbed_cell() {
        // List splitter: the dragged border lands under the pointer. Anchor is
        // the list's fixed left edge (x=0).
        assert_eq!(splitter_drag_width(Splitter::List, 50, 0, 0), 51);
        assert_eq!(splitter_drag_width(Splitter::List, 50, 1, 0), 50);
        // Grabbed the right-hand cell (the lineage's border): same edge
        // tracking, one column over.
        assert_eq!(splitter_drag_width(Splitter::List, 34, 1, 0), 34);
        // Inspector splitter: grows leftward from the terminal's right edge.
        assert_eq!(splitter_drag_width(Splitter::Inspector, 118, 1, 160), 42);
        assert_eq!(splitter_drag_width(Splitter::Inspector, 117, 0, 160), 42);
        assert_eq!(splitter_drag_width(Splitter::Inspector, 100, 0, 160), 59);
        // Out-of-range intent saturates at 0 for the caller to clamp.
        assert_eq!(splitter_drag_width(Splitter::List, 0, 1, 0), 0);
        assert_eq!(splitter_drag_width(Splitter::Inspector, 200, 0, 160), 0);
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
