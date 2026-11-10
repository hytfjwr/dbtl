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

/// The smallest terminal the normal UI renders on (§12): below this in
/// either dimension the event loop draws only the `terminal too small`
/// screen — no panes, no overlays, no pointer routing.
pub const MIN_TERMINAL_W: u16 = 50;
/// The minimum terminal height, paired with [`MIN_TERMINAL_W`].
pub const MIN_TERMINAL_H: u16 = 12;

/// Below this terminal width the list never sits inline (§12): the saved
/// `list_visible` pref is overridden and the list exists only as the
/// overlay drawer `Ctrl-b` opens. The band edge is exclusive — at exactly
/// [`LIST_AUTO_HIDE_W`] columns the inline pane layout applies again.
pub const LIST_AUTO_HIDE_W: u16 = 80;
/// The list drawer's share cap in the narrow band: `min(34, 60%)` —
/// the default list width, bounded by the same 60% the inline pane caps at.
const LIST_DRAWER_PCT: u16 = 60;

/// The `inspector: auto` visibility floor (§4/§12): the auto pref shows the
/// pane at this many columns or wider, never below it.
pub const INSPECTOR_AUTO_W: u16 = 160;

/// Whether the terminal falls under §12's minimum usable size — the
/// whole-UI yield condition the event loop checks before drawing or
/// routing any input.
pub fn terminal_too_small(area: Rect) -> bool {
    area.width < MIN_TERMINAL_W || area.height < MIN_TERMINAL_H
}

/// The narrow-band list drawer's width for a body `body_w` columns wide:
/// `min(34, 60%)` per §12 — the default list width, capped by the same
/// terminal share the inline pane gets. Never called above the band
/// ([`pane_layout`] gates the rect itself).
pub fn list_drawer_width(body_w: u16) -> u16 {
    // u32 math like `clamp_list_width`: `w * 60` overflows u16 past ~1000.
    DEFAULT_LIST_WIDTH.min((body_w as u32 * LIST_DRAWER_PCT as u32 / 100) as u16)
}

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
    /// Open the list as the narrow-band overlay drawer (the <80-columns
    /// meaning of `Ctrl-b`, §12). Inert above the band — `pane_layout` only
    /// ever carves the rect while `body.width < LIST_AUTO_HIDE_W`, so a stale
    /// flag is ignored rather than honoured.
    pub list_drawer: bool,
}

