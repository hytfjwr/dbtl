use super::drag::minimap_scroll_at;
use super::*;

/// Route a release-confirmed `Click`/`DoubleClick` to the surface the
/// point resolves to: `Selection` runs the double-click table below, `Home`
/// treats a row click as the row's Enter, and a covering-overlay mode
/// routes through [`overlay_click`]. `double` distinguishes the two router
/// gestures; `suppress_double` is the one-shot flag a pick that closed a
/// transient overlay sets — the follow-up double then resolves NO target
/// (the surface it would hit-test is one the user never aimed at).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_click(
    app: &mut App,
    mouse: &mut MouseRouter,
    outcome: &mut Outcome,
    reanchor: &mut bool,
    button: MouseButton,
    at: ratatui::layout::Position,
    double: bool,
    suppress_double: bool,
    chrome: &ChromeGeom,
    lineage: Option<&Layout>,
    area: Rect,
    overlay: Option<Rect>,
) {
    // A pick that closed the drawer suppresses the follow-up
    // double: the target it would resolve now sits on the
    // REVEALED surface — a row, node or pane the user never
    // aimed at — so no arm fires.
    let inert = suppress_double && double;
    let target = if inert {
        None
    } else {
        let g = chrome.hit_geometry(app, lineage, area, overlay);
        hit_target(&g, at.x, at.y)
    };
    if let Some(t) = target.as_ref() {
        if matches!(app.mode, Mode::Selection) {
            // The double-click table: a double-click is NOT the click
            // action twice — a node double IS Enter, a list
            // row's double opens the structure modal, and
            // every other target takes no second action.
            let double_left = double && button == MouseButton::Left;
            match t {
                // A splitter double-click resets that
                // pane's saved width to default.
                HitTarget::PaneBorder(which) if double_left => {
                    app.ui_state.reset_pane_width(*which);
                }
                // A lineage node's double-click IS
                // Enter — park the cursor on it, then run the
                // Enter arm itself (a model re-roots, a
                // `+M more` marker expands, anything else
                // opens the structure modal).
                HitTarget::LineageNode(uid) if double_left => {
                    app.commit_lineage_node(uid);
                    apply_mouse(app, outcome, Action::DetailOpen);
                }
                // A list row's double-click opens the
                // structure modal — the first Click already
                // re-rooted and took list focus, so
                // `DetailOpen` resolves the row's model
                // through `focus_target`.
                HitTarget::ListRow(_) if double_left => {
                    apply_mouse(app, outcome, Action::DetailOpen);
                }
                // An inspector row's single click: focus the
                // pane (the universal interior-click meaning
                // via `click_target`), then pick the column
                // row under the point when the line IS one —
                // the loop owns the screen→row mapping
                // (content + scroll are live loop data).
                // Enter then traces the picked column.
                HitTarget::InspectorRow(y) if !double_left => {
                    let o = app.click_target(t, button, (at.x, at.y));
                    outcome.quit |= o.quit;
                    outcome.effects.extend(o.effects);
                    outcome.reanchor |= o.reanchor;
                    if button == MouseButton::Left {
                        if let Some(view) = app.inspector_view() {
                            let inner = pane_interior(
                                pane_layout(area, &app.ui_state.pane_config()).inspector,
                            );
                            if let Some(i) = dbtl::ui::inspector_column_at(
                                &view,
                                app.glyph_mode,
                                app.active_theme(),
                                inner.width as usize,
                                app.ui_state.inspector_scroll(),
                                *y as u16,
                            ) {
                                apply_mouse(app, outcome, Action::InspectorColumnPick(i));
                            }
                        }
                    }
                }
                // Everywhere else a double-click is inert:
                // tabs, `×`, `+`, arrows, inspector rows,
                // blanks, the minimap and the status bar take
                // no second action (a `×` double-click must
                // NOT close the tab that slid under the
                // pointer; inspector rows keep their own
                // rule).
                _ if double_left => {}
                // A left click on the minimap centres the
                // viewport on the cell's covered grid block —
                // the drag scrubs through the same mapping.
                // Left only, like every pane gesture.
                HitTarget::Minimap if button == MouseButton::Left => {
                    minimap_scroll_at(app, lineage, area, at.x, at.y);
                }
                // A pick on a drawer row IS "selecting a
                // model from the drawer" — the same
                // `click_target` arm the inline list runs —
                // then the surface's job is done and it
                // closes. A right-click's context menu keeps
                // it open (a menu is not a selection).
                HitTarget::ListRow(_)
                    if app.ui_state.list_drawer() && button != MouseButton::Right =>
                {
                    let o = app.click_target(t, button, (at.x, at.y));
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
                    // Clickable chrome returns a
                    // real Outcome (a hint's action, a chip's
                    // dispatch) — merge it like every other
                    // reducer result.
                    let o = app.click_target(t, button, (at.x, at.y));
                    outcome.quit |= o.quit;
                    outcome.effects.extend(o.effects);
                    outcome.reanchor |= o.reanchor;
                }
            }
        } else if matches!(app.mode, Mode::Home) {
            // The home surface: rows, the `⌂`/`+`/tab
            // cells, arrows, status hints and chips all
            // resolve through `click_target` — a `HomeRow`
            // click is the row's Enter (`HomePick`), chrome
            // clicks behave exactly as under `Selection`.
            let o = app.click_target(t, button, (at.x, at.y));
            outcome.quit |= o.quit;
            outcome.effects.extend(o.effects);
            outcome.reanchor |= o.reanchor;
            // A row pick that opened a model tab (or a
            // command modal) changed the surface under the
            // pointer — the follow-up double-click must not
            // re-hit-test what it revealed.
            if button == MouseButton::Left && matches!(t, HitTarget::HomeRow(_)) {
                mouse.suppress_double();
            }
        } else {
            overlay_click(app, outcome, reanchor, t, button, double, overlay);
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
