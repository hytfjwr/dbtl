use super::*;

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
