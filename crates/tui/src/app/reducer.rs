//! [`apply_action`] — the size-unaware domain reducer: the exhaustive
//! `Action` → state transition table, plus its small shared arms
//! (yank-with-notice, the modal scroll slot, the wrap-around list cycle).

use crate::action::{
    Action, DetailView, Direction, HelpView, MenuAnchor, MenuEntry, Mode, PaletteRow, PaletteState,
    SearchState, SearchTarget, SqlView, BINDINGS,
};
use crate::effect::Effect;
use crate::{build_model_list_grouped, reduce_selection, Focus, LineageLens};

use super::{App, HomeItem, LineageView, ListFilter, Outcome};

/// The shared yank arm: record the intent toast and request the clipboard
/// effect, or no-op when there is nothing to copy (no selection / empty SQL).
/// `run_effect` overwrites the notice if the clipboard write fails, so the
/// optimistic text never survives a failed copy.
fn yank_with_notice(app: &mut App, text: Option<String>, notice: &str) -> Outcome {
    match text {
        Some(text) => {
            app.set_notice(notice);
            Outcome::effect(Effect::Yank(text))
        }
        None => Outcome::cont(),
    }
}

/// The structure modal's snapshot — every Dag-derived field resolved ONCE at
/// open so the render layer needs no `Dag` (the modal is a pure payload).
/// `col_names` is the cursor-addressable list — declared columns, else the
/// resolver's inferred names so an undocumented model still gets a cursor +
/// counts. `test_index` starts detached (`usize::MAX`): the COLUMNS section
/// owns the h/l cursor until `Tab` moves it to the tests side.
fn detail_view_for(app: &App, uid: String) -> DetailView {
    let name = app.node_name_or_uid(&uid);
    let detail = app.dag.detail(&uid).cloned().unwrap_or_default();
    let tests = app.dag.tests(&uid).to_vec();
    // Blast radius of THIS node (the focus target — may be a non-root
    // source/seed/snapshot the lineage cursor sits on, NOT the root).
    let (downstream_count, upstream_count) = app.impact_counts_for(&uid);
    let catalog = app.catalog_note(&uid, &detail);
    // Column-level lineage snapshot (lazy + memoized in the resolver;
    // resolving THIS node + its direct children is the only cost).
    let col_counts = app.column_counts_for(&uid);
    let col_names: Vec<String> = if detail.columns.is_empty() {
        col_counts.keys().cloned().collect()
    } else {
        detail.columns.iter().map(|c| c.name.clone()).collect()
    };
    let col_index = if col_names.is_empty() { usize::MAX } else { 0 };
    let stale = matches!(
        crate::resolution_sql(&app.dag, &uid),
        Some((_, crate::Confidence::Stale))
    );
    // `column_counts_for` just resolved this uid, so the fallback
    // flag is settled: `[name-match]` marks the Issue 07 path.
    let name_match = app.cll.is_fallback(&uid);
    DetailView {
        model_id: uid,
        name,
        detail,
        tests,
        downstream_count,
        upstream_count,
        catalog,
        col_names,
        col_index,
        test_index: usize::MAX,
        col_counts,
        stale,
        name_match,
        scroll: 0,
    }
}

/// The inspector pane's snapshot for `uid` — built ONCE per
/// `(uid, generation)` into `LineageCaches::inspector`, so the per-frame
/// redraw is an `Rc` clone. The `detail` field is the verbatim output of
/// [`detail_view_for`] (screen-spec §4: the pane shares the structure
/// modal's construction, it never reassembles domain data itself); the
/// remaining fields are the pane-only extras the modal doesn't carry:
/// materialization tag, the third (exposure) component of the impact
/// breakdown, per-column catalog types, the last-run row, this node's lint
/// findings, and the diff readout.
pub(super) fn inspector_view_for(app: &App, uid: String) -> crate::InspectorView {
    let detail = detail_view_for(app, uid.clone());
    // `impact_breakdown_for`'s third component — the downstream EXPOSURE
    // count `impact_counts_for` deliberately drops for the modal's
    // two-number blast radius.
    let (_, _, exposures) = app.impact_breakdown_for(&uid);
    // The owner entry under the configured meta key (`owner_meta_key`,
    // default `"owner"`) — resolved here so the render layer never reads
    // the config.
    let owner = detail.detail.meta.get(&app.owner_meta_key).cloned();
    let tag = app
        .dag
        .get(&uid)
        .map(|n| {
            crate::MaterializationClass::classify(
                n.resource_type.as_str(),
                n.materialized.as_deref(),
            )
            .tag()
        })
        .unwrap_or("model");
    // The catalog's real warehouse type per declared column — keyed by the
    // column name as declared (the row lookup needs no normalization; the
    // catalog's own matching is case-insensitive already). Skips the
    // `catalog_note` line shape entirely: the pane renders types INLINE on
    // each column row, not as a separate `  col: type` block.
    let mut col_types = std::collections::BTreeMap::new();
    if let Some(node) = app.catalog().and_then(|c| c.get(&uid)) {
        for col in &detail.detail.columns {
            if let Some(ty) = node.column_type(&col.name) {
                col_types.insert(col.name.clone(), ty.to_string());
            }
        }
    }
    // The `run_results.json` row — `(status word, seconds)`. The status word
    // follows the artifact's own vocabulary so the pane reads like `dbt
    // run`'s summary line.
    let run = app.run_results().and_then(|rr| {
        rr.get(&uid).map(|r| {
            let word = match &r.status {
                crate::RunStatus::Success => "success".to_string(),
                crate::RunStatus::Error => "error".to_string(),
                crate::RunStatus::Fail => "fail".to_string(),
                crate::RunStatus::Warn => "warn".to_string(),
                crate::RunStatus::Skipped => "skipped".to_string(),
                crate::RunStatus::Other(s) => s.clone(),
            };
            (word, r.execution_time)
        })
    });
    // This node's lint findings — `"{rule}: {message}"`, in `lint_findings`'
    // own deterministic order. A finding lands here only when `node_uid` IS
    // this node: `related` uids are context on the OTHER node's violation.
    let lint = app
        .lint_findings()
        .iter()
        .filter(|f| f.node_uid == uid)
        .map(|f| format!("{}: {}", f.rule.name(), f.message))
        .collect();
    // The diff readout for THIS node — `+ added`, or `~ modified:` plus the
    // baseline's recorded reasons (a fixed-order human list). `None` on
    // `Unchanged` (nothing to show) or when no baseline is loaded.
    let diff = app.diff().and_then(|d| match d.status(&uid) {
        crate::DiffStatus::Added => Some("+ added".to_string()),
        crate::DiffStatus::Modified => Some(match d.modified.get(&uid) {
            Some(reasons) if !reasons.is_empty() => {
                format!("~ modified: {}", reasons.join(", "))
            }
            _ => "~ modified".to_string(),
        }),
        crate::DiffStatus::Unchanged => None,
    });
    crate::InspectorView {
        detail,
        tag,
        exposures,
        owner,
        col_types,
        run,
        lint,
        diff,
    }
}

