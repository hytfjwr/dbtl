//! [`apply_action`] — the size-unaware domain reducer: the exhaustive
//! `Action` → state transition table, plus its small shared arms
//! (yank-with-notice, the modal scroll slot, the wrap-around list cycle).

use crate::action::{
    palette_candidates, Action, DetailView, Direction, Mode, PaletteState, SearchState,
    SearchTarget, SqlView,
};
use crate::effect::Effect;
use crate::{build_model_list, reduce_selection, Focus};

use super::{App, LineageView, ListFilter, Outcome};

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

/// Apply an [`Action`] to the app, returning the [`Outcome`] (quit + effects).
///
/// Size-unaware: it never reads pane dimensions. UiState-only actions are
/// forwarded to [`reduce_selection`]; domain actions and mode transitions are
/// handled here. The match is exhaustive over [`Action`], so a new variant is a
/// compile error until it is given behaviour.
pub fn apply_action(app: &mut App, action: Action) -> Outcome {
    match action {
        Action::Quit => Outcome::quit(),
        // Movement keys are focus-routed: lineage-pane focus moves the lineage
        // CURSOR (Dag-aware, so it lives here, not in the UiState sub-reducer);
        // list focus forwards to the sub-reducer's list selection. Neither pans
        // the viewport — the loop follows the cursor with a minimal
        // ensure-visible.
        Action::MoveDown | Action::MoveUp | Action::MoveLeft | Action::MoveRight => {
            if app.ui_state.focus() == Focus::RightPane {
                let dir = match action {
                    Action::MoveDown => Direction::Down,
                    Action::MoveUp => Direction::Up,
                    Action::MoveLeft => Direction::Left,
                    _ => Direction::Right,
                };
                app.move_lineage_cursor(dir);
            } else {
                reduce_selection(&mut app.ui_state, action);
            }
            Outcome::cont()
        }
        // UiState-only arms: forward to the sub-reducer.
        Action::JumpTop
        | Action::JumpBottom
        | Action::PageDown
        | Action::PageUp
        | Action::ToggleFocus
        | Action::ToggleListPane => {
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
            app.mode = if matches!(app.mode, Mode::Help { .. }) {
                Mode::Selection
            } else {
                Mode::Help { scroll: 0 }
            };
            Outcome::cont()
        }
        Action::DetailOpen => {
            // Enter acts on the lineage CURSOR when the lineage pane is focused
            // (else the selection): an off-root cursor on a model RE-ROOTS to it
            // (committing the cursor, same as a mouse click); anything else —
            // the root itself, or a non-selectable source/seed/snapshot — opens
            // the structure modal, with detail + tests snapshotted into the mode
            // payload (cloned from the Dag side maps), so the render layer needs
            // no Dag.
            let root = app.selected_unique_id();
            let Some(uid) = app.focus_target() else {
                return Outcome::cont();
            };
            if Some(&uid) != root.as_ref()
                && app
                    .dag
                    .get(&uid)
                    .is_some_and(|n| n.resource_type == "model")
            {
                app.jump_to(&uid);
                return Outcome::cont();
            }
            let name = app.node_name_or_uid(&uid);
            let detail = app.dag.detail(&uid).cloned().unwrap_or_default();
            let tests = app.dag.tests(&uid).to_vec();
            // Blast radius of THIS node (the focus target — may be a non-root
            // source/seed/snapshot the lineage cursor sits on, NOT the root).
            let (downstream_count, upstream_count) = app.impact_counts_for(&uid);
            app.mode = Mode::Detail(DetailView {
                model_id: uid,
                name,
                detail,
                tests,
                downstream_count,
                upstream_count,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::DetailClose => {
            app.mode = Mode::Selection;
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
        // ---- command palette ----
        // Open the fuzzy command finder. Its editing keys reuse the Search* arms,
        // routed by `Mode::Palette` below.
        Action::PaletteOpen => {
            app.mode = Mode::Palette(PaletteState::default());
            Outcome::cont()
        }
        // ---- search ----
        Action::SearchOpen => {
            let target = if app.ui_state.focus() == Focus::List {
                SearchTarget::List
            } else {
                SearchTarget::Lineage
            };
            let origin = app.selected_node().map(|n| n.unique_id.clone());
            app.mode = Mode::Search(SearchState {
                target,
                query: String::new(),
                origin_uid: origin,
                match_idx: 0,
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
                    s.match_idx = 0; // query changed → matches changed
                    app.refilter();
                }
                Mode::Palette(p) => {
                    p.query.push(c);
                    p.selected = 0;
                }
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchBackspace => {
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.pop();
                    s.match_idx = 0;
                    app.refilter();
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
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchMove(dir) => {
            // Command palette: move `selected` over the current candidates,
            // clamped at both ends (no wrap), mirroring a list cursor.
            if let Mode::Palette(p) = &mut app.mode {
                let count = palette_candidates(&p.query).len();
                if count > 0 {
                    p.selected = match dir {
                        Direction::Down => (p.selected + 1).min(count - 1),
                        Direction::Up => p.selected.saturating_sub(1),
                        _ => p.selected,
                    };
                }
                return Outcome::cont();
            }
            // List search: move within the filtered list (loop's ensure_visible
            // follows; move_* require List focus, which a list search has).
            // Lineage search: cycle the match cursor (the loop anchors to it).
            let target = match &app.mode {
                Mode::Search(s) => Some((s.target, s.query.clone())),
                _ => None,
            };
            match target {
                Some((SearchTarget::List, _)) => match dir {
                    Direction::Down => reduce_selection(&mut app.ui_state, Action::MoveDown),
                    Direction::Up => reduce_selection(&mut app.ui_state, Action::MoveUp),
                    _ => {}
                },
                Some((SearchTarget::Lineage, query)) => {
                    let count = app.lineage_matches(&query).len();
                    if count > 0 {
                        if let Mode::Search(s) = &mut app.mode {
                            s.match_idx = match dir {
                                Direction::Down => (s.match_idx + 1) % count,
                                Direction::Up => (s.match_idx + count - 1) % count,
                                _ => s.match_idx,
                            };
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
                app.mode = Mode::Selection;
                return Outcome::cont();
            }
            let origin = match &app.mode {
                Mode::Search(s) => s.origin_uid.clone(),
                _ => None,
            };
            app.close_search(origin);
            Outcome::cont()
        }
        Action::SearchConfirm => {
            // Palette confirm: resolve the selected candidate to its Action,
            // close the palette FIRST, then recursively apply the chosen action
            // and propagate its WHOLE Outcome (so choosing "quit" quits, and
            // choosing "open SQL in $EDITOR" yields the effect). `Action` is
            // `Copy` and the candidate refs are `'static`, so the borrow ends
            // before the mutation.
            if let Mode::Palette(p) = &app.mode {
                let chosen = palette_candidates(&p.query)
                    .get(p.selected)
                    .map(|b| b.action);
                app.mode = Mode::Selection;
                return match chosen {
                    Some(a) => apply_action(app, a),
                    None => Outcome::cont(),
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
                    app.mode = Mode::Selection;
                    if let Some(uid) = hit {
                        app.jump_to(&uid);
                    }
                }
                _ => {
                    // List search: keep the highlighted match (resolve by id in
                    // the full list).
                    let chosen = app.selected_node().map(|n| n.unique_id.clone());
                    app.close_search(chosen);
                }
            }
            Outcome::cont()
        }
        // ---- effects (performed by the loop, never here) ----
        Action::OpenEditor => match app.selected_file_path() {
            Some(path) => Outcome::effect(Effect::OpenEditor(path)),
            None => Outcome::cont(),
        },
        Action::YankId => {
            let text = app.selected_unique_id();
            yank_with_notice(app, text, "Copied unique_id")
        }
        Action::YankName => {
            let text = app.selected_name();
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
            let text = app.selected_raw_sql();
            yank_with_notice(app, text, "Copied raw SQL")
        }
        Action::YankImpact => {
            let text = app.impact_report();
            yank_with_notice(app, text, "Copied impact report")
        }
        Action::ExportLineage => match (app.selected_name(), app.lineage_ascii()) {
            (Some(name), Some(contents)) => {
                let path = format!("{name}_lineage.txt");
                // Optimistic intent; `run_effect` overwrites it if the write fails.
                app.set_notice(format!("Exported {path}"));
                Outcome::effect(Effect::WriteFile { path, contents })
            }
            _ => Outcome::cont(),
        },
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
        Action::ToggleUpstream => {
            app.lineage_view.upstream = !app.lineage_view.upstream;
            Outcome::cont()
        }
        Action::ToggleDownstream => {
            app.lineage_view.downstream = !app.lineage_view.downstream;
            Outcome::cont()
        }
        Action::DepthDecrease => {
            app.lineage_view.depth = match app.lineage_view.depth {
                None => Some(3),
                Some(n) => Some(n.saturating_sub(1).max(1)),
            };
            Outcome::cont()
        }
        Action::DepthIncrease => {
            app.lineage_view.depth = match app.lineage_view.depth {
                None => None,
                Some(n) if n >= 8 => None, // widen past 8 hops → unlimited
                Some(n) => Some(n + 1),
            };
            Outcome::cont()
        }
        Action::ResetView => {
            app.lineage_view = LineageView::default();
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
            // when the lineage pane is focused, else the list selection); unlike
            // `Enter`, `s` only PREVIEWS — it never re-roots.
            let Some(uid) = app.focus_target() else {
                return Outcome::cont();
            };
            let name = app.node_name_or_uid(&uid);
            // Sources/seeds (and manifests omitting raw_code) get a placeholder —
            // there is no transient-status channel in this app.
            let sql = match app.dag.raw_code(&uid) {
                Some(code) => code.to_string(),
                None => {
                    let rt = app
                        .dag
                        .get(&uid)
                        .map(|n| n.resource_type.as_str())
                        .unwrap_or("node");
                    format!("(no SQL for this {rt})")
                }
            };
            let path = app
                .dag
                .detail(&uid)
                .and_then(|d| d.original_file_path.clone());
            app.mode = Mode::Sql(SqlView {
                model_id: uid,
                name,
                sql,
                path,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::StatsOpen => {
            app.mode = Mode::Stats(app.compute_stats_view());
            Outcome::cont()
        }
        // Lineage lens cycle: a pure view-pref mutation. Routed DIRECTLY here (not
        // through reduce_selection, which is only the legacy list-movement arms)
        // per the two-level-reducer contract.
        Action::CycleLens => {
            app.ui_state.cycle_lens();
            Outcome::cont()
        }
        // ---- bookmarks + list sort (Step C) — App-level data, so handled here
        // (never reduce_selection, which is UiState-only and can't reach them). ----
        // Toggle a bookmark on the SELECTED model, regardless of focus: the list
        // holds only models, so the selection always has a row to draw the badge
        // on (a lineage-cursor source/seed/snapshot would have no list-row home).
        Action::BookmarkToggle => {
            if let Some(uid) = app.selected_unique_id() {
                let name = app.node_name_or_uid(&uid);
                if app.bookmarks.insert(uid.clone()) {
                    app.set_notice(format!("Bookmarked: {name}"));
                } else {
                    app.bookmarks.remove(&uid);
                    app.set_notice(format!("Removed bookmark: {name}"));
                }
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
            if let Some(i) = target {
                app.ui_state.set_selected(i);
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
            if let Some(i) = target {
                app.ui_state.set_selected(i);
            }
            Outcome::cont()
        }
        // Cycle the within-group sort, rebuild the list in the new order, and
        // re-resolve the selection BY unique_id (never a raw index across the
        // rebuild). `refilter` re-derives the filtered view if a search is active.
        Action::SortCycle => {
            app.sort = app.sort.next();
            let current = app.selected_unique_id();
            app.model_list = build_model_list(&app.dag, app.sort);
            // Re-derive the persistent-filter view from the re-sorted list (it
            // refilters the open search itself, so the search view inherits
            // the new order too).
            app.apply_list_filter();
            if let Some(uid) = &current {
                app.select_by_unique_id(uid);
            }
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
    }
}

/// The scroll slot of whichever overlay modal is open (`None` in non-modal
/// modes). The ONE place the four scrollable modals are enumerated, shared by
/// the line / page / home / end scroll arms so a new modal can't be wired into
/// some arms and missed in others.
fn modal_scroll_mut(mode: &mut Mode) -> Option<&mut usize> {
    match mode {
        Mode::Help { scroll } => Some(scroll),
        Mode::Detail(dv) => Some(&mut dv.scroll),
        Mode::Sql(sv) => Some(&mut sv.scroll),
        Mode::Stats(sv) => Some(&mut sv.scroll),
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
