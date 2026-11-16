use super::*;

/// The `ScrollDown`/`ScrollUp` arm: the panes-owning idle modes own the
/// wheel — `Selection` routes by pane, `Home` moves the home cursor (the
/// same notch→cursor semantics the list wheel keeps — j/k in mouse form) —
/// and a real covering overlay scrolls its own content instead. Over the
/// tab strip the wheel scrolls the STRIP, not the panes.
pub(super) fn handle_wheel(app: &mut App, me: &MouseEvent, area: Rect) -> MouseHandled {
    let mut outcome = Outcome::cont();
    if matches!(app.mode, Mode::Search(_)) {
        return MouseHandled {
            dirty: false,
            reanchor: false,
            outcome,
        };
    }
    let rects = pane_layout(area, &app.ui_state.pane_config());
    let down = me.kind == MouseEventKind::ScrollDown;
    if matches!(app.mode, Mode::Selection | Mode::Home) {
        let shift = me.modifiers.contains(KeyModifiers::SHIFT);
        // The wheel over the tab strip scrolls the STRIP (one
        // tab per notch) — the panes below never see it.
        if within(rects.tab_bar, me.column, me.row) {
            app.scroll_tab_bar(if down { 1 } else { -1 });
        } else if app.home_active() {
            app.move_home_sel(if down { Direction::Down } else { Direction::Up });
        } else {
            wheel(app, &rects, me.column, me.row, down, shift);
        }
    } else {
        // A covering overlay owns the screen, so the wheel scrolls
        // the modal's own content regardless of the pointer cell —
        // the shared DetailScroll arm (non-scrollable overlays
        // no-op it; the per-frame clamp bounds the result).
        apply_mouse(
            app,
            &mut outcome,
            Action::DetailScroll(if down { Direction::Down } else { Direction::Up }),
        );
    }
    MouseHandled {
        dirty: true,
        reanchor: false,
        outcome,
    }
}

/// The `ScrollRight`/`ScrollLeft` arm: a native horizontal notch (trackpad
/// swipe / tilt wheel). Only the lineage diagram scrolls horizontally — and
/// over the tab strip it scrolls the tabs, like the wheel; overlays ignore
/// it.
pub(super) fn handle_wheel_x(app: &mut App, me: &MouseEvent, area: Rect) -> MouseHandled {
    let outcome = Outcome::cont();
    if !matches!(app.mode, Mode::Selection | Mode::Home) {
        return MouseHandled {
            dirty: false,
            reanchor: false,
            outcome,
        };
    }
    let rects = pane_layout(area, &app.ui_state.pane_config());
    if within(rects.tab_bar, me.column, me.row) {
        app.scroll_tab_bar(if me.kind == MouseEventKind::ScrollRight {
            1
        } else {
            -1
        });
    } else if !app.home_active() {
        // Home has no horizontal axis — the strip is the only
        // horizontally scrollable chrome it keeps.
        wheel_x(
            app,
            &rects,
            me.column,
            me.row,
            me.kind == MouseEventKind::ScrollRight,
        );
    }
    MouseHandled {
        dirty: true,
        reanchor: false,
        outcome,
    }
}

/// Whether screen cell `(x, y)` is inside `r`.
fn within(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Route a vertical wheel notch to whichever pane the cursor is over. Over the
/// lineage pane, Shift remaps the notch to a horizontal pan (down = right) —
/// the usual convention for mice without a horizontal wheel; over the list,
/// Shift changes nothing (the list has no horizontal axis).
fn wheel(app: &mut App, rects: &PaneRects, col: u16, row: u16, down: bool, shift: bool) {
    // The home body owns its band (the caller routes home notches to the
    // home cursor — this arm only guards a stale call).
    if app.home_active() {
        return;
    }
    // The drawer owns the wheel while it floats: its interior IS the list
    // surface (the same notch→cursor+adopt the inline pane gets) and its
    // frame is inert chrome. Cells OUTSIDE it keep their pane meaning — the
    // lineage to the right of the drawer stays scrollable; only clicks
    // outside dismiss it.
    if let Some(dr) = rects.list_drawer {
        if within(dr, col, row) {
            if within(pane_interior(dr), col, row) {
                let before = app.ui_state.selected();
                app.ui_state.wheel_list(down);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            return;
        }
    }
    if within(pane_interior(rects.list), col, row) {
        // Browsing is selecting: a wheel notch over the list moves the
        // CURSOR — adopt it as the active tab's root exactly like a j/k
        // move (guarded on the index actually changing, so a clamped
        // notch at the boundary never re-roots a detached tab).
        let before = app.ui_state.selected();
        app.ui_state.wheel_list(down);
        if app.ui_state.selected() != before {
            app.adopt_list_selection();
        }
    } else if within(pane_interior(rects.inspector), col, row) {
        app.ui_state.wheel_inspector(down);
    } else if within(pane_interior(rects.lineage), col, row) {
        if shift {
            app.ui_state.wheel_lineage_x(down);
        } else {
            app.ui_state.wheel_lineage(down);
        }
    }
}

/// Route a horizontal wheel notch to the lineage pane — the only horizontally
/// scrollable surface. The saturating pan intent is bounded by the loop's
/// `clamp_lineage` next frame, same as the vertical wheel.
fn wheel_x(app: &mut App, rects: &PaneRects, col: u16, row: u16, right: bool) {
    if app.home_active() {
        return;
    }
    // The drawer swallows every wheel kind over its surface — a notch there
    // must never reach the lineage it covers.
    if rects.list_drawer.is_some_and(|dr| within(dr, col, row)) {
        return;
    }
    if within(pane_interior(rects.lineage), col, row) {
        app.ui_state.wheel_lineage_x(right);
    }
}
