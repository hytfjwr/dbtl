//! The interactive event loop: the only size-aware layer. Follows scroll
//! (list + lineage), draws, reads an event, maps it to an [`Action`] via the
//! keymap, applies it, runs any [`Effect`]s, and repeats until quit — plus the
//! impure helpers those effects need (editor suspend, clipboard, `dbt parse`).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use dbtl::action::{dispatch, reanchors_lineage, Action};
use dbtl::app::{apply_action, App, LineageView};
use dbtl::effect::Effect;
use dbtl::layout::Layout;
use dbtl::ui::{
    draw, hit_target, mouse_dirty, pane_interior, pane_layout, splitter_drag_width, Focus,
    HitGeometry, HitTarget, LineageLens, MouseInput, MouseOutcome, MouseRouter, RenderCtx,
    Splitter, StatusSegments,
};
use dbtl::{Mode, SortMode};

use ratatui::crossterm::event::{
    self, Event, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;

use crate::terminal::{set_keyboard_enhancement, set_mouse_capture};
use crate::watch::Watch;

/// The event loop: follow scroll (size-aware), draw, read an event, map it to an
/// [`Action`] via the keymap, apply it, run any effects, repeat until quit.
///
/// The size-aware steps (`ensure_visible`, `anchor_lineage`,
/// `ensure_lineage_visible`, `clamp_lineage`) stay here — the pure reducer
/// (`apply_action`) never sees pane geometry. Only `KeyEventKind::Press` drives
/// state, so Press+Release/Repeat terminals don't double-fire.
pub fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    watch: &mut Option<Watch>,
) -> Result<()> {
    // Track the selection (by unique_id, so a filter change keeping the same
    // node doesn't churn) and the lineage cursor, to re-anchor / follow on
    // change. `last_selection: None` forces an anchor on the first frame;
    // `force_anchor` is set by actions that reshape/recentre the lineage
    // without moving the selection.
    let mut last_selection: Option<String> = None;
    let mut last_cursor: Option<String> = None;
    let mut force_anchor = false;
    // The on-screen toast stack (Issue 08): each App notice is stamped with
    // its arrival time and pushed to the TOP — the loop owns the per-level
    // lifetime (info 2.5s / warning 5s, the reducer stays clock-free), and an
    // expired entry drops on the next iteration — the 250ms idle tick
    // guarantees that redraw happens promptly. Past `ToastStack::MAX` the
    // oldest evicts.
    let mut toasts = ToastStack::default();
    // The mouse gesture router: tracks the live press (release-confirmed
    // clicks, the double-click window, the drag threshold) and the last
    // reported hover target — the `Moved` redraw gate. The loop owns the
    // clock; the router only compares `Instant`s it is handed.
    let mut mouse = MouseRouter::new();
    // The live splitter drag: the grabbed boundary plus the pointer's offset
    // inside its 2-cell strip at press time. Loop-owned (like the router):
    // the drag tracks pointer columns, so it is size-aware state by nature.
    let mut splitter_drag: Option<SplitterDrag> = None;
    // Dirty-flag redraw: the frame work below only runs when something
    // actually changed (first frame, any event, a toast flip, a --watch
    // reload). An idle 250ms tick with none of those skips straight back to
    // `event::poll` — before this, every tick recomputed the O(N) status
    // aggregates and rewrote the whole terminal.
    let mut dirty = true;
    // The last frame's lineage layout + full area: recomputed only inside a
    // dirty pass, but the mouse handler reads them after the draw section —
    // hit-testing against a stale layout would misattribute clicks.
    let mut lineage: Option<std::rc::Rc<dbtl::Layout>> = None;
    let mut area = ratatui::layout::Rect::default();

    loop {
        // Drain the pending notice WITH its level: it becomes BOTH a toast and
        // a stamped notification-log entry (the centre's HH:MM:SS comes from
        // this wall clock — `App` itself never reads a clock).
        if let Some((level, text)) = app.take_notice_leveled() {
            app.record_notification(dbtl::NotificationEntry {
                stamp: now_hhmmss(),
                level,
                text: text.clone(),
            });
            toasts.push(ToastItem {
                level,
                text,
                at: Instant::now(),
            });
            dirty = true;
        }
        if toasts.retain_fresh(Instant::now()) {
            dirty = true;
        }
        if dirty {
            let sel = app.ui_state.selected();
            let sel_uid = app.selected_unique_id();
            let selection_changed = last_selection != sel_uid;
            if selection_changed {
                // The single cursor-reset chokepoint: EVERY selection change (keys,
                // mouse click, search confirm, history, reload-restore) funnels
                // through here before the next draw, so the cursor always re-homes
                // to the new root — no per-call-site resets to forget.
                app.reset_lineage_cursor();
                // Same chokepoint for the sibling-collapse expansions: they are
                // per-root state (a group key means nothing across a re-root),
                // so they re-initialise on every selection change.
                app.clear_collapse_expansions();
            }

            // Follow list scroll BEFORE drawing (display-row space) so a jump never
            // renders a stale frame with the selection off-screen. Read the row
            // numbers from the active list first (immutable borrow), then mutate.
            let (reveal_row, model_row, row_count) = {
                let list = app.active_list();
                (
                    list.reveal_row_of_model(sel),
                    list.row_of_model(sel),
                    list.row_count(),
                )
            };
            let list_height = list_pane_height(terminal)?;
            app.ui_state
                .ensure_visible(list_height, reveal_row, model_row, row_count);

            // The lineage layout for the current selection — memoized in App, so an
            // idle tick or a non-lineage keypress reuses the previous frame's
            // subgraph + layout + styles instead of recomputing them. The DISPLAY
            // subgraph carries the lineage cursor as its `selected`, so the
            // emphasis highlight and `selected_rect` below both track the cursor;
            // the viewport is then sized exactly as `draw` will size it, so
            // anchor/clamp matches the drawn window (the 2D display-row guarantee).
            lineage = app.styled_lineage_layout();
            let (view_w, view_h) = lineage_viewport(terminal, &app.ui_state)?;
            let cursor = app.lineage_cursor_uid();
            if let Some(lay) = lineage.as_deref() {
                // A lineage node-jump search anchors the viewport on the cycled match
                // (size-aware), overriding the usual selection-based anchor/clamp.
                // `current_lineage_match` is `None` for an empty query, so opening
                // search never jolts the view off the rooted node.
                let search_rect = app
                    .current_lineage_match()
                    .and_then(|uid| lay.rects.get(&uid).copied());
                if let Some(rect) = search_rect {
                    app.ui_state
                        .anchor_lineage(Some(rect), &lay.grid, view_w, view_h);
                } else if selection_changed || force_anchor {
                    // New root / reshaped view: centre on the display-selected node
                    // (the cursor, which a selection change just re-homed to the root).
                    app.ui_state
                        .anchor_lineage(lay.selected_rect, &lay.grid, view_w, view_h);
                } else if last_cursor != cursor {
                    // Cursor moved within the same graph: scroll MINIMALLY so the
                    // cursor's box shows — never a re-center (that would bring back
                    // the per-keypress viewport pan the cursor replaced).
                    if let Some(rect) = lay.selected_rect {
                        app.ui_state.ensure_lineage_visible(
                            rect,
                            lay.grid.width(),
                            lay.grid.height(),
                            view_w,
                            view_h,
                        );
                    }
                } else {
                    app.ui_state
                        .clamp_lineage(lay.grid.width(), lay.grid.height(), view_w, view_h);
                }
            }
            last_selection = sel_uid;
            last_cursor = cursor;
            force_anchor = false;

            // Overlay scroll clamp (size-aware), same seam as the lineage clamp: the
            // pure reducer records scroll intent, the loop bounds it to the screen.
            area = full_area(terminal)?;
            match &mut app.mode {
                dbtl::Mode::Help(h) => {
                    h.scroll = dbtl::ui::clamp_help_scroll(area, h);
                }
                dbtl::Mode::Detail(dv) => {
                    dv.scroll = dbtl::ui::clamp_detail_scroll(area, dv);
                }
                dbtl::Mode::Sql(sv) => {
                    sv.scroll = dbtl::ui::clamp_sql_scroll(area, sv);
                }
                dbtl::Mode::Stats(sv) => {
                    sv.scroll = dbtl::ui::clamp_stats_scroll(area, sv);
                }
                dbtl::Mode::Diff(dv) => {
                    dv.scroll = dbtl::ui::clamp_diff_scroll(area, dv);
                }
                dbtl::Mode::Columns(cv) => {
                    cv.scroll = dbtl::ui::clamp_columns_scroll(area, cv);
                }
                dbtl::Mode::Notifications(nv) => {
                    nv.scroll = dbtl::ui::clamp_notifications_scroll(area, nv);
                }
                _ => {}
            }

            // Precompute Dag-aware strings so RenderCtx stays Dag-free; the borrows
            // live for the draw call below.
            let status_note = app.selected_status_note();
            let view_label = app.lineage_view_label();
            // The re-root breadcrumb for the lineage pane title. `App::breadcrumb` is
            // size-unaware (max_width is a parameter); the FULL trail is handed to the
            // draw seam, which is the single width authority — it strictly re-truncates
            // the crumb so the trailing `[{v}]{lens}` title suffixes always survive.
            // `None` until the user re-roots.
            let breadcrumb = app.breadcrumb(usize::MAX);
            let overview_title = app.overview_title();

            // Status-bar segments (all owned `String`s precomputed here; the borrows
            // live to the draw call). Each is set only when meaningful — an absent
            // `Option` simply omits that segment (graceful degradation seam).
            let focus_s = match app.ui_state.focus() {
                Focus::List => "list",
                Focus::RightPane => "lineage",
            };
            // Position is meaningful only when the active list has selectable models;
            // an empty project or a zero-match search omits the segment entirely
            // (rather than papering over the empty case as a misleading `1/1`).
            let pos_s = if app.active_list().is_empty() {
                None
            } else {
                Some(format!(
                    "{}/{}",
                    app.ui_state.selected() + 1,
                    app.active_list().len()
                ))
            };
            // The column-focus chip (`column:model.col ↑N ↓M`) and the lineage
            // title's ` [col:name]` suffix — present only while a column is
            // traced (and suppressed with the rest of the focus by the
            // overview). Both are `None` otherwise: no focus, no footprint.
            // Computed BEFORE the view surfaces: a live trace also suppresses
            // them (the rooted-view filters the trace ignores must not
            // advertise themselves on screen).
            let column_s = app.column_status();
            let col_suffix = app.column_focus_label();
            let view_seg = if app.ui_state.overview() {
                Some("overview")
            } else if column_s.is_none() && app.lineage_view != LineageView::default() {
                Some(view_label.as_str())
            } else {
                None
            };
            // The cov% segment keys on the COVERAGE lens specifically (the other
            // lenses recolour the lineage but quote no coverage number).
            let coverage_s = if app.ui_state.lens() == LineageLens::Coverage {
                let (tested, total) = app.coverage_summary();
                let pct = (tested * 100).checked_div(total).unwrap_or(0);
                Some(format!("cov {pct}% ({tested}/{total})"))
            } else {
                None
            };
            let bookmarks_s = if app.bookmarks.is_empty() {
                None
            } else {
                Some(format!("bm:{}", app.bookmarks.len()))
            };
            let sort_s = if app.sort != SortMode::default() {
                Some(format!("sort:{}", app.sort.label()))
            } else {
                None
            };
            // Blast radius of the selected node (emitted every frame; arrows
            // from the active glyph mode's Chrome badges — never hardcoded).
            // Built in App so the string source is single and unit-testable.
            let impact_s = app.impact_status();
            // The unread-warning badge (`!N`, Issue 08) — sits right after
            // impact in the chip order; absent when nothing is unread.
            let notify_s = if app.unread_warnings() > 0 {
                Some(format!("!{}", app.unread_warnings()))
            } else {
                None
            };
            // The baseline-diff chip (present only with a --diff baseline loaded).
            let diff_s = app.diff_status_label();
            // The run_results chip (coverage + freshness of the loaded artifact).
            let run_s = app.run_status_label();
            // The package-scope tag — emitted on every frame (`scope:root`
            // default / `scope:+pkgs` under --include-packages) since it states
            // which graph the numbers describe, but still a droppable segment:
            // a narrow-enough terminal sheds it like any other chip.
            let scope_s = format!("scope:{}", app.scope().tag());
            // The toast stack for the draw seam: borrows of the live entries
            // (newest first), capped at the stack's MAX.
            let toast_lines = toasts.lines();
            // Layer bands: the lineage pane's bottom-border column annotations,
            // present exactly while the Layer lens is active (cheap: one pass over
            // the layout's rects, only computed when the lens is on).
            let layer_bands = match (&lineage, app.ui_state.lens()) {
                (Some(lay), LineageLens::Layer) => Some(app.layer_bands(lay)),
                _ => None,
            };
            // The frame is being redrawn for a non-hover reason (a click
            // re-root, a wheel pan, a resize, a reload): the cell under the
            // pointer may now sit on a different surface. Re-resolve the
            // remembered hover target against the FRESH geometry so it can't
            // go stale between `Moved` events.
            // TODO(05): Issues 05/07 read `mouse.hover()` for the frame-buffer
            // highlight/tooltip overlay — keep it out of CharGrid and every
            // cache key (a post-blit overlay input only).
            if let Some(cell) = mouse.pointer() {
                let target = if matches!(app.mode, Mode::Selection) {
                    let g = hit_geometry(app, lineage.as_deref(), area);
                    hit_target(&g, cell.x, cell.y)
                } else {
                    None
                };
                mouse.track_hover(target);
            }
            // §5 splitter feedback: the accent border + a status hint. The
            // hint names the live saved width mid-drag; on hover it teaches
            // the gesture (`·` is ambiguous-width, so ASCII mode swaps it).
            let (highlight, splitter_hint) = match (splitter_drag, mouse.hover()) {
                (Some(d), _) => (
                    Some(d.which),
                    Some(format!(
                        "{} {} cols",
                        d.which.label(),
                        app.ui_state.pane_width(d.which)
                    )),
                ),
                (None, Some(HitTarget::PaneBorder(which))) => (
                    Some(*which),
                    Some(
                        if app.glyph_mode == dbtl::GlyphMode::Ascii {
                            "drag to resize, double-click to reset"
                        } else {
                            "drag to resize · double-click to reset"
                        }
                        .to_string(),
                    ),
                ),
                _ => (None, None),
            };
            // The command palette's row model — Dag-aware (recent commands,
            // `@`-search model results, availability marks), computed at the
            // loop seam so `RenderCtx` stays Dag-free. Only built while the
            // palette is open.
            let palette_rows = match &app.mode {
                dbtl::Mode::Palette(p) => Some(app.palette_rows(&p.query)),
                _ => None,
            };
            // The empty-list reason (`no models in scope` / `no match` / the
            // persistent-filter reason) — `Some` only while the active list is
            // empty, so non-empty frames keep the legacy pane untouched.
            let empty_reason = app.list_empty_reason();
            {
                let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_deref());
                ctx.mode = &app.mode;
                ctx.status = status_note.as_deref();
                ctx.stats = Some(&app.stats);
                // The title's `[view]` suffix is a rooted-view label — while a
                // column is traced the pane shows the filtered graph (whose
                // shape ignores those filters), so the suffix drops with the
                // status segment and ` [col:name]` takes the slot instead.
                ctx.lineage_label = if column_s.is_some() {
                    None
                } else {
                    Some(&view_label)
                };
                ctx.breadcrumb = breadcrumb.as_deref();
                ctx.overview_title = overview_title.as_deref();
                ctx.glyphs = app.glyph_mode;
                ctx.bookmarks = Some(&app.bookmarks);
                // Full model count for the search title's N/M (M = full list len).
                ctx.full_model_count = Some(app.model_list.len());
                ctx.col_suffix = col_suffix.as_deref();
                ctx.splitter_highlight = highlight;
                ctx.segments = StatusSegments {
                    hint: splitter_hint.as_deref(),
                    impact: impact_s.as_deref(),
                    notify: notify_s.as_deref(),
                    column: column_s.as_deref(),
                    focus: Some(focus_s),
                    view: view_seg,
                    position: pos_s.as_deref(),
                    coverage: coverage_s.as_deref(),
                    bookmarks: bookmarks_s.as_deref(),
                    sort: sort_s.as_deref(),
                    diff: diff_s.as_deref(),
                    run: run_s.as_deref(),
                    scope: Some(&scope_s),
                    filter: app.list_filter_label(),
                };
                ctx.minimap = app.ui_state.minimap_visible();
                ctx.filter_label = app.list_filter_label();
                ctx.layer_bands = layer_bands.as_deref();
                ctx.toasts = &toast_lines;
                ctx.palette_rows = palette_rows.as_deref();
                ctx.empty_reason = empty_reason.as_deref();
                ctx.theme = app.active_theme();
                terminal
                    .draw(|frame| draw(frame, &ctx))
                    .context("failed to draw frame")?;
            }
            dirty = false;
        }

        // Block for the next event (no busy-poll). An idle tick (timeout) is
        // where `--watch` checks the data source for on-disk changes.
        if !event::poll(Duration::from_millis(250)).context("event poll failed")? {
            if let Some(w) = watch.as_mut() {
                if w.poll() {
                    // Same semantics as the `r` key: on failure the app keeps
                    // running on the old data; the reshaped graph re-centres.
                    // Load warnings and failures are WARNING-level notices —
                    // a clean reload stays info.
                    let (level, note) = match app.reload() {
                        Ok(w) if w.is_empty() => (
                            dbtl::NoticeLevel::Info,
                            "Reloaded (source changed)".to_string(),
                        ),
                        Ok(w) => (
                            dbtl::NoticeLevel::Warning,
                            format!("Reloaded (source changed) — {}", warnings_tail(&w)),
                        ),
                        Err(_) => (
                            dbtl::NoticeLevel::Warning,
                            "Reload failed (kept previous data)".to_string(),
                        ),
                    };
                    app.set_notice_level(level, note);
                    force_anchor = true;
                    dirty = true;
                }
            }
            continue;
        }
        {
            match event::read().context("event read failed")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    dirty = true; // a keypress may change the frame
                    if let Some(action) = dispatch(&app.mode, key) {
                        // Actions that change the lineage *shape/centring* without
                        // moving the selection must force the anchor branch next
                        // frame (a size-aware loop concern). Reload rebuilds the
                        // Dag and restores selection by id, so the reshaped graph
                        // must re-centre even when the selected id is unchanged.
                        let reanchor = reanchors_lineage(action);
                        let outcome = apply_action(app, action);
                        // `<`/`>` stepped the saved width size-blind (the §1
                        // floor lives in `nudge_list_width`); the §1 max is
                        // terminal-relative, so it clamps HERE — the same
                        // `set_pane_width` write a splitter drag makes.
                        if matches!(action, Action::PaneNarrow | Action::PaneWiden) {
                            let w = app.ui_state.list_width();
                            app.ui_state.set_pane_width(Splitter::List, w, area.width);
                        }
                        for effect in outcome.effects {
                            run_effect(terminal, app, effect)?;
                        }
                        // An effect can swap the data source (dbt parse adopts
                        // the manifest); re-baseline the watch poller so it
                        // follows the NEW origin instead of the stale one.
                        if let Some(w) = watch.as_mut() {
                            let (root, recursive) = app.watch_root();
                            if w.root != root || w.recursive != recursive {
                                *w = Watch::new(root.to_path_buf(), recursive);
                            }
                        }
                        if outcome.quit {
                            return Ok(());
                        }
                        // The dispatched action's own check OR the flag an
                        // indirectly-applied action carries (a palette-run
                        // command the dispatch only saw as SearchConfirm).
                        if reanchor || outcome.reanchor {
                            force_anchor = true;
                        }
                    }
                }
                Event::Mouse(me) => {
                    dirty |= handle_mouse(
                        app,
                        &mut mouse,
                        &mut splitter_drag,
                        me,
                        lineage.as_deref(),
                        area,
                    );
                }
                // Resize / focus / paste and any other delivered event may
                // change the frame.
                _ => dirty = true,
            }
        }
    }
}

