use super::*;

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
    /// `HitTarget::HomeRow(i)`; every other line is inert).
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