/// Export filenames embed the node name, which comes from an untrusted
/// `manifest.json`: a crafted name (e.g. `../../x`) would let the export write
/// outside the working directory. Keep ASCII alphanumerics plus `.`/`-`/`_`
/// and map everything else (separators included) to `_`.
fn sanitize_file_stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Apply an [`Action`] to the app, returning the [`Outcome`] (quit + effects).
///
/// Size-unaware: it never reads pane dimensions. UiState-only actions are
/// forwarded to [`reduce_selection`]; domain actions and mode transitions are
/// handled here. The match is exhaustive over [`Action`], so a new variant is a
/// compile error until it is given behaviour.
pub fn apply_action(app: &mut App, action: Action) -> Outcome {
    // The overview shows the WHOLE graph: the rooted view filters
    // (direction/depth) and the density toggle have no visible effect while
    // it is on, so gate them to a notice instead of mutating state that
    // would surprisingly apply the moment the user drills back in.
    if app.ui_state.overview()
        && app.ui_state.focus() != Focus::Inspector
        && matches!(
            action,
            Action::ToggleUpstream
                | Action::ToggleDownstream
                | Action::DepthDecrease
                | Action::DepthIncrease
                | Action::ResetView
                | Action::ToggleDensity
        )
    {
        app.set_notice("view filters are off in overview (w to exit)");
        return Outcome::cont();
    }
    // A column trace pins the filtered graph's shape: the rooted view filters
    // (direction/depth) have no visible effect while it is on, so gate them
    // like the overview does — a silent mutation would apply the moment Esc
    // exits the trace. (`v` density stays live: Compact legitimately hides
    // the column rows.)
    if app.active_column_focus().is_some()
        && app.ui_state.focus() != Focus::Inspector
        && matches!(
            action,
            Action::ToggleUpstream
                | Action::ToggleDownstream
                | Action::DepthDecrease
                | Action::DepthIncrease
                | Action::ResetView
        )
    {
        app.set_notice("view filters are off while tracing a column (Esc to exit)");
        return Outcome::cont();
    }
    // The yank-prefix menu is a one-shot which-key: ANY action dispatched out
    // of it (a yank, the Esc/q cancel) closes it afterwards. Snapshotting the
    // mode BEFORE the match keeps the close logic in this one place instead of
    // duplicating it into every Yank-mode arm.
    let in_yank_menu = matches!(app.mode, Mode::Yank { .. });
    let outcome = match action {
        Action::Quit => Outcome::quit(),
        // Movement keys are focus-routed: lineage-pane focus moves the lineage
        // CURSOR (Dag-aware, so it lives here, not in the UiState sub-reducer);
        // list focus forwards to the sub-reducer's list selection. Neither pans
        // the viewport — the loop follows the cursor with a minimal
        // ensure-visible.
        Action::MoveDown | Action::MoveUp | Action::MoveLeft | Action::MoveRight => {
            if app.ui_state.focus() == Focus::Inspector {
                // Inspector focus: j/k scroll the pane's content; h/l have no
                // meaning there (the column cursor is a mouse affordance —
                // §4's keyboard set is scroll + Enter only).
                let scroll = app.ui_state.inspector_scroll();
                app.ui_state
                    .set_inspector_scroll(if action == Action::MoveDown {
                        scroll.saturating_add(1)
                    } else if action == Action::MoveUp {
                        scroll.saturating_sub(1)
                    } else {
                        scroll
                    });
            } else if app.ui_state.focus() == Focus::RightPane {
                let dir = match action {
                    Action::MoveDown => Direction::Down,
                    Action::MoveUp => Direction::Up,
                    Action::MoveLeft => Direction::Left,
                    _ => Direction::Right,
                };
                // Column focus turns h/l into a COLUMN step (the traced column
                // walks to its next upstream/downstream neighbour); j/k keep
                // their node-cursor meaning inside the filtered graph.
                if app.active_column_focus().is_some()
                    && matches!(dir, Direction::Left | Direction::Right)
                {
                    app.column_focus_step(dir);
                } else {
                    app.move_lineage_cursor(dir);
                }
            } else {
                // §2 T1: a list-cursor move IS the selection — adopt it as the
                // tab's root (with history). Guarded on the index actually
                // changing, so a boundary-clamped keypress (or an h/l no-op on
                // the list) never re-roots a detached tab onto the cursor.
                let before = app.ui_state.selected();
                reduce_selection(&mut app.ui_state, action);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            Outcome::cont()
        }
        // Inspector focus reroutes the shared jump/page bindings to the pane's
        // own scroll slot — they never reach the list path below.
        Action::JumpTop | Action::JumpBottom => {
            if app.ui_state.focus() == Focus::Inspector {
                app.ui_state
                    .set_inspector_scroll(if action == Action::JumpTop {
                        0
                    } else {
                        // `usize::MAX` is the "end" intent the loop's clamp bounds
                        // to content — the modal scrolls use the same idiom.
                        usize::MAX
                    });
            } else {
                // UiState-only arm: forward to the sub-reducer, then adopt on
                // the same changed-index rule as the move keys (§2 T1).
                let before = app.ui_state.selected();
                reduce_selection(&mut app.ui_state, action);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            Outcome::cont()
        }
        Action::PageDown | Action::PageUp => {
            if app.ui_state.focus() == Focus::Inspector {
                let scroll = app.ui_state.inspector_scroll();
                app.ui_state
                    .set_inspector_scroll(if action == Action::PageDown {
                        scroll.saturating_add(crate::ui::INSPECTOR_PAGE_STEP)
                    } else {
                        scroll.saturating_sub(crate::ui::INSPECTOR_PAGE_STEP)
                    });
            } else {
                let before = app.ui_state.selected();
                reduce_selection(&mut app.ui_state, action);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            Outcome::cont()
        }
        // `d`/`u` in Selection mode bind to the lineage view filters — while
        // the inspector is focused they mean what every scrollable pane reads
        // them as: a half-page-style scroll step (the same fixed 10 the modal
        // `DetailScrollPage` uses).
        Action::ToggleDownstream | Action::ToggleUpstream => {
            if app.ui_state.focus() == Focus::Inspector {
                let scroll = app.ui_state.inspector_scroll();
                app.ui_state
                    .set_inspector_scroll(if action == Action::ToggleDownstream {
                        scroll.saturating_add(crate::ui::INSPECTOR_PAGE_STEP)
                    } else {
                        scroll.saturating_sub(crate::ui::INSPECTOR_PAGE_STEP)
                    });
            } else {
                let v = &mut app.tab_mut().lineage_view;
                if action == Action::ToggleUpstream {
                    v.upstream = !v.upstream;
                } else {
                    v.downstream = !v.downstream;
                }
            }
            Outcome::cont()
        }
        // UiState-only arms: forward to the sub-reducer. The focus and
        // visibility toggles never move the cursor, so they never adopt.
        Action::ToggleFocus | Action::ToggleListPane => {
            reduce_selection(&mut app.ui_state, action);
            Outcome::cont()
        }
        // Column-extreme cursor jumps act on the lineage regardless of focus
        // (they have no list meaning, unlike the focus-routed h/l).
        Action::LineageLeftmost | Action::LineageRightmost => {
            app.move_lineage_cursor_extreme(matches!(action, Action::LineageRightmost));
            Outcome::cont()
        }
        // ---- overlays ----
        Action::HelpToggle => {
            // Esc/`?`/`q` peel the INNERMOST help state first: a live
            // `/`-filter (editing or committed) clears before the overlay
            // itself closes — and closing restores the mode it was opened
            // over (`back` keeps a modal's snapshot, payload and all).
            app.mode = match &app.mode {
                Mode::Help(h) if h.filter.is_some() => Mode::Help(HelpView {
                    scroll: h.scroll,
                    filter: None,
                    editing: false,
                    from: h.from,
                    back: h.back.clone(),
                }),
                Mode::Help(h) => (*h.back).clone(),
                _ => Mode::Help(HelpView::new(app.mode.clone())),
            };
            Outcome::cont()
        }
        Action::DetailOpen => {
            // Inspector-focused Enter commits the pane's cursored COLUMN row
            // to the column-trace view — the same flow Enter on the
            // structure modal's column cursor takes (§4: カラム行で Enter
            // → カラム Lineage 遷移). No row cursored → nothing to commit;
            // the inspector keeps focus.
            if app.ui_state.focus() == Focus::Inspector {
                let col = {
                    let idx = app.ui_state.inspector_col();
                    app.inspector_view().and_then(|v| {
                        v.detail
                            .col_names
                            .get(idx)
                            .map(|c| crate::ColumnRef::new(&v.detail.model_id, c))
                    })
                };
                if let Some(col) = col {
                    app.column_focus_enter(col);
                }
                return Outcome::cont();
            }
            // Enter acts on the lineage CURSOR when the lineage pane is focused
            // (else the selection): an off-root cursor on a model RE-ROOTS to it
            // (committing the cursor, same as a mouse click); anything else —
            // the root itself, or a non-selectable source/seed/snapshot — opens
            // the structure modal, with detail + tests snapshotted into the mode
            // payload (cloned from the Dag side maps), so the render layer needs
            // no Dag.
            let root = app.selected_unique_id();
            let Some(uid) = app.pending_or_focus() else {
                return Outcome::cont();
            };
            // A `+M more` / `-M less` marker under the cursor TOGGLES its
            // sibling group instead of opening the structure modal (the marker
            // is chrome, not a Dag node — there is nothing to detail).
            if let Some(group) = app.collapse_group(&uid) {
                app.toggle_collapse(&group.key);
                return Outcome::cont();
            }
            if app.ui_state.focus() == Focus::RightPane
                && Some(&uid) != root.as_ref()
                && app
                    .dag
                    .get(&uid)
                    .is_some_and(|n| n.resource_type == "model")
            {
                app.jump_to(&uid);
                return Outcome::cont();
            }
            app.mode = Mode::Detail(detail_view_for(app, uid));
            Outcome::cont()
        }
        Action::DetailClose => {
            // Esc/q is mode-agnostic on purpose — whichever BINDINGS row named
            // it, the close restores what the overlay stacked over: the yank
            // which-key and the node menu carry their origin mode in `back`
            // (so Structure -> y -> Esc lands back on the structure modal,
            // not the idle layer), every other overlay closes to the idle
            // mode (Selection — or Home while the home tab is active).
            let idle = app.idle_mode();
            app.mode = match std::mem::replace(&mut app.mode, Mode::Selection) {
                Mode::Yank { back, .. } => back.map(|b| *b).unwrap_or_else(|| idle.clone()),
                Mode::Menu(ms) => ms.back.map(|b| *b).unwrap_or_else(|| idle.clone()),
                _ => idle,
            };
            Outcome::cont()
        }
        // Open the yank-prefix which-key (`y` + key). The menu itself is
        // rendered from the M::Yank BINDINGS rows; the one-shot close lives in
        // the post-match step at the bottom of this function. Over the Detail
        // modal BOTH the modal's node and its column target are stashed — and
        // the modal itself is stacked into `back` so the which-key's close
        // RESTORES it instead of landing on Selection (Issue 06) — or every
        // `y<key>` yank would silently act on the list selection instead of
        // the modal's node. (The `y` row exists in Mode::Detail's bindings.)
        Action::YankPrefix => {
            let back = match std::mem::replace(&mut app.mode, Mode::Selection) {
                Mode::Detail(dv) => {
                    app.pending_node = Some(dv.model_id.clone());
                    app.pending_col = if dv.col_index != usize::MAX {
                        dv.col_names
                            .get(dv.col_index)
                            .map(|c| crate::ColumnRef::new(&dv.model_id, c))
                    } else {
                        None
                    };
                    Some(Box::new(Mode::Detail(dv)))
                }
                m => {
                    app.pending_node = None;
                    app.pending_col = None;
                    match m {
                        // A non-overlay origin needs no restore slot: closing
                        // lands on the idle mode by default anyway.
                        Mode::Selection | Mode::Home | Mode::Search(_) | Mode::Menu(_) => None,
                        m => Some(Box::new(m)),
                    }
                }
            };
            app.mode = Mode::Yank { back, sel: 0 };
            Outcome::cont()
        }
        // ---- node context menu (Issue 06) ----
        // `a` (and the event loop's right-click, which goes through
        // `App::open_menu` directly) opens the context menu for the CURRENT
        // node: over a Detail/Sql modal that's the modal's own node (the same
        // target rule `y` uses), in Selection the focus target. The box's
        // origin mode rides in `MenuState.back` so Esc/execute restores it.
        Action::MenuOpen => {
            match app.menu_uid() {
                Some(uid) => app.open_menu(&uid, MenuAnchor::Node),
                None => app.set_notice("nothing to act on"),
            }
            Outcome::cont()
        }
        Action::MenuMove(dir) => {
            // j/k walk the ITEM rows only — separators are unselectable. The
            // cursor wraps at both ends (a menu is a ring, like the yank
            // prefix's implicit "any key closes" ring of options).
            if let Mode::Menu(ms) = &mut app.mode {
                let n = ms.items.len();
                if n > 0 {
                    let mut i = ms.selected;
                    for _ in 0..n {
                        i = match dir {
                            Direction::Up => (i + n - 1) % n,
                            _ => (i + 1) % n,
                        };
                        if matches!(ms.items[i], MenuEntry::Item { .. }) {
                            ms.selected = i;
                            break;
                        }
                    }
                }
            }
            Outcome::cont()
        }
        Action::MenuConfirm => {
            // Enter runs the highlighted row — the same path an item shortcut
            // or a row double-click takes.
            let sel = match &app.mode {
                Mode::Menu(ms) => Some(ms.selected),
                _ => None,
            };
            if let Some(i) = sel {
                return apply_action(app, Action::MenuInvoke(i));
            }
            Outcome::cont()
        }
        Action::MenuInvoke(i) => {
            // Resolve the items-row index to the BINDINGS row it points at —
            // the action, key and help text all live there, so the menu
            // literally cannot drift from the keymap. The menu is one-shot
            // like the which-key: restore the stacked origin mode, retarget
            // the selection at the menu's node (its execution contract:
            // items act on the node they were opened for, never on wherever
            // the selection happened to be), then apply the action itself.
            let resolved = match &app.mode {
                Mode::Menu(ms) => match ms.items.get(i) {
                    Some(MenuEntry::Item { binding, .. }) => Some((
                        BINDINGS[*binding].action,
                        ms.back.clone(),
                        ms.target.clone(),
                    )),
                    // A separator row (or a stale index) is inert — the menu
                    // stays open rather than close on a mis-click.
                    _ => None,
                },
                _ => None,
            };
            let Some((action, back, target)) = resolved else {
                return Outcome::cont();
            };
            app.mode = back.map(|b| *b).unwrap_or_else(|| app.idle_mode());
            app.move_selection_to(&target);
            // Arm the overlay target stash for exactly this dispatch: the
            // selection move above can't land on a non-model node (the list
            // holds models only), so every producer that reads the selection
            // — the yanks, bookmark/path, the editor open, `Enter`/`s` —
            // must see the MENU's target instead or the item would act on
            // whatever happened to be selected.
            app.pending_node = Some(target);
            let mut out = apply_action(app, action);
            app.pending_node = None;
            // Same re-anchor contract as the palette confirm below: the loop
            // can't see the menu-resolved action, so the flag travels on the
            // Outcome.
            out.reanchor |= crate::action::reanchors_lineage(action);
            return out;
        }
        // ---- pointer row actions (Issue 06 mouse) ----
        // A row click in a row-bearing overlay is a SELECTION first (per the
        // modal-mouse rule); a double-click then fires the row's execute
        // action. None of these are key-bound — the event loop produces them.
        Action::MenuSelect(i) => {
            // Hover highlight; separators never take the cursor.
            if let Mode::Menu(ms) = &mut app.mode {
                if matches!(ms.items.get(i), Some(MenuEntry::Item { .. })) {
                    ms.selected = i;
                }
            }
            Outcome::cont()
        }
        Action::YankPick(i) => {
            if let Mode::Yank { sel, .. } = &mut app.mode {
                if i < crate::action::yank_bindings().count() {
                    *sel = i;
                }
            }
            Outcome::cont()
        }
        Action::YankInvoke(i) => {
            // Run the which-key row's own action through the same dispatch the
            // key would take — the row IS its binding.
            if let Some(action) = crate::action::yank_bindings().nth(i).map(|b| b.action) {
                return apply_action(app, action);
            }
            Outcome::cont()
        }
        Action::PalettePick(i) => {
            // `i` indexes the SELECTABLE rows (`palette_selectable_rows` —
            // headers stripped), the same space `p.selected` and the
            // confirm/move arms use.
            let query = match &app.mode {
                Mode::Palette(p) => p.query.clone(),
                _ => return Outcome::cont(),
            };
            if i < app.palette_selectable_rows(&query).len() {
                if let Mode::Palette(p) = &mut app.mode {
                    p.selected = i;
                }
            }
            Outcome::cont()
        }
        Action::ColumnPick(i) => {
            // Same landing rule the j/k cursor follows: headers/separators
            // (uid == None) are not selectable rows.
            if let Mode::Columns(cv) = &mut app.mode {
                if cv.rows.get(i).is_some_and(|r| r.uid.is_some()) {
                    cv.cursor = i;
                }
            }
            Outcome::cont()
        }
        // The structure modal's column cursor (h/l): a SELECTION, not a scroll —
        // the loop's per-frame `clamp_detail_scroll` does the ensure-visible.
        // (`Direction::Up`/`Down` here mean previous/next ROW of the column
        // list — the enum is reused for a vertical cursor, not for the
        // lineage's upstream/downstream axes.)
        Action::DetailColumnMove(dir) => {
            if let Mode::Detail(dv) = &mut app.mode {
                if dv.test_index != usize::MAX {
                    // The tests section owns the cursor: walk only the
                    // JUMPABLE rows (relationships `to:` targets) — plain test
                    // rows are skipped the way the columns modal skips
                    // uid-less rows.
                    let back = matches!(dir, Direction::Up | Direction::Left);
                    let next = if back {
                        (0..dv.test_index)
                            .rev()
                            .find(|&i| dv.tests[i].ref_uid.is_some())
                    } else {
                        (dv.test_index + 1..dv.tests.len()).find(|&i| dv.tests[i].ref_uid.is_some())
                    };
                    if let Some(i) = next {
                        dv.test_index = i;
                    }
                } else if dv.col_index != usize::MAX && !dv.col_names.is_empty() {
                    let last = dv.col_names.len() - 1;
                    dv.col_index = match dir {
                        Direction::Up | Direction::Left => dv.col_index.saturating_sub(1),
                        _ => (dv.col_index + 1).min(last),
                    };
                }
            }
            Outcome::cont()
        }
        Action::DetailSectionToggle => {
            // `Tab` inside the structure modal moves the h/l cursor between
            // the COLUMNS list and the TESTS list. The tests side only accepts
            // the cursor when at least one row is jumpable (a relationships
            // test with a resolved `to:` target) — otherwise Tab notices and
            // stays on columns, so Enter can never strand on a dead row.
            if let Mode::Detail(dv) = &mut app.mode {
                if dv.test_index == usize::MAX {
                    match dv.tests.iter().position(|t| t.ref_uid.is_some()) {
                        Some(i) => dv.test_index = i,
                        None => app.set_notice("no relationship test targets on this node"),
                    }
                } else {
                    dv.test_index = usize::MAX;
                }
            }
            Outcome::cont()
        }
        // Overlay scroll: acts inside whichever scrollable overlay is open. The
        // size-aware upper clamp is the loop's.
        Action::DetailScroll(dir) | Action::DetailScrollPage(dir) => {
            // Line vs page step; the loop's per-frame clamp bounds the result.
            let amount = if matches!(action, Action::DetailScroll(_)) {
                1
            } else {
                10
            };
            if let Some(s) = modal_scroll_mut(&mut app.mode) {
                *s = if matches!(dir, crate::action::Direction::Down) {
                    s.saturating_add(amount)
                } else {
                    s.saturating_sub(amount)
                };
            }
            Outcome::cont()
        }
        Action::DetailScrollHome | Action::DetailScrollEnd => {
            // End records MAX as "as far as it goes"; the loop's per-frame
            // clamp (the same one every scroll passes through) bounds it to
            // the content height, so the reducer stays size-unaware.
            if let Some(s) = modal_scroll_mut(&mut app.mode) {
                *s = if matches!(action, Action::DetailScrollHome) {
                    0
                } else {
                    usize::MAX
                };
            }
            Outcome::cont()
        }
        // ---- column-level lineage (Issue 09) ----
        Action::ColumnFocusEnter => {
            // Enter is contextual over the structure modal: while the tests
            // section owns the cursor it jumps to the relationships `to:`
            // target (the same re-root contract as the column modal's Enter —
            // a non-model target notices instead); over the columns section it
            // commits the cursored column to the trace view; a column-less
            // modal keeps Enter's classic close behaviour.
            if let Mode::Detail(dv) = &app.mode {
                if dv.test_index != usize::MAX {
                    let target = dv.tests.get(dv.test_index).and_then(|t| t.ref_uid.clone());
                    match target {
                        Some(uid)
                            if app
                                .dag
                                .get(&uid)
                                .is_some_and(|n| n.resource_type == "model") =>
                        {
                            app.jump_to(&uid);
                            app.mode = app.idle_mode();
                        }
                        Some(_) => app.set_notice("cannot re-root to a non-model node"),
                        None => {}
                    }
                    return Outcome::cont();
                }
            }
            let col = match &app.mode {
                Mode::Detail(dv) if dv.col_index != usize::MAX => dv
                    .col_names
                    .get(dv.col_index)
                    .map(|c| crate::ColumnRef::new(&dv.model_id, c)),
                _ => None,
            };
            app.mode = app.idle_mode();
            if let Some(col) = col {
                app.column_focus_enter(col);
            }
            Outcome::cont()
        }
        Action::ColumnFocusExit => {
            // Esc exits the trace view — the lineage returns to the normal
            // rooted subgraph of whatever is selected.
            app.tab_mut().column_focus = None;
            Outcome::cont()
        }
        Action::ColumnsOpen => {
            // The modal's column: the structure modal's cursored column when
            // opened over it, else the traced column's (Selection `K`).
            let col = match &app.mode {
                Mode::Detail(dv) if dv.col_index != usize::MAX => dv
                    .col_names
                    .get(dv.col_index)
                    .map(|c| crate::ColumnRef::new(&dv.model_id, c)),
                _ => app.tab().column_focus.as_ref().map(|f| f.col.clone()),
            };
            match col {
                Some(col) => app.mode = Mode::Columns(app.columns_view_for(&col)),
                None => app.set_notice("no column selected — Enter a model, h/l pick a column"),
            }
            Outcome::cont()
        }
        Action::ColumnRowMove(dir) => {
            // The column modal's row cursor (j/k): a SELECTION — the loop's
            // `clamp_columns_scroll` does the ensure-visible follow.
            if let Mode::Columns(cv) = &mut app.mode {
                cv.move_cursor(dir);
            }
            Outcome::cont()
        }
        Action::ColumnJump => {
            // Re-root to the cursored row's model (records history like a
            // lineage click; landing on a DIFFERENT model exits the trace via
            // reroot_select — the traced column's own row keeps it). A
            // non-model row (a source/seed column) notices instead.
            let uid = match &app.mode {
                Mode::Columns(cv) => cv.rows.get(cv.cursor).and_then(|r| r.uid.clone()),
                _ => None,
            };
            match uid {
                Some(uid)
                    if app
                        .dag
                        .get(&uid)
                        .is_some_and(|n| n.resource_type == "model") =>
                {
                    app.jump_to(&uid);
                    app.mode = app.idle_mode();
                }
                Some(_) => app.set_notice("cannot re-root to a non-model node"),
                None => {}
            }
            Outcome::cont()
        }
        // ---- command palette ----
        // Open the fuzzy command finder. Its editing keys reuse the Search* arms,
        // routed by `Mode::Palette` below.
        Action::PaletteOpen => {
            app.mode = Mode::Palette(PaletteState::default());
            Outcome::cont()
        }
        // ---- search ----
        Action::SearchOpen => {
            // Over the help overlay, `/` arms the line filter instead — the
            // Search* editing actions route into it by mode (see dispatch's
            // editing pre-emption).
            if let Mode::Help(h) = &mut app.mode {
                h.editing = true;
                h.filter.get_or_insert_with(String::new);
                h.scroll = 0;
                return Outcome::cont();
            }
            let target = if app.home_active() {
                // Issue 10: `/` on home searches models for a TAB to open —
                // no list cursor exists, so there is no origin to restore.
                SearchTarget::Home
            } else if app.ui_state.focus() == Focus::List {
                SearchTarget::List
            } else {
                SearchTarget::Lineage
            };
            // The search's restore point: the list CURSOR (what Esc lands
            // back on), not the root — under a detached root the cursor sits
            // on a different model. A home-target search has NO origin —
            // restoring one would re-root nothing yet still record the
            // unseen list-cursor model into the Recent MRU.
            let origin = if matches!(target, SearchTarget::Home) {
                None
            } else {
                app.list_sel_uid()
            };
            app.mode = Mode::Search(SearchState {
                target,
                query: String::new(),
                origin_uid: origin,
                match_idx: 0,
                hist: None,
            });
            app.refilter(); // no-op for a lineage-target search
            Outcome::cont()
        }
        Action::SearchType(c) => {
            // Route by mode: the palette and the search share the action, never
            // the state. In the palette, typing only ever NARROWS the candidate
            // list, so reset `selected` to 0 (which also satisfies the
            // "selection clamps when the filter shrinks" contract).
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.push(c);
                    s.hist = None; // editing leaves the history browse
                    s.match_idx = 0; // query changed → matches changed
                    let lineage = s.target == SearchTarget::Lineage;
                    app.refilter();
                    // A hidden (collapsed-away) match auto-expands its group so
                    // the loop's anchor has a rect to land on.
                    if lineage {
                        app.reveal_current_lineage_match();
                    }
                }
                Mode::Palette(p) => {
                    p.query.push(c);
                    p.selected = 0;
                }
                // The help `/`-filter: typing edits it (and pulls the overlay
                // back to the top — the match set changed under it).
                Mode::Help(h) if h.editing => {
                    if let Some(f) = h.filter.as_mut() {
                        f.push(c);
                    }
                    h.scroll = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchBackspace => {
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.pop();
                    s.hist = None;
                    s.match_idx = 0;
                    let lineage = s.target == SearchTarget::Lineage;
                    app.refilter();
                    if lineage {
                        app.reveal_current_lineage_match();
                    }
                }
                Mode::Palette(p) => {
                    p.query.pop();
                    // Reset to the top EXACTLY like the SearchType arm, so the
                    // highlighted row is a pure function of the query — the same
                    // query always lands the same row regardless of arrival path
                    // (typing vs. backspacing). (Previously kept-and-clamped, which
                    // left ~1/3 of backspace transitions on an unrelated command.)
                    p.selected = 0;
                }
                Mode::Help(h) if h.editing => {
                    if let Some(f) = h.filter.as_mut() {
                        f.pop();
                    }
                    h.scroll = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchMove(dir) => {
            // Command palette: move `selected` over the SELECTABLE rows (the
            // section headers are display-only, never a target), clamped at
            // both ends (no wrap), mirroring a list cursor. An `@`-prefixed
            // query lists MODELS (`palette_selectable_rows` routes to the
            // model rows) — commands otherwise.
            if let Mode::Palette(p) = &app.mode {
                let count = app.palette_selectable_rows(&p.query).len();
                let next = match dir {
                    Direction::Down if count > 0 => (p.selected + 1).min(count - 1),
                    Direction::Up => p.selected.saturating_sub(1),
                    _ => p.selected,
                };
                if let Mode::Palette(p) = &mut app.mode {
                    p.selected = next;
                }
                return Outcome::cont();
            }
            // List search: while the query is EMPTY — or an Up/Down browse is
            // already live — the arrows recall the saved search history
            // (older on Up, newer on Down; Down past the newest entry returns
            // to the live empty query). A non-empty live query keeps the
            // original filtered-list move. Lineage search: cycle matches.
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone(), s.hist)),
                _ => None,
            };
            match target {
                Some((SearchTarget::List, query, hist)) => {
                    let browsing = hist.is_some() || query.is_empty();
                    let nav = matches!(dir, Direction::Up | Direction::Down);
                    if browsing && nav && !app.search_history.is_empty() {
                        let next: Option<usize> = match (dir, hist) {
                            (Direction::Up, None) => Some(0),
                            (Direction::Up, Some(i)) => {
                                Some((i + 1).min(app.search_history.len() - 1))
                            }
                            // Down past the newest (or at the live position):
                            // back to the live empty query — a no-op when
                            // already live.
                            (Direction::Down, Some(0)) | (Direction::Down, None) => None,
                            (Direction::Down, Some(i)) => Some(i - 1),
                            _ => hist,
                        };
                        let recalled = next.map(|i| app.search_history[i].clone());
                        if let Mode::Search(s) = &mut app.mode {
                            s.hist = next;
                            s.query = recalled.unwrap_or_default();
                            s.match_idx = 0;
                        }
                        app.refilter();
                    } else {
                        // Browsing IS selecting (§2 T1): a moved cursor adopts
                        // its model — same changed-index guard as move keys.
                        let before = app.ui_state.selected();
                        match dir {
                            Direction::Down => {
                                reduce_selection(&mut app.ui_state, Action::MoveDown)
                            }
                            Direction::Up => reduce_selection(&mut app.ui_state, Action::MoveUp),
                            _ => {}
                        }
                        if app.ui_state.selected() != before {
                            app.adopt_list_selection();
                        }
                    }
                }
                Some((SearchTarget::Lineage, query, _)) => {
                    let count = app.lineage_matches(&query).len();
                    if count > 0 {
                        if let Mode::Search(s) = &mut app.mode {
                            s.match_idx = match dir {
                                Direction::Down => (s.match_idx + 1) % count,
                                Direction::Up => (s.match_idx + count - 1) % count,
                                _ => s.match_idx,
                            };
                        }
                        // Cycling to a hidden match expands its group — the
                        // anchor needs the match drawn to land on it.
                        app.reveal_current_lineage_match();
                    }
                }
                Some((SearchTarget::Home, query, hist)) => {
                    // Same recall contract as the list target: Up/Down on an
                    // EMPTY query (or a live browse) walks `search_history`;
                    // a typed query moves `match_idx` over the result rows
                    // (`home_data` recomputes them — the reducer and the
                    // renderer share one source).
                    let browsing = hist.is_some() || query.is_empty();
                    let nav = matches!(dir, Direction::Up | Direction::Down);
                    if browsing && nav && !app.search_history.is_empty() {
                        let next: Option<usize> = match (dir, hist) {
                            (Direction::Up, None) => Some(0),
                            (Direction::Up, Some(i)) => {
                                Some((i + 1).min(app.search_history.len() - 1))
                            }
                            (Direction::Down, Some(0)) | (Direction::Down, None) => None,
                            (Direction::Down, Some(i)) => Some(i - 1),
                            _ => hist,
                        };
                        let recalled = next.map(|i| app.search_history[i].clone());
                        if let Mode::Search(s) = &mut app.mode {
                            s.hist = next;
                            s.query = recalled.unwrap_or_default();
                            s.match_idx = 0;
                        }
                    } else {
                        let count = app.home_search_len();
                        if count > 0 {
                            if let Mode::Search(s) = &mut app.mode {
                                s.match_idx = match dir {
                                    Direction::Down => (s.match_idx + 1).min(count - 1),
                                    Direction::Up => s.match_idx.saturating_sub(1),
                                    _ => s.match_idx,
                                };
                            }
                        }
                    }
                }
                None => {}
            }
            Outcome::cont()
        }
        Action::SearchCancel => {
            // Palette cancel just closes the overlay — no filter/list to restore.
            if matches!(app.mode, Mode::Palette(_)) {
                app.mode = app.idle_mode();
                return Outcome::cont();
            }
            let origin = match &app.mode {
                Mode::Search(s) => s.origin_uid.clone(),
                _ => None,
            };
            // Cancel restores the pre-search root — folds untouched (the
            // unfold-on-confirm policy is the CONFIRM arm's alone).
            app.close_search(origin, false);
            Outcome::cont()
        }
        Action::SearchConfirm => {
            // Help `/`-filter: Enter COMMITS the filter — editing ends but
            // the filter stays applied (Esc clears it outright). A committed
            // EMPTY filter normalizes away (Some("") filters nothing yet
            // would still ride the title and swallow an Esc). Outside editing
            // Help swallows the action — no Enter binding exists there anyway.
            if let Mode::Help(h) = &mut app.mode {
                if h.editing {
                    h.editing = false;
                    if h.filter.as_deref() == Some("") {
                        h.filter = None;
                    }
                }
                return Outcome::cont();
            }
            // Palette confirm: resolve the selected row (headers aren't
            // selectable, so `selected` always lands on a Command or Model —
            // `None` only when zero candidates exist, which still closes).
            // `selected` can overshoot when the candidates shrank under a
            // FIXED query (e.g. a --watch reload mid-palette): clamp to the
            // last row rather than treating it as "no candidates".
            // `PaletteRow` is owned, so the mode borrow ends before mutation.
            if let Mode::Palette(p) = &app.mode {
                let new_tab = p.new_tab;
                let rows = app.palette_selectable_rows(&p.query);
                let chosen = if rows.is_empty() {
                    None
                } else {
                    rows.get(p.selected.min(rows.len() - 1)).cloned()
                };
                return match chosen {
                    Some(PaletteRow::Command {
                        action,
                        unavailable,
                        ..
                    }) => {
                        // A greyed command can't run: keep the palette open
                        // and toast WHY (the spec's "reason on Enter").
                        if let Some(reason) = unavailable {
                            if let Mode::Palette(p) = &mut app.mode {
                                p.selected = p.selected.min(rows.len() - 1);
                            }
                            app.set_notice(reason);
                            Outcome::cont()
                        } else {
                            app.record_palette_run(action);
                            app.mode = app.idle_mode();
                            // Close FIRST, then apply the chosen action and
                            // propagate its WHOLE Outcome (so "quit" quits and
                            // "$EDITOR" yields the effect) — same contract the
                            // plain `palette_candidates` path had. The flag
                            // carries the resolved action's re-anchor intent —
                            // the loop's dispatched-key check can't see it.
                            let mut out = apply_action(app, action);
                            out.reanchor |= crate::action::reanchors_lineage(action);
                            out
                        }
                    }
                    // `@` model search: Enter re-roots to the model — the
                    // spec's "active tab root" (`jump_to` records history like
                    // a click does). The `+` cell's `new_tab` palette instead
                    // opens the model AS a new tab (dedupes to its tab when
                    // already open — §2/§7).
                    Some(PaletteRow::Model { uid, .. }) => {
                        app.mode = app.idle_mode();
                        if new_tab {
                            app.open_node_as_tab(&uid);
                        } else {
                            app.jump_to(&uid);
                        }
                        Outcome::cont()
                    }
                    Some(PaletteRow::Header(_)) | None => {
                        app.mode = app.idle_mode();
                        Outcome::cont()
                    }
                };
            }
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone())),
                _ => None,
            };
            match target {
                Some((SearchTarget::Lineage, _)) => {
                    // Jump to the currently-cycled match: if it is a selectable
                    // model, re-root to it (recording history); else just leave.
                    let hit = app.current_lineage_match();
                    app.mode = app.idle_mode();
                    if let Some(uid) = hit {
                        app.jump_to(&uid);
                    }
                }
                Some((SearchTarget::Home, query)) => {
                    // Home search: Enter opens the highlighted result AS A
                    // TAB (T2 dedupes) and records the query — the same
                    // `search_history` the list target's recall browses. The
                    // close restores nothing (home has no list cursor).
                    app.record_search_query(&query);
                    let picked = app.home_search_pick();
                    app.close_search(None, false);
                    if let Some(uid) = picked {
                        app.open_node_as_tab(&uid);
                    }
                }
                other => {
                    // List search: keep the highlighted match — the list
                    // CURSOR's model (resolve by id in the full list) — and
                    // record the confirmed query: the `/`-mode Up/Down recall
                    // browses this history.
                    if let Some((SearchTarget::List, query)) = &other {
                        app.record_search_query(query);
                    }
                    let chosen = app.list_sel_uid();
                    // A confirmed match may live in a folded group (search
                    // ignores folds): unfold its group so the pick lands.
                    app.close_search(chosen, true);
                }
            }
            Outcome::cont()
        }
        // ---- effects (performed by the loop, never here) ----
        Action::OpenEditor => match app
            .pending_or_selection()
            .and_then(|uid| app.file_path_for(&uid))
        {
            Some(path) => Outcome::effect(Effect::OpenEditor(path)),
            None => Outcome::cont(),
        },
        Action::YankId => {
            let text = app.yank_target_uid();
            yank_with_notice(app, text, "Copied unique_id")
        }
        Action::YankName => {
            let text = app.yank_target_uid().map(|uid| app.node_name_or_uid(&uid));
            yank_with_notice(app, text, "Copied model name")
        }
        Action::YankMermaid => {
            let text = app.lineage_mermaid();
            yank_with_notice(app, text, "Copied Mermaid lineage")
        }
        Action::YankDot => {
            let text = app.lineage_dot();
            yank_with_notice(app, text, "Copied DOT lineage")
        }
        Action::YankAscii => {
            let text = app.lineage_ascii();
            yank_with_notice(app, text, "Copied ASCII lineage")
        }
        Action::YankSql => {
            let text = app.yank_target_uid().and_then(|uid| app.raw_sql_for(&uid));
            yank_with_notice(app, text, "Copied raw SQL")
        }
        Action::YankCompiledSql => {
            // The yank target's compiled SQL (`yC` stays raw-only). Same
            // blank-is-absent rule as `selected_raw_sql` — an empty expansion
            // must not clobber the clipboard with nothing.
            let text = app
                .yank_target_uid()
                .and_then(|uid| app.dag.compiled(&uid).map(|c| (c.code.clone(), c.stale)))
                .filter(|(code, _)| !code.trim().is_empty());
            match text {
                Some((code, stale)) => {
                    let notice = if stale {
                        "Copied compiled SQL (stale)"
                    } else {
                        "Copied compiled SQL"
                    };
                    yank_with_notice(app, Some(code), notice)
                }
                None => {
                    app.set_notice("no compiled SQL for this node (run dbt compile)");
                    Outcome::cont()
                }
            }
        }
        Action::YankColumnLineage => {
            // `pending_col` is rewritten on every YankPrefix: `Some` = the
            // menu opened over the structure modal's column cursor; `None` =
            // it came from selection, where an ACTIVE trace still names a
            // column (the traced `col` is the fallback target). Only with no
            // column at all do we explain how to pick one.
            match app
                .pending_col
                .take()
                .or_else(|| app.tab().column_focus.as_ref().map(|f| f.col.clone()))
            {
                Some(col) => {
                    let text =
                        app.column_lineage_markdown(&col, crate::ClosureDirection::Downstream);
                    yank_with_notice(app, Some(text), "Copied column lineage")
                }
                None => {
                    app.set_notice("no column selected — Enter, h/l pick a column, then y l");
                    Outcome::cont()
                }
            }
        }
        Action::YankImpact => {
            let text = app
                .yank_target_uid()
                .and_then(|uid| app.impact_report_for(&uid));
            yank_with_notice(app, text, "Copied impact report")
        }
        Action::YankSelector => {
            let text = app
                .yank_target_uid()
                .and_then(|uid| app.dbt_selector_command_for(&uid));
            yank_with_notice(app, text, "Copied dbt selector")
        }
        Action::ExportLineage => {
            // The overview exports the whole graph — name the file after the
            // project, not the (incidental) selection.
            let stem = if app.ui_state.overview() {
                app.dag
                    .project_name()
                    .map(|p| format!("{}_overview", sanitize_file_stem(p)))
            } else {
                app.selected_name()
                    .map(|n| format!("{}_lineage", sanitize_file_stem(&n)))
            };
            match (stem, app.lineage_ascii()) {
                (Some(stem), Some(contents)) => {
                    let path = format!("{stem}.txt");
                    // Optimistic intent; `run_effect` overwrites it if the write fails.
                    app.set_notice(format!("Exported {path}"));
                    Outcome::effect(Effect::WriteFile { path, contents })
                }
                _ => Outcome::cont(),
            }
        }
        Action::Reload => Outcome::effect(Effect::ReloadManifest),
        // The loop performs the whole flow (resolve dbt from PATH, suspend the
        // TUI, run, adopt the manifest) and reports the outcome on the notice
        // channel — the reducer stays free of filesystem/PATH reads.
        Action::DbtParse => Outcome::effect(Effect::DbtParse),
        // Recenter and the lineage-view actions re-anchor the lineage; that is
        // applied size-aware in the loop, so the reducer just records intent.
        // Recenter additionally sends the cursor home, so `z` always means
        // "back to the rooted node".
        Action::Recenter => {
            app.reset_lineage_cursor();
            Outcome::cont()
        }
        Action::DepthDecrease => {
            let v = &mut app.tab_mut().lineage_view;
            v.depth = match v.depth {
                None => Some(3),
                Some(n) => Some(n.saturating_sub(1).max(1)),
            };
            Outcome::cont()
        }
        Action::DepthIncrease => {
            let v = &mut app.tab_mut().lineage_view;
            v.depth = match v.depth {
                None => None,
                Some(n) if n >= 8 => None, // widen past 8 hops → unlimited
                Some(n) => Some(n + 1),
            };
            Outcome::cont()
        }
        Action::ResetView => {
            app.tab_mut().lineage_view = LineageView::default();
            Outcome::cont()
        }
        Action::HistoryBack => {
            app.history_back();
            Outcome::cont()
        }
        Action::HistoryForward => {
            app.history_forward();
            Outcome::cont()
        }
        // ---- SQL preview / stats dashboard modals ----
        Action::SqlOpen => {
            // Focus-aware target (`s` previews whatever the lineage cursor is on
            // when the lineage pane is focused, else the list selection — an
            // armed overlay stash wins, so a menu-invoked `s` previews the
            // MENU's node); unlike `Enter`, `s` only PREVIEWS — it never
            // re-roots.
            let Some(uid) = app.pending_or_focus() else {
                return Outcome::cont();
            };
            // A collapse marker has no SQL — `s` previews nodes, not chrome.
            if app.collapse_group(&uid).is_some() {
                app.set_notice("no SQL for a collapse marker (Enter toggles it)");
                return Outcome::cont();
            }
            let name = app.node_name_or_uid(&uid);
            // Sources/seeds (and manifests omitting raw_code) get a placeholder —
            // there is no transient-status channel in this app.
            let raw = app.dag.raw_code(&uid).map(str::to_string);
            let sql = match &raw {
                Some(code) => code.clone(),
                None => {
                    let rt = app
                        .dag
                        .get(&uid)
                        .map(|n| n.resource_type.as_str())
                        .unwrap_or("node");
                    format!("(no SQL for this {rt})")
                }
            };
            // The compiled variant rides in the same snapshot (RenderCtx stays
            // Dag-free): `c` inside the modal only flips a flag.
            let (compiled, compiled_stale) = app
                .dag
                .compiled(&uid)
                .map(|c| (Some(c.code.clone()), c.stale))
                .unwrap_or((None, false));
            // Open on the compiled text when it is the only real SQL there is
            // — `raw_code` absent OR blank/whitespace-only (dbt emits `""` for
            // e.g. ephemeral config-only nodes, and an empty modal helps nobody).
            let show_compiled =
                raw.as_deref().is_none_or(|c| c.trim().is_empty()) && compiled.is_some();
            let path = app
                .dag
                .detail(&uid)
                .and_then(|d| d.original_file_path.clone());
            app.mode = Mode::Sql(SqlView {
                model_id: uid,
                name,
                sql,
                compiled,
                compiled_stale,
                show_compiled,
                path,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::SqlToggleCompiled => {
            match &mut app.mode {
                // Flip the display flag only; both texts were snapshotted at
                // open. The scroll resets — raw and compiled are different
                // texts, so a carried offset would land on an arbitrary line.
                Mode::Sql(sv) if sv.compiled.is_some() => {
                    sv.show_compiled = !sv.show_compiled;
                    sv.scroll = 0;
                }
                Mode::Sql(_) => app.set_notice("no compiled SQL for this node (run dbt compile)"),
                _ => {}
            }
            Outcome::cont()
        }
        Action::StatsOpen => {
            app.mode = Mode::Stats(app.compute_stats_view());
            Outcome::cont()
        }
        // The notification centre (`Ctrl-n`, Issue 08): opens on a snapshot
        // of the log; opening is the read gesture — `!N` zeroes the moment
        // the modal appears.
        Action::NotificationsOpen => {
            app.mark_notifications_read();
            app.mode = Mode::Notifications(app.notifications_snapshot());
            Outcome::cont()
        }
        // The baseline-diff summary: opens only with a --diff baseline loaded
        // (the payload is snapshotted like the other modals); otherwise a toast
        // explains how to get one.
        Action::DiffOpen => {
            match app.compute_diff_view() {
                Some(dv) => app.mode = Mode::Diff(dv),
                None => app.set_notice("No diff baseline (start with --diff <manifest|project>)"),
            }
            Outcome::cont()
        }
        // Export the PR impact report from the open Diff modal: render the
        // snapshotted `DiffView` to Markdown (so the file matches the screen) and
        // write it to `<project>_pr_impact.md`. The borrow of `app.mode` is
        // released before the mutating notice/effect.
        Action::ExportPrImpact => {
            let payload = if let Mode::Diff(dv) = &app.mode {
                let project = app.dag.project_name().unwrap_or("dbt").to_string();
                Some((project, app.pr_impact_markdown(dv)))
            } else {
                None
            };
            match payload {
                Some((project, contents)) => {
                    let path = format!("{}_pr_impact.md", sanitize_file_stem(&project));
                    // Optimistic intent; `run_effect` overwrites it on failure.
                    app.set_notice(format!("Exported {path}"));
                    Outcome::effect(Effect::WriteFile { path, contents })
                }
                None => Outcome::cont(),
            }
        }
        // Copy the suggested `dbt build --select …` command from the open Diff
        // modal. `y` takes the column-narrowed selector when the analysis
        // produced one (the most precise buildable set), falling back to the
        // wide `ci_command`; `Y` forces the wide command. A blank command
        // (nothing buildable changed) notices instead of silently clobbering
        // the clipboard. The `&app.mode` borrow is released (the command is
        // cloned) before the mutating notice/yank.
        Action::YankPrSelector | Action::YankPrSelectorWide => {
            let wide = matches!(action, Action::YankPrSelectorWide);
            let cmd = match &app.mode {
                Mode::Diff(dv) => Some(if wide {
                    (dv.pr.ci_command.clone(), "Copied dbt build command")
                } else {
                    match &dv.pr.ci_command_columns {
                        // The narrowed selector exists — prefer it.
                        Some(c) => (Some(c.clone()), "Copied column-level dbt build command"),
                        None => (dv.pr.ci_command.clone(), "Copied dbt build command"),
                    }
                }),
                _ => None,
            };
            match cmd {
                Some((Some(c), notice)) => yank_with_notice(app, Some(c), notice),
                Some((None, _)) => {
                    app.set_notice("No changed models to build");
                    Outcome::cont()
                }
                None => Outcome::cont(),
            }
        }
        // Lineage lens cycle: a pure view-pref mutation. Routed DIRECTLY here (not
        // through reduce_selection, which is only the legacy list-movement arms)
        // per the two-level-reducer contract.
        Action::CycleLens => {
            app.ui_state.cycle_lens();
            // Data-less lenses look identical to Off — skip them so the cycle
            // never lands on a dead slot: Lint with no findings, Diff without a
            // --diff baseline, RunStatus/RunTime without a loaded
            // run_results.json.
            loop {
                let dead = match app.ui_state.lens() {
                    LineageLens::Lint => app.lint_findings().is_empty(),
                    LineageLens::Diff => app.diff().is_none(),
                    LineageLens::RunStatus | LineageLens::RunTime => app.run_results().is_none(),
                    // The ColumnImpact lens only tints inside a column trace —
                    // without one it looks identical to Off, so skip the slot.
                    // The EFFECTIVE focus is the test: an overview-suppressed
                    // focus (state alive, display off) is a dead slot too.
                    LineageLens::ColumnImpact => app.active_column_focus().is_none(),
                    _ => false,
                };
                if !dead {
                    break;
                }
                app.ui_state.cycle_lens();
            }
            Outcome::cont()
        }
        // Pointer-only lens set: the status chips name the lens they want
        // (the `run …` chip jumps straight to RunStatus rather than pedalling
        // the `t` cycle through dead slots). Never key-bound — the chip only
        // exists while its lens has data, so no dead-slot skip is needed.
        Action::SetLens(lens) => {
            app.ui_state.set_lens(lens);
            Outcome::cont()
        }
        // ---- bookmarks + list sort (Step C) — App-level data, so handled here
        // (never reduce_selection, which is UiState-only and can't reach them). ----
        // Toggle a bookmark on the SELECTED model, regardless of focus — an
        // armed overlay stash wins, so a menu-invoked Bookmark toggles the
        // menu's node (the menu hides this row for non-models anyway). The
        // list holds only models, so the target always has a badge row.
        Action::BookmarkToggle => {
            if let Some(uid) = app.pending_or_selection() {
                let name = app.node_name_or_uid(&uid);
                if app.bookmarks.insert(uid.clone()) {
                    app.set_notice(format!("Bookmarked: {name}"));
                } else {
                    app.bookmarks.remove(&uid);
                    app.set_notice(format!("Removed bookmark: {name}"));
                }
                app.home_stamp += 1;
                // Under the Bookmarked filter the toggle changes the view's
                // membership — rebuild it so an un-bookmarked row leaves it.
                if app.list_filter == ListFilter::Bookmarked {
                    app.apply_list_filter();
                }
            }
            Outcome::cont()
        }
        Action::ToggleUntestedFilter | Action::ToggleBookmarkFilter => {
            let target = if matches!(action, Action::ToggleUntestedFilter) {
                ListFilter::Untested
            } else {
                ListFilter::Bookmarked
            };
            // Toggling the active filter turns it off; a different one replaces it.
            app.list_filter = if app.list_filter == target {
                ListFilter::All
            } else {
                target
            };
            app.apply_list_filter();
            Outcome::cont()
        }
        // `-`: fold/unfold the group under the cursor through the SAME
        // apply_list_filter rebuild the `T`/`*` toggles use — a folded group
        // keeps its header row but sheds its models from selection space
        // (§10; the fold state is the persisted `folded_groups` set, §13).
        Action::ToggleGroupFold => {
            app.toggle_selected_group_fold();
            Outcome::cont()
        }
        // Jump to the next bookmarked model in the ACTIVE list's visible order
        // (filtered order during search), wrapping from selected+1. A selection
        // jump like history, not a focus-routed move; no-op when no bookmark is
        // visible. The target index is computed under the immutable `active_list`
        // borrow, then applied — so the borrow is released before the mutation.
        Action::BookmarkCycle | Action::BookmarkCycleBack => {
            let forward = matches!(action, Action::BookmarkCycle);
            let target = cycle_to(app.active_list(), app.ui_state.selected(), forward, |m| {
                app.bookmarks.contains(&m.unique_id)
            });
            // A cycle landing on a different model is a re-root — §2 T1 makes
            // it history like any other selection (`jump_to` owns the push;
            // `reroot_select` inside it ends the column trace).
            if let Some(uid) = target
                .and_then(|i| app.active_list().model_at(i))
                .map(|m| m.unique_id.clone())
            {
                app.jump_to(&uid);
            }
            Outcome::cont()
        }
        Action::GapNext | Action::GapPrev => {
            let forward = matches!(action, Action::GapNext);
            let target = cycle_to(
                app.active_list(),
                app.ui_state.selected(),
                forward,
                crate::coverage_gap,
            );
            if let Some(uid) = target
                .and_then(|i| app.active_list().model_at(i))
                .map(|m| m.unique_id.clone())
            {
                app.jump_to(&uid);
            }
            Outcome::cont()
        }
        // Cycle the within-group sort, rebuild the list in the new order, and
        // re-resolve the selection BY unique_id (never a raw index across the
        // rebuild). `refilter` re-derives the filtered view if a search is active.
        Action::SortCycle => {
            app.sort = app.sort.next();
            let current = app.selected_unique_id();
            app.model_list =
                build_model_list_grouped(&app.dag, app.sort, app.group_by_meta.as_deref());
            // Re-derive the persistent-filter view from the re-sorted list (it
            // refilters the open search itself, so the search view inherits
            // the new order too).
            app.apply_list_filter();
            if let Some(uid) = &current {
                app.select_by_unique_id(uid);
            }
            // The rebuilt order is a home input too (search results follow
            // `model_list`) — the cache must miss or home shows the old sort.
            app.home_stamp += 1;
            Outcome::cont()
        }
        // Toggle the lineage minimap (a pure UiState view-pref). Routed DIRECTLY
        // here, mirroring `CycleLens` — NOT through `reduce_selection`, which is
        // the UiState-only legacy list-movement arms (spec D5).
        Action::ToggleMinimap => {
            app.ui_state.toggle_minimap();
            Outcome::cont()
        }
        // Toggle the lineage density (a pure UiState view-pref, like the
        // minimap). The loop force-anchors next frame — the grid reshapes.
        Action::ToggleDensity => {
            app.ui_state.toggle_density();
            Outcome::cont()
        }
        // Toggle the whole-graph overview (a pure UiState view-pref, like the
        // density). Entering also switches the minimap on (the diagram is
        // guaranteed to overflow) and toasts an orientation hint; the loop
        // force-anchors next frame — the grid reshapes completely.
        Action::ToggleOverview => {
            // The home body has no lineage to overview — toggling here
            // would leak the flag into the next model tab opened.
            if app.home_active() {
                app.set_notice("overview needs a model tab".to_string());
                return Outcome::cont();
            }
            app.ui_state.toggle_overview();
            if app.ui_state.overview() {
                if !app.ui_state.minimap_visible() {
                    app.ui_state.toggle_minimap();
                }
                // ASCII-only: the toast draws in BOTH glyph modes, and an em
                // dash is ambiguous-width (the exact desync ASCII mode exists
                // to avoid).
                app.set_notice(format!("overview: {} nodes (w to exit)", app.dag.len()));
            }
            Outcome::cont()
        }
        // `<`/`>`: nudge the saved pane width by §5's 4-column step, a pure
        // UiState view-pref like the minimap — direct here, never through
        // `reduce_selection`. The pane follows §5's focus rule: the inspector
        // while focused, the list otherwise. The floor is in the write; the
        // terminal-relative cap is the event loop's post-clamp (size-aware)
        // plus `pane_layout`'s display clamp — the reducer stays size-blind.
        Action::PaneNarrow | Action::PaneWiden => {
            let step = if matches!(action, Action::PaneWiden) {
                4
            } else {
                -4
            };
            if app.ui_state.focus() == Focus::Inspector {
                app.ui_state.nudge_inspector_width(step);
            } else {
                app.ui_state.nudge_list_width(step);
            }
            Outcome::cont()
        }
        // `=`: every resizable pane's saved width back to its §1 default
        // (§5 "全部を既定値に" — the inspector's saved width included, though
        // its pane only materializes in Issue 04).
        Action::PaneWidthsReset => {
            app.ui_state.reset_pane_width(crate::ui::Splitter::List);
            app.ui_state
                .reset_pane_width(crate::ui::Splitter::Inspector);
            Outcome::cont()
        }
        // ---- tabs (Issue 03; §2) ----
        // `O`: the focus target's model opens in a NEW tab right of the
        // active one — the lineage cursor's node when the lineage pane is
        // focused, else the list cursor's (a non-model / filtered-out node
        // toasts its kind instead). Already-open roots just activate (T2).
        Action::TabOpen => {
            if let Some(uid) = app.focus_target() {
                app.open_node_as_tab(&uid);
            }
            Outcome::cont()
        }
        // `X`/`Ctrl-w`: close the ACTIVE tab (the right neighbour, else the
        // left, takes over; the last tab refuses with a toast inside).
        Action::TabClose => {
            app.close_tab(app.active_tab());
            Outcome::cont()
        }
        // `{`/`}`: cycle one tab, wrapping at both ends.
        Action::TabPrev | Action::TabNext => {
            app.cycle_tab(matches!(action, Action::TabNext));
            Outcome::cont()
        }
        // `1`..=`9`: activate the tab at that position — the bar numbers
        // MODEL tabs 1-9 and home sits at index 0, so the key IS the strip
        // index (out-of-range is a no-op; `~` reaches home).
        Action::TabActivate(n) => {
            app.activate_tab(usize::from(n));
            Outcome::cont()
        }
        // ---- home (Issue 10; §3) ----
        // `~` (or the `⌂` cell): activate the fixed home tab. From home it
        // is a no-op (idx 0 is already active).
        Action::HomeGo => {
            app.enter_home();
            Outcome::cont()
        }
        // `j`/`k` under `Mode::Home`: move the item cursor over the flat
        // selectable rows (clamped, no wrap — the list-cursor rule).
        Action::HomeMove(dir) => {
            app.move_home_sel(dir);
            Outcome::cont()
        }
        // Enter / a row click: open the row's model AS A TAB (T2 dedupes to
        // the open one), or run its command — §3 drops the unavailable ones
        // at build, so a drawn command always runs.
        Action::HomeOpen | Action::HomePick(_) => {
            let idx = match action {
                Action::HomePick(i) => i,
                _ => app.home_sel,
            };
            match app.home_data().selectable.get(idx) {
                Some(HomeItem::Model { uid }) => {
                    let uid = uid.clone();
                    app.open_node_as_tab(&uid);
                }
                Some(HomeItem::Command { action: a }) => {
                    return apply_action(app, *a);
                }
                None => {}
            }
            Outcome::cont()
        }
        // `I`: cycle the inspector visibility pref `auto → on → off` — the
        // persisted value only; the event loop resolves `auto` against the
        // terminal width and publishes `inspector_shown` next frame.
        Action::ToggleInspector => {
            app.ui_state.cycle_inspector();
            Outcome::cont()
        }
        // A click on an inspector column row moves the pane's column cursor
        // to it — the same select-before-Enter contract the columns modal's
        // `ColumnPick` keeps (double-click wiring lands with Issue 05).
        Action::InspectorColumnPick(i) => {
            let cols = app
                .inspector_view()
                .map(|v| v.detail.col_names.len())
                .unwrap_or(0);
            app.ui_state.set_inspector_col(if cols == 0 {
                usize::MAX
            } else {
                i.min(cols - 1)
            });
            Outcome::cont()
        }
        // `p` two-node path: first press anchors the path start at the
        // selection (the highlight then runs anchor↔cursor), second press
        // clears it back to root↔cursor. Pure state + a toast; the
        // path-set BFS reads `path_anchor` next frame — no cache reset needed
        // (styles are recomputed per draw anyway).
        Action::PathToggle => {
            if let Some(uid) = app.tab_mut().path_anchor.take() {
                let name = app.node_name_or_uid(&uid);
                app.set_notice(format!("path anchor cleared ({name})"));
            } else if let Some(uid) = app.pending_or_selection() {
                let name = app.node_name_or_uid(&uid);
                app.tab_mut().path_anchor = Some(uid);
                app.set_notice(format!(
                    "path anchor: {name} — move cursor to target (p clears)"
                ));
            }
            Outcome::cont()
        }
        // Cycle the colour theme (App-level data: the loaded list lives on the
        // App, not UiState). Pure state + a toast; the loop hands the new
        // active theme to `RenderCtx` next frame — no cache is touched (the
        // cached layout's attrs are semantic, colours resolve at draw).
        Action::ThemeCycle => {
            app.cycle_theme();
            Outcome::cont()
        }
    };
    if in_yank_menu
        && !matches!(
            action,
            Action::YankPick(_)
                | Action::DetailScroll(_)
                | Action::DetailScrollPage(_)
                | Action::DetailScrollHome
                | Action::DetailScrollEnd
        )
    {
        // A yank / an unhandled key closes the one-shot which-key: the mode it
        // stacked over (`back`, e.g. the structure modal) comes back — NOT a
        // jump to Selection (Issue 06's nested-overlay rule). Two gestures are
        // exempt: `YankPick` (a pointer hover/row click only moves the
        // highlight) and the DetailScroll family (a wheel over the which-key
        // is a no-op — the menu has no scroll slot — and must not dismiss it).
        if let Mode::Yank { back, .. } = &mut app.mode {
            if let Some(back) = back.take() {
                app.mode = *back;
            } else {
                app.mode = app.idle_mode();
            }
        }
    }
    // The yank stashes live only while the which-key is open — that INCLUDES
    // a `?` help layer sitting on top of it (`Help` whose `back` is `Yank`):
    // Esc peels the help back to the menu, which must find the modal target
    // still stashed, or the next `y<key>` silently yanks the list selection
    // instead of the modal's node. Any other mode — after a yank, or an
    // Esc/q cancel — drops the modal context, so a stale
    // `pending_node`/`pending_col` can never leak into a later yank (both
    // are also rewritten on every `y` press; this just closes the stale-read
    // window after a cancel). `MenuInvoke` arms `pending_node` for its own
    // dispatch window and clears it right after, so it never reaches here.
    let yank_still_open = matches!(app.mode, Mode::Yank { .. })
        || matches!(&app.mode, Mode::Help(h) if matches!(h.back.as_ref(), Mode::Yank { .. }));
    if !yank_still_open {
        app.pending_col = None;
        app.pending_node = None;
    }
    outcome
}

/// The scroll slot of whichever overlay modal is open (`None` in non-modal
/// modes). The ONE place every scrollable modal is enumerated, shared by the
/// line / page / home / end scroll arms so a new modal can't be wired into
/// some arms and missed in others.
fn modal_scroll_mut(mode: &mut Mode) -> Option<&mut usize> {
    match mode {
        Mode::Help(h) => Some(&mut h.scroll),
        Mode::Detail(dv) => Some(&mut dv.scroll),
        Mode::Sql(sv) => Some(&mut sv.scroll),
        Mode::Stats(sv) => Some(&mut sv.scroll),
        Mode::Diff(dv) => Some(&mut dv.scroll),
        Mode::Columns(cv) => Some(&mut cv.scroll),
        Mode::Notifications(nv) => Some(&mut nv.scroll),
        _ => None,
    }
}

/// The next selection index (scanning from `start`, exclusive, wrapping) whose
/// model satisfies `pred`, or `None` when no model does. Shared by the
/// bookmark and coverage-gap cycles; `start` itself is checked LAST, so with a
/// single matching model the cycle still lands on it.
fn cycle_to(
    list: &crate::model_list::ModelList,
    start: usize,
    forward: bool,
    pred: impl Fn(&crate::NodeInfo) -> bool,
) -> Option<usize> {
    let n = list.len();
    if n == 0 {
        return None;
    }
    (1..=n).find_map(|off| {
        let i = if forward {
            (start + off) % n
        } else {
            (start + n - off % n) % n
        };
        list.model_at(i).filter(|m| pred(m)).map(|_| i)
    })
}