/// The live splitter drag: which boundary was grabbed plus the pointer's
/// offset inside its 2-column strip at press time (`0` = the left pane's
/// right-border cell, `1` = the right pane's left-border cell). Carrying the
/// offset keeps the grabbed border under the pointer instead of snapping the
/// pane edge to it.
#[derive(Clone, Copy)]
struct SplitterDrag {
    which: Splitter,
    grab_off: u16,
}

/// Resolve a drag press cell to a [`SplitterDrag`] when it lands on a pane
/// boundary — `None` anywhere else (other drags stay inert until Issue 05
/// gives the lineage pan a meaning).
fn begin_splitter_drag(
    app: &App,
    lineage: Option<&Layout>,
    area: Rect,
    origin: ratatui::layout::Position,
) -> Option<SplitterDrag> {
    let g = hit_geometry(app, lineage, area);
    let HitTarget::PaneBorder(which) = hit_target(&g, origin.x, origin.y)? else {
        return None;
    };
    let strip = g.panes.splitter(which)?;
    Some(SplitterDrag {
        which,
        grab_off: origin.x.saturating_sub(strip.x),
    })
}

/// Write the width a live drag asks for: the pointer's column resolved
/// through [`splitter_drag_width`] (grab-offset honoured) against the
/// dragged pane's FIXED outer edge — the list's left `x`, the inspector's
/// right edge — then clamped to the §1 min/max for this terminal inside
/// `set_pane_width`, so a dragged-out-of-range value clamps identically to
/// the `<`/`>` keys.
fn apply_splitter_drag(app: &mut App, drag: SplitterDrag, pointer_x: u16, area: Rect) {
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

/// Handle a mouse event (size-aware, so it lives in the loop, not the reducer).
/// Raw kinds go through the [`MouseRouter`] (release-confirmed clicks, the
/// double-click window, the ≥1-cell drag threshold, hover dedup); the
/// resulting point maps to a [`HitTarget`] via the pure [`hit_target`] — the
/// same geometry the renderer draws — and dispatches through `App`.
/// Acts only in `Selection` mode — overlays own the screen.
///
/// Returns whether the event may change the frame (the caller's `dirty`
/// contribution): every kind except `Moved` may; `Moved` only when the
/// hovered target CHANGED, so pointer motion doesn't repaint the screen.
fn handle_mouse(
    app: &mut App,
    mouse: &mut MouseRouter,
    splitter_drag: &mut Option<SplitterDrag>,
    me: MouseEvent,
    lineage: Option<&Layout>,
    area: Rect,
) -> bool {
    match me.kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            if !matches!(app.mode, Mode::Selection) {
                return false;
            }
            let rects = pane_layout(area, &app.ui_state.pane_config());
            let down = me.kind == MouseEventKind::ScrollDown;
            let shift = me.modifiers.contains(KeyModifiers::SHIFT);
            wheel(
                app,
                pane_interior(rects.list),
                pane_interior(rects.lineage),
                me.column,
                me.row,
                down,
                shift,
            );
            true
        }
        // A native horizontal notch (trackpad swipe / tilt wheel). Only the
        // lineage diagram scrolls horizontally; the list pane ignores it.
        MouseEventKind::ScrollRight | MouseEventKind::ScrollLeft => {
            if !matches!(app.mode, Mode::Selection) {
                return false;
            }
            let rects = pane_layout(area, &app.ui_state.pane_config());
            wheel_x(
                app,
                pane_interior(rects.lineage),
                me.column,
                me.row,
                me.kind == MouseEventKind::ScrollRight,
            );
            true
        }
        _ => {
            let Some(input) = MouseInput::from_kind(me.kind, me.column, me.row) else {
                return true; // an unclassified kind — conservatively redraw
            };
            let Some(outcome) = mouse.feed(input, Instant::now()) else {
                return false; // press / deduped move / stray event: nothing visible
            };
            let sel = matches!(app.mode, Mode::Selection);
            let mut target_changed = false;
            match &outcome {
                MouseOutcome::Hover { at } => {
                    // Non-Selection modes own the screen: the panes report no
                    // hover target (modal hit-testing is Issue 06).
                    let target = if sel {
                        let g = hit_geometry(app, lineage, area);
                        hit_target(&g, at.x, at.y)
                    } else {
                        None
                    };
                    target_changed = mouse.track_hover(target);
                }
                MouseOutcome::Click { button, at } if sel => {
                    let target = {
                        let g = hit_geometry(app, lineage, area);
                        hit_target(&g, at.x, at.y)
                    };
                    if let Some(t) = target.as_ref() {
                        app.click_target(t, *button);
                    }
                }
                MouseOutcome::DoubleClick { button, at } if sel => {
                    let target = {
                        let g = hit_geometry(app, lineage, area);
                        hit_target(&g, at.x, at.y)
                    };
                    match target.as_ref() {
                        // §5: a splitter double-click resets that pane's saved
                        // width to default — it is NOT the click action twice.
                        Some(HitTarget::PaneBorder(which)) if *button == MouseButton::Left => {
                            app.ui_state.reset_pane_width(*which);
                        }
                        // Everywhere else a double-click performs the click
                        // action again — today's click actions are idempotent
                        // (re-rooting the same node, re-selecting the same
                        // row), so the effect matches the old
                        // two-clicks-in-a-row behaviour until Issue 05 gives
                        // double-click its own meaning.
                        Some(t) => app.click_target(t, *button),
                        None => {}
                    }
                }
                // §5: a drag that began on a pane boundary resizes that pane.
                // The press cell is hit-tested (only a boundary grab counts);
                // afterwards the stored `which`+`grab_off` tracks the pointer
                // — mid-drag motion never re-hit-tests, so a pointer dragged
                // past a clamp keeps the grabbed border's tracking.
                MouseOutcome::DragStart { button, origin, at } if sel => {
                    if *button == MouseButton::Left {
                        if let Some(d) = begin_splitter_drag(app, lineage, area, *origin) {
                            apply_splitter_drag(app, d, at.x, area);
                            *splitter_drag = Some(d);
                        }
                    }
                }
                MouseOutcome::DragMove { at, .. } if sel => {
                    if let Some(d) = *splitter_drag {
                        apply_splitter_drag(app, d, at.x, area);
                    }
                }
                MouseOutcome::DragEnd { button, origin, at }
                    if sel && *button == MouseButton::Left =>
                {
                    // A terminal can deliver no Drag events for a fast
                    // flick (press → release elsewhere): the router still
                    // reports DragEnd, so resolve the press cell like
                    // DragStart before finishing.
                    let d = (*splitter_drag)
                        .or_else(|| begin_splitter_drag(app, lineage, area, *origin));
                    if let Some(d) = d {
                        apply_splitter_drag(app, d, at.x, area);
                    }
                    *splitter_drag = None;
                }
                // Other drags are recognized by the router but inert until
                // Issue 05 (lineage pan) gives them meaning.
                _ => {}
            }
            mouse_dirty(Some(&outcome), sel, target_changed)
        }
    }
}

