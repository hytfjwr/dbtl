//! [`apply_action`] — the size-unaware domain reducer: the exhaustive
//! `Action` → state transition table, plus its small shared arms
//! (yank-with-notice, the modal scroll slot, the wrap-around list cycle).

use crate::action::{
    palette_candidates, Action, DetailView, Direction, Mode, PaletteState, SearchState,
    SearchTarget, SqlView,
};
use crate::effect::Effect;
use crate::{build_model_list_grouped, reduce_selection, Focus, LineageLens};

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
    let in_yank_menu = matches!(app.mode, Mode::Yank);
    let outcome = match action {
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
            // A `+M more` / `-M less` marker under the cursor TOGGLES its
            // sibling group instead of opening the structure modal (the marker
            // is chrome, not a Dag node — there is nothing to detail).
            if let Some(group) = app.collapse_group(&uid) {
                app.toggle_collapse(&group.key);
                return Outcome::cont();
            }
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
            let catalog = app.catalog_note(&uid, &detail);
            // Column-level lineage snapshot (lazy + memoized in the resolver;
            // resolving THIS node + its direct children is the only cost).
            // `col_names` is the cursor-addressable list — declared columns,
            // else the resolver's inferred names so an undocumented model
            // still gets a cursor + counts.
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
            app.mode = Mode::Detail(DetailView {
                model_id: uid,
                name,
                detail,
                tests,
                downstream_count,
                upstream_count,
                catalog,
                col_names,
                col_index,
                col_counts,
                stale,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::DetailClose => {
            app.mode = Mode::Selection;
            Outcome::cont()
        }
        // Open the yank-prefix which-key (`y` + key). The menu itself is
        // rendered from the M::Yank BINDINGS rows; the one-shot close lives in
        // the post-match step at the bottom of this function. Over the Detail
        // modal BOTH the modal's node and its column target are stashed — the
        // which-key closes to Selection after any key, so the modal context
        // must be captured BEFORE the mode swap, or every `y<key>` yank would
        // silently act on the list selection instead of the modal's node.
        // (The `y` row exists in Mode::Detail's bindings.)
        Action::YankPrefix => {
            match &app.mode {
                Mode::Detail(dv) => {
                    app.pending_node = Some(dv.model_id.clone());
                    app.pending_col = if dv.col_index != usize::MAX {
                        dv.col_names
                            .get(dv.col_index)
                            .map(|c| crate::ColumnRef::new(&dv.model_id, c))
                    } else {
                        None
                    };
                }
                _ => {
                    app.pending_node = None;
                    app.pending_col = None;
                }
            }
            app.mode = Mode::Yank;
            Outcome::cont()
        }
        // The structure modal's column cursor (h/l): a SELECTION, not a scroll —
        // the loop's per-frame `clamp_detail_scroll` does the ensure-visible.
        // (`Direction::Up`/`Down` here mean previous/next ROW of the column
        // list — the enum is reused for a vertical cursor, not for the
        // lineage's upstream/downstream axes.)
        Action::DetailColumnMove(dir) => {
            if let Mode::Detail(dv) = &mut app.mode {
                if dv.col_index != usize::MAX && !dv.col_names.is_empty() {
                    let last = dv.col_names.len() - 1;
                    dv.col_index = match dir {
                        Direction::Up | Direction::Left => dv.col_index.saturating_sub(1),
                        _ => (dv.col_index + 1).min(last),
                    };
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
            // Enter/`c` on the structure modal's column cursor commits the
            // column to the trace view; a column-less modal (or an invalid
            // cursor) keeps Enter's classic close behaviour.
            let col = match &app.mode {
                Mode::Detail(dv) if dv.col_index != usize::MAX => dv
                    .col_names
                    .get(dv.col_index)
                    .map(|c| crate::ColumnRef::new(&dv.model_id, c)),
                _ => None,
            };
            app.mode = Mode::Selection;
            if let Some(col) = col {
                app.column_focus_enter(col);
            }
            Outcome::cont()
        }
        Action::ColumnFocusExit => {
            // Esc exits the trace view — the lineage returns to the normal
            // rooted subgraph of whatever is selected.
            app.column_focus = None;
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
                _ => app.column_focus.as_ref().map(|f| f.col.clone()),
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
                    app.mode = Mode::Selection;
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
                _ => {}
            }
            Outcome::cont()
        }
        Action::SearchBackspace => {
            match &mut app.mode {
                Mode::Search(s) => {
                    s.query.pop();
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
                        // Cycling to a hidden match expands its group — the
                        // anchor needs the match drawn to land on it.
                        app.reveal_current_lineage_match();
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
                .or_else(|| app.column_focus.as_ref().map(|f| f.col.clone()))
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
        // modal. A blank command (nothing buildable changed) notices instead of
        // silently clobbering the clipboard. The `&app.mode` borrow is released
        // (the command is cloned) before the mutating notice/yank.
        Action::YankPrSelector => {
            let cmd = match &app.mode {
                Mode::Diff(dv) => Some(dv.pr.ci_command.clone()),
                _ => None,
            };
            match cmd {
                Some(Some(c)) => yank_with_notice(app, Some(c), "Copied dbt build command"),
                Some(None) => {
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
            // A cycle landing on a different model is a re-root — it ends the
            // column trace like jump_to does (`reroot_select` owns that rule).
            if let Some(uid) = target
                .and_then(|i| app.active_list().model_at(i))
                .map(|m| m.unique_id.clone())
            {
                app.reroot_select(&uid);
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
                app.reroot_select(&uid);
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
        // `p` two-node path: first press anchors the path start at the
        // selection (the highlight then runs anchor↔cursor), second press
        // clears it back to root↔cursor. Pure state + a toast; the
        // path-set BFS reads `path_anchor` next frame — no cache reset needed
        // (styles are recomputed per draw anyway).
        Action::PathToggle => {
            if let Some(uid) = app.path_anchor.take() {
                let name = app.node_name_or_uid(&uid);
                app.set_notice(format!("path anchor cleared ({name})"));
            } else if let Some(uid) = app.selected_unique_id() {
                let name = app.node_name_or_uid(&uid);
                app.path_anchor = Some(uid);
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
    if in_yank_menu && matches!(app.mode, Mode::Yank) {
        app.mode = Mode::Selection;
    }
    // The yank stashes live only while the which-key is open: once the mode is
    // anything else — after a yank, or an Esc/q cancel — the modal context is
    // dropped, so a stale `pending_node`/`pending_col` can never leak into a
    // later yank (both are also rewritten on every `y` press; this just closes
    // the stale-read window after a cancel).
    if !matches!(app.mode, Mode::Yank) {
        app.pending_col = None;
        app.pending_node = None;
    }
    outcome
}

/// The scroll slot of whichever overlay modal is open (`None` in non-modal
/// modes). The ONE place the five scrollable modals are enumerated, shared by
/// the line / page / home / end scroll arms so a new modal can't be wired into
/// some arms and missed in others.
fn modal_scroll_mut(mode: &mut Mode) -> Option<&mut usize> {
    match mode {
        Mode::Help { scroll } => Some(scroll),
        Mode::Detail(dv) => Some(&mut dv.scroll),
        Mode::Sql(sv) => Some(&mut sv.scroll),
        Mode::Stats(sv) => Some(&mut sv.scroll),
        Mode::Diff(dv) => Some(&mut dv.scroll),
        Mode::Columns(cv) => Some(&mut cv.scroll),
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
