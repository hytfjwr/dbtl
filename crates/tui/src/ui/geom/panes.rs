use super::*;

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
