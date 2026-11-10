use super::*;

/// The live pane drag: which surface owns the tracking (§5 splitter, §6
/// lineage pan / minimap scrub). A single slot covers every drag surface —
/// the press cell picks it once, and mid-drag motion never re-hit-tests, so
/// a pointer dragged past a bound keeps the grabbed surface's tracking.
#[derive(Clone, Copy)]
pub(super) enum PaneDrag {
    /// A pane-boundary resize (§5).
    Splitter(SplitterDrag),
    /// A lineage pan (§6): the press cell plus the scroll offset at press
    /// time, replayed through [`pan_offset`] so the diagram tracks the
    /// pointer 1:1 in both axes. Begins on ANY interior cell — blank or
    /// node — because the router only reports it once the 1-cell threshold
    /// already ruled the gesture a drag, never a click (M3).
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
}

/// The live splitter drag: which boundary was grabbed plus the pointer's
/// offset inside its 2-column strip at press time (`0` = the left pane's
/// right-border cell, `1` = the right pane's left-border cell). Carrying the
/// offset keeps the grabbed border under the pointer instead of snapping the
/// pane edge to it.
#[derive(Clone, Copy)]
pub(super) struct SplitterDrag {
    pub(super) which: Splitter,
    pub(super) grab_off: u16,
}

/// The settled-hover tooltip's live state (Issue 05): which node's hover
/// armed it, the lineage viewport at arm time, when the rest began, and
/// whether the [`TOOLTIP_DELAY`] timer already fired. The loop owns the
/// clock; both the reducer and the renderer stay clock-free.
pub(super) struct HoverTip {
    pub(super) uid: String,
    /// The `(scroll_x, scroll_y)` the arm read — a viewport move since then
    /// left the anchored box pointing at stale content, so it re-arms.
    pub(super) scroll: (usize, usize),
    pub(super) since: Instant,
    pub(super) visible: bool,
}

/// The stationary-hover delay before the tooltip shows (§9).
pub(super) const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// Reconcile the settled-hover state against the freshest inputs (§9's
/// "消す条件": press / drag / scroll / leave). The tip arms only while the
/// resolved hover rests on a lineage node that is ALSO a real Dag node —
/// a `+M more` marker is display chrome: it accents its frame but has no
/// `dag.get`, so arming would just dirty the 500ms flip for a `tip_view`
/// that is always `None`. A held button (press or pan), a viewport move
/// under a same-node pointer (the anchor went stale), or a target change
/// re-arms/drops; `visible` only ever flips on the idle-tick timer.
pub(super) fn reconcile_hover_tip(
    hover_tip: &mut Option<HoverTip>,
    hover: Option<&HitTarget>,
    pressed: bool,
    scroll: (usize, usize),
    dag: &dbtl::Dag,
) {
    let hover_uid = match (hover, pressed) {
        (Some(HitTarget::LineageNode(uid)), false) => Some(uid.clone()),
        _ => None,
    };
    match (&mut *hover_tip, &hover_uid) {
        // Same node AND the same viewport: the rest continues undisturbed.
        (Some(t), Some(uid)) if t.uid == *uid && t.scroll == scroll => {}
        (tip, armed) => {
            *tip = armed
                .clone()
                .filter(|uid| dag.get(uid).is_some())
                .map(|uid| HoverTip {
                    uid,
                    scroll,
                    since: Instant::now(),
                    visible: false,
                });
        }
    }
}

/// Resolve a drag's press cell to the [`PaneDrag`] it starts — `None` on a
/// surface with no drag meaning (the list, the strip, the status bar, pane
/// chrome). The same resolution runs for `DragStart` and the `DragEnd`
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
        // §6: a drag that began anywhere inside the drawn lineage — blank
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
        _ => None,
    }
}

/// Apply one tracked step of a live [`PaneDrag`]: the splitter writes its
/// width, the pan replays `pan_offset` 1:1, the minimap centres on the
/// scrubbed cell. The stored offsets then take the loop's per-frame
/// `clamp_lineage` like every other scroll writer.
pub(super) fn apply_pane_drag(
    app: &mut App,
    drag: PaneDrag,
    lineage: Option<&Layout>,
    area: Rect,
    at: ratatui::layout::Position,
) {
    match drag {
        PaneDrag::Splitter(d) => apply_splitter_drag(app, d, at.x, area),
        PaneDrag::LineagePan {
            origin,
            scroll_x,
            scroll_y,
        } => app.ui_state.pan_lineage(
            pan_offset(scroll_x, origin.x, at.x),
            pan_offset(scroll_y, origin.y, at.y),
        ),
        PaneDrag::Minimap => minimap_scroll_at(app, lineage, area, at.x, at.y),
    }
}