impl Default for PaneConfig {
    /// The spec §1 defaults: list shown at 34, inspector hidden at a saved 42,
    /// no tab bar, no drawer.
    fn default() -> Self {
        PaneConfig {
            list_visible: true,
            list_width: DEFAULT_LIST_WIDTH,
            inspector_visible: false,
            inspector_width: DEFAULT_INSPECTOR_WIDTH,
            tab_bar: false,
            list_drawer: false,
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
    /// The narrow-band list drawer (§12): the same bordered list, floating
    /// over the lineage pane's left edge at `min(34, 60%)` of the body.
    /// `Some` only while the body is narrower than [`LIST_AUTO_HIDE_W`] AND
    /// `cfg.list_drawer` is set — above the band the flag is ignored, and
    /// below it the inline `list` rect is always zero-width instead.
    pub list_drawer: Option<Rect>,
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
    // terminal-width cap. §12's narrow band overrides the list pref FIRST:
    // below `LIST_AUTO_HIDE_W` columns the pane never sits inline — the
    // drawer below is the only list surface there, so the explicit `Ctrl-b`
    // pref wins everywhere except the mandatory drawer band.
    let mut list_w = if cfg.list_visible && body.width >= LIST_AUTO_HIDE_W {
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
    // §12's list drawer: the narrow band's only list surface — the same
    // bordered pane floated over the lineage's left edge, full body height,
    // `min(34, 60%)` wide. Gated on the band itself so a stale flag at ≥80
    // columns resolves to `None` exactly like a closed one.
    let list_drawer = (cfg.list_drawer && body.width < LIST_AUTO_HIDE_W).then(|| Rect {
        x: body.x,
        y: body.y,
        width: list_drawer_width(body.width).min(body.width),
        height: body.height,
    });
    PaneRects {
        tab_bar,
        list,
        lineage,
        inspector,
        status,
        list_splitter: strip(list, lineage),
        inspector_splitter: strip(lineage, inspector),
        list_drawer,
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

/// The widest cell a modal may grow to: `min(80% of the terminal, 120
/// columns)` (screen-spec §9 — on wide terminals a short sentence in a
/// 160-col box is unreadable).
const MODAL_MAX_W: u16 = 120;

/// Every scrollable modal's outer rect: `min(80%, 120 cols)` × 80% of `area`,
/// centred. The SINGLE source for modal geometry — the renderers draw it, the
/// scroll clamps count with it, and the mouse hit-test routes by it (computed
/// rect == drawn rect == tested rect).
pub fn modal_rect(area: Rect) -> Rect {
    let w = ((area.width as u32 * 80 / 100).max(1) as u16)
        .min(MODAL_MAX_W)
        .min(area.width.max(1));
    let h = ((area.height as u32 * 80 / 100).max(1) as u16).min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// The `y` which-key menu's frame rect: content-sized (the longest
/// `y<key>  help` row + padding), horizontally centred, hugging the bottom
/// edge (which-key convention). Shared by `draw_yank_menu` and the mouse
/// hit-test so a row click lands on the drawn row. `None` on terminals too
/// small to host the box (the draw skips it there too).
pub fn yank_menu_rect(area: Rect) -> Option<Rect> {
    let rows = crate::action::yank_bindings().count();
    // Row shape: `"  y<key>"` (6 cells) + `" help"` — measured without
    // styling so the hit-test can rebuild the same box the draw produced.
    let row_w = crate::action::yank_bindings()
        .map(|b| 6 + 1 + b.help.chars().count())
        .max()
        .unwrap_or(10);
    let width = (row_w as u16)
        .saturating_add(4)
        .min(area.width.saturating_sub(2));
    let height = (rows as u16 + 2).min(area.height);
    if width < 8 || height < 4 {
        return None;
    }
    Some(
        Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height + 1),
            width,
            height,
        }
        .intersection(area),
    )
}

/// Resolve a [`MenuAnchor`](crate::action::MenuAnchor) to the two screen
/// cells [`menu_rect`] hangs the box off: the preferred corner sits
/// bottom-right of the first, the flipped corner ends at the second.
/// `Cell` is the pointer cell for both; `Node` hangs the menu off the
/// target's drawn box (preferred = its top-right corner; flipped = its
/// left / bottom edges), falling back to the screen centre when the target
/// is not drawn (off-screen data, a stale target).
fn menu_anchors(
    ms: &crate::action::MenuState,
    area: Rect,
    lin_inner: Rect,
    layout: Option<&crate::Layout>,
    scroll_x: usize,
    scroll_y: usize,
) -> ((u16, u16), (u16, u16)) {
    match ms.anchor {
        crate::action::MenuAnchor::Cell { col, row } => ((col, row), (col, row)),
        crate::action::MenuAnchor::Node => {
            // The target's drawn box, mapped grid → screen through the same
            // content-rect origin + scroll the hit-test uses.
            let node = layout.and_then(|lay| {
                let r = lay.rects.get(&ms.target)?;
                let content = lineage_content_rect(lin_inner, lay.grid.width(), lay.grid.height());
                let sx = (content.x as usize + r.x).saturating_sub(scroll_x);
                let sy = (content.y as usize + r.y).saturating_sub(scroll_y);
                Some((sx, sy, r.width, r.height))
            });
            match node {
                Some((x, y, w, h)) => (
                    // Preferred: hang off the box's top-right corner cell →
                    // the menu's top-left lands exactly at the box's right
                    // edge, level with its top row.
                    ((x + w).saturating_sub(1) as u16, y.saturating_sub(1) as u16),
                    // Flipped: the menu's right edge ends at the box's left
                    // edge; its bottom aligns with the box's bottom edge.
                    (x as u16, (y + h) as u16),
                ),
                None => {
                    let c = (area.x + area.width / 2, area.y + area.height / 2);
                    (c, c)
                }
            }
        }
    }
}

/// The node context menu's frame rect: sized to its `label  key` rows plus
/// borders, placed bottom-right of the anchor and flipped to the other side
/// when the preferred box would overflow the terminal (screen-spec §9's
/// "右下（はみ出すなら反転）" rule). Shared by `draw_menu` and the mouse
/// hit-test — the tested rect is the drawn rect.
pub fn menu_rect(
    area: Rect,
    lin_inner: Rect,
    layout: Option<&crate::Layout>,
    scroll_x: usize,
    scroll_y: usize,
    ms: &crate::action::MenuState,
) -> Rect {
    // Width: `  label  key` — two cells of lead-in, the longest label, two
    // cells of gap, the widest key label (from the binding rows), and the two
    // border columns. The title (`{name}`) can only shrink the box, never
    // widen it past what fits.
    let label_w = ms
        .items
        .iter()
        .map(|e| match e {
            crate::action::MenuEntry::Item { label, .. } => label.chars().count(),
            crate::action::MenuEntry::Separator => 0,
        })
        .max()
        .unwrap_or(0);
    let key_w = ms
        .items
        .iter()
        .filter_map(|e| match e {
            crate::action::MenuEntry::Item { binding, .. } => Some(
                crate::action::BINDINGS[*binding]
                    .key_label()
                    .chars()
                    .count(),
            ),
            crate::action::MenuEntry::Separator => None,
        })
        .max()
        .unwrap_or(0);
    let title_w = ms.name.chars().count() + 2;
    let want_w = (2 + label_w + 2 + key_w).max(title_w) as u16 + 2;
    let w = want_w.min(area.width.max(1)).max(1);
    let h = (ms.items.len() as u16 + 2).min(area.height.max(1)).max(1);

    let (anchor, flip) = menu_anchors(ms, area, lin_inner, layout, scroll_x, scroll_y);
    let mut x = anchor.0 + 1;
    if x + w > area.x + area.width {
        x = flip.0.saturating_sub(w);
    }
    let mut y = anchor.1 + 1;
    if y + h > area.y + area.height {
        y = flip.1.saturating_sub(h);
    }
    Rect {
        x: x.max(area.x).min(area.x + area.width.saturating_sub(w)),
        y: y.max(area.y).min(area.y + area.height.saturating_sub(h)),
        width: w,
        height: h,
    }
}

/// The outer rect of the covering overlay `mode` draws, if any: the modal
/// box for the scrollable modals, the palette's own box, the `y` which-key
/// menu, the node context menu (anchored via the drawn lineage layout).
/// `None` for non-covering modes (Selection / Search) — their panes stay
/// live underneath. The SINGLE overlay-rect authority: the dimming pass
/// inverts it, the overlay draws into it, and the hit-test routes by it —
/// "outside = dismiss, inside = the overlay's own cells".
pub fn overlay_rect(
    mode: &crate::action::Mode,
    area: Rect,
    lin_inner: Rect,
    layout: Option<&crate::Layout>,
    scroll_x: usize,
    scroll_y: usize,
) -> Option<Rect> {
    use crate::action::Mode;
    match mode {
        Mode::Help(_)
        | Mode::Detail(_)
        | Mode::Sql(_)
        | Mode::Stats(_)
        | Mode::Diff(_)
        | Mode::Columns(_)
        | Mode::Notifications(_) => Some(modal_rect(area)),
        Mode::Palette(_) => Some(centered_rect(60, 60, area).intersection(area)),
        Mode::Yank { .. } => yank_menu_rect(area),
        Mode::Menu(ms) => Some(menu_rect(area, lin_inner, layout, scroll_x, scroll_y, ms)),
        Mode::Selection | Mode::Search(_) | Mode::Home => None,
    }
}

/// The overlay a mode draws UNDERNEATH itself, when it stacked over a modal
/// — `Mode::Yank`/`Mode::Menu` carry their origin in `back`, and the renderer
/// draws that origin's overlay first so the box on top reads as a layer, not
/// a replacement. `None` for unstacked modes (and non-overlay backs, which
/// draw as the plain panes anyway).
pub fn underlay_mode(mode: &crate::action::Mode) -> Option<&crate::action::Mode> {
    use crate::action::Mode;
    match mode {
        Mode::Yank { back, .. } => back.as_deref(),
        Mode::Menu(ms) => ms.back.as_deref(),
        // `?` is a layer too — over a modal it preserves it underneath,
        // same as the which-key / node menu.
        Mode::Help(h) => Some(h.back.as_ref()),
        _ => None,
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

/// One tab's on-screen cells inside the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabCell {
    /// The tab's index in the open-tab list (its IDENTITY — not its drawn
    /// slot), so a hit lands on the right tab even while the strip scrolls.
    pub index: usize,
    /// The whole ` {n} {name} × ` span — left-click switches, middle-click
    /// closes (the fixed home tab's span is just ` ⌂ ` — no close slot).
    pub rect: Rect,
    /// The `×` glyph's column inside `rect` — the close sub-target. `None`
    /// on an unclosable tab (the home cell): no glyph draws, no target.
    pub close_x: Option<u16>,
}

/// The resolved tab-strip layout for one frame: every interactive cell's
/// screen rect. Computed by [`tab_bar_layout`] — the ONE source the renderer
/// and [`hit_target`] share (computed window == drawn window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabBarLayout {
    /// The drawn tabs (the scroll window), in tab order.
    pub tabs: Vec<TabCell>,
    /// The first VISIBLE tab index — the resolved scroll (the input scroll
    /// after the active-visibility and bounds clamps ran).
    pub scroll: usize,
    /// The `‹` scroll-back cell, present iff tabs hide on the left.
    pub left_arrow: Option<Rect>,
    /// The `›` scroll-forward cell, present iff tabs hide on the right.
    pub right_arrow: Option<Rect>,
    /// The ` + ` new-tab cell — pinned at the scrollable strip's right end,
    /// so it never scrolls away with the tab run (§2).
    pub plus: Rect,
    /// The right-aligned project-name zone (draw-only — no hit target).
    /// `None` when the strip left no room for it.
    pub project: Option<Rect>,
    /// The bar's full rect — the region a wheel event counts as "over the
    /// tab bar" (blank strip included).
    pub area: Rect,
}

/// The ` + ` new-tab cell's width in columns.
const PLUS_W: u16 = 3;

/// Resolve the tab strip inside `area`: where each visible tab's cell sits,
/// the `‹`/`›` overflow arrows, the pinned `+`, and the right-edge project
/// zone. The bar draws at most what fits: consecutive whole tab cells from
/// `scroll`'s first-visible index, arrows on whichever side still hides
/// tabs, `+` fixed at the strip's right end.
///
/// `labels` are the precomputed per-tab texts (`{n} {name}` — the event
/// loop's Dag-aware product; `labels.len()` IS the tab count), `closable`
/// the parallel per-tab flag (`false` for the fixed home tab — its cell is
/// ` ⌂ ` with no `×` glyph or close target). `scroll` is the intended
/// first-visible index (the stored wheel/arrow offset); with
/// `ensure_active` the resolved scroll also keeps the ACTIVE tab inside the
/// window — the §2 "アクティブタブは常に見える位置までスクロールする" —
/// applied when a switch/structure change could leave it hidden (a manual
/// `‹`/`›`/wheel scroll passes `false`: the user may look around the strip).
///
/// Pure and size-unaware — the caller hands it the already-computed strip
/// rect, same rule as [`pane_layout`].
pub fn tab_bar_layout(
    area: Rect,
    labels: &[String],
    closable: &[bool],
    active: usize,
    scroll: usize,
    project: &str,
    ensure_active: bool,
) -> TabBarLayout {
    let mut out = TabBarLayout {
        tabs: Vec::new(),
        scroll: 0,
        left_arrow: None,
        right_arrow: None,
        plus: Rect::default(),
        project: None,
        area,
    };
    let count = labels.len();
    if area.width == 0 || area.height == 0 || count == 0 {
        return out;
    }
    // Cell widths in DISPLAY columns: a closable cell is ` ` + label +
    // ` × ` = label + 4, an unclosable one ` ` + label + ` ` = label + 2.
    // The labels are pre-truncated single-width text (model names + digits —
    // ambiguous-width glyphs are stripped at source or `..`-elided), so
    // chars == cells here.
    let widths: Vec<usize> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            l.chars().count()
                + if closable.get(i).copied().unwrap_or(true) {
                    4
                } else {
                    2
                }
        })
        .collect();
    // The strip's right-end reservations: ` + ` pinned, then the project
    // name — kept only while the scrollable zone keeps ≥10 columns for tabs
    // (a narrower bar sheds the name first, never the `+`).
    let project_w = {
        let want = project.chars().count() + 1;
        (want > 1 && area.width as usize >= want + PLUS_W as usize + 10).then_some(want)
    };
    if let Some(w) = project_w {
        out.project = Some(Rect {
            x: area.x + area.width - w as u16,
            width: w as u16,
            ..area
        });
    }
    let strip_r = area.x + area.width - project_w.unwrap_or(0) as u16;
    out.plus = Rect {
        x: strip_r.saturating_sub(PLUS_W).max(area.x),
        width: PLUS_W.min(strip_r.saturating_sub(area.x)),
        ..area
    };
    // The scrollable zone between the bar's left edge and the `+`.
    let mut zone_l = area.x;
    let zone_r = out.plus.x;
    let mut zone_w = zone_r.saturating_sub(zone_l) as usize;

    // §2's pinned `⌂`: a leading unclosable cell (the home tab) sits fixed
    // at the strip's left edge like `+` sits at the right — the scroll
    // window then windows only the tabs after it, so it can never scroll
    // off or gain a left arrow.
    let pinned = usize::from(count > 0 && !closable.first().copied().unwrap_or(true));
    for (i, w) in widths.iter().enumerate().take(pinned) {
        let w = (*w as u16).min(zone_r.saturating_sub(zone_l));
        if w == 0 {
            break;
        }
        out.tabs.push(TabCell {
            index: i,
            rect: Rect {
                x: zone_l,
                width: w,
                ..area
            },
            close_x: None,
        });
        zone_l += w;
        zone_w = zone_w.saturating_sub(w as usize);
    }

    // First visible index: the stored intent clamped to the scrollable
    // range `pinned..count` — and, on an ensure pass, never right of the
    // active tab (a pinned active is always in view already).
    let mut first = if count > pinned {
        scroll.clamp(pinned, count - 1)
    } else {
        count
    };
    if ensure_active && active >= pinned && active < first {
        first = active;
    }
    // How many consecutive whole tabs fit from `first` under a given arrow
    // state: each arrow lives inside the zone, so it shrinks the budget.
    let fits = |first: usize, left: bool, right: bool| -> usize {
        let mut x = usize::from(left);
        let budget = zone_w.saturating_sub(usize::from(right));
        let mut n = 0;
        for w in &widths[first.min(count)..] {
            if x + w > budget {
                break;
            }
            x += w;
            n += 1;
        }
        n
    };
    // The window: pack whole cells; a right arrow is needed iff the rest
    // don't all fit (and eats a column itself). `ensure_active` then bumps
    // `first` right until the window contains the active tab (or `first ==
    // active` — a lone too-wide tab draws clipped to the zone; a pinned
    // active is already visible).
    let (shown, right) = loop {
        let left = first > pinned;
        let (shown, right) = if first + fits(first, left, false) >= count {
            (fits(first, left, false), false)
        } else {
            (fits(first, left, true), true)
        };
        if !ensure_active
            || active < pinned
            || (first..first + shown).contains(&active)
            || first == active
        {
            break (shown, right);
        }
        first += 1;
    };
    out.scroll = first.min(count.saturating_sub(1));
    if first > pinned {
        out.left_arrow = Some(Rect {
            x: zone_l,
            width: 1,
            ..area
        });
    }
    if right {
        out.right_arrow = Some(Rect {
            x: zone_r.saturating_sub(1).max(zone_l),
            width: 1,
            ..area
        });
    }
    let mut x = zone_l + u16::from(first > pinned);
    for (i, w) in widths.iter().enumerate().skip(first).take(shown) {
        let w = *w as u16;
        out.tabs.push(TabCell {
            index: i,
            rect: Rect {
                x,
                width: w,
                ..area
            },
            // ` {label} × `: the × sits 2 columns in from the cell's right
            // edge — `None` on the unclosable (home) cell.
            close_x: closable
                .get(i)
                .copied()
                .unwrap_or(true)
                .then_some(x + w.saturating_sub(2)),
        });
        x += w;
    }
    if shown == 0 && first < count && zone_w > usize::from(first > pinned) + usize::from(right) {
        // A lone tab wider than the whole zone: draw it clipped to the zone
        // (the alternative — no tab at all — leaves the bar looking empty
        // while a tab is open).
        let x0 = zone_l + u16::from(first > pinned);
        let x1 = zone_r.saturating_sub(u16::from(right));
        out.tabs.push(TabCell {
            index: first,
            rect: Rect {
                x: x0,
                width: x1 - x0,
                ..area
            },
            // The clipped cell paints only the label's head — the `×` never
            // reaches the drawn surface, so a close_x here would be a
            // phantom target.
            close_x: None,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Status line + pane title placement (screen-spec §8): ONE pure calculation
// per surface — `draw_status`/`draw_lineage_pane` render from it and
// `hit_target` routes clicks through it, so a cell can never hit a span the
// screen did not draw (nor draw one it cannot hit).
// ---------------------------------------------------------------------------

/// Which droppable status chip a [`ChipSpan`] renders — the click-dispatch
/// key. The declaration order IS the §8 drop order: `Impact` is the headline
/// (sheds last), `View` sheds first, and the `!N` notification badge sits
/// right after `Impact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusChip {
    /// The selected node's blast radius (`impact ↓1 ↑2`) — inert.
    Impact,
    /// Unread-warning badge (`!N`) — opens the notification centre.
    Notify,
    /// The live column trace (`column:m.c ↑4 ↓0`) — opens the column modal.
    Column,
    /// Baseline-diff summary (`diff +A ~M -R`) — opens the diff modal.
    Diff,
    /// `run_results` coverage/freshness (`run 12/34 @…`) — switches to the
    /// RunStatus lens.
    Run,
    /// The package-scope tag (`scope:root`) — inert.
    Scope,
    /// Coverage-lens percentage (`cov 42% (19/45)`) — toggles the lens.
    Coverage,
    /// Bookmark count (`bm:N`) — jumps to the next bookmark.
    Bookmarks,
    /// The persistent list filter (`marked`/`untested`) — inert readout
    /// (§8's chip table gives it no click action; `T`/`*` untoggle it).
    Filter,
    /// Non-default list sort (`sort:tests`) — cycles the sort.
    Sort,
    /// Cursor position (`12/45`) — inert.
    Position,
    /// The focused pane (`list`/`lineage`) — toggles focus.
    Focus,
    /// The rooted-view tag (`↑↓ ≤3`/`overview`) — resets the view, or leaves
    /// the overview when that is the current view.
    View,
}

/// One status-line span's placement — a hint on the left or a chip on the
/// right — as a status-rect-relative cell range `[x0, x1)` plus its
/// [`HitTarget`]. Owned data (copied from the hint/chip inputs) so the layout
/// outlives the frame's borrows — the loop stashes it for the hit-test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSpan {
    /// First covered cell (0-based, relative to the status rect's x).
    pub x0: u16,
    /// One past the last covered cell.
    pub x1: u16,
    /// What a click on this span resolves to.
    pub target: HitTarget,
}

/// The pure status-line placement (§8): contextual hints left-anchored,
/// status chips right-anchored, both droppable at their low-priority ends.
/// `draw_status` renders exactly these spans; `hit_target` maps a cell to the
/// same ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusLayout {
    /// The kept hint spans (left→right), each `HitTarget::StatusHint`.
    pub hints: Vec<StatusSpan>,
    /// The kept chip spans (left→right, right-aligned as a block), each
    /// `HitTarget::StatusChip`.
    pub chips: Vec<StatusSpan>,
}

/// The display-cell width one hint occupies: ` {key}[ {desc}]`.
fn hint_w(h: &crate::action::StatusHint) -> u16 {
    use unicode_width::UnicodeWidthStr;
    (1 + UnicodeWidthStr::width(h.key.as_str())
        + if h.desc.is_empty() {
            0
        } else {
            1 + UnicodeWidthStr::width(h.desc.as_str())
        }) as u16
}

/// The display-cell width one chip occupies: ` {label}` — the label arrives
/// already bracketed (`[!3]`, `[cov 42%]`).
fn chip_w(label: &str) -> u16 {
    use unicode_width::UnicodeWidthStr;
    (1 + UnicodeWidthStr::width(label)) as u16
}

/// Place `hints` + `chips` on a `width`-column status line (§8). The inputs
/// arrive PRIORITY-SORTED (highest-survival first): a too-narrow line sheds
/// the hints' tail first, then the chips' tail — the kept spans tile `[0,
/// hints_w)` left-to-right and `[width - chips_w, width)` right-aligned.
/// Pure — this is the single measurement both drawing and hit-testing use.
pub fn status_layout(
    width: u16,
    hints: &[crate::action::StatusHint],
    chips: &[(StatusChip, String)],
) -> StatusLayout {
    // §8's shed order on a short line: the HINTS' tail first (lowest
    // priority out), then — only once the hints are gone — the chips' tail.
    // The kept set is always a PREFIX of each priority-sorted input.
    let mut kept_h = hints.len();
    let mut kept_c = chips.len();
    let fits = |h: usize, c: usize| -> bool {
        hints[..h].iter().map(hint_w).sum::<u16>()
            + chips[..c].iter().map(|(_, l)| chip_w(l)).sum::<u16>()
            <= width
    };
    while !fits(kept_h, kept_c) {
        if kept_h > 0 {
            kept_h -= 1;
        } else if kept_c > 0 {
            kept_c -= 1;
        } else {
            break;
        }
    }
    let kept_hints = &hints[..kept_h];
    let kept_chips = &chips[..kept_c];
    let chips_w: u16 = kept_chips.iter().map(|(_, l)| chip_w(l)).sum();

    let mut x = 0u16;
    let hints_placed = kept_hints
        .iter()
        .map(|h| {
            let w = hint_w(h);
            let span = StatusSpan {
                x0: x,
                x1: x + w,
                target: HitTarget::StatusHint(h.action),
            };
            x += w;
            span
        })
        .collect();
    // Chips are a RIGHT-anchored block: the block's left edge is
    // `width - chips_w` and the spans tile inside it in priority order.
    let mut cx = width.saturating_sub(chips_w);
    let chips_placed = kept_chips
        .iter()
        .map(|(chip, label)| {
            let w = chip_w(label);
            let span = StatusSpan {
                x0: cx,
                x1: cx + w,
                target: HitTarget::StatusChip(*chip),
            };
            cx += w;
            span
        })
        .collect();
    StatusLayout {
        hints: hints_placed,
        chips: chips_placed,
    }
}

/// Which clickable span of a pane title a [`TitleSpan`] marks — currently
/// the lineage title's breadcrumb entries and its `[{v}]` / `[lens:…]` tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleKind {
    /// A breadcrumb entry `steps` re-root hops back: 1 = the direct parent,
    /// `n` = an earlier ancestor. A click replays `App::history_back` that
    /// many times — the same behaviour `b` gives, no second history model.
    Crumb(usize),
    /// The `[{v}]` view tag (`↑ ≤3`/`overview`) — a click resets the rooted
    /// view filters, the `0` key's [`Action::ResetView`](crate::Action::ResetView).
    View,
    /// The `[lens:…]` tag — a click cycles the lineage lens (`t`).
    Lens,
}

/// One title span's placement: the cell range `[x0, x1)` it occupies inside
/// the pane's top border, measured from the title TEXT's first cell (the
/// pane's `x + 1` — Block titles start one cell in from the corner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleSpan {
    /// What a click here dispatches.
    pub kind: TitleKind,
    /// First covered cell.
    pub x0: u16,
    /// One past the last covered cell.
    pub x1: u16,
}

/// The pure lineage-title placement — the composed text plus the cell ranges
/// of its clickable parts. `draw_lineage_pane` sets `text` verbatim (byte-
/// identical to the old `compose_lineage_title` output); `hit_target` maps a
/// title-row cell through `spans`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleLayout {
    /// The full title text as the Block draws it.
    pub text: String,
    /// The clickable spans in display order.
    pub spans: Vec<TitleSpan>,
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
    /// A tab in the tab bar: its index in `App`'s tab list (the identity —
    /// the strip's drawn slot order is resolved by [`tab_bar_layout`]).
    /// Left-click activates it, middle-click closes it (§2/§6).
    Tab(usize),
    /// The `×` close cell inside tab `usize` — left-click closes.
    TabClose(usize),
    /// The `+` new-tab cell — opens the model-search palette.
    TabNew,
    /// The `‹` overflow arrow — scrolls the strip one tab left.
    TabScrollLeft,
    /// The `›` overflow arrow — scrolls the strip one tab right.
    TabScrollRight,
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
    /// An interior ROW of the inspector pane (Issue 04): `(row - inner.y)`,
    /// 0-based inside its border — the same shape [`HitTarget::OverlayRow`]
    /// uses for covering overlays. What the row MEANS (a column row vs other
    /// content) is content-dependent, so the event loop resolves it against
    /// the scrolled [`InspectorView`](crate::InspectorView).
    InspectorRow(usize),
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
    /// A contextual hint on the status line (screen-spec §8): the payload is
    /// the [`Action`](crate::Action) a click dispatches — `None` marks a
    /// transient readout (splitter hover/drag), which a click leaves inert.
    StatusHint(Option<crate::Action>),
    /// A droppable chip on the status line — which one, so the click dispatch
    /// consults the §6/§8 chip table.
    StatusChip(StatusChip),
    /// A selectable row on the home screen (Issue 10): the flat index into
    /// `HomeData::selectable` — the `HomePick` payload. A click opens the
    /// row's model tab / runs its command, identical to Enter.
    HomeRow(usize),
    /// A breadcrumb entry in the lineage pane's title — the number of
    /// `history_back` steps a click replays.
    TitleCrumb(usize),
    /// The lineage title's `[{v}]` view tag — a click resets the view filters.
    TitleView,
    /// The lineage title's `[lens:…]` tag — a click cycles the lens.
    TitleLens,
    /// An interior ROW of a covering overlay: `(row - inner.y)`, 0-based
    /// inside the overlay's border. Which row MEANS what is mode-specific —
    /// the event loop resolves it (a menu item index, a which-key row, a
    /// palette candidate, a columns row), keeping the hit test mode-agnostic.
    OverlayRow(usize),
    /// Inside a covering overlay's frame but not on an interior row (its
    /// border / title / padding) — inert: the click neither acts nor
    /// dismisses.
    ModalInterior,
    /// Outside a covering overlay while it owns the screen — the
    /// click-outside-to-dismiss target (the loop applies the mode's own
    /// Esc action).
    ModalExterior,
}

