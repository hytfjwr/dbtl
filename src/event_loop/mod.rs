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

mod effects;
mod mouse;
mod proc;
#[cfg(test)]
mod tests;
mod toasts;
mod viewport;

use effects::run_effect;
use mouse::{
    drawer_shortcut, handle_mouse, reconcile_hover_tip, ChromeGeom, HoverTip, PaneDrag,
    TOOLTIP_DELAY,
};
use proc::{dbt_command, open_in_editor, yank_to_clipboard};
#[cfg(test)]
use proc::{editor_failure_notice, feed_and_reap, find_in_path_dirs, split_editor_command};
#[cfg(test)]
use toasts::toast_ttl;
use toasts::{warnings_tail, ToastItem, ToastStack};
use viewport::{full_area, lineage_viewport, list_pane_height};

pub(crate) use proc::find_dbt;
pub(crate) use toasts::now_hhmmss;
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
    // The on-screen toast stack: each App notice is stamped with
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
    // The settled-hover tooltip: armed when the resolved hover
    // rests on a lineage node with no button held, shown after
    // `TOOLTIP_DELAY` stationary, dropped when the hover leaves or a press
    // lands. The loop owns the clock — the 250ms idle tick supplies the
    // flip's redraw, the reducer and renderer stay clock-free.
    let mut hover_tip: Option<HoverTip> = None;
    // The status/title chrome geometry: the status-line + lineage-title
    // layouts the LAST draw painted — recomputed alongside the ctx each
    // dirty pass and handed to `hit_geometry`, so a hover/click lands on
    // the exact spans the frame showed (the draw and the hit-test share
    // the layout).
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
        // The responsive size gates, resolved once per iteration in the ONLY
        // size-aware layer: below the usable floor the whole app suspends behind the
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
        // The tooltip's 500ms flip is itself a redraw reason: an idle
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
            // incoming tab's own cursor and collapse expansions restore,
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
            // (`on`/`off` answer for themselves; `auto` shows at >=160
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
            // The lineage-search overlays: every matching node's
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
                    // The verbatim restore: the switch already swapped the
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
            // manual scroll, so `‹`/`›`/wheel can actually look around ("the
            // active tab stays scrolled into view" is a switch-time
            // guarantee). The RESOLVED first-visible index is written back,
            // so a hit-test always sees what the last frame drew.
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
            let inspector = if want && !app.home_active() && panes.inspector.width > 0 {
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
            // The unread-warning badge (`!N`) — sits right after
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
            // hover accent and the tooltip's arm/drop below — both
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
            // The settled-hover reconcile: the tip arms only while the
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
            // Splitter feedback: the accent border + a status hint. The
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
            // The status line: the LEFT side is the contextual hints
            // (`App::status_hints` — generated from BINDINGS), REPLACED by the
            // splitter's transient readout while its border is hot (the
            // pointer is the gesture, so its action is `None`). The RIGHT
            // side is the chip order `status_chips` derives from the segments
            // (impact/notify head the drop order).
            let panes = pane_layout(area, &app.ui_state.pane_config());
            // The home body's draw product — built only while the
            // fixed home tab is active (a `None` ctx.home keeps the panes).
            // The size-aware seam then clamps `home_sel`/`match_idx` into
            // range and scrolls so the cursor's line stays visible — the
            // same ensure-visible job the list's offset does.
            let home_data = if app.home_active() {
                Some(app.home_data())
            } else {
                None
            };
            if let Some(data) = &home_data {
                let body = panes.list.union(panes.lineage).union(panes.inspector);
                let n = data.selectable.len();
                let cur = match &mut app.mode {
                    dbtl::Mode::Search(s) if s.target == dbtl::SearchTarget::Home => {
                        let c = s.match_idx.min(n.saturating_sub(1));
                        s.match_idx = c;
                        c
                    }
                    _ => {
                        app.home_sel = app.home_sel.min(n.saturating_sub(1));
                        app.home_sel
                    }
                };
                let plan = dbtl::ui::home_lines(data);
                let sel_line = dbtl::ui::home_line_of(&plan, cur);
                let h = body.height as usize;
                if sel_line < app.home_scroll {
                    app.home_scroll = sel_line;
                } else if sel_line >= app.home_scroll + h.max(1) {
                    app.home_scroll = sel_line + 1 - h.max(1);
                }
                app.home_scroll = app.home_scroll.min(plan.len().saturating_sub(1));
            }
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
                // The `focus:` chip names a pane focus — meaningless on the
                // home body (it owns the whole band), so it drops there.
                focus: (!app.home_active()).then_some(focus_s),
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
                // The pointer overlays — all post-blit frame-buffer
                // effects (the CharGrid bytes and the layout cache never
                // see them): the hovered node's accent frame + incident
                // edge cells, the search-match frames + `i/N` title, and
                // the settled-hover tooltip.
                ctx.hover_node = hover_uid.as_deref();
                ctx.search_matches = &search_matches;
                ctx.search_pos = search_pos;
                ctx.tooltip = tip_view.as_ref();
                ctx.folded_groups = Some(&app.folded_groups);
                // The row hover — only list targets tint (a tab-strip hover
                // already feeds `tab_bar.hover`; a lineage hover has its own
                // overlay).
                ctx.list_hover = mouse
                    .hover()
                    .filter(|h| matches!(h, HitTarget::ListRow(_) | HitTarget::ListHeader(_)));
                ctx.theme = app.active_theme();
                ctx.inspector = inspector.as_deref();
                // `Some` IS the body switch — the home view owns
                // the whole band while the fixed tab is active.
                ctx.home = home_data.as_deref();
                ctx.home_sel = app.home_sel;
                ctx.home_scroll = app.home_scroll;
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
                        // The narrow band owns two keys itself — `Ctrl-b`
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
                        // Enter IS the drawer's keyboard pick — the
                        // row's model commits (re-root/detail) and the
                        // transient surface closes, the same contract a
                        // mouse click-pick keeps.
                        if matches!(action, Action::DetailOpen) && app.ui_state.list_drawer() {
                            app.ui_state.set_list_drawer(false);
                        }
                        // `<`/`>` stepped the saved width size-blind (the pane
                        // floor lives in `nudge_list_width`); the cap is
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
