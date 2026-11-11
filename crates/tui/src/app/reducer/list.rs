use super::*;

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

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
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
        Action::ToggleUntestedFilter | Action::ToggleBookmarkFilter | Action::ToggleLintFilter => {
            let target = match action {
                Action::ToggleUntestedFilter => ListFilter::Untested,
                Action::ToggleBookmarkFilter => ListFilter::Bookmarked,
                _ => ListFilter::Linted,
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
        // (the fold state is the persisted `folded_groups` set).
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
            // A cycle landing on a different model is a re-root — the move makes
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
        // `n`/`N` cycles the flagged-model set: the `coverage_gap` members
        // under every lens — except the Lint lens, where the same keys cycle
        // the linted models instead (the lens on screen IS the cycle's set,
        // so the help stays honest). Wraparound and filtered-scope both come
        // from the shared `cycle_to`.
        Action::GapNext | Action::GapPrev => {
            let forward = matches!(action, Action::GapNext);
            let target = if app.ui_state.lens() == crate::LineageLens::Lint {
                let linted: std::collections::HashSet<&str> = app
                    .lint_findings()
                    .iter()
                    .map(|f| f.node_uid.as_str())
                    .collect();
                cycle_to(app.active_list(), app.ui_state.selected(), forward, |m| {
                    linted.contains(m.unique_id.as_str())
                })
            } else {
                cycle_to(
                    app.active_list(),
                    app.ui_state.selected(),
                    forward,
                    crate::coverage_gap,
                )
            };
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
        // the UiState-only legacy list-movement arms.
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