/// The pure inputs [`hit_target`] needs, resolved once per mouse event by the
/// (only) size-aware layer, the event loop. New surfaces extend this bundle
/// rather than growing the signature — the loop already computes every field
/// for drawing, so the hit-test always sees what the frame drew.
pub struct HitGeometry<'a> {
    /// The frame's pane rects (`pane_layout(area, &pane_config)`).
    pub panes: PaneRects,
    /// The resolved tab-strip cells (`tab_bar_layout` on `panes.tab_bar` —
    /// OWNED: the loop computes it per event from the stored scroll, the
    /// same value the last draw used). `None` = no tab bar this frame.
    pub tab_bar: Option<TabBarLayout>,
    /// The active model list (its `rows` interleave group headers).
    pub list: &'a ModelList,
    /// First visible display row (`UiState::offset`).
    pub list_offset: usize,
    /// The selected model index (`UiState::selected`) — the sticky header's
    /// suppression rule needs it: the pin never covers the selection's own
    /// row, so the row the pin CLAIMS depends on where the selection sits.
    pub list_selected: usize,
    /// The styled layout the lineage pane draws (`None` = empty diagram).
    pub layout: Option<&'a crate::Layout>,
    /// Lineage scroll offsets (CharGrid space).
    pub scroll_x: usize,
    /// Ditto, vertical.
    pub scroll_y: usize,
    /// Whether the minimap inset is on — its cells never reach nodes below.
    pub minimap: bool,
    /// The covering overlay's outer rect ([`overlay_rect`]'s output), when one
    /// owns the screen. While `Some`, the panes are NOT routed: interior rows
    /// report [`HitTarget::OverlayRow`], the frame's border cells
    /// [`HitTarget::ModalInterior`], and every outside cell
    /// [`HitTarget::ModalExterior`].
    pub overlay: Option<Rect>,
    /// The status-line placement the last draw used ([`status_layout`]'s
    /// output) — `None` renders every status cell plain
    /// [`HitTarget::StatusBar`]. The loop stashes the same layout it drew so
    /// a click lands on a span the screen showed.
    pub status: Option<&'a StatusLayout>,
    /// The lineage title's placement the last draw used
    /// ([`TitleLayout`]) — `None` leaves the title row inert chrome.
    pub title: Option<&'a TitleLayout>,
    /// The home body's line plan while the home tab is active —
    /// [`home_lines`] over the SAME `HomeData` + body height the draw used,
    /// so a click lands on the row the screen drew (a `Row(i)` cell reports
    /// [`HitTarget::HomeRow(i)`]; every other line is inert).
    pub home: Option<Vec<HomeLine>>,
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
    // A covering overlay owns the whole screen: its interior rows route as
    // `OverlayRow`, its frame as `ModalInterior`, and EVERYTHING else — the
    // panes, the tab strip, the status line, the dimmed backdrop — as
    // `ModalExterior` (the click-outside-to-dismiss surface; the tab bar
    // does not switch under a modal).
    if let Some(or) = g.overlay {
        let inner = pane_interior(or);
        return Some(if inner.contains(at) {
            HitTarget::OverlayRow((row - inner.y) as usize)
        } else if or.contains(at) {
            HitTarget::ModalInterior
        } else {
            HitTarget::ModalExterior
        });
    }
    // The home body owns the whole pane band (Issue 10): every interior cell
    // routes through the line plan — a drawn `Row(i)` is clickable, the
    // title/health/headers/blank lines are dead chrome. Checked before the
    // drawer/tab strip arms: the band union is disjoint from both anyway,
    // but a stale `list_drawer` flag must never shadow home's surface (draw
    // never paints the drawer over home).
    if let Some(lines) = &g.home {
        let body = p.list.union(p.lineage).union(p.inspector);
        if body.contains(at) {
            return match lines.get((row - body.y) as usize) {
                Some(HomeLine::Row(i)) => Some(HitTarget::HomeRow(*i)),
                _ => None,
            };
        }
    }
    // §12's list drawer is a covering surface too, one band narrower in
    // scope: while it floats, its interior rows resolve exactly like the
    // inline list's (the SAME row/offset space), its frame is inert chrome,
    // and every other cell — panes, tab strip, status — reports
    // `ModalExterior`, the click-outside-to-dismiss surface the loop closes
    // on. Only reachable in the panes-owning modes: a real covering
    // overlay above already won the return.
    if let Some(dr) = p.list_drawer {
        let inner = pane_interior(dr);
        return Some(if inner.contains(at) {
            list_cell_target(g, inner, row)
        } else if dr.contains(at) {
            HitTarget::ModalInterior
        } else {
            HitTarget::ModalExterior
        });
    }
    // The tab strip claims row 0 — checked before the panes (its cells never
    // overlap a pane, but the early-out keeps the priority readable: chrome
    // above content, like the splitters over pane interiors below).
    if let Some(tb) = &g.tab_bar {
        if tb.area.contains(at) {
            if tb.plus.contains(at) {
                return Some(HitTarget::TabNew);
            }
            if tb.left_arrow.is_some_and(|r| r.contains(at)) {
                return Some(HitTarget::TabScrollLeft);
            }
            if tb.right_arrow.is_some_and(|r| r.contains(at)) {
                return Some(HitTarget::TabScrollRight);
            }
            for cell in &tb.tabs {
                if cell.rect.contains(at) {
                    return Some(if cell.close_x == Some(col) {
                        HitTarget::TabClose(cell.index)
                    } else {
                        HitTarget::Tab(cell.index)
                    });
                }
            }
            // Blank strip / project zone: a dead surface (no target), same
            // as pane chrome below.
            return None;
        }
    }
    if p.status.contains(at) {
        // §8: a span-covered cell reports the span's own target (a hint's
        // action, a chip's identity); uncovered status cells stay StatusBar.
        if let Some(l) = g.status {
            let rel = col.saturating_sub(p.status.x);
            for span in l.hints.iter().chain(l.chips.iter()) {
                if rel >= span.x0 && rel < span.x1 {
                    return Some(span.target.clone());
                }
            }
        }
        return Some(HitTarget::StatusBar);
    }
    // The lineage pane's title row (its top border, between the corner
    // glyphs): cells the shared `TitleLayout` covers report their span target;
    // every other title cell is inert chrome — same as before, and the two
    // border columns still belong to the splitters below.
    if row == p.lineage.y && col > p.lineage.x && col + 1 < p.lineage.x + p.lineage.width {
        if let Some(t) = g.title {
            let rel = col - (p.lineage.x + 1);
            for span in &t.spans {
                if rel >= span.x0 && rel < span.x1 {
                    return Some(match span.kind {
                        TitleKind::Crumb(n) => HitTarget::TitleCrumb(n),
                        TitleKind::View => HitTarget::TitleView,
                        TitleKind::Lens => HitTarget::TitleLens,
                    });
                }
            }
        }
        return None;
    }
    for which in [Splitter::List, Splitter::Inspector] {
        if p.splitter(which).is_some_and(|r| r.contains(at)) {
            return Some(HitTarget::PaneBorder(which));
        }
    }
    let list_inner = pane_interior(p.list);
    if list_inner.contains(at) {
        return Some(list_cell_target(g, list_inner, row));
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
    let ins_inner = pane_interior(p.inspector);
    if ins_inner.contains(at) {
        // The whole interior reports its 0-based row — column rows and every
        // other line alike (which row IS a column row depends on the
        // InspectorView's content + scroll, both loop-side data).
        return Some(HitTarget::InspectorRow((row - ins_inner.y) as usize));
    }
    None
}