/// Centre the lineage viewport on the grid block minimap cell `(col, row)`
/// covers — §6's click-to-centre and each step of a scrub share it. The
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
/// right edge — then clamped to the §1 min/max for this terminal inside
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
/// click-outside dismissal ([`overlay_click`]). `Search` stays pointer-inert.
/// Over the tab strip the wheel scrolls the STRIP, not the panes (§6).
///
/// `chrome` is the §8/§6 span geometry the last draw painted (the loop's
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
    let mut outcome = Outcome::cont();
    match me.kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            if matches!(app.mode, Mode::Search(_)) {
                return MouseHandled {
                    dirty: false,
                    reanchor: false,
                    outcome,
                };
            }
            let rects = pane_layout(area, &app.ui_state.pane_config());
            let down = me.kind == MouseEventKind::ScrollDown;
            // The panes-owning idle modes own the wheel: `Selection` routes
            // by pane, `Home` moves the home cursor (the same notch→cursor
            // semantics the list wheel keeps — j/k in mouse form). A real
            // covering overlay scrolls its own content instead.
            if matches!(app.mode, Mode::Selection | Mode::Home) {
                let shift = me.modifiers.contains(KeyModifiers::SHIFT);
                // §6: the wheel over the tab strip scrolls the STRIP (one
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
        // A native horizontal notch (trackpad swipe / tilt wheel). Only the
        // lineage diagram scrolls horizontally — and over the tab strip it
        // scrolls the tabs, like the wheel; overlays ignore it.
        MouseEventKind::ScrollRight | MouseEventKind::ScrollLeft => {
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
        _ => {
            let Some(input) = MouseInput::from_kind(me.kind, me.column, me.row) else {
                return MouseHandled {
                    dirty: true, // an unclassified kind — conservatively redraw
                    reanchor: false,
                    outcome,
                };
            };
            // A press is gesture-less but not state-free: the held button is
            // an input the hover reconcile reads (a settled tooltip drops on
            // press — §9's "消す条件"). Without a dirty pass the reconcile
            // would never observe `mouse.press()` — the router clears it on
            // release before the Click gesture, so a whole click would slide
            // under a visible tip. Deduped moves and stray events still cost
            // nothing.
            let pressed = matches!(input, MouseInput::Press { .. });
            let Some(gesture) = mouse.feed(input, Instant::now()) else {
                return MouseHandled {
                    dirty: pressed,
                    reanchor: false,
                    outcome,
                };
            };
            let live = app.mode.mouse_live();
            let sel = matches!(app.mode, Mode::Selection);
            // The panes-owning idle modes (`Selection` and `Home`) route
            // clicks to the chrome/body hit-test; covering-overlay modes
            // take `overlay_click` instead. `Home` shares `click_target` —
            // its `HomeRow` arm IS the row's Enter (§3: 項目はクリックでも開く).
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
            // hide (the Issue 02 carry-over), so no abandoned drag resizes
            // or pans on a later unrelated event.
            let continues_drag = sel
                && match &gesture {
                    MouseOutcome::DragStart { button, .. }
                    | MouseOutcome::DragEnd { button, .. } => *button == MouseButton::Left,
                    MouseOutcome::DragMove { .. } => true,
                    _ => false,
                };
            if !continues_drag {
                *pane_drag = None;
            }
            match &gesture {
                MouseOutcome::Hover { at } if live => {
                    let target = {
                        let g = chrome.hit_geometry(app, lineage, area, overlay);
                        hit_target(&g, at.x, at.y)
                    };
                    // Menu rows take a hover cursor — the pointer visibly
                    // selects (separators are skipped by the arm).
                    if let (Mode::Menu(_), Some(HitTarget::OverlayRow(i))) =
                        (&app.mode, target.as_ref())
                    {
                        apply_mouse(app, &mut outcome, Action::MenuSelect(*i));
                    }
                    target_changed = mouse.track_hover(target);
                }
                MouseOutcome::Click { button, at } | MouseOutcome::DoubleClick { button, at }
                    if live =>
                {
                    // A pick that closed the drawer suppresses the follow-up
                    // double: the target it would resolve now sits on the
                    // REVEALED surface — a row, node or pane the user never
                    // aimed at — so no arm fires.
                    let inert =
                        suppress_double && matches!(gesture, MouseOutcome::DoubleClick { .. });
                    let target = if inert {
                        None
                    } else {
                        let g = chrome.hit_geometry(app, lineage, area, overlay);
                        hit_target(&g, at.x, at.y)
                    };
                    if let Some(t) = target.as_ref() {
                        if sel {
                            // §6's M1 table: a double-click is NOT the click
                            // action twice — a node double IS Enter, a list
                            // row's double opens the structure modal, and
                            // every other target takes no second action.
                            let double_left = matches!(gesture, MouseOutcome::DoubleClick { .. })
                                && *button == MouseButton::Left;
                            match t {
                                // §5: a splitter double-click resets that
                                // pane's saved width to default.
                                HitTarget::PaneBorder(which) if double_left => {
                                    app.ui_state.reset_pane_width(*which);
                                }
                                // §6/M1: a lineage node's double-click IS
                                // Enter — park the cursor on it, then run the
                                // Enter arm itself (a model re-roots, a
                                // `+M more` marker expands, anything else
                                // opens the structure modal).
                                HitTarget::LineageNode(uid) if double_left => {
                                    app.commit_lineage_node(uid);
                                    apply_mouse(app, &mut outcome, Action::DetailOpen);
                                }
                                // §6: a list row's double-click opens the
                                // structure modal — the first Click already
                                // re-rooted and took list focus, so
                                // `DetailOpen` resolves the row's model
                                // through `focus_target`.
                                HitTarget::ListRow(_) if double_left => {
                                    apply_mouse(app, &mut outcome, Action::DetailOpen);
                                }
                                // An inspector row's single click: focus the
                                // pane (the universal interior-click meaning
                                // via `click_target`), then pick the column
                                // row under the point when the line IS one —
                                // the loop owns the screen→row mapping
                                // (content + scroll are live loop data).
                                // Enter then traces the picked column.
                                HitTarget::InspectorRow(y) if !double_left => {
                                    let o = app.click_target(t, *button, (at.x, at.y));
                                    outcome.quit |= o.quit;
                                    outcome.effects.extend(o.effects);
                                    outcome.reanchor |= o.reanchor;
                                    if *button == MouseButton::Left {
                                        if let Some(view) = app.inspector_view() {
                                            let inner = pane_interior(
                                                pane_layout(area, &app.ui_state.pane_config())
                                                    .inspector,
                                            );
                                            if let Some(i) = dbtl::ui::inspector_column_at(
                                                &view,
                                                app.glyph_mode,
                                                app.active_theme(),
                                                inner.width as usize,
                                                app.ui_state.inspector_scroll(),
                                                *y as u16,
                                            ) {
                                                apply_mouse(
                                                    app,
                                                    &mut outcome,
                                                    Action::InspectorColumnPick(i),
                                                );
                                            }
                                        }
                                    }
                                }
                                // §6 marks double-click "—" everywhere else:
                                // tabs, `×`, `+`, arrows, inspector rows,
                                // blanks, the minimap and the status bar take
                                // no second action (a `×` double-click must
                                // NOT close the tab that slid under the
                                // pointer; Issue 05 gives inspector rows
                                // theirs).
                                _ if double_left => {}
                                // §6: a left click on the minimap centres the
                                // viewport on the cell's covered grid block —
                                // the drag scrubs through the same mapping.
                                // Left only, like every pane gesture.
                                HitTarget::Minimap if *button == MouseButton::Left => {
                                    minimap_scroll_at(app, lineage, area, at.x, at.y);
                                }
                                // §12: a pick on a drawer row IS "selecting a
                                // model from the drawer" — the same
                                // `click_target` arm the inline list runs —
                                // then the surface's job is done and it
                                // closes. A right-click's context menu keeps
                                // it open (a menu is not a selection).
                                HitTarget::ListRow(_)
                                    if app.ui_state.list_drawer()
                                        && *button != MouseButton::Right =>
                                {
                                    let o = app.click_target(t, *button, (at.x, at.y));
                                    outcome.quit |= o.quit;
                                    outcome.effects.extend(o.effects);
                                    outcome.reanchor |= o.reanchor;
                                    app.ui_state.set_list_drawer(false);
                                    // The drawer is gone: the follow-up
                                    // double-click must not re-hit-test the
                                    // revealed surface.
                                    mouse.suppress_double();
                                }
                                // A click outside the open drawer dismisses
                                // it — the band's modal-exterior surface
                                // (interior/frame/exterior are the only
                                // targets the drawer hit-test can produce;
                                // `ModalExterior` is unreachable otherwise
                                // in `sel`, where no covering overlay owns
                                // the screen).
                                HitTarget::ModalExterior if app.ui_state.list_drawer() => {
                                    app.ui_state.set_list_drawer(false);
                                }
                                _ => {
                                    // §8/§6's clickable chrome returns a
                                    // real Outcome (a hint's action, a chip's
                                    // dispatch) — merge it like every other
                                    // reducer result.
                                    let o = app.click_target(t, *button, (at.x, at.y));
                                    outcome.quit |= o.quit;
                                    outcome.effects.extend(o.effects);
                                    outcome.reanchor |= o.reanchor;
                                }
                            }
                        } else if matches!(app.mode, Mode::Home) {
                            // §3's home surface: rows, the `⌂`/`+`/tab
                            // cells, arrows, status hints and chips all
                            // resolve through `click_target` — a `HomeRow`
                            // click is the row's Enter (`HomePick`), chrome
                            // clicks behave exactly as under `Selection`.
                            let o = app.click_target(t, *button, (at.x, at.y));
                            outcome.quit |= o.quit;
                            outcome.effects.extend(o.effects);
                            outcome.reanchor |= o.reanchor;
                            // A row pick that opened a model tab (or a
                            // command modal) changed the surface under the
                            // pointer — the follow-up double-click must not
                            // re-hit-test what it revealed.
                            if *button == MouseButton::Left && matches!(t, HitTarget::HomeRow(_)) {
                                mouse.suppress_double();
                            }
                        } else {
                            overlay_click(
                                app,
                                &mut outcome,
                                &mut reanchor,
                                t,
                                *button,
                                matches!(gesture, MouseOutcome::DoubleClick { .. }),
                                overlay,
                            );
                        }
                    }
                }
                // §5/§6: a drag's press cell picks its surface once — pane
                // boundary (resize), drawn lineage (pan), or minimap (scrub)
                // — and the stored drag tracks the pointer from then on:
                // mid-drag motion never re-hit-tests, so a pointer dragged
                // past a bound keeps the grabbed surface's tracking. The
                // assignment also clears any drag a superseding press left
                // (a press on a non-drag surface begins nothing).
                MouseOutcome::DragStart { button, origin, at } if sel => {
                    if *button == MouseButton::Left {
                        *pane_drag = begin_pane_drag(app, lineage, area, *origin, chrome);
                        if let Some(d) = *pane_drag {
                            apply_pane_drag(app, d, lineage, area, *at);
                        }
                    }
                }
                MouseOutcome::DragMove { at, .. } if sel => {
                    if let Some(d) = *pane_drag {
                        apply_pane_drag(app, d, lineage, area, *at);
                    }
                }
                MouseOutcome::DragEnd { button, origin, at }
                    if sel && *button == MouseButton::Left =>
                {
                    // A terminal can deliver no Drag events for a fast
                    // flick (press → release elsewhere): the router still
                    // reports DragEnd, so resolve the press cell like
                    // DragStart before finishing.
                    let d = (*pane_drag)
                        .or_else(|| begin_pane_drag(app, lineage, area, *origin, chrome));
                    if let Some(d) = d {
                        apply_pane_drag(app, d, lineage, area, *at);
                    }
                    *pane_drag = None;
                }
                _ => {}
            }
            MouseHandled {
                dirty: mouse_dirty(Some(&gesture), live, target_changed),
                reanchor,
                outcome,
            }
        }
    }
}

