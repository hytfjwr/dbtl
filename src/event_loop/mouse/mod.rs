//! Mouse input routing: [`handle_mouse`] splits a raw event by gesture
//! kind into the sibling files, all `pub(super)` so `mouse::X` paths are
//! unchanged:
//! - [`wheel`] — the vertical/horizontal wheel arms (pane routing, the
//!   tab strip, the home cursor, modal scrolling).
//! - [`click`] — release-confirmed clicks and double-clicks (the
//!   `Selection` double-click table, the `Home` row pick, the covering
//!   overlay's row semantics + outside-click dismissal).
//! - [`drag`] — the live pane drag (splitter resize, lineage pan,
//!   minimap scrub, tab reorder).
//! - [`hover`] — hover tracking plus the settled-hover tooltip's state.
//!
//! Shared plumbing stays here: the [`MouseHandled`] accumulation, the
//! [`apply_mouse`] outcome merge, the [`ChromeGeom`]/[`hit_geometry`]
//! hit-test seam, and the drawer shortcut.

use super::*;

mod click;
mod drag;
mod hover;
mod wheel;

pub(super) use drag::PaneDrag;
pub(super) use hover::{reconcile_hover_tip, HoverTip, TOOLTIP_DELAY};

/// What one mouse event produced: the frame-dirty flag plus the accumulated
/// reducer [`Outcome`] (overlay surfaces fire real actions — a menu row
/// invoke, an outside-click Esc — whose effects the caller must run) and the
/// reanchor flag a resolved palette action may need.
pub(super) struct MouseHandled {
    pub(super) dirty: bool,
    pub(super) reanchor: bool,
    pub(super) outcome: Outcome,
}

/// Apply `action` and merge its [`Outcome`] into the running accumulation —
/// one mouse gesture can legitimately chain two applies (a columns row's
/// double-click = pick + jump; a palette row's = pick + confirm).
pub(super) fn apply_mouse(app: &mut App, outcome: &mut Outcome, action: Action) {
    let o = apply_action(app, action);
    outcome.quit |= o.quit;
    outcome.effects.extend(o.effects);
    // The flag a reducer-set reanchor already carries OR the same
    // dispatched-action check the key path runs — a mouse-fired ResetView
    // recentres identically to `0`.
    outcome.reanchor |= o.reanchor || reanchors_lineage(action);
}

/// Handle a mouse event (size-aware, so it lives in the loop, not the reducer).
/// Raw kinds go through the [`MouseRouter`] (release-confirmed clicks, the
/// double-click window, the ≥1-cell drag threshold, hover dedup); the
/// resulting point maps to a [`HitTarget`] via the pure [`hit_target`] — the
/// same geometry the renderer draws.
///
/// `Selection` routes points to the panes (clicks dispatch through
/// [`App::click_target`]); every other mouse-live mode routes through the
/// covering overlay's rect — its interior rows, its inert frame, and the
/// click-outside dismissal ([`click::overlay_click`]). `Search` stays
/// pointer-inert. Over the tab strip the wheel scrolls the STRIP, not the
/// panes.
///
/// `chrome` is the status/title span geometry the last draw painted (the loop's
/// stash) — status hints/chips and title parts hit-test against the exact
/// cells the screen showed.
pub(super) fn handle_mouse(
    app: &mut App,
    mouse: &mut MouseRouter,
    pane_drag: &mut Option<PaneDrag>,
    me: MouseEvent,
    lineage: Option<&Layout>,
    area: Rect,
    chrome: &ChromeGeom,
) -> MouseHandled {
    match me.kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            wheel::handle_wheel(app, &me, area)
        }
        // A native horizontal notch (trackpad swipe / tilt wheel). Only the
        // lineage diagram scrolls horizontally — and over the tab strip it
        // scrolls the tabs, like the wheel; overlays ignore it.
        MouseEventKind::ScrollRight | MouseEventKind::ScrollLeft => {
            wheel::handle_wheel_x(app, &me, area)
        }
        _ => handle_gesture(app, mouse, pane_drag, me, lineage, area, chrome),
    }
}