/// The list-interior cell → target resolution the inline pane and the
/// narrow-band drawer share (§12): the sticky pin claims interior row 0 when
/// it covers a mid-group row, everything below resolves by display row —
/// identical on both surfaces so a row means the same thing either way.
fn list_cell_target(g: &HitGeometry, inner: Rect, row: u16) -> HitTarget {
    // The sticky header (§10) is drawn OVER the first interior row while
    // scrolling mid-group — that row reports the PINNED header's target,
    // not the model row the pin covers: the pointer sees a header, so a
    // click must fold it. Everything below row 0 resolves normally.
    if row == inner.y {
        if let Some(DisplayRow::Header { layer, .. }) =
            g.list.sticky_header(g.list_offset, g.list_selected)
        {
            return HitTarget::ListHeader(layer.clone());
        }
    }
    let r = g.list_offset + (row - inner.y) as usize;
    match g.list.rows.get(r) {
        Some(DisplayRow::Model { model_index, .. }) => HitTarget::ListRow(*model_index),
        Some(DisplayRow::Header { layer, .. }) => HitTarget::ListHeader(layer.clone()),
        None => HitTarget::ListBlank,
    }
}

/// The screen row (0-based inside the list interior) a list `HitTarget`
/// currently occupies — the reverse of [`hit_target`]'s row resolution, so
/// the renderer's hover tint lands on exactly the row the pointer sees.
/// `None` when the target scrolled out of the window, or the sticky pin
/// covers it: a covered model row is never hoverable (the pinned header
/// reports `ListHeader` there instead), mirroring the hit-test's rule.
pub fn list_target_row(
    list: &ModelList,
    offset: usize,
    selected: usize,
    target: &HitTarget,
    inner_height: usize,
) -> Option<usize> {
    let sticky = match list.sticky_header(offset, selected) {
        Some(DisplayRow::Header { layer, .. }) => Some(layer.as_str()),
        _ => None,
    };
    let row = match target {
        // The pinned header claims interior row 0; any other header sits at
        // its own display row.
        HitTarget::ListHeader(layer) if sticky == Some(layer.as_str()) => Some(0),
        HitTarget::ListHeader(layer) => {
            list.row_of_group(layer).and_then(|r| r.checked_sub(offset))
        }
        HitTarget::ListRow(i) => list
            .row_of_model(*i)
            .checked_sub(offset)
            .filter(|&y| !(y == 0 && sticky.is_some())),
        _ => None,
    };
    row.filter(|&y| y < inner_height)
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

/// The scroll offsets a minimap cell `(col, row)` asks for — click-to-centre
/// and drag-scrub share it (§6: `ミニマップ` click moves the viewport, drag
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

/// The hover tooltip's widest box in columns (screen-spec §9's floating-chrome
/// rule — a name-plus-two-lines box never becomes a wall of text).
pub const TOOLTIP_MAX_W: u16 = 48;

/// The hover tooltip's frame rect: hangs one cell below-right of the pointer
/// cell `at`, flips to the cell's left/top when the preferred corner would
/// overflow `area` (§9's "右下（はみ出すなら反転）" rule — the same anchor
/// policy [`menu_rect`] applies), then clamps inside `area`. `w`/`h` are the
/// caller's content-sized box. Pure, so the draw and a test measure the same
/// rect — the tooltip itself is a frame-buffer overlay, never a CharGrid
/// input (it must not perturb the layout cache or the hit-test).
pub fn tooltip_rect(at: Position, w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width.max(1)).max(1);
    let h = h.min(area.height.max(1)).max(1);
    let mut x = at.x.saturating_add(1);
    if x + w > area.x + area.width {
        x = at.x.saturating_sub(w);
    }
    let mut y = at.y.saturating_add(1);
    if y + h > area.y + area.height {
        y = at.y.saturating_sub(h);
    }
    Rect {
        x: x.clamp(area.x, area.x + area.width.saturating_sub(w)),
        y: y.clamp(area.y, area.y + area.height.saturating_sub(h)),
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------------------
// Home screen line plan (Issue 10, §3): ONE pure sequence — the renderer
// draws it top-down into the body band and `hit_target` resolves a cell the
// same way, so "computed window == drawn window" holds for the home too.
// ---------------------------------------------------------------------------

/// One line in the home body's vertical plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeLine {
    /// `dbtl home - {project} · {source} · scope:{scope}`.
    Title,
    /// A blank spacer row.
    Pad,
    /// The `/` search bar (live query + caret while open, dim hint while
    /// closed).
    SearchBar,
    /// A section's non-selectable content row — §3's middle rows (Health's
    /// coverage gauge, Last run's status counts, Diff's delta). The index
    /// addresses [`HomeData::info_flat`] — never selectable.
    Info(usize),
    /// A section header (`[Resume]` / `[Attention]` / …).
    SectionHeader(&'static str),
    /// A selectable row — the flat index into `HomeData::selectable` (while
    /// a home search is open, the result rows).
    Row(usize),
    /// A dim note row (`no matches — Esc to clear`).
    Note,
    /// A capped section's `+{n} more` overflow line — non-selectable
    /// (`S`/`'` reach the full sets; §3's Attention overflow points at `S`).
    Tail(usize),
    /// The trailing `+{n} more` elision when the body clipped rows.
    More(usize),
    /// §3's fixed `Tip:` line — closes the plan (never selectable).
    Tip,
}

/// The home body's FULL line sequence (unclipped) — the plan
/// [`home_window`] scrolls/clips and `hit_target` re-derives, so a `Row(i)`
/// cell always lands on the row the screen painted. Pure + mode-independent
/// (§15).
///
/// Order (fixed, §3): title → pad → search bar → pad → either the open
/// search's result rows (which own the cursor) or the section list in
/// `HomeData::sections` order — each section a header, its `info` content
/// rows (§3's Health/Last run/Diff middle rows sit here in section order),
/// its selectable `rows`, then a `+N more` tail — padded between.
pub fn home_lines(data: &crate::app::HomeData) -> Vec<HomeLine> {
    let mut lines = vec![
        HomeLine::Title,
        HomeLine::Pad,
        HomeLine::SearchBar,
        HomeLine::Pad,
    ];
    match &data.search {
        Some(s) => {
            lines.extend((0..s.results.len()).map(HomeLine::Row));
            if s.results.is_empty() {
                lines.push(HomeLine::Note);
            }
        }
        None => {
            let mut flat = 0;
            let mut info_i = 0;
            for (si, sec) in data.sections.iter().enumerate() {
                if si > 0 {
                    lines.push(HomeLine::Pad);
                }
                lines.push(HomeLine::SectionHeader(sec.title));
                for _ in &sec.info {
                    lines.push(HomeLine::Info(info_i));
                    info_i += 1;
                }
                for _ in &sec.rows {
                    lines.push(HomeLine::Row(flat));
                    flat += 1;
                }
                if let Some(n) = sec.tail {
                    lines.push(HomeLine::Tail(n));
                }
            }
            // §3's fixed closing line (`Actions` always exists, so the pad
            // before it can never lead the plan).
            lines.push(HomeLine::Pad);
            lines.push(HomeLine::Tip);
        }
    }
    lines
}

/// The `scroll`-offset, `height`-tall window into a [`home_lines`] plan —
/// the SAME sequence the renderer draws and `hit_target` routes. Rows past
/// the window's bottom collapse into a trailing `+{n} more` line (the count
/// names every row the screen dropped, the replaced line included).
pub fn home_window(lines: &[HomeLine], height: usize, scroll: usize) -> Vec<HomeLine> {
    let scroll = scroll.min(lines.len().saturating_sub(1));
    let end = (scroll + height).min(lines.len());
    let mut window: Vec<HomeLine> = lines[scroll.min(end)..end].to_vec();
    let below = lines.len() - end;
    if below > 0 && !window.is_empty() {
        let last = window.len() - 1;
        window[last] = HomeLine::More(below + 1);
    }
    window
}

/// The plan index of `Row(sel)` — the loop's ensure-visible input (which
/// line must be inside the scroll window). Falls back to the plan's end so
/// an out-of-range cursor scrolls to the bottom rather than wrapping.
pub fn home_line_of(lines: &[HomeLine], sel: usize) -> usize {
    lines
        .iter()
        .position(|l| matches!(l, HomeLine::Row(i) if *i == sel))
        .unwrap_or_else(|| lines.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{HomeData, HomeInfo, HomeItem, HomeRow, HomeSection, HomeTone};

    /// A minimal `HomeData` (no Dag needed — it's plain display data) with
    /// one section of `rows` selectable model rows.
    fn home_fixture(rows: usize) -> HomeData {
        let make_row = |i: usize| HomeRow {
            item: HomeItem::Model {
                uid: format!("model.p.m{i}"),
            },
            label: format!("m{i}"),
            detail: String::new(),
            tone: HomeTone::Normal,
        };
        HomeData {
            project: "p".to_string(),
            source: "manifest m.json".to_string(),
            scope: "project",
            info_flat: vec![HomeInfo::Text(format!("{rows} models"))],
            search: None,
            sections: vec![HomeSection {
                title: "Recent",
                info: vec![HomeInfo::Text(format!("{rows} models"))],
                rows: (0..rows).map(make_row).collect(),
                tail: None,
            }],
            selectable: (0..rows).map(|i| make_row(i).item).collect(),
        }
    }

    #[test]
    fn home_lines_orders_chrome_then_sections() {
        // §3's fixed plan: title, pad, search bar, pad — then one header
        // per section followed by its info content rows and flat-indexed
        // selectable rows, and the fixed `Tip` line closes the plan
        // (search replaces it all).
        let lines = home_lines(&home_fixture(2));
        assert_eq!(
            lines,
            vec![
                HomeLine::Title,
                HomeLine::Pad,
                HomeLine::SearchBar,
                HomeLine::Pad,
                HomeLine::SectionHeader("Recent"),
                HomeLine::Info(0),
                HomeLine::Row(0),
                HomeLine::Row(1),
                HomeLine::Pad,
                HomeLine::Tip,
            ]
        );
    }

    #[test]
    fn home_window_clips_and_counts_dropped_rows() {
        // A 7-row window into an 11-line plan keeps the top chrome and folds
        // the overflow into `+N more` (the count names every dropped row —
        // the one it replaced included).
        let plan = home_lines(&home_fixture(3)); // 11 lines
        let w = home_window(&plan, 7, 0);
        assert_eq!(w.len(), 7);
        assert_eq!(w.last(), Some(&HomeLine::More(5)), "5 of the 11 lines clip");
        // Scrolling to the cursor's line shows the tail instead.
        let w = home_window(&plan, 7, 1);
        assert_eq!(w.first(), Some(&HomeLine::Pad));
        assert_eq!(w.last(), Some(&HomeLine::More(4)));
        // `home_line_of` maps a flat row index to its plan position — the
        // ensure-visible input the loop scrolls to.
        assert_eq!(home_line_of(&plan, 0), 6);
        assert_eq!(home_line_of(&plan, 2), 8);
        assert_eq!(home_line_of(&plan, 99), 10, "stale cursors land at the end");
    }

    #[test]
    fn home_hit_routes_rows_and_drops_chrome() {
        // The body band under home routes `Row(i)` cells to `HomeRow(i)`;
        // title/health/headers/pads/footer are dead cells.
        let data = home_fixture(2);
        let plan = home_lines(&data);
        let area = Rect::new(0, 0, 40, 12);
        let list = crate::model_list::build_model_list(
            &crate::load_dag(HIT_FIXTURE).expect("fixture loads"),
            crate::SortMode::Layer,
        );
        let g = HitGeometry {
            panes: pane_layout(
                area,
                &PaneConfig {
                    tab_bar: true,
                    ..cfg()
                },
            ),
            tab_bar: None,
            home: Some(home_window(&plan, 9, 0)),
            list: &list,
            list_offset: 0,
            list_selected: 0,
            layout: None,
            scroll_x: 0,
            scroll_y: 0,
            minimap: false,
            overlay: None,
            status: None,
            title: None,
        };
        // Body starts under the tab strip (row 1): plan line 0 (Title) is
        // dead, line 6 (Row 0) hits, line 7 (Row 1) hits; the window's last
        // line collapses the still-hidden Pad+Tip into `More` (inert).
        assert_eq!(hit_target(&g, 2, 1), None, "the title line is inert");
        assert_eq!(hit_target(&g, 2, 7), Some(HitTarget::HomeRow(0)));
        assert_eq!(hit_target(&g, 2, 8), Some(HitTarget::HomeRow(1)));
        assert_eq!(hit_target(&g, 2, 9), None, "the +N more line is inert");
        // The search bar, headers, and info content rows never click.
        assert_eq!(hit_target(&g, 2, 3), None, "the search bar is inert");
        assert_eq!(hit_target(&g, 2, 5), None, "the section header is inert");
        assert_eq!(hit_target(&g, 2, 6), None, "the info row is inert");
    }

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
        // Below §12's 80-column band edge the LIST never sits inline — the
        // squeeze can't demonstrate on it (its only surface there is the
        // drawer), so the INSPECTOR carries the below-floor demo: body 55,
        // saved 42 → 50%-cap 27 → floor 30, then the deficit squeezes the
        // displayed width to 15 — below the floor, the saved value intact.
        let cfg = PaneConfig {
            inspector_visible: true,
            ..cfg()
        };
        let p = pane_layout(Rect::new(0, 0, 55, 30), &cfg);
        assert_eq!(p.list.width, 0, "§12's band hides the inline list");
        assert_eq!(p.inspector.width, 15, "55 - 40 = a 15-column inspector");
        assert_eq!(p.lineage.width, MIN_LINEAGE_WIDTH);
        assert_eq!(cfg.inspector_width, 42, "the saved width never changes");
    }

    #[test]
    fn terminal_too_small_boundary() {
        // §12's usable floor: EITHER axis below it gates the app.
        let small = |w: u16, h: u16| terminal_too_small(Rect::new(0, 0, w, h));
        assert!(small(49, 12), "one column under the width floor");
        assert!(small(50, 11), "one row under the height floor");
        assert!(small(0, 0) && small(1, 30), "degenerate areas count too");
        assert!(!small(50, 12), "exactly the floor is usable");
        assert!(!small(200, 60), "a comfortable terminal is usable");
    }

    #[test]
    fn pane_layout_narrow_band_hides_inline_list() {
        // §12: below 80 columns the list never sits inline — even with the
        // pref on — because the drawer is its only surface there. The pref's
        // saved width is not consulted at all (not even squeezed).
        for w in [50u16, 60, 79] {
            let p = pane_layout(Rect::new(0, 0, w, 30), &cfg());
            assert_eq!(p.list.width, 0, "{w}: the band zeroes the inline list");
            assert_eq!(p.lineage.width, w, "{w}: lineage takes the whole body");
            assert_eq!(p.list_splitter, None, "{w}: no boundary to grab");
        }
        // Exactly 80 restores the inline pane — the band edge is `<`, not `<=`.
        let p = pane_layout(Rect::new(0, 0, LIST_AUTO_HIDE_W, 30), &cfg());
        assert_eq!(p.list.width, DEFAULT_LIST_WIDTH, "80: inline list returns");
    }

    #[test]
    fn pane_layout_drawer_only_floats_in_the_band() {
        // §12: `min(34, 60%)` wide, the body's full height, over its left edge.
        let open = PaneConfig {
            list_drawer: true,
            ..cfg()
        };
        let p = pane_layout(Rect::new(0, 0, 70, 30), &open);
        // body is 29 rows (30 - status); min(34, 60% of 70 = 42) = 34.
        assert_eq!(p.list_drawer, Some(Rect::new(0, 0, 34, 29)));
        // The 60% cap binds before the 34-col default on the narrowest bands.
        let p = pane_layout(Rect::new(0, 0, 50, 30), &open);
        assert_eq!(
            p.list_drawer,
            Some(Rect::new(0, 0, 30, 29)),
            "60% of 50 = 30 < 34"
        );
        // Above the band the flag is inert — the same `true` resolves None.
        let p = pane_layout(Rect::new(0, 0, LIST_AUTO_HIDE_W, 30), &open);
        assert_eq!(p.list_drawer, None, "80+: no drawer, inline list instead");
        assert_eq!(p.list.width, DEFAULT_LIST_WIDTH);
        // And a closed drawer allocates nothing anywhere.
        let p = pane_layout(Rect::new(0, 0, 70, 30), &cfg());
        assert_eq!(p.list_drawer, None);
    }

    #[test]
    fn hit_target_drawer_routes_interior_chrome_and_exterior() {
        let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
        let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
        let g = HitGeometry {
            panes: pane_layout(
                Rect::new(0, 0, 70, 24),
                &PaneConfig {
                    list_drawer: true,
                    ..cfg()
                },
            ),
            tab_bar: None,
            list: &list,
            list_offset: 0,
            list_selected: 0,
            home: None,
            layout: None,
            scroll_x: 0,
            scroll_y: 0,
            minimap: false,
            overlay: None,
            status: None,
            title: None,
        };
        // The drawer floats at x 0..34 over the body's 23 rows; its interior
        // (x 1..33, y 1..22) resolves EXACTLY like the inline list's — header
        // on row 0, the first model on row 1 (the same display-row space).
        let layer = match &list.rows[0] {
            DisplayRow::Header { layer, .. } => layer.clone(),
            other => panic!("row 0 must be a header, got {other:?}"),
        };
        assert_eq!(hit_target(&g, 2, 1), Some(HitTarget::ListHeader(layer)));
        assert_eq!(hit_target(&g, 2, 2), Some(HitTarget::ListRow(0)));
        // Its border is inert chrome — the click neither acts nor dismisses.
        for (x, y) in [(0, 2), (33, 2), (2, 0), (2, 22)] {
            assert_eq!(
                hit_target(&g, x, y),
                Some(HitTarget::ModalInterior),
                "{x},{y} on the drawer frame"
            );
        }
        // Everything outside dismisses: the covered lineage cells, the
        // (zero-height) tab row's band, and the status line alike.
        for (x, y) in [(40, 2), (40, 0), (40, 23), (0, 23)] {
            assert_eq!(
                hit_target(&g, x, y),
                Some(HitTarget::ModalExterior),
                "{x},{y} outside the drawer"
            );
        }
        // A covering overlay still outranks it — the modal surface wins.
        let g = HitGeometry {
            overlay: Some(Rect::new(10, 5, 30, 10)),
            ..HitGeometry {
                panes: g.panes,
                tab_bar: None,
                list: &list,
                list_offset: 0,
                list_selected: 0,
                home: None,
                layout: None,
                scroll_x: 0,
                scroll_y: 0,
                minimap: false,
                overlay: None,
                status: None,
                title: None,
            }
        };
        assert_eq!(
            hit_target(&g, 12, 6),
            Some(HitTarget::OverlayRow(0)),
            "a real modal's row beats the drawer"
        );
        assert_eq!(hit_target(&g, 2, 2), Some(HitTarget::ModalExterior));
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

    /// A strip label set `tab_bar_layout` can pack predictably: cell width =
    /// ` {label} × ` = chars + 4.
    fn labels(ns: &[&str]) -> Vec<String> {
        ns.iter().map(|s| s.to_string()).collect()
    }

    const HIT_FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/manifest.json"
    );

    #[test]
    fn tab_bar_layout_packs_whole_cells_then_plus_then_project() {
        // 100 cols, 3 short tabs, project name pinned right: zone_w = 100 -
        // 3 (plus) - 7 (project) = 90; cells "1 aa"=8, "2 bbb"=9, "3 c"=7.
        let strip = Rect::new(0, 0, 100, 1);
        let lay = tab_bar_layout(
            strip,
            &labels(&["1 aa", "2 bbb", "3 c"]),
            &[true; 3],
            1,
            0,
            "myproj",
            true,
        );
        assert_eq!(lay.scroll, 0);
        assert!(lay.left_arrow.is_none() && lay.right_arrow.is_none());
        assert_eq!(lay.tabs.len(), 3);
        assert_eq!(lay.tabs[0].rect, Rect::new(0, 0, 8, 1));
        assert_eq!(
            lay.tabs[0].close_x,
            Some(6),
            "the x sits 2 in from the right"
        );
        assert_eq!(lay.tabs[1].rect, Rect::new(8, 0, 9, 1));
        assert_eq!(lay.tabs[2].rect, Rect::new(17, 0, 7, 1));
        assert_eq!(lay.plus, Rect::new(90, 0, 3, 1), "+ pinned at strip end");
        assert_eq!(lay.project, Some(Rect::new(93, 0, 7, 1)));
        assert_eq!(lay.area, strip);
    }

    #[test]
    fn tab_bar_layout_overflow_shows_arrows_and_whole_cells_only() {
        // 30 cols, no project (zone_w = 27): cells "1 alpha"=11, "2 beta"=10,
        // "3 gamma"=11, "4 delta"=11 — 11+10=21 fits under the `›`-adjusted
        // budget (26), +11 doesn't → `›`.
        let strip = Rect::new(0, 0, 30, 1);
        let all = labels(&["1 alpha", "2 beta", "3 gamma", "4 delta"]);
        let lay = tab_bar_layout(strip, &all, &[true; 4], 0, 0, "", true);
        assert!(lay.left_arrow.is_none(), "scrolled to the left edge");
        let arrow = lay.right_arrow.expect("tabs hide on the right");
        assert_eq!(arrow, Rect::new(26, 0, 1, 1), "› at the zone's last col");
        // Whole-cell packing under the arrow-adjusted budget.
        let drawn: Vec<usize> = lay.tabs.iter().map(|c| c.index).collect();
        assert_eq!(drawn, vec![0, 1]);
        let mut x = 0;
        for c in &lay.tabs {
            assert_eq!(c.rect.x, x);
            x += c.rect.width;
        }
        assert!(x <= 26, "cells stay left of the arrow");
    }

    #[test]
    fn tab_bar_layout_ensure_active_pulls_the_active_tab_into_view() {
        let strip = Rect::new(0, 0, 30, 1);
        let all = labels(&["1 alpha", "2 beta", "3 gamma", "4 delta"]);
        // Without ensure, a stored scroll just replays — the active tab may
        // be out of the window (manual scrolls may look around).
        let lay = tab_bar_layout(strip, &all, &[true; 4], 3, 0, "", false);
        assert!(!lay.tabs.iter().any(|c| c.index == 3));
        // With ensure (a switch/structure change) the window bumps right
        // until the active tab is inside — and `‹` appears.
        let lay = tab_bar_layout(strip, &all, &[true; 4], 3, 0, "", true);
        assert!(lay.tabs.iter().any(|c| c.index == 3));
        assert!(lay.left_arrow.is_some(), "scrolled in from the left");
        assert_eq!(
            lay.left_arrow.unwrap(),
            Rect::new(0, 0, 1, 1),
            "‹ at the zone's first col"
        );
        assert_eq!(
            lay.tabs.last().unwrap().index,
            3,
            "the active tab is the window's tail"
        );
    }

    #[test]
    fn tab_bar_layout_narrow_strip_sheds_project_before_plus() {
        // 20 cols: project can't coexist with 10 cols of tab zone — the name
        // drops, ` + ` stays pinned.
        let strip = Rect::new(0, 0, 20, 1);
        let lay = tab_bar_layout(
            strip,
            &labels(&["1 aa"]),
            &[true; 1],
            0,
            0,
            "longprojectname",
            true,
        );
        assert!(lay.project.is_none(), "the name sheds first");
        assert_eq!(lay.plus, Rect::new(17, 0, 3, 1));
        // An empty strip is a no-op (the bar hides below the size floor).
        let lay = tab_bar_layout(
            Rect::new(0, 0, 0, 1),
            &labels(&["1 aa"]),
            &[true; 1],
            0,
            0,
            "p",
            true,
        );
        assert!(lay.tabs.is_empty() && lay.plus.width == 0);
    }

    #[test]
    fn hit_target_resolves_tab_cells_arrows_close_and_plus() {
        // 30 cols, four tabs → the same overflow scene as the layout test:
        // `›` at x=26, cells for tabs 0-2, ` + ` at x=27..30.
        let strip = Rect::new(0, 0, 30, 1);
        let all = labels(&["1 alpha", "2 beta", "3 gamma", "4 delta"]);
        let tb = tab_bar_layout(strip, &all, &[true; 4], 0, 0, "", true);
        let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
        let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
        let g = HitGeometry {
            panes: pane_layout(
                Rect::new(0, 0, 30, 10),
                &PaneConfig {
                    tab_bar: true,
                    ..cfg()
                },
            ),
            tab_bar: Some(tb),
            home: None,
            list: &list,
            list_offset: 0,
            list_selected: 0,
            layout: None,
            scroll_x: 0,
            scroll_y: 0,
            minimap: false,
            overlay: None,
            status: None,
            title: None,
        };
        assert_eq!(hit_target(&g, 1, 0), Some(HitTarget::Tab(0)));
        // The x cell: tab 0's close_x is rect.x + w - 2 = 9.
        assert_eq!(hit_target(&g, 9, 0), Some(HitTarget::TabClose(0)));
        assert_eq!(hit_target(&g, 15, 0), Some(HitTarget::Tab(1)));
        assert_eq!(hit_target(&g, 26, 0), Some(HitTarget::TabScrollRight));
        assert_eq!(hit_target(&g, 28, 0), Some(HitTarget::TabNew));
        // A point below the strip doesn't report a tab target.
        assert_ne!(hit_target(&g, 1, 3), Some(HitTarget::Tab(0)));
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
        // first (the least essential pane), the list keeps 18. (80 is also
        // §12's band edge — the inline list still exists at exactly 80.)
        let p = pane_layout(Rect::new(0, 0, 80, 30), &cfg);
        assert_eq!(
            (p.list.width, p.lineage.width, p.inspector.width),
            (18, 40, 22),
            "inspector dips below its floor before the list does"
        );
        // 55 cols — inside §12's band: the inline list is gone entirely (its
        // only surface is the drawer), so the squeeze never touches it; the
        // inspector dips below its floor to 15 and the lineage keeps its 40.
        let p = pane_layout(Rect::new(0, 0, 55, 30), &cfg);
        assert_eq!(p.list.width, 0, "§12's band hides the inline list");
        assert_eq!((p.lineage.width, p.inspector.width), (40, 15));
        // Narrower still the inspector too is squeezed away.
        let p = pane_layout(Rect::new(0, 0, 50, 30), &cfg);
        assert_eq!(p.inspector.width, 10);
        assert_eq!(p.inspector_splitter, Some(Rect::new(39, 0, 2, 29)));
    }

    #[test]
    fn pane_layout_max_cap_interacts_with_squeeze() {
        // A saved width past the cap on a narrow terminal: cap first, then the
        // squeeze decides (cap 60% of 80 = 48, then lineage floor shrinks it).
        // Uses 80 — the narrowest terminal where the inline list exists at all
        // under §12's band rule.
        let p = pane_layout(
            Rect::new(0, 0, 80, 30),
            &PaneConfig {
                list_width: 80,
                ..cfg()
            },
        );
        assert_eq!(p.list.width, 40, "80 - 40; the cap already held it to 48");
        assert_eq!(p.lineage.width, 40);
        // Inside the band the saved width is not consulted at all — a 200-col
        // pref displays exactly like the default: zero.
        let p = pane_layout(
            Rect::new(0, 0, 60, 30),
            &PaneConfig {
                list_width: 80,
                ..cfg()
            },
        );
        assert_eq!(p.list.width, 0, "§12's band overrides even a huge pref");
        assert_eq!(p.lineage.width, 60);
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

    #[test]
    fn minimap_scroll_to_centres_the_clicked_block() {
        // 160×60 grid in a 16×6 inset at (80,2): sx = 10, sy = 10.
        let mm = Rect::new(80, 2, 16, 6);
        // Cell (0,0) covers grid [0,10) — its centre is (5,5); a 40×20 window
        // centred there clamps to scroll (0,0).
        assert_eq!(minimap_scroll_to(mm, 80, 2, 160, 60, 40, 20), (0, 0));
        // Cell (8,3) covers grid x [80,90), y [30,40) — centre (85,35) →
        // scroll = centre - view/2 = (65,25).
        assert_eq!(minimap_scroll_to(mm, 88, 5, 160, 60, 40, 20), (65, 25));
        // The last cell saturates toward the grid's far edge (the caller's
        // clamp_lineage lands it on the max offset — here it just can't go
        // below the centre-minus-half floor).
        assert_eq!(minimap_scroll_to(mm, 95, 7, 160, 60, 40, 20), (135, 45));
        // A scrub outside the rect keeps tracking (saturating left/top).
        assert_eq!(minimap_scroll_to(mm, 79, 1, 160, 60, 40, 20), (0, 0));
        assert_eq!(minimap_scroll_to(mm, 99, 9, 160, 60, 40, 20), (175, 65));
    }

    #[test]
    fn pan_offset_tracks_the_pointer_one_to_one() {
        // Grab at (50,·) with scroll 10: drag left 5 → scroll 15 (content
        // follows the pointer); drag right 5 → scroll 5.
        assert_eq!(pan_offset(10, 50, 45), 15);
        assert_eq!(pan_offset(10, 50, 55), 5);
        // Past the diagram's home edge: saturate at 0 (the far edge is the
        // caller's clamp_lineage).
        assert_eq!(pan_offset(4, 50, 60), 0);
        assert_eq!(pan_offset(10, 50, 50), 10, "no movement → no change");
    }

    #[test]
    fn tooltip_rect_hangs_below_right_and_flips_on_overflow() {
        let area = Rect::new(0, 0, 100, 40);
        let at = Position { x: 10, y: 10 };
        // Preferred: one cell below-right of the pointer cell.
        assert_eq!(
            tooltip_rect(at, 30, 6, area),
            Rect::new(11, 11, 30, 6),
            "below-right anchor"
        );
        // Right-edge overflow: the box lands LEFT of the cell.
        assert_eq!(
            tooltip_rect(Position { x: 95, y: 10 }, 30, 6, area),
            Rect::new(65, 11, 30, 6),
            "flipped left"
        );
        // Bottom-edge overflow: the box ends AT the cell's row.
        assert_eq!(
            tooltip_rect(Position { x: 10, y: 39 }, 30, 6, area),
            Rect::new(11, 33, 30, 6),
            "flipped up"
        );
        // Both edges: bottom-left of the cell.
        assert_eq!(
            tooltip_rect(Position { x: 95, y: 39 }, 30, 6, area),
            Rect::new(65, 33, 30, 6),
            "flipped both ways"
        );
        // A box bigger than the area shrinks to it and clamps inside.
        assert_eq!(
            tooltip_rect(Position { x: 50, y: 20 }, 200, 60, area),
            Rect::new(0, 0, 100, 40),
            "clamped into the area"
        );
        // Degenerate area never panics.
        let _ = tooltip_rect(Position { x: 0, y: 0 }, 5, 3, Rect::new(0, 0, 1, 1));
    }

    // ---- Issue 07: status-line + title chrome geometry ----

    fn hint(key: &str, desc: &str, action: crate::Action, prio: u8) -> crate::action::StatusHint {
        crate::action::StatusHint {
            key: key.to_string(),
            desc: desc.to_string(),
            action: Some(action),
            prio,
        }
    }

    #[test]
    fn status_layout_tiles_hints_left_and_chips_right() {
        use crate::Action as A;
        let hints = vec![
            hint("j/k", "move", A::MoveDown, 10),
            hint("?", "help", A::HelpToggle, 90),
        ];
        let chips = vec![
            (StatusChip::Impact, "[impact v1 ^2]".to_string()),
            (StatusChip::Focus, "[list]".to_string()),
        ];
        let l = status_layout(60, &hints, &chips);
        // Hints: " j/k move" = 9 cells, " ? help" = 7 — contiguous from x=0.
        assert_eq!(
            l.hints,
            vec![
                StatusSpan {
                    x0: 0,
                    x1: 9,
                    target: HitTarget::StatusHint(Some(A::MoveDown)),
                },
                StatusSpan {
                    x0: 9,
                    x1: 16,
                    target: HitTarget::StatusHint(Some(A::HelpToggle)),
                },
            ]
        );
        // Chips are a RIGHT-anchored block: " [impact v1 ^2]" = 15, " [list]"
        // = 7 → the 22-cell block occupies [38, 60).
        assert_eq!(
            l.chips,
            vec![
                StatusSpan {
                    x0: 38,
                    x1: 53,
                    target: HitTarget::StatusChip(StatusChip::Impact),
                },
                StatusSpan {
                    x0: 53,
                    x1: 60,
                    target: HitTarget::StatusChip(StatusChip::Focus),
                },
            ]
        );
    }

    #[test]
    fn status_layout_drops_the_hint_tail_then_the_chip_tail() {
        use crate::Action as A;
        // §8's shed order: the hints' low-priority tail first, then — only
        // once the hints are gone — the chips' tail. The kept set is always
        // a PREFIX of each priority-sorted input.
        let hints = vec![
            hint("j/k", "move", A::MoveDown, 10),       // 9 cells
            hint("Enter", "open", A::DetailOpen, 20),   // 11
            hint("/", "search", A::SearchOpen, 30),     // 9
            hint("O", "new tab", A::TabOpen, 40),       // 10
            hint("Tab", "lineage", A::ToggleFocus, 50), // 12
            hint("?", "help", A::HelpToggle, 90),       // 7
        ];
        let chips = vec![
            (StatusChip::Impact, "[i]".to_string()),
            (StatusChip::Focus, "[f]".to_string()),
            (StatusChip::View, "[v]".to_string()),
        ];
        // 58 hint cells + 12 chip cells = 70 > 60: `? help` (7) sheds → 63
        // still over → `Tab lineage` (12) sheds → 51 fits.
        let l = status_layout(60, &hints, &chips);
        assert_eq!(l.hints.len(), 4, "the two tail hints shed");
        assert_eq!(l.chips.len(), 3, "every chip still fits");
        // At 30: all six hints must go (58 alone > 30), then chips shed to
        // fit — impact + focus (8 cells) survive, view drops.
        let l = status_layout(30, &hints, &chips);
        // 30 - hints 9+11+9=29 keeps 3 hints… recompute: 29+12=41>30 → drop
        // "help" (51→)… walk the prefix: the layout keeps the longest hint
        // prefix that leaves the chips their room.
        let kept_hints = l.hints.len();
        let hints_w: u16 = [9u16, 11, 9, 10, 12, 7][..kept_hints].iter().sum();
        let chips_w: u16 = l.chips.len() as u16 * 4;
        assert!(hints_w + chips_w <= 30, "kept prefix fits");
        assert_eq!(
            l.chips.first().map(|s| &s.target),
            Some(&HitTarget::StatusChip(StatusChip::Impact)),
            "impact survives longest — kept chips are a head-prefix"
        );
        // Degenerate: a width smaller than the impact chip alone keeps
        // nothing — never a panic, never a span past the line.
        let l = status_layout(3, &hints, &chips);
        assert!(l.hints.iter().all(|s| s.x1 <= 3) && l.chips.iter().all(|s| s.x1 <= 3));
    }

    #[test]
    fn hit_target_reports_status_spans_then_plain_status_bar() {
        use crate::Action as A;
        let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
        let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
        let area = Rect::new(0, 0, 80, 24);
        let panes = pane_layout(
            area,
            &PaneConfig {
                tab_bar: false,
                ..cfg()
            },
        );
        let hints = vec![hint("j/k", "move", A::MoveDown, 10)];
        let chips = vec![(StatusChip::Focus, "[list]".to_string())];
        let status_l = status_layout(panes.status.width, &hints, &chips);
        let g = HitGeometry {
            panes,
            tab_bar: None,
            home: None,
            list: &list,
            list_offset: 0,
            list_selected: 0,
            layout: None,
            scroll_x: 0,
            scroll_y: 0,
            minimap: false,
            overlay: None,
            status: Some(&status_l),
            title: None,
        };
        let y = panes.status.y;
        // The first hint's span covers its leading-space cell at x=0.
        assert_eq!(
            hit_target(&g, 0, y),
            Some(HitTarget::StatusHint(Some(A::MoveDown)))
        );
        // The right-anchored chip: " [list]" = 7 cells → [73, 80).
        assert_eq!(
            hit_target(&g, 79, y),
            Some(HitTarget::StatusChip(StatusChip::Focus))
        );
        assert_eq!(
            hit_target(&g, 73, y),
            Some(HitTarget::StatusChip(StatusChip::Focus))
        );
        // The dead cells between the two regions stay plain StatusBar.
        assert_eq!(hit_target(&g, 40, y), Some(HitTarget::StatusBar));
    }

    #[test]
    fn hit_target_reports_title_spans_between_the_corner_cells() {
        let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
        let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
        let panes = pane_layout(
            Rect::new(0, 0, 80, 24),
            &PaneConfig {
                tab_bar: false,
                ..cfg()
            },
        );
        // Title spans are RELATIVE to the first title cell (pane.x + 1):
        // a crumb entry at cells 1..4, the view tag at 20..24, lens at 25..34.
        let title_l = TitleLayout {
            text: " a > b > root         [v] [lens:x]".to_string(),
            spans: vec![
                TitleSpan {
                    kind: TitleKind::Crumb(2),
                    x0: 1,
                    x1: 4,
                },
                TitleSpan {
                    kind: TitleKind::View,
                    x0: 20,
                    x1: 24,
                },
                TitleSpan {
                    kind: TitleKind::Lens,
                    x0: 25,
                    x1: 34,
                },
            ],
        };
        let g = HitGeometry {
            panes,
            tab_bar: None,
            home: None,
            list: &list,
            list_offset: 0,
            list_selected: 0,
            layout: None,
            scroll_x: 0,
            scroll_y: 0,
            minimap: false,
            overlay: None,
            status: None,
            title: Some(&title_l),
        };
        let row = panes.lineage.y;
        let x0 = panes.lineage.x + 1;
        assert_eq!(hit_target(&g, x0 + 2, row), Some(HitTarget::TitleCrumb(2)));
        assert_eq!(hit_target(&g, x0 + 21, row), Some(HitTarget::TitleView));
        assert_eq!(hit_target(&g, x0 + 30, row), Some(HitTarget::TitleLens));
        // An uncovered title cell (between the crumb and the view tag) is
        // inert chrome — None, never a stale span claim.
        assert_eq!(hit_target(&g, x0 + 10, row), None);
        // The border corner columns still belong to the splitters/chrome.
        assert_ne!(
            hit_target(&g, panes.lineage.x, row),
            Some(HitTarget::TitleCrumb(2))
        );
    }
}