/// Bundle the frame's hit-test inputs, all from the SAME sources the draw
/// uses: `pane_layout` on the live area + `pane_config`, the last styled
/// `Layout`, and `UiState`'s scroll offsets — so a click lands on what the
/// screen shows.
fn hit_geometry<'a>(app: &'a App, lineage: Option<&'a Layout>, area: Rect) -> HitGeometry<'a> {
    HitGeometry {
        panes: pane_layout(area, &app.ui_state.pane_config()),
        list: app.active_list(),
        list_offset: app.ui_state.offset(),
        layout: lineage,
        scroll_x: app.ui_state.lineage_scroll_x(),
        scroll_y: app.ui_state.lineage_scroll_y(),
        minimap: app.ui_state.minimap_visible(),
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
fn wheel(
    app: &mut App,
    list_inner: Rect,
    lin_inner: Rect,
    col: u16,
    row: u16,
    down: bool,
    shift: bool,
) {
    if within(list_inner, col, row) {
        app.ui_state.wheel_list(down);
    } else if within(lin_inner, col, row) {
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
fn wheel_x(app: &mut App, lin_inner: Rect, col: u16, row: u16, right: bool) {
    if within(lin_inner, col, row) {
        app.ui_state.wheel_lineage_x(right);
    }
}

/// Perform a side effect requested by the reducer. The only impure surface:
/// it suspends/reinits the terminal for `$EDITOR`, shells out to the clipboard,
/// or reloads the manifest. Effects that fail non-fatally (editor, clipboard,
/// reload, file write) keep the TUI running and surface the failure on the
/// notice channel — overwriting the reducer's optimistic intent toast, so the
/// toast never claims a copy/export that didn't happen.
fn run_effect(terminal: &mut DefaultTerminal, app: &mut App, effect: Effect) -> Result<()> {
    match effect {
        Effect::OpenEditor(path) => open_in_editor(terminal, app, &path)?,
        Effect::Yank(text) => {
            if yank_to_clipboard(&text).is_err() {
                app.set_notice_level(
                    dbtl::NoticeLevel::Warning,
                    "Copy failed (clipboard unavailable)",
                );
            }
        }
        Effect::ReloadManifest => {
            // On failure the app is left unchanged (reload `?`-returns before
            // mutating), so we keep running on the old data. Load warnings and
            // failures are WARNING-level; a clean reload stays info.
            let (level, note) = match app.reload() {
                Ok(w) if w.is_empty() => (dbtl::NoticeLevel::Info, "Reloaded".to_string()),
                Ok(w) => (
                    dbtl::NoticeLevel::Warning,
                    format!("Reloaded — {}", warnings_tail(&w)),
                ),
                Err(_) => (
                    dbtl::NoticeLevel::Warning,
                    "Reload failed (kept previous data)".to_string(),
                ),
            };
            app.set_notice_level(level, note);
        }
        Effect::WriteFile { path, contents } => {
            if std::fs::write(&path, contents).is_err() {
                app.set_notice_level(dbtl::NoticeLevel::Warning, format!("Export failed: {path}"));
            }
        }
        Effect::DbtParse => run_dbt_parse(terminal, app)?,
    }
    Ok(())
}

/// Run `dbt parse` in the project root and adopt the regenerated
/// `target/manifest.json` as the data source. Every failure (no dbt on PATH, no
/// project file, non-zero exit, missing/corrupt manifest) keeps the TUI running
/// on the current data and reports via the notice toast.
///
/// The TUI is suspended around the subprocess (same dance as `$EDITOR`) so
/// dbt's own progress/error output is visible while it runs.
fn run_dbt_parse(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let Some(dbt) = find_dbt() else {
        app.set_notice_level(dbtl::NoticeLevel::Warning, "dbt not found in PATH");
        return Ok(());
    };
    let root = app.project_root().to_path_buf();
    if !root.join("dbt_project.yml").is_file() {
        app.set_notice_level(
            dbtl::NoticeLevel::Warning,
            format!("No dbt_project.yml in {}", root.display()),
        );
        return Ok(());
    }

    // Leave the TUI exactly like the editor suspend; always re-init even if
    // the subprocess failed, or the terminal stays corrupted.
    set_keyboard_enhancement(false);
    set_mouse_capture(false);
    ratatui::restore();
    println!("dbtl: running `dbt parse` in {} ...", root.display());
    let status = dbt_command(&dbt).arg("parse").current_dir(&root).status();
    *terminal = ratatui::try_init().context("failed to re-init terminal after dbt parse")?;
    set_keyboard_enhancement(true);
    set_mouse_capture(true);
    let _ = terminal.clear();

    match status {
        Ok(s) if s.success() => {}
        Ok(_) => {
            app.set_notice_level(
                dbtl::NoticeLevel::Warning,
                "dbt parse failed (kept current data)",
            );
            return Ok(());
        }
        Err(_) => {
            app.set_notice_level(
                dbtl::NoticeLevel::Warning,
                "Failed to launch dbt (kept current data)",
            );
            return Ok(());
        }
    }
    let manifest = root.join("target").join("manifest.json");
    if !manifest.is_file() {
        app.set_notice_level(
            dbtl::NoticeLevel::Warning,
            "dbt parse wrote no target/manifest.json",
        );
        return Ok(());
    }
    // On a load error `adopt_manifest` restores the previous source, so the
    // app keeps running on the pre-parse data. Load warnings (unhandled
    // resource_types, scope warnings) ride the toast like a manual reload.
    let (level, note) = match app.adopt_manifest(manifest) {
        Ok(w) if w.is_empty() => (
            dbtl::NoticeLevel::Info,
            "dbt parse OK: now reading target/manifest.json".to_string(),
        ),
        Ok(w) => (
            dbtl::NoticeLevel::Warning,
            format!("dbt parse OK — {}", warnings_tail(&w)),
        ),
        Err(_) => (
            dbtl::NoticeLevel::Warning,
            "dbt parse OK but the manifest failed to load (kept current data)".to_string(),
        ),
    };
    app.set_notice_level(level, note);
    Ok(())
}

/// One live toast in the top-right stack (Issue 08): the notice text, its
/// severity (drives the TTL and the border colour), and the arrival stamp the
/// loop adds — the reducer stays clock-free.
struct ToastItem {
    level: dbtl::NoticeLevel,
    text: String,
    at: Instant,
}

/// The visible toast stack: newest FIRST (`items[0]` renders on top), capped
/// at [`ToastStack::MAX`]. [`ToastStack::push`] drops the oldest past the cap;
/// [`ToastStack::retain_fresh`] drops anything past its per-level TTL.
#[derive(Default)]
struct ToastStack {
    items: Vec<ToastItem>,
}

impl ToastStack {
    /// At most 3 toasts on screen at once (Issue 08).
    const MAX: usize = 3;

    /// Push `item` to the TOP (newest first), evicting the oldest past `MAX`.
    fn push(&mut self, item: ToastItem) {
        self.items.insert(0, item);
        self.items.truncate(Self::MAX);
    }

    /// Drop every toast past its per-level TTL at `now`; true when anything
    /// expired (the caller marks the frame dirty). Takes `now` so expiry
    /// compares against a fixed instant — `saturating_duration_since` treats
    /// a future `at` as fresh instead of panicking.
    fn retain_fresh(&mut self, now: Instant) -> bool {
        let before = self.items.len();
        self.items
            .retain(|t| now.saturating_duration_since(t.at) < toast_ttl(t.level));
        self.items.len() != before
    }

    /// The draw-seam view: borrows of the live entries, newest first.
    fn lines(&self) -> Vec<dbtl::ToastLine<'_>> {
        self.items
            .iter()
            .map(|t| dbtl::ToastLine {
                level: t.level,
                text: t.text.as_str(),
            })
            .collect()
    }
}

/// Per-level on-screen lifetime: info ~2.5s (the legacy toast TTL), warning
/// ~5s so failures stay readable. Owned here, not in `NoticeLevel` — the
/// TTL is a UI policy, the level itself is UI-free.
fn toast_ttl(level: dbtl::NoticeLevel) -> Duration {
    match level {
        dbtl::NoticeLevel::Info => Duration::from_millis(2500),
        dbtl::NoticeLevel::Warning => Duration::from_millis(5000),
    }
}

/// Local wall-clock `HH:MM:SS` for notification stamps. `std` has no local
/// time API and adding `time`/`chrono` would trip the orchestration's
/// dependency stop-condition — so the platform `date` answers "what time is
/// it here". The fallback when `date` is absent is UTC, NOT local time — the
/// stamp degrades to a fixed-offset clock rather than guessing the zone.
/// `pub(crate)` because `cli::build_app` stamps startup warnings through the
/// same clock.
pub(crate) fn now_hhmmss() -> String {
    if let Ok(out) = Command::new("date").arg("+%H:%M:%S").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            if s.len() == 8 {
                return s.to_string();
            }
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

/// The reload/`dbt parse` toast tail for a non-empty load-warnings vec:
/// every warning joined (`"1 warning: <w>"` / `"N warnings: <w1>; <w2>"`) —
/// quoting only the first would hide the rest of what the manifest carries.
fn warnings_tail(warnings: &[String]) -> String {
    format!(
        "{} warning{}: {}",
        warnings.len(),
        if warnings.len() == 1 { "" } else { "s" },
        warnings.join("; ")
    )
}

/// Locate the `dbt` executable on `PATH`, cross-platform. Returns the full
/// path so the caller can run it without re-resolving (and so Windows `.bat`/
/// `.cmd` launchers can be routed through `cmd /C`). `pub(crate)` because the
/// source-mode startup notice in `main` checks the same probe.
pub(crate) fn find_dbt() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_path_dirs("dbt", std::env::split_paths(&path))
}

/// PATH-lookup core, parameterized over the directory list so tests never
/// mutate the process-global `PATH` (env mutation races parallel tests).
fn find_in_path_dirs(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for cand in candidate_names(name) {
            let p = dir.join(&cand);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// The filenames that count as "the `name` command" on this platform: the bare
/// name on Unix; on Windows, `name` + each `PATHEXT` extension (`dbt.exe`,
/// `dbt.cmd`, …) the way `where` resolves commands.
#[cfg(not(windows))]
fn candidate_names(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(windows)]
fn candidate_names(name: &str) -> Vec<String> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|e| format!("{name}{}", e.to_ascii_lowercase()))
        .collect()
}

/// Whether `path` is an executable file. Unix checks the execute bits (a
/// plain data file named `dbt` on PATH is not a command); Windows has no
/// execute bit — the `PATHEXT` extension (already applied by
/// [`candidate_names`]) is what marks executability there.
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Build the `Command` for a resolved dbt executable. On Windows, `.bat`/`.cmd`
/// launchers (common for pip-installed tools) cannot be spawned directly by
/// `CreateProcess` — route them through `cmd /C`.
fn dbt_command(exe: &std::path::Path) -> Command {
    #[cfg(windows)]
    {
        let ext = exe
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("bat" | "cmd")) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(exe);
            return c;
        }
    }
    Command::new(exe)
}

/// Split an editor value like `code --wait` into program + leading arguments.
/// Values such as `$VISUAL` commonly carry flags, so the whole string must not
/// be treated as a single program name. Whitespace-only values yield `None`.
fn split_editor_command(value: &str) -> Option<(String, Vec<String>)> {
    let mut tokens = value.split_whitespace().map(String::from);
    let program = tokens.next()?;
    Some((program, tokens.collect()))
}

/// Suspend the TUI, open `path` in `$VISUAL`/`$EDITOR` (default `vi`), then
/// re-enter the alternate screen and force a redraw. A missing re-init would
/// leave the terminal corrupted, so we always re-init even if the editor failed.
/// Editor spawn/exit failures are NOT propagated — tearing the TUI down over a
/// missing editor binary would discard the whole session — they surface on the
/// notice channel like every other non-fatal effect failure.
fn open_in_editor(terminal: &mut DefaultTerminal, app: &mut App, path: &str) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_default();
    let (program, args) =
        split_editor_command(&editor).unwrap_or_else(|| ("vi".to_string(), Vec::new()));

    // Leave the TUI (pop the keyboard enhancement and disable mouse first, then
    // restore raw mode + alt screen) — the editor gets the terminal in the same
    // plain state a shell would.
    set_keyboard_enhancement(false);
    set_mouse_capture(false);
    ratatui::restore();
    let status = Command::new(&program).args(&args).arg(path).status();
    // Re-enter: re-init the terminal, re-arm keyboard + mouse, force a redraw.
    *terminal = ratatui::try_init().context("failed to re-init terminal after editor")?;
    set_keyboard_enhancement(true);
    set_mouse_capture(true);
    let _ = terminal.clear();

    if let Some(note) = editor_failure_notice(&program, &status) {
        app.set_notice_level(dbtl::NoticeLevel::Warning, note);
    }
    Ok(())
}

