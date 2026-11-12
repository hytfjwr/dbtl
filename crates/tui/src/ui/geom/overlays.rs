use super::*;

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
/// columns)` — on wide terminals a short sentence in a 160-col box is
/// unreadable.
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

/// The `y` menu's frame rect: content-sized (the longest `y<key>  help`
/// row + padding), horizontally centred, anchored to the bottom edge so it
/// stays clear of the content it acts on. Shared by `draw_yank_menu` and the
/// mouse hit-test so a row click lands on the drawn row. `None` on terminals
/// too small to host the box (the draw skips it there too).
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
/// when the preferred box would overflow the terminal (the anchor-below-
/// right, flip-on-overflow rule). Shared by `draw_menu` and the mouse
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
/// box for the scrollable modals, the palette's own box, the `y` yank
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
        // same as the yank / node menu.
        Mode::Help(h) => Some(h.back.as_ref()),
        _ => None,
    }
}

/// The centered sub-rect a `grid_w × grid_h` lineage diagram occupies inside the
/// lineage pane `interior`: as large as the diagram up to the interior, padded to
/// the centre. The SINGLE source of this geometry — both `draw_lineage_pane` and
/// the mouse `hit_test` use it, so "computed window == drawn window" holds for
/// hit-testing too (clicks in the pad margin map to no node).
/// The hover tooltip's widest box in columns (the floating-chrome
/// rule — a name-plus-two-lines box never becomes a wall of text).
pub const TOOLTIP_MAX_W: u16 = 48;

/// The hover tooltip's frame rect: hangs one cell below-right of the pointer
/// cell `at`, flips to the cell's left/top when the preferred corner would
/// overflow `area` (the same anchor policy [`menu_rect`] applies), then
/// clamps inside `area`. `w`/`h` are the
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