/// Route one router-emitted [`MouseOutcome`] gesture to its kind's handler:
/// hover settles the pointer target, clicks run the release-confirmed click
/// tables, and the drag arms own the live [`PaneDrag`] slot.
///
/// The shared prelude feeds the raw kind through the router (a press is
/// gesture-less but not state-free: the held button is an input the hover
/// reconcile reads — a settled tooltip drops on press, one of the dismissal
/// inputs. Without a dirty pass the reconcile would never observe
/// `mouse.press()` — the router clears it on release before the Click
/// gesture, so a whole click would slide under a visible tip. Deduped moves
/// and stray events still cost nothing). The panes-owning idle modes
/// (`Selection` and `Home`) route clicks to the chrome/body hit-test —
/// `Home` shares `click_target` (its `HomeRow` arm IS the row's Enter) —
/// while covering-overlay modes take `overlay_click` instead.
fn handle_gesture(
    app: &mut App,
    mouse: &mut MouseRouter,
    pane_drag: &mut Option<PaneDrag>,
    me: MouseEvent,
    lineage: Option<&Layout>,
    area: Rect,
    chrome: &ChromeGeom,
) -> MouseHandled {
    let mut outcome = Outcome::cont();
    let Some(input) = MouseInput::from_kind(me.kind, me.column, me.row) else {
        return MouseHandled {
            dirty: true, // an unclassified kind — conservatively redraw
            reanchor: false,
            outcome,
        };
    };
    let pressed = matches!(input, MouseInput::Press { .. });
    let Some(gesture) = mouse.feed(input, Instant::now()) else {
        return MouseHandled {
            dirty: pressed,
            reanchor: false,
            outcome,
        };
    };
    let live = app.mode.mouse_live();
    // The panes-owning idle modes (`Selection` and `Home`) route
    // clicks to the chrome/body hit-test; covering-overlay modes
    // take `overlay_click` instead. `Home` shares `click_target` —
    // its `HomeRow` arm IS the row's Enter (a home row opens on click).
    let panes_own = matches!(app.mode, Mode::Selection | Mode::Home);
    // One consume per emitted gesture: a pick that closed a
    // transient overlay flags its follow-up `DoubleClick` inert (see
    // `MouseRouter::suppress_double`); any other gesture in between
    // simply clears it.
    let suppress_double = mouse.take_suppress_double();
    // The covering overlay's outer rect — the hit-test routes by it
    // (interior rows / frame / exterior), computed from the SAME
    // layout + scroll the frame drew.
    let overlay = if panes_own {
        None
    } else {
        let rects = pane_layout(area, &app.ui_state.pane_config());
        overlay_rect(
            &app.mode,
            area,
            pane_interior(rects.lineage),
            lineage,
            app.ui_state.lineage_scroll_x(),
            app.ui_state.lineage_scroll_y(),
        )
    };
    let mut target_changed = false;
    let mut reanchor = false;
    // The stored drag is live ONLY while the gesture continues it:
    // a press starts a new gesture before the old release arrives
    // (the router keeps the newest press — the in-flight drag is
    // abandoned), a mode flip ends pointer routing, and a non-left
    // drag never resizes/pans. Anything else clears it HERE — the
    // '_' fallback arm is the last place a stale splitter drag can
    // hide (the stale-drag carry-over), so no abandoned drag resizes
    // or pans on a later unrelated event.
    let continues_drag = panes_own
        && match &gesture {
            MouseOutcome::DragStart { button, .. } | MouseOutcome::DragEnd { button, .. } => {
                *button == MouseButton::Left
            }
            MouseOutcome::DragMove { .. } => true,
            _ => false,
        };
    if !continues_drag {
        *pane_drag = None;
    }
    match &gesture {
        MouseOutcome::Hover { at } if live => {
            target_changed = hover::handle_hover(
                app,
                mouse,
                &mut outcome,
                *at,
                chrome,
                lineage,
                area,
                overlay,
            );
        }
        MouseOutcome::Click { button, at } | MouseOutcome::DoubleClick { button, at } if live => {
            click::handle_click(
                app,
                mouse,
                &mut outcome,
                &mut reanchor,
                *button,
                *at,
                matches!(gesture, MouseOutcome::DoubleClick { .. }),
                suppress_double,
                chrome,
                lineage,
                area,
                overlay,
            );
        }
        // A drag's press cell picks its surface once — pane
        // boundary (resize), drawn lineage (pan), or minimap (scrub)
        // — and the stored drag tracks the pointer from then on:
        // mid-drag motion never re-hit-tests, so a pointer dragged
        // past a bound keeps the grabbed surface's tracking. The
        // assignment also clears any drag a superseding press left
        // (a press on a non-drag surface begins nothing).
        MouseOutcome::DragStart { button, origin, at } if panes_own => {
            drag::handle_drag_start(app, pane_drag, *button, *origin, *at, chrome, lineage, area);
        }
        MouseOutcome::DragMove { at, .. } if panes_own => {
            drag::handle_drag_move(app, pane_drag, *at, chrome, lineage, area);
        }
        MouseOutcome::DragEnd { button, origin, at }
            if panes_own && *button == MouseButton::Left =>
        {
            drag::handle_drag_end(app, pane_drag, *origin, *at, chrome, lineage, area);
        }
        _ => {}
    }
    MouseHandled {
        dirty: mouse_dirty(Some(&gesture), live, target_changed),
        reanchor,
        outcome,
    }
}