/// Route a release-confirmed click on a covering overlay's surface — the
/// `hit_target` result while `overlay` owns the screen. Row semantics follow
/// the modal-mouse rule: a row CLICK selects/highlights; a row DOUBLE-CLICK
/// (or the menu's single click, an execute surface) runs the row's action;
/// the border/title/padding is inert; anything outside dismisses through the
/// mode's OWN Esc binding ([`esc_action`] — BINDINGS-derived, so it can't
/// drift from what the key does).
pub(super) fn overlay_click(
    app: &mut App,
    outcome: &mut Outcome,
    reanchor: &mut bool,
    t: &HitTarget,
    button: MouseButton,
    double: bool,
    overlay: Option<Rect>,
) {
    match t {
        HitTarget::OverlayRow(r) => {
            if button != MouseButton::Left {
                return; // row actions are left-click only
            }
            match &app.mode {
                Mode::Menu(_) => {
                    // A menu row is an execute surface — click and
                    // double-click alike fire the item (nothing to select).
                    apply_mouse(app, outcome, Action::MenuInvoke(*r));
                }
                Mode::Yank { .. } => {
                    if double {
                        apply_mouse(app, outcome, Action::YankInvoke(*r));
                    } else {
                        apply_mouse(app, outcome, Action::YankPick(*r));
                    }
                }
                Mode::Palette(p) => {
                    // Interior row 0 is the query line; below it the display
                    // row at `off + r - 1` in `palette_rows` space — the same
                    // window math `draw_palette` uses. The drawn rows include
                    // non-selectable headers, while `selected` indexes the
                    // selectable-only list, so the click maps display →
                    // selectable before `PalettePick`. (Copy the state out:
                    // the applies below need `app` mutably.)
                    let inner_h = overlay.map(|o| pane_interior(o).height).unwrap_or(0);
                    let view_h = inner_h.saturating_sub(1) as usize;
                    let query = p.query.clone();
                    let selected = p.selected;
                    if *r >= 1 && view_h > 0 {
                        let rows = app.palette_rows(&query);
                        let selectable = rows
                            .iter()
                            .filter(|r| !matches!(r, PaletteRow::Header(_)))
                            .count();
                        let sel = if selectable == 0 {
                            usize::MAX
                        } else {
                            selected.min(selectable - 1)
                        };
                        // The clamped selection's DISPLAY position — draw's
                        // scroll window homes on it.
                        let mut si = 0usize;
                        let mut sel_display = 0usize;
                        for (di, row) in rows.iter().enumerate() {
                            if !matches!(row, PaletteRow::Header(_)) {
                                if si == sel {
                                    sel_display = di;
                                }
                                si += 1;
                            }
                        }
                        let total = rows.len();
                        let off = sel_display
                            .saturating_sub(view_h - 1)
                            .min(total.saturating_sub(view_h));
                        let di = off + r - 1;
                        // Display index → selectable index; a header cell is
                        // chrome, not a row — the click is inert there.
                        let pick = rows.get(di).and_then(|row| {
                            if matches!(row, PaletteRow::Header(_)) {
                                return None;
                            }
                            Some(
                                rows.iter()
                                    .take(di)
                                    .filter(|r| !matches!(r, PaletteRow::Header(_)))
                                    .count(),
                            )
                        });
                        if let Some(i) = pick {
                            apply_mouse(app, outcome, Action::PalettePick(i));
                            if double {
                                // Peek at what SearchConfirm resolves to so a
                                // reshaping pick can flag the anchor BEFORE
                                // the apply hides which action ran.
                                let resolved = match rows.get(di) {
                                    Some(PaletteRow::Command { action, .. }) => Some(*action),
                                    _ => None,
                                };
                                *reanchor = resolved.is_some_and(reanchors_lineage);
                                apply_mouse(app, outcome, Action::SearchConfirm);
                            }
                        }
                    }
                }
                Mode::Columns(cv) => {
                    // Interior rows ARE `cv.rows` offset by the drawn scroll
                    // (per-frame clamped), so `scroll + r` is the line index.
                    let i = cv.scroll + *r;
                    if i < cv.rows.len() {
                        apply_mouse(app, outcome, Action::ColumnPick(i));
                        if double {
                            apply_mouse(app, outcome, Action::ColumnJump);
                        }
                    }
                }
                // Scrollable text modals (help/detail/sql/stats/diff): rows
                // are content, not commands — clicking them is inert.
                _ => {}
            }
        }
        HitTarget::ModalInterior => {} // frame chrome: inert by design
        HitTarget::ModalExterior => {
            if let Some(a) = esc_action(&app.mode) {
                apply_mouse(app, outcome, a);
            }
        }
        // The overlay owns the whole screen — pane targets can't be produced.
        _ => {}
    }
}