/// Map the editor subprocess outcome to a failure notice (`None` = clean exit).
/// Pure so the headless suite can cover it.
fn editor_failure_notice(
    program: &str,
    status: &std::io::Result<std::process::ExitStatus>,
) -> Option<String> {
    match status {
        Ok(s) if s.success() => None,
        Ok(s) => Some(format!("Editor failed ({program}): {s}")),
        Err(_) => Some(format!("Failed to launch editor ({program})")),
    }
}

/// The clipboard commands to try on this platform, in order. Linux has no
/// single standard tool, so Wayland's `wl-copy` is probed first, then the two
/// common X11 ones — the first that spawns wins.
#[cfg(target_os = "macos")]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[("pbcopy", &[])];

#[cfg(target_os = "linux")]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

#[cfg(target_os = "windows")]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[("clip", &[])];

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
const CLIPBOARD_COMMANDS: &[(&str, &[&str])] = &[];

/// Copy `text` to the system clipboard via the platform's clipboard command
/// ([`CLIPBOARD_COMMANDS`]). Best-effort; the caller surfaces errors as a
/// toast so a missing clipboard tool never breaks the TUI.
fn yank_to_clipboard(text: &str) -> Result<()> {
    for (program, args) in CLIPBOARD_COMMANDS {
        let Ok(child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .spawn()
        else {
            continue;
        };
        return feed_and_reap(child, program, text);
    }
    anyhow::bail!("no clipboard command available")
}

/// Feed `text` to a spawned clipboard child's stdin and ALWAYS reap it: an
/// early `?` on the write would skip `wait()` and leave a zombie for the rest
/// of the process lifetime. The write error (e.g. a broken pipe from a child
/// that exited without reading) is reported after the reap.
fn feed_and_reap(mut child: std::process::Child, program: &str, text: &str) -> Result<()> {
    let write_result = match child.stdin.take() {
        // `stdin` drops at the end of this arm, closing the pipe so the child
        // sees EOF and `wait()` below cannot deadlock.
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Ok(()),
    };
    let wait_result = child.wait();
    write_result.with_context(|| format!("failed to write to {program}"))?;
    wait_result.with_context(|| format!("{program} failed"))?;
    Ok(())
}

/// Compute the number of visible *display rows* in the list pane, mirroring the
/// draw layout: total height minus the status line (1) minus the pane's top and
/// bottom borders (2). `max(1)` guards tiny terminals so we never produce a
/// zero-height window.
fn list_pane_height(terminal: &DefaultTerminal) -> Result<usize> {
    let size = terminal.size().context("failed to read terminal size")?;
    let body = size.height.saturating_sub(1); // status line
    let inner = body.saturating_sub(2); // top + bottom border
    Ok(inner.max(1) as usize)
}

/// The lineage pane's interior size `(width, height)` for the current terminal,
/// computed through the SAME pure geometry (`pane_layout` on `pane_config` +
/// `pane_interior`) that `draw` uses — pane widths included — so the
/// anchor/clamp viewport equals the blitted/drawn viewport.
fn lineage_viewport(terminal: &DefaultTerminal, ui: &dbtl::ui::UiState) -> Result<(usize, usize)> {
    let interior = pane_interior(pane_layout(full_area(terminal)?, &ui.pane_config()).lineage);
    Ok((interior.width as usize, interior.height as usize))
}

/// The full terminal rect (origin at 0,0). Used for overlay geometry / clamps.
fn full_area(terminal: &DefaultTerminal) -> Result<ratatui::layout::Rect> {
    let size = terminal.size().context("failed to read terminal size")?;
    Ok(ratatui::layout::Rect::new(0, 0, size.width, size.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_editor_command_separates_program_and_args() {
        assert_eq!(
            split_editor_command("code --wait"),
            Some(("code".to_string(), vec!["--wait".to_string()]))
        );
        assert_eq!(
            split_editor_command("vim"),
            Some(("vim".to_string(), vec![]))
        );
        // Runs of whitespace (tabs included) never produce empty arguments.
        assert_eq!(
            split_editor_command("  emacs \t -nw  "),
            Some(("emacs".to_string(), vec!["-nw".to_string()]))
        );
    }

    #[test]
    fn split_editor_command_rejects_blank_values() {
        assert_eq!(split_editor_command(""), None);
        assert_eq!(split_editor_command("   \t "), None);
    }

    /// The editor outcome→notice mapping: a clean exit is silent; a spawn
    /// failure (missing binary) and a non-zero exit both produce a toast that
    /// names the program — neither may tear the TUI down.
    #[test]
    fn editor_failure_notice_maps_outcomes() {
        let err: std::io::Result<std::process::ExitStatus> =
            Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        let note = editor_failure_notice("no-such-editor", &err).expect("spawn failure notices");
        assert!(note.contains("no-such-editor"), "{note}");

        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let ok = Ok(std::process::ExitStatus::from_raw(0));
            assert_eq!(
                editor_failure_notice("vi", &ok),
                None,
                "clean exit is silent"
            );
            let fail = Ok(std::process::ExitStatus::from_raw(0x100)); // exit code 1
            let note = editor_failure_notice("vi", &fail).expect("non-zero exit notices");
            assert!(note.contains("vi"), "{note}");
        }
    }

    /// The yank pipe must reap its child unconditionally: a child that exits
    /// without reading stdin breaks the pipe mid-write, and the old early `?`
    /// skipped `wait()` (zombie). Now the write error comes back as `Err`
    /// AFTER the reap — so this returns (no hang) with the failure visible.
    #[cfg(unix)]
    #[test]
    fn feed_and_reap_reaps_child_and_reports_write_failure() {
        // Success path: a child that drains stdin.
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("cat spawns");
        assert!(feed_and_reap(child, "cat", "hello").is_ok());

        // `false` exits immediately; > pipe-buffer text forces a broken pipe.
        let child = Command::new("false")
            .stdin(Stdio::piped())
            .spawn()
            .expect("false spawns");
        let big = "x".repeat(1 << 20);
        assert!(feed_and_reap(child, "false", &big).is_err());
    }

    /// `restore` is the single teardown path; it must be safe to call multiple
    /// times (the panic hook and normal exit may both reach it) and must not
    /// panic in a non-TTY test environment.
    #[test]
    fn restore_is_idempotent_and_tty_free() {
        ratatui::restore();
        ratatui::restore();
    }

    /// Issue 08: the stack holds at most 3 toasts, newest on top — a 4th push
    /// evicts the OLDEST entry so a burst never wedges on stale text.
    #[test]
    fn toast_stack_caps_at_three_and_evicts_oldest() {
        let item = |text: &str| ToastItem {
            level: dbtl::NoticeLevel::Info,
            text: text.into(),
            at: Instant::now(),
        };
        let mut s = ToastStack::default();
        for t in ["a", "b", "c", "d"] {
            s.push(item(t));
        }
        let texts: Vec<&str> = s.lines().iter().map(|l| l.text).collect();
        assert_eq!(texts, vec!["d", "c", "b"], "newest first, 'a' evicted");
    }

    /// Per-level TTL: info entries die at 2.5s while same-age warnings still
    /// live (5s) — the retain pass only drops expired rows and reports the
    /// flip so the loop redraws.
    #[test]
    fn toast_ttl_differs_by_level_and_retain_drops_expired() {
        assert_eq!(
            toast_ttl(dbtl::NoticeLevel::Info),
            Duration::from_millis(2500)
        );
        assert_eq!(
            toast_ttl(dbtl::NoticeLevel::Warning),
            Duration::from_millis(5000)
        );
        let mut s = ToastStack::default();
        // An info toast stamped at t0 is dead 3s later; a warning of the same
        // age is not. (Forwards-in-time `now` — no `Instant - Duration`
        // backdating, which can underflow-panic on some platforms.)
        let t0 = Instant::now();
        s.push(ToastItem {
            level: dbtl::NoticeLevel::Info,
            text: "old info".into(),
            at: t0,
        });
        s.push(ToastItem {
            level: dbtl::NoticeLevel::Warning,
            text: "same-age warning".into(),
            at: t0,
        });
        assert!(
            s.retain_fresh(t0 + Duration::from_secs(3)),
            "the expired info dropped"
        );
        let texts: Vec<&str> = s.lines().iter().map(|l| l.text).collect();
        assert_eq!(texts, vec!["same-age warning"]);
        assert!(
            !s.retain_fresh(t0 + Duration::from_secs(3)),
            "nothing left to expire"
        );
    }

    /// The notification stamp is a parseable `HH:MM:SS` from the platform
    /// `date` (with the std UTC fallback never producing garbage either).
    #[test]
    fn now_hhmmss_is_well_formed() {
        let s = now_hhmmss();
        assert_eq!(s.len(), 8, "HH:MM:SS shape: {s:?}");
        let parts: Vec<&str> = s.split(':').collect();
        assert_eq!(parts.len(), 3);
        let h: u32 = parts[0].parse().expect("hour");
        let m: u32 = parts[1].parse().expect("minute");
        let sec: u32 = parts[2].parse().expect("second");
        assert!(h < 24 && m < 60 && sec < 60, "{s} out of range");
    }

    /// The warnings toast tail pluralizes correctly and joins EVERY warning —
    /// a manifest carrying several unknown kinds must not quietly drop all
    /// but the first line.
    #[test]
    fn warnings_tail_pluralizes_and_joins_all() {
        assert_eq!(
            warnings_tail(&["w1".to_string()]),
            "1 warning: w1",
            "singular"
        );
        assert_eq!(
            warnings_tail(&["w1".to_string(), "w2".to_string(), "w3".to_string()]),
            "3 warnings: w1; w2; w3",
            "plural, all warnings joined"
        );
    }

    #[test]
    fn find_in_path_dirs_resolves_executables_only() {
        let dir = std::env::temp_dir().join(format!("dbtl_path_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(if cfg!(windows) { "dbt.exe" } else { "dbt" });
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            // Without the execute bit, a file named `dbt` on PATH is data, not a command.
            assert_eq!(find_in_path_dirs("dbt", [dir.clone()]), None);
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find_in_path_dirs("dbt", [dir.clone()]), Some(exe));
        // A dir without the command (and empty path entries) yields None.
        assert_eq!(
            find_in_path_dirs("dbt", [PathBuf::new(), dir.join("missing")]),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // §5 splitter gestures through `handle_mouse`: MouseEvent -> MouseRouter
    // -> pane_layout hit-test -> set_pane_width — the same path the loop feeds
    // (headless: the router is pure, only `Instant::now()` comes from here).
    // ------------------------------------------------------------------

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

    fn fixture_app() -> App {
        let dag = dbtl::load_dag(FIXTURE).expect("fixture loads");
        App::new(dag, PathBuf::from(FIXTURE))
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Feed one event through the production handler (Selection mode, no
    /// lineage layout needed — the splitter strip is pane geometry only).
    fn feed(
        app: &mut App,
        router: &mut MouseRouter,
        drag: &mut Option<SplitterDrag>,
        kind: MouseEventKind,
        x: u16,
        y: u16,
    ) {
        handle_mouse(
            app,
            router,
            drag,
            mouse(kind, x, y),
            None,
            Rect::new(0, 0, 100, 30),
        );
    }

    #[test]
    fn splitter_drag_resizes_the_list_and_tracks_the_grab_offset() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        // The 34-column default: the boundary strip is columns 33-34, so a
        // press on 33 records grab offset 0 (the list's own border cell).
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Left),
            33,
            10,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Left),
            50,
            10,
        );
        assert_eq!(
            app.ui_state.list_width(),
            51,
            "grab 0: pointer col 50 wants the border there -> width 51"
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Left),
            45,
            10,
        );
        assert_eq!(app.ui_state.list_width(), 46);
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Up(MouseButton::Left),
            45,
            10,
        );
        assert_eq!(app.ui_state.list_width(), 46, "release commits the width");

        // Grabbing the strip's RIGHT cell (the lineage pane's left border,
        // offset 1) keeps that border under the pointer instead of snapping.
        // Width 46: the strip is columns 45-46.
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Left),
            46,
            10,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Left),
            60,
            10,
        );
        assert_eq!(
            app.ui_state.list_width(),
            60,
            "grab 1: pointer col 60 wants the right border there -> width 60"
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Up(MouseButton::Left),
            60,
            10,
        );
    }

    #[test]
    fn splitter_drag_clamps_to_the_pane_bounds() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Left),
            33,
            10,
        );
        // Past the 60%-of-terminal cap at 100 cols.
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Left),
            95,
            10,
        );
        assert_eq!(
            app.ui_state.list_width(),
            60,
            "the cap wins over the pointer"
        );
        // Below the 18-column floor.
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Left),
            2,
            10,
        );
        assert_eq!(
            app.ui_state.list_width(),
            18,
            "the floor wins over the pointer"
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Up(MouseButton::Left),
            2,
            10,
        );
        assert_eq!(app.ui_state.list_width(), 18);
    }

    #[test]
    fn splitter_flick_without_drag_events_still_resizes() {
        // A fast press -> release-farther-right reports no Drag motion at all;
        // the DragEnd fallback resolves the press cell and commits anyway.
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Left),
            33,
            10,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Up(MouseButton::Left),
            45,
            10,
        );
        assert_eq!(app.ui_state.list_width(), 46, "release col 45 -> width 46");
        assert!(drag.is_none(), "the drag state cleared on release");
    }

    #[test]
    fn splitter_double_click_resets_the_saved_width() {
        let mut app = fixture_app();
        app.ui_state.set_pane_width(Splitter::List, 50, 100);
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        // Width 50: the strip is columns 49-50. A plain click is inert — only
        // the double-click resets.
        for (kind, x) in [
            (K::Down(MouseButton::Left), 49),
            (K::Up(MouseButton::Left), 49),
        ] {
            feed(&mut app, &mut router, &mut drag, kind, x, 10);
        }
        assert_eq!(
            app.ui_state.list_width(),
            50,
            "a single click keeps the width"
        );
        for (kind, x) in [
            (K::Down(MouseButton::Left), 49),
            (K::Up(MouseButton::Left), 49),
        ] {
            feed(&mut app, &mut router, &mut drag, kind, x, 10);
        }
        assert_eq!(
            app.ui_state.list_width(),
            dbtl::ui::DEFAULT_LIST_WIDTH,
            "the double-click resets to the §1 default"
        );
    }

    #[test]
    fn non_splitter_drag_and_right_button_are_inert() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        // A drag that begins on a list row: no splitter, no width change.
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Left),
            5,
            5,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Left),
            60,
            5,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Up(MouseButton::Left),
            60,
            5,
        );
        assert_eq!(app.ui_state.list_width(), dbtl::ui::DEFAULT_LIST_WIDTH);
        assert!(drag.is_none());
        // A right-button drag on the boundary is not a resize either.
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Right),
            33,
            10,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Drag(MouseButton::Right),
            60,
            10,
        );
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Up(MouseButton::Right),
            60,
            10,
        );
        assert_eq!(app.ui_state.list_width(), dbtl::ui::DEFAULT_LIST_WIDTH);
        assert!(drag.is_none());
    }
}