/// The status/title span geometry the last draw painted — the status line's
/// hint/chip placement and the lineage title's crumb/`[{v}]`/`[lens:…]`
/// placement — stashed by the loop and handed to [`hit_geometry`] so a
/// hover/click lands on the exact spans the screen showed.
#[derive(Default)]
pub(super) struct ChromeGeom {
    pub(super) status: Option<StatusLayout>,
    pub(super) title: Option<TitleLayout>,
}

impl ChromeGeom {
    /// [`hit_geometry`] with the stash's borrows — the one call shape every
    /// mouse path uses.
    pub(super) fn hit_geometry<'a>(
        &'a self,
        app: &'a App,
        lineage: Option<&'a Layout>,
        area: Rect,
        overlay: Option<Rect>,
    ) -> HitGeometry<'a> {
        hit_geometry(
            app,
            lineage,
            area,
            overlay,
            self.status.as_ref(),
            self.title.as_ref(),
        )
    }
}

/// Bundle the frame's hit-test inputs, all from the SAME sources the draw
/// uses: `pane_layout` on the live area + `pane_config`, the last styled
/// `Layout`, and `UiState`'s scroll offsets — so a click lands on what the
/// screen shows. `overlay` is the covering overlay's outer rect
/// ([`overlay_rect`]'s output) — `None` while the panes are the surface.
/// `status`/`title` are the layouts the loop stashed after the last
/// draw — the hit-test reads the SAME spans the frame painted.
pub(super) fn hit_geometry<'a>(
    app: &'a App,
    lineage: Option<&'a Layout>,
    area: Rect,
    overlay: Option<Rect>,
    status: Option<&'a StatusLayout>,
    title: Option<&'a TitleLayout>,
) -> HitGeometry<'a> {
    // The drawer's flag is masked while the home body owns the band — a
    // stale `list_drawer` must never feed the hit-test surface draw never
    // paints (draw skips it under `ctx.home`; this mirrors that here).
    let mut cfg = app.ui_state.pane_config();
    cfg.list_drawer &= !app.home_active();
    let panes = pane_layout(area, &cfg);
    let home = home_hit_lines(app, &panes);
    HitGeometry {
        panes,
        // Replay the stored scroll — synced to the RESOLVED scroll after
        // every draw, so the hit-test sees the cells the frame painted
        // (`ensure_active: false`: the ensure already ran at draw time).
        tab_bar: Some(app.tab_bar_layout_for(panes.tab_bar, false)),
        list: app.active_list(),
        list_offset: app.ui_state.offset(),
        list_selected: app.ui_state.selected(),
        layout: lineage,
        scroll_x: app.ui_state.lineage_scroll_x(),
        scroll_y: app.ui_state.lineage_scroll_y(),
        minimap: app.ui_state.minimap_visible(),
        overlay,
        status,
        title,
        home,
    }
}

/// Route the drawer-owned keys in the narrow band — `Esc` closes an open
/// drawer, `Ctrl-b` toggles it. Returns `true` when the key was consumed
/// HERE, so the caller leaves the reducer (and the inline-list pref
/// `Ctrl-b` guards) untouched. Outside the band the keys keep their
/// ordinary meanings; inside it only Selection's `Esc` reaches the drawer
/// (a covering modal owns its own `Esc` while it's up).
pub(super) fn drawer_shortcut(app: &mut App, key: event::KeyEvent, action: Action) -> bool {
    if !app.ui_state.list_auto_hidden() {
        return false;
    }
    if matches!(app.mode, Mode::Selection) && key.code == KeyCode::Esc && app.ui_state.list_drawer()
    {
        app.ui_state.set_list_drawer(false);
        return true;
    }
    if matches!(action, Action::ToggleListPane) {
        // The list isn't part of the home body — `Ctrl-b` has no surface to
        // toggle there, so the key stays inert rather than flagging a
        // drawer that never draws.
        if app.home_active() {
            return true;
        }
        app.ui_state.toggle_list_drawer();
        return true;
    }
    false
}

/// The home body's line window while the home tab is active — the SAME
/// plan/scroll/height the draw used (the loop resolved `app.home_scroll`
/// before stashing it), so a click lands on the row the screen painted.
pub(super) fn home_hit_lines(
    app: &App,
    panes: &dbtl::ui::PaneRects,
) -> Option<Vec<dbtl::ui::HomeLine>> {
    if !app.home_active() {
        return None;
    }
    let body = panes.list.union(panes.lineage).union(panes.inspector);
    let data = app.home_data();
    Some(dbtl::ui::home_window(
        &dbtl::ui::home_lines(&data),
        body.height as usize,
        app.home_scroll,
    ))
}