/// The §8/§6 span geometry the last draw painted — the status line's
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
/// `status`/`title` are the §8/§6 layouts the loop stashed after the last
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

/// Route §12's drawer-owned keys in the narrow band — `Esc` closes an open
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

/// Whether screen cell `(x, y)` is inside `r`.
pub(super) fn within(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Route a vertical wheel notch to whichever pane the cursor is over. Over the
/// lineage pane, Shift remaps the notch to a horizontal pan (down = right) —
/// the usual convention for mice without a horizontal wheel; over the list,
/// Shift changes nothing (the list has no horizontal axis).
pub(super) fn wheel(app: &mut App, rects: &PaneRects, col: u16, row: u16, down: bool, shift: bool) {
    // The home body owns its band (the caller routes home notches to the
    // home cursor — this arm only guards a stale call).
    if app.home_active() {
        return;
    }
    // §12's drawer owns the wheel while it floats: its interior IS the list
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
        // §2 T1: a wheel notch over the list moves the CURSOR — adopt it as
        // the active tab's root exactly like a j/k move (guarded on the
        // index actually changing, so a clamped notch at the boundary
        // never re-roots a detached tab).
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
pub(super) fn wheel_x(app: &mut App, rects: &PaneRects, col: u16, row: u16, right: bool) {
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
