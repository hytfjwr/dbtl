//! The interactive event loop: the only size-aware layer. Follows scroll
//! (list + lineage), draws, reads an event, maps it to an [`Action`] via the
//! keymap, applies it, runs any [`Effect`]s, and repeats until quit — plus the
//! impure helpers those effects need (editor suspend, clipboard, `dbt parse`).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use dbtl::action::{
    dispatch, esc_action, reanchors_lineage, Action, Direction, PaletteRow, SearchTarget,
    StatusHint,
};
use dbtl::app::{apply_action, App, LineageView, Outcome};
use dbtl::effect::Effect;
use dbtl::layout::Layout;
use dbtl::ui::{
    draw, draw_too_small, hit_target, lineage_content_rect, minimap_rect, minimap_scroll_to,
    mouse_dirty, overlay_rect, pan_offset, pane_interior, pane_layout, splitter_drag_width,
    status_chips, status_layout, terminal_too_small, Focus, HitGeometry, HitTarget, LineageLens,
    MouseInput, MouseOutcome, MouseRouter, PaneRects, RenderCtx, Splitter, StatusLayout,
    StatusSegments, TabBarView, TitleLayout, LIST_AUTO_HIDE_W,
};
use dbtl::{Mode, SortMode};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
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
    // The live pane drag: splitter resize, lineage pan, or minimap scrub —
    // one slot covers every drag surface so a superseding press can never
    // leave two alive. Loop-owned (like the router): a drag tracks pointer
    // cells, so it is size-aware state by nature.
    let mut pane_drag: Option<PaneDrag> = None;
    // The settled-hover tooltip (§9, Issue 05): armed when the resolved
    // hover rests on a lineage node with no button held, shown after
    // `TOOLTIP_DELAY` stationary, dropped when the hover leaves or a press
    // lands. The loop owns the clock — the 250ms idle tick supplies the
    // flip's redraw, the reducer and renderer stay clock-free.
    let mut hover_tip: Option<HoverTip> = None;
    // §8/§6's chrome geometry: the status-line + lineage-title layouts the
    // LAST draw painted — recomputed alongside the ctx each dirty pass and
    // handed to `hit_geometry`, so a hover/click lands on the exact spans
    // the frame showed (the draw and the hit-test share the layout).
    let mut chrome = ChromeGeom::default();
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
    // The tab count the last resolved strip saw — its change (an open/close)
    // is one of the two events that re-ensure the active tab's visibility,
    // the other being an actual switch (`take_tab_switch`).
    let mut last_tab_count = 0usize;

    loop {
        // §12's size gates, resolved once per iteration in the ONLY size-aware
        // layer: below the usable floor the whole app suspends behind the
        // too-small screen; below the narrow band the list exists only as its
        // overlay drawer.
        let cur_area = full_area(terminal)?;
        if terminal_too_small(cur_area) {
            // Nothing but the message draws and nothing but `q`/`Ctrl-c`
            // acts — the reducer never sees the session while it lasts, so
            // whatever screen and state the shrink interrupted resurface
            // verbatim once the terminal grows.
            if dirty {
                terminal
                    .draw(|frame| draw_too_small(frame, app.glyph_mode, app.active_theme()))
                    .context("failed to draw frame")?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(250)).context("event poll failed")? {
                if let Event::Key(key) = event::read().context("event read failed")? {
                    if key.kind == KeyEventKind::Press
                        && matches!(dispatch(&Mode::Selection, key), Some(Action::Quit))
                    {
                        return Ok(());
                    }
                }
                // Any delivered event may change the message itself (the
                // current `WxH` it reports), so the next pass redraws.
                dirty = true;
            }
            continue;
        }
        // Publish the narrow-band verdict — the ONLY writer (the reducer and
        // the prefs stay size-blind). In the band the inline list is always
        // hidden and `Ctrl-b` reroutes to the drawer (see `drawer_shortcut`).
        app.ui_state
            .set_list_auto_hidden(cur_area.width < LIST_AUTO_HIDE_W);

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
        // The tooltip's 500ms flip is itself a redraw reason (§9): an idle
        // tick crosses the delay under a still pointer, and nothing else
        // would mark the frame for a frame-buffer-only overlay.
        if let Some(t) = hover_tip.as_mut() {
            if !t.visible && t.since.elapsed() >= TOOLTIP_DELAY {
                t.visible = true;
                dirty = true;
            }
        }
        if dirty {
            // A tab switch is a root change in every sense but two: the
            // incoming tab's own cursor and collapse expansions restore (§2),
            // so the chokepoint resets below skip it. Drained once per dirty
            // pass — the flag also feeds the strip's ensure-active decision.
            let tab_switch = app.take_tab_switch();
            let sel = app.ui_state.selected();
            let sel_uid = app.selected_unique_id();
            let selection_changed = last_selection != sel_uid;
            if selection_changed && !tab_switch {
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

            // The inspector's effective visibility for THIS frame — resolved
            // in two phases, both in this size-aware seam. Phase 1 publishes
            // the INTENT: the pref resolved against the terminal width
            // (`on`/`off` answer for themselves; `auto` shows at §12's >=160
            // columns — `InspectorPolicy::shown_at` is the single pure
            // predicate the boundary lives behind), so `pane_config` (the
            // layout below) knows whether to carve the region at all.
            // Phase 2 — after `pane_layout` + the view build — publishes the
            // ANSWER: `want && width>0 && a view exists`, so `hit_target` and
            // the focus ring never meet an invisible or content-less pane.
            // `inspector_target` then re-homes the pane's scroll + column
            // cursor the moment the inspected node changes (the
            // follow-the-cursor rule).
            let want = app.ui_state.inspector_policy().shown_at(cur_area.width);
            app.ui_state.set_inspector_shown(want);
            if let Some(uid) = app.inspector_target() {
                app.ui_state.set_inspector_target(&uid);
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
            let list_height = list_pane_height(terminal, &app.ui_state)?;
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
            // The lineage-search overlays (§6/M4): every matching node's
            // frame accents, the title rides `i/N`, and the viewport anchors
            // on the cycled match — `Some` only while a Lineage-target search
            // is open, computed ONCE for all three consumers. `match_idx`
            // cycles mod the count so the position is the same 1-based index
            // n/N step; `(0, 0)` while the query matches nothing.
            let (search_matches, current_match, search_pos) = match &app.mode {
                Mode::Search(s) if s.target == SearchTarget::Lineage => {
                    let m = app.lineage_matches(&s.query);
                    let pos = if m.is_empty() {
                        (0, 0)
                    } else {
                        (s.match_idx % m.len() + 1, m.len())
                    };
                    let cur = if m.is_empty() {
                        None
                    } else {
                        m.get(s.match_idx % m.len()).cloned()
                    };
                    (m, cur, Some(pos))
                }
                _ => (Vec::new(), None, None),
            };
            if let Some(lay) = lineage.as_deref() {
                // A lineage node-jump search anchors the viewport on the cycled match
                // (size-aware), overriding the usual selection-based anchor/clamp.
                // `current_match` is `None` for an empty query, so opening
                // search never jolts the view off the rooted node.
                let search_rect = current_match.and_then(|uid| lay.rects.get(&uid).copied());
                if let Some(rect) = search_rect {
                    app.ui_state
                        .anchor_lineage(Some(rect), &lay.grid, view_w, view_h);
                } else if tab_switch && app.tab_scroll_saved() {
                    // §2's verbatim restore: the switch already swapped the
                    // stashed viewport into UiState — only re-clamp it, since
                    // a resize may have shrunk the window since the stash.
                    app.ui_state
                        .clamp_lineage(lay.grid.width(), lay.grid.height(), view_w, view_h);
                } else if tab_switch || selection_changed || force_anchor {
                    // New root / reshaped view / fresh tab: centre on the
                    // display-selected node (the cursor, which a selection
                    // change just re-homed to the root).
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
            area = cur_area;
            // The tab strip's resolved cells — the same `pane_layout` +
            // `tab_bar_layout` the hit-test replays. `ensure_active` fires on
            // a switch or a strip-size change (open/close) — never on a
            // manual scroll, so `‹`/`›`/wheel can actually look around (§2's
            // "アクティブタブは常に見える" is a switch-time guarantee). The
            // RESOLVED first-visible index is written back, so a hit-test
            // always sees what the last frame drew.
            let panes = pane_layout(area, &app.ui_state.pane_config());
            let tab_labels = app.tab_labels();
            let tab_layout = app.tab_bar_layout_for(
                panes.tab_bar,
                tab_switch || app.tab_count() != last_tab_count,
            );
            app.set_tab_scroll(tab_layout.scroll);
            last_tab_count = app.tab_count();
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

            // The inspector snapshot for the draw seam: the (uid,
            // generation)-cached `Rc` clone `RenderCtx` borrows as
            // `Option<&InspectorView>` — `None` keeps the pane's field empty
            // (and the loop never pays the build cost for a hidden pane).
            // Built only when the intent AND the carved rect leave it room —
            // a forced `On` on a narrow screen squeezes the region to zero
            // cols, which is NOT a pane.
            let inspector = if want && panes.inspector.width > 0 {
                app.inspector_view()
            } else {
                None
            };
            // Phase 2 of the visibility resolution: the RESOLVED answer is
            // "a built snapshot exists". A `want` whose rect squeezed to zero
            // cols, or whose target has no `Dag` node to describe (empty dag,
            // `+M more` marker under the cursor), means no pane at all —
            // publishing `false` keeps the focus ring and the hit-test off an
            // invisible surface and lets `draw`'s own `pane_layout` hand the
            // cells back to the list/lineage.
            app.ui_state.set_inspector_shown(inspector.is_some());
            // Clamp the pane's scroll to the fresh content + interior (the
            // same size-aware seam the modal scrolls use — the reducer's
            // scroll intents land unclamped; this bounds them). A mouse pick's
            // one-shot reveal passes the cursored row for THIS frame only —
            // afterwards j/k/wheel scroll freely instead of the clamp
            // dragging the viewport back to the picked row every frame.
            if let Some(view) = inspector.as_deref() {
                let inner = pane_interior(panes.inspector);
                let reveal = if app.ui_state.take_inspector_reveal() {
                    app.ui_state.inspector_col()
                } else {
                    usize::MAX
                };
                let scroll = dbtl::ui::clamp_inspector_scroll(
                    view,
                    app.glyph_mode,
                    app.active_theme(),
                    inner.width as usize,
                    inner.height as usize,
                    app.ui_state.inspector_scroll(),
                    reveal,
                );
                app.ui_state.set_inspector_scroll(scroll);
            }

            // Precompute Dag-aware strings so RenderCtx stays Dag-free; the borrows
            // live for the draw call below.
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
                Focus::Inspector => "inspector",
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
            } else if column_s.is_none() && app.tab().lineage_view != LineageView::default() {
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
            // go stale between `Moved` events. The resolved target feeds the
            // Issue 05 hover accent and the tooltip's arm/drop below — both
            // frame-buffer overlay inputs, never CharGrid or a cache key.
            if let Some(cell) = mouse.pointer() {
                let target = if app.mode.mouse_live() {
                    let overlay = if matches!(app.mode, Mode::Selection) {
                        None
                    } else {
                        let rects = pane_layout(area, &app.ui_state.pane_config());
                        overlay_rect(
                            &app.mode,
                            area,
                            pane_interior(rects.lineage),
                            lineage.as_deref(),
                            app.ui_state.lineage_scroll_x(),
                            app.ui_state.lineage_scroll_y(),
                        )
                    };
                    let g = chrome.hit_geometry(app, lineage.as_deref(), area, overlay);
                    hit_target(&g, cell.x, cell.y)
                } else {
                    None
                };
                mouse.track_hover(target);
            }
            // Issue 05's settled-hover reconcile: the tip arms only while the
            // resolved hover rests on a real lineage NODE with no button held
            // (a press — a click or the start of a pan — is not a hover), and
            // drops when the viewport moves under a same-node pointer.
            let hover_uid = match (mouse.hover(), mouse.press()) {
                (Some(HitTarget::LineageNode(uid)), None) => Some(uid.clone()),
                _ => None,
            };
            reconcile_hover_tip(
                &mut hover_tip,
                mouse.hover(),
                mouse.press().is_some(),
                (
                    app.ui_state.lineage_scroll_x(),
                    app.ui_state.lineage_scroll_y(),
                ),
                &app.dag,
            );
            let tip_view = match (&hover_tip, mouse.pointer()) {
                (Some(t), Some(at)) if t.visible => app.tooltip_view(&t.uid, (at.x, at.y)),
                _ => None,
            };
            // §5 splitter feedback: the accent border + a status hint. The
            // hint names the live saved width mid-drag; on hover it teaches
            // the gesture (`·` is ambiguous-width, so ASCII mode swaps it).
            let live_splitter = match pane_drag {
                Some(PaneDrag::Splitter(d)) => Some(d),
                _ => None,
            };
            let (highlight, splitter_hint) = match (live_splitter, mouse.hover()) {
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
            // §8's status line: the LEFT side is the contextual hints
            // (`App::status_hints` — generated from BINDINGS), REPLACED by the
            // splitter's transient readout while its border is hot (the
            // pointer is the gesture, so its action is `None`). The RIGHT
            // side is the chip order `status_chips` derives from the segments
            // (impact/notify head the drop order).
            let panes = pane_layout(area, &app.ui_state.pane_config());
            let hints_vec: Vec<StatusHint> = match &splitter_hint {
                Some(text) => vec![StatusHint {
                    key: text.clone(),
                    desc: String::new(),
                    action: None,
                    prio: 0,
                }],
                None => app.status_hints(),
            };
            let segments = StatusSegments {
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
            // The layouts the draw paints AND the hit-test reads — stashed
            // so a hover/click lands on the spans the frame shows. The
            // title's inputs mirror the ctx's: the `[view]` suffix drops
            // while a column is traced, the search title takes over from
            // `mode`, and ` [col:name]` owns the suffix slot then.
            let chips = status_chips(&segments);
            chrome.status = Some(status_layout(panes.status.width, &hints_vec, &chips));
            let lineage_query = match &app.mode {
                dbtl::Mode::Search(s) if s.target == SearchTarget::Lineage => {
                    Some(s.query.as_str())
                }
                _ => None,
            };
            chrome.title = Some(app.lineage_title(
                lineage_query.map(|q| (q, search_pos)),
                if column_s.is_some() {
                    None
                } else {
                    Some(view_label.as_str())
                },
                breadcrumb.as_deref(),
                overview_title.as_deref(),
                col_suffix.as_deref(),
                panes.lineage.width,
            ));
            {
                let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_deref());
                ctx.mode = &app.mode;
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
                ctx.hints = &hints_vec;
                ctx.hover = mouse.hover();
                ctx.segments = segments;
                ctx.minimap = app.ui_state.minimap_visible();
                ctx.filter_label = app.list_filter_label();
                ctx.tab_bar = Some(TabBarView {
                    labels: &tab_labels,
                    layout: &tab_layout,
                    active: app.active_tab(),
                    project: Some(app.stats.project.as_str()),
                    hover: mouse.hover(),
                });
                ctx.layer_bands = layer_bands.as_deref();
                ctx.toasts = &toast_lines;
                ctx.palette_rows = palette_rows.as_deref();
                ctx.empty_reason = empty_reason.as_deref();
                // Issue 05's pointer overlays — all post-blit frame-buffer
                // effects (the CharGrid bytes and the layout cache never
                // see them): the hovered node's accent frame + incident
                // edge cells, the search-match frames + `i/N` title, and
                // the settled-hover tooltip.
                ctx.hover_node = hover_uid.as_deref();
                ctx.search_matches = &search_matches;
                ctx.search_pos = search_pos;
                ctx.tooltip = tip_view.as_ref();
                ctx.folded_groups = Some(&app.folded_groups);
                // §6's row hover — only list targets tint (a tab-strip hover
                // already feeds `tab_bar.hover`; a lineage hover is issue-05's).
                ctx.list_hover = mouse
                    .hover()
                    .filter(|h| matches!(h, HitTarget::ListRow(_) | HitTarget::ListHeader(_)));
                ctx.theme = app.active_theme();
                ctx.inspector = inspector.as_deref();
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
                        // §12's narrow band owns two keys itself — `Ctrl-b`
                        // toggles the overlay drawer (the inline pref it flips
                        // above the band governs a surface that cannot exist
                        // here, so it stays untouched) and `Esc` closes the
                        // open drawer. Consumed keys never reach the reducer.
                        if drawer_shortcut(app, key, action) {
                            continue;
                        }
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
                    // Overlay surfaces can produce real reducer outcomes
                    // (a menu row invoke, an outside-click Esc): run their
                    // effects and honor quit exactly like the key path.
                    let handled = handle_mouse(
                        app,
                        &mut mouse,
                        &mut pane_drag,
                        me,
                        lineage.as_deref(),
                        area,
                        &chrome,
                    );
                    dirty |= handled.dirty;
                    for effect in handled.outcome.effects {
                        run_effect(terminal, app, effect)?;
                    }
                    // Same watch re-baseline as the key path: a palette-run
                    // `dbt parse` (double-click confirm) can swap the source.
                    if let Some(w) = watch.as_mut() {
                        let (root, recursive) = app.watch_root();
                        if w.root != root || w.recursive != recursive {
                            *w = Watch::new(root.to_path_buf(), recursive);
                        }
                    }
                    if handled.outcome.quit {
                        return Ok(());
                    }
                    if handled.reanchor || handled.outcome.reanchor {
                        force_anchor = true;
                    }
                }
                // Resize / focus / paste and any other delivered event may
                // change the frame.
                _ => dirty = true,
            }
        }
    }
}

/// The live pane drag: which surface owns the tracking (§5 splitter, §6
/// lineage pan / minimap scrub). A single slot covers every drag surface —
/// the press cell picks it once, and mid-drag motion never re-hit-tests, so
/// a pointer dragged past a bound keeps the grabbed surface's tracking.
#[derive(Clone, Copy)]
enum PaneDrag {
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
struct SplitterDrag {
    which: Splitter,
    grab_off: u16,
}

/// The settled-hover tooltip's live state (Issue 05): which node's hover
/// armed it, the lineage viewport at arm time, when the rest began, and
/// whether the [`TOOLTIP_DELAY`] timer already fired. The loop owns the
/// clock; both the reducer and the renderer stay clock-free.
struct HoverTip {
    uid: String,
    /// The `(scroll_x, scroll_y)` the arm read — a viewport move since then
    /// left the anchored box pointing at stale content, so it re-arms.
    scroll: (usize, usize),
    since: Instant,
    visible: bool,
}

/// The stationary-hover delay before the tooltip shows (§9).
const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// Reconcile the settled-hover state against the freshest inputs (§9's
/// "消す条件": press / drag / scroll / leave). The tip arms only while the
/// resolved hover rests on a lineage node that is ALSO a real Dag node —
/// a `+M more` marker is display chrome: it accents its frame but has no
/// `dag.get`, so arming would just dirty the 500ms flip for a `tip_view`
/// that is always `None`. A held button (press or pan), a viewport move
/// under a same-node pointer (the anchor went stale), or a target change
/// re-arms/drops; `visible` only ever flips on the idle-tick timer.
fn reconcile_hover_tip(
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
fn begin_pane_drag(
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
fn apply_pane_drag(
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
fn minimap_scroll_at(app: &mut App, lineage: Option<&Layout>, area: Rect, col: u16, row: u16) {
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

/// What one mouse event produced: the frame-dirty flag plus the accumulated
/// reducer [`Outcome`] (overlay surfaces fire real actions — a menu row
/// invoke, an outside-click Esc — whose effects the caller must run) and the
/// reanchor flag a resolved palette action may need.
struct MouseHandled {
    dirty: bool,
    reanchor: bool,
    outcome: Outcome,
}

/// Apply `action` and merge its [`Outcome`] into the running accumulation —
/// one mouse gesture can legitimately chain two applies (a columns row's
/// double-click = pick + jump; a palette row's = pick + confirm).
fn apply_mouse(app: &mut App, outcome: &mut Outcome, action: Action) {
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
fn handle_mouse(
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
            if matches!(app.mode, Mode::Selection) {
                let shift = me.modifiers.contains(KeyModifiers::SHIFT);
                // §6: the wheel over the tab strip scrolls the STRIP (one
                // tab per notch) — the panes below never see it.
                if within(rects.tab_bar, me.column, me.row) {
                    app.scroll_tab_bar(if down { 1 } else { -1 });
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
            if !matches!(app.mode, Mode::Selection) {
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
            } else {
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
            // The covering overlay's outer rect — the hit-test routes by it
            // (interior rows / frame / exterior), computed from the SAME
            // layout + scroll the frame drew.
            let overlay = if sel {
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
                    let target = {
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
fn overlay_click(
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
struct ChromeGeom {
    status: Option<StatusLayout>,
    title: Option<TitleLayout>,
}

impl ChromeGeom {
    /// [`hit_geometry`] with the stash's borrows — the one call shape every
    /// mouse path uses.
    fn hit_geometry<'a>(
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
fn hit_geometry<'a>(
    app: &'a App,
    lineage: Option<&'a Layout>,
    area: Rect,
    overlay: Option<Rect>,
    status: Option<&'a StatusLayout>,
    title: Option<&'a TitleLayout>,
) -> HitGeometry<'a> {
    let panes = pane_layout(area, &app.ui_state.pane_config());
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
    }
}

/// Route §12's drawer-owned keys in the narrow band — `Esc` closes an open
/// drawer, `Ctrl-b` toggles it. Returns `true` when the key was consumed
/// HERE, so the caller leaves the reducer (and the inline-list pref
/// `Ctrl-b` guards) untouched. Outside the band the keys keep their
/// ordinary meanings; inside it only Selection's `Esc` reaches the drawer
/// (a covering modal owns its own `Esc` while it's up).
fn drawer_shortcut(app: &mut App, key: event::KeyEvent, action: Action) -> bool {
    if !app.ui_state.list_auto_hidden() {
        return false;
    }
    if matches!(app.mode, Mode::Selection) && key.code == KeyCode::Esc && app.ui_state.list_drawer()
    {
        app.ui_state.set_list_drawer(false);
        return true;
    }
    if matches!(action, Action::ToggleListPane) {
        app.ui_state.toggle_list_drawer();
        return true;
    }
    false
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
fn wheel_x(app: &mut App, rects: &PaneRects, col: u16, row: u16, right: bool) {
    // The drawer swallows every wheel kind over its surface — a notch there
    // must never reach the lineage it covers.
    if rects.list_drawer.is_some_and(|dr| within(dr, col, row)) {
        return;
    }
    if within(pane_interior(rects.lineage), col, row) {
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

/// Compute the number of visible *display rows* in the list pane — through the
/// SAME `pane_layout`/`pane_interior` the draw and the hit-test share, so the
/// tab-bar row (and any future chrome) can't silently diverge from what the
/// list actually gets. `max(1)` guards tiny terminals so we never produce a
/// zero-height window.
fn list_pane_height(terminal: &DefaultTerminal, ui: &dbtl::ui::UiState) -> Result<usize> {
    let inner = pane_interior(pane_layout(full_area(terminal)?, &ui.pane_config()).list);
    Ok((inner.height as usize).max(1))
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
        drag: &mut Option<PaneDrag>,
        kind: MouseEventKind,
        x: u16,
        y: u16,
    ) {
        feed_l(app, router, drag, kind, x, y, None);
    }

    /// [`feed`] with a real lineage `Layout` — the Issue 05 gestures (lineage
    /// pan, minimap, node hover) hit-test against the drawn diagram. Geometry
    /// at 100x30 with the fixture: tab strip row 0, body rows 1..29, list
    /// 0..34 (splitter cols 33-34), lineage interior 35..99 x 2..28; the
    /// 114x7 grid centres into a 64x7 content rect at y 11..18 — so (40,12)
    /// lands the seed box, (50,20) is interior blank, and the enabled
    /// minimap sits at 83..99 x 2..8.
    fn feed_l(
        app: &mut App,
        router: &mut MouseRouter,
        drag: &mut Option<PaneDrag>,
        kind: MouseEventKind,
        x: u16,
        y: u16,
        lineage: Option<&Layout>,
    ) {
        handle_mouse(
            app,
            router,
            drag,
            mouse(kind, x, y),
            lineage,
            Rect::new(0, 0, 100, 30),
            &ChromeGeom::default(),
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

    #[test]
    fn wheel_over_the_list_adopts_the_cursor_model_as_tab_root() {
        // §2 T1 review: a wheel notch over the list moves the CURSOR, and
        // like a j/k move the cursor's model becomes the active tab's root
        // (with the history push) — root≠cursor must not detach here.
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        let root0 = app.selected_unique_id().unwrap();
        // (10,10) at 100x30 sits inside the list pane's interior.
        feed(
            &mut app,
            &mut router,
            &mut drag,
            MouseEventKind::ScrollDown,
            10,
            10,
        );
        let landed = app.active_list().model_at(3).unwrap().unique_id.clone();
        assert_eq!(
            app.selected_unique_id().as_deref(),
            Some(landed.as_str()),
            "the wheeled cursor's model became the active tab's root"
        );
        assert_eq!(
            app.tab().back,
            vec![root0.clone()],
            "the wheel re-root pushed the old root onto back"
        );
        app.history_back();
        assert_eq!(
            app.selected_unique_id().as_deref(),
            Some(root0.as_str()),
            "`b` walks the wheel's re-root back"
        );
    }

    // ------------------------------------------------------------------
    // §6 Issue 05 gestures: lineage pan (blank AND node-origin), the
    // minimap's click-to-centre + drag-scrub, the hover resolution the
    // accent frame keys off, and the abandoned-drag carry-over fix.
    // ------------------------------------------------------------------

    /// §6/M3: a drag that began on lineage blank space pans the diagram 1:1
    /// in BOTH axes — the release is a DragEnd, so it can never become the
    /// click that would refocus the pane.
    #[test]
    fn lineage_blank_drag_pans_horizontally_and_vertically() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        let lineage = app.styled_lineage_layout();
        use MouseEventKind as K;
        // Seed the offsets so both directions have room to move.
        app.ui_state.pan_lineage(10, 4);
        for (kind, x, y) in [
            (K::Down(MouseButton::Left), 50, 20),
            (K::Drag(MouseButton::Left), 45, 17),
        ] {
            feed_l(
                &mut app,
                &mut router,
                &mut drag,
                kind,
                x,
                y,
                lineage.as_deref(),
            );
        }
        // Pointer moved (-5, -3) from the press: the picture follows, so the
        // scroll offsets grow by the same delta.
        assert_eq!(app.ui_state.lineage_scroll_x(), 15);
        assert_eq!(app.ui_state.lineage_scroll_y(), 7);
        for (kind, x, y) in [
            (K::Drag(MouseButton::Left), 55, 22),
            (K::Up(MouseButton::Left), 55, 22),
        ] {
            feed_l(
                &mut app,
                &mut router,
                &mut drag,
                kind,
                x,
                y,
                lineage.as_deref(),
            );
        }
        // +5/+2 from the press: saturating pan leaves the offsets clamped at
        // zero (the frame's clamp_lineage bounds the far edge).
        assert_eq!(app.ui_state.lineage_scroll_x(), 5);
        assert_eq!(app.ui_state.lineage_scroll_y(), 2);
        assert!(drag.is_none(), "release ends the pan");
    }

    /// §6/M3: a drag that began ON a node box is a pan — not a click. The
    /// cursor the click would have committed stays home, and the release
    /// (a DragEnd) never turns into one either.
    #[test]
    fn lineage_node_drag_pans_and_never_clicks() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        let lineage = app.styled_lineage_layout();
        use MouseEventKind as K;
        // (40,12) is inside the seed box (grid 0..26 x 0..3, content origin
        // 35,11 at scroll 0).
        let cursor_before = app.lineage_cursor_uid();
        for (kind, x, y) in [
            (K::Down(MouseButton::Left), 40, 12),
            (K::Drag(MouseButton::Left), 38, 12),
            (K::Up(MouseButton::Left), 38, 12),
        ] {
            feed_l(
                &mut app,
                &mut router,
                &mut drag,
                kind,
                x,
                y,
                lineage.as_deref(),
            );
        }
        assert_eq!(app.ui_state.lineage_scroll_x(), 2, "the node drag panned");
        assert_eq!(
            app.lineage_cursor_uid(),
            cursor_before,
            "no click happened: the cursor never committed to the dragged node"
        );
    }

    /// §6: a left click on the minimap centres the viewport on the clicked
    /// cell's grid block — (95,4) is 12 cells into the 16-wide inset.
    #[test]
    fn minimap_click_centres_the_viewport() {
        let mut app = fixture_app();
        app.ui_state.set_minimap(true);
        let mut router = MouseRouter::new();
        let mut drag = None;
        let lineage = app.styled_lineage_layout();
        use MouseEventKind as K;
        for kind in [K::Down(MouseButton::Left), K::Up(MouseButton::Left)] {
            feed_l(
                &mut app,
                &mut router,
                &mut drag,
                kind,
                95,
                4,
                lineage.as_deref(),
            );
        }
        // mx=12, sx=ceil(114/16)=8 -> 12*8+4-32 = 68 (stored raw; the frame's
        // clamp_lineage bounds it to the far edge like every scroll writer).
        assert_eq!(app.ui_state.lineage_scroll_x(), 68);
    }

    /// §6: a minimap drag scrubs the viewport — every tracked cell recentres
    /// through the same `minimap_scroll_to` mapping the click uses.
    #[test]
    fn minimap_drag_scrubs_the_viewport() {
        let mut app = fixture_app();
        app.ui_state.set_minimap(true);
        let mut router = MouseRouter::new();
        let mut drag = None;
        let lineage = app.styled_lineage_layout();
        use MouseEventKind as K;
        for (kind, x, y) in [
            (K::Down(MouseButton::Left), 85, 4),
            (K::Drag(MouseButton::Left), 90, 4),
            (K::Drag(MouseButton::Left), 95, 4),
            (K::Up(MouseButton::Left), 95, 4),
        ] {
            feed_l(
                &mut app,
                &mut router,
                &mut drag,
                kind,
                x,
                y,
                lineage.as_deref(),
            );
        }
        assert_eq!(
            app.ui_state.lineage_scroll_x(),
            68,
            "the scrub tracked the release cell's block centre"
        );
        assert!(drag.is_none());
    }

    /// The accent frame's input: a `Moved` event over a node resolves the
    /// hover target the renderer accents — deduped on the same cell, cleared
    /// off the node.
    #[test]
    fn hover_tracks_the_node_under_the_pointer() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        let lineage = app.styled_lineage_layout();
        use MouseEventKind as K;
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            K::Moved,
            40,
            12,
            lineage.as_deref(),
        );
        assert_eq!(
            router.hover(),
            Some(&HitTarget::LineageNode(
                "seed.jaffle_finance.source_datetime_policy".to_string()
            ))
        );
        // Moving to blank space clears the node target.
        feed_l(
            &mut app,
            &mut router,
            &mut drag,
            K::Moved,
            50,
            20,
            lineage.as_deref(),
        );
        assert_eq!(router.hover(), Some(&HitTarget::LineageBlank));
    }

    /// Issue 02 carry-over: a splitter drag abandoned by a superseding press
    /// can never resize on the new press's motion. The router keeps the
    /// newest press, so the in-flight drag is orphaned — the gesture gate
    /// must clear it, not keep it tracking.
    #[test]
    fn abandoned_splitter_drag_clears_on_a_superseding_press() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        // Arm a splitter drag, then press the RIGHT button mid-drag (a real
        // multi-button press or a terminal that dropped the left release).
        for (kind, x) in [
            (K::Down(MouseButton::Left), 33),
            (K::Drag(MouseButton::Left), 50),
        ] {
            feed(&mut app, &mut router, &mut drag, kind, x, 10);
        }
        assert_eq!(app.ui_state.list_width(), 51, "the left drag resized");
        feed(
            &mut app,
            &mut router,
            &mut drag,
            K::Down(MouseButton::Right),
            10,
            5,
        );
        for x in [15, 20] {
            feed(
                &mut app,
                &mut router,
                &mut drag,
                K::Drag(MouseButton::Right),
                x,
                5,
            );
        }
        assert_eq!(
            app.ui_state.list_width(),
            51,
            "the abandoned drag stopped tracking instead of following the right drag"
        );
        assert!(drag.is_none());
    }

    /// Issue 02 carry-over, second half: a mode change mid-drag abandons it
    /// too — the stored drag is cleared the moment pointer routing leaves
    /// `Selection`, so a later inert-mode move can't resize either.
    #[test]
    fn abandoned_splitter_drag_clears_on_a_mode_change() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        use MouseEventKind as K;
        for (kind, x) in [
            (K::Down(MouseButton::Left), 33),
            (K::Drag(MouseButton::Left), 50),
        ] {
            feed(&mut app, &mut router, &mut drag, kind, x, 10);
        }
        assert_eq!(app.ui_state.list_width(), 51);
        app.mode = dbtl::Mode::Help(dbtl::HelpView::new(dbtl::Mode::Selection));
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
            51,
            "the modal mode abandoned the drag"
        );
        assert!(drag.is_none());
    }

    // ------------------------------------------------------------------
    // §9 Issue 05 review: the settled tooltip drops on press / scroll /
    // leave, and never arms for a `+M more` display-chrome marker.
    // ------------------------------------------------------------------

    /// The reconcile's press half lives in `handle_mouse`: a Down feeds the
    /// router no gesture, so the event must itself dirty — otherwise no
    /// reconcile pass ever observes `mouse.press()` (the router clears it
    /// on release before the Click gesture) and a click slides under a
    /// visible tip.
    #[test]
    fn a_press_dirties_so_the_reconcile_observes_the_held_button() {
        let mut app = fixture_app();
        let mut router = MouseRouter::new();
        let mut drag = None;
        let lineage = app.styled_lineage_layout();
        let handled = handle_mouse(
            &mut app,
            &mut router,
            &mut drag,
            mouse(MouseEventKind::Down(MouseButton::Left), 40, 12),
            lineage.as_deref(),
            Rect::new(0, 0, 100, 30),
            &ChromeGeom::default(),
        );
        assert!(
            handled.dirty,
            "a press dirties so the held button drops the tip"
        );
        assert!(router.press().is_some(), "the press is now observable");
    }

    /// §9's "消す条件": the tip drops on a press (even though the same node
    /// still sits under the pointer) and on a viewport move that leaves the
    /// SAME node under the pointer — the anchored box went stale.
    #[test]
    fn tooltip_clears_on_press_and_on_same_node_scroll() {
        let app = fixture_app();
        let node = HitTarget::LineageNode("seed.jaffle_finance.source_datetime_policy".to_string());
        let mut tip = Some(HoverTip {
            uid: "seed.jaffle_finance.source_datetime_policy".to_string(),
            scroll: (0, 0),
            since: Instant::now(),
            visible: true,
        });
        // A held button: hover_uid would still resolve the node, but the
        // pressed check drops the tip outright.
        reconcile_hover_tip(&mut tip, Some(&node), true, (0, 0), &app.dag);
        assert!(tip.is_none(), "a press drops the tip");

        // Re-arm, settle, then scroll with the same node still hovered: the
        // visible tip can't ride along at its stale anchor — it re-arms.
        reconcile_hover_tip(&mut tip, Some(&node), false, (0, 0), &app.dag);
        tip.as_mut().unwrap().visible = true;
        reconcile_hover_tip(&mut tip, Some(&node), false, (4, 0), &app.dag);
        let t = tip.as_ref().expect("a same-node hover re-arms");
        assert!(
            !t.visible,
            "a scroll under the pointer drops the visible tip"
        );
        assert_eq!(t.scroll, (4, 0), "the re-arm tracked the new viewport");
        // Same node AND same viewport: the rest continues undisturbed.
        tip.as_mut().unwrap().visible = true;
        reconcile_hover_tip(&mut tip, Some(&node), false, (4, 0), &app.dag);
        assert!(tip.as_ref().unwrap().visible, "no scroll → the tip stays");
    }

    /// `+M more` markers hit-test as `LineageNode` but are display chrome —
    /// `dag.get` has nothing to describe, so the tip must never arm (it
    /// would only dirty the 500ms flip for a `tip_view` that is `None`).
    #[test]
    fn tooltip_never_arms_for_a_collapse_marker() {
        let app = fixture_app();
        let marker = HitTarget::LineageNode("model.jaffle_finance.stg_x+more".to_string());
        let mut tip = None;
        reconcile_hover_tip(&mut tip, Some(&marker), false, (0, 0), &app.dag);
        assert!(tip.is_none(), "a marker uid arms no tooltip");
        // And a REAL node arms normally — the guard doesn't over-filter.
        let node = HitTarget::LineageNode("seed.jaffle_finance.source_datetime_policy".to_string());
        reconcile_hover_tip(&mut tip, Some(&node), false, (0, 0), &app.dag);
        assert!(tip.is_some(), "a real node still arms the tip");
    }
}
