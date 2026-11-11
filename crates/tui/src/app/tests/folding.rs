use super::*;

// ---- Issue 11: group folding (list pipeline stage) -------------------------

#[test]
fn dash_binding_dispatches_toggle_group_fold() {
    // `-` in Selection mode must resolve to the fold action — the keyboard
    // twin of the group-header click.
    use crate::action::dispatch;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    assert_eq!(
        dispatch(
            &Mode::Selection,
            KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE)
        ),
        Some(Action::ToggleGroupFold),
        "the `-` row in BINDINGS routes to ToggleGroupFold"
    );
}

#[test]
fn fold_removes_models_but_keeps_the_header_and_full_count() {
    let mut a = app();
    // Selection starts on staging's first model (index 0): `-` folds staging.
    apply_action(&mut a, Action::ToggleGroupFold);
    assert!(
        a.folded_groups.contains("staging"),
        "the fold state records the group key"
    );
    let list = a.active_list();
    // 45 - 11 staging models = 34 selectable models; the group still emits
    // its header (count = FULL membership, not the visible 0).
    assert_eq!(list.len(), 34, "folded models leave selection space");
    assert!(
        list.models.iter().all(|m| !m.name.starts_with("stg_")),
        "no staging model is selectable"
    );
    match &list.rows[0] {
        DisplayRow::Header { layer, count } => {
            assert_eq!(layer, "staging");
            assert_eq!(*count, 11, "a folded header reports FULL membership");
        }
        other => panic!("row 0 must stay the staging header, got {other:?}"),
    }
    // Display-row space shrank by the folded members (headers survive).
    assert_eq!(list.row_count(), 45 + 4 - 11, "rows = models + headers");
}

#[test]
fn fold_hiding_the_selection_moves_the_cursor_to_the_next_visible_model() {
    let mut a = app();
    // Park the selection mid-staging (index 5), then fold: the cursor's model
    // leaves the list, so the cursor lands where staging's models vacated —
    // the next group's first model (intermediate index 0 post-fold). The
    // ROOT stays on the hidden model (the §2 detach — same rule as a
    // filtered-out root).
    let hidden = a.active_list().models[5].unique_id.clone();
    a.select_by_unique_id(&hidden);
    apply_action(&mut a, Action::ToggleGroupFold);
    let cursor_uid = a.list_sel_uid().expect("a survivor is selected");
    assert_ne!(
        Some(cursor_uid.as_str()),
        Some(hidden.as_str()),
        "the hidden model can't hold the cursor"
    );
    assert_eq!(
        a.active_list().models[a.ui_state.selected()].name,
        "int_delivery_lanes__combined",
        "the cursor re-seats on the next group's first model"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(hidden.as_str()),
        "the root detaches — the lineage keeps the folded model"
    );
}

#[test]
fn unfold_restores_the_groups_models() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleGroupFold);
    assert_eq!(a.active_list().len(), 34);
    // Unfold staging through the same key-path (the header click's twin).
    a.toggle_group_fold("staging");
    assert!(!a.folded_groups.contains("staging"), "re-toggle unfolds");
    assert_eq!(a.active_list().len(), 45, "staging's models return");
    assert!(
        a.active_list()
            .models
            .iter()
            .any(|m| m.name == "stg_finance__budgets"),
        "staging models are selectable again"
    );
}

#[test]
fn list_search_ignores_folds_and_restores_them_on_close() {
    let mut a = app();
    a.toggle_group_fold("staging");
    assert_eq!(a.active_list().len(), 34);
    // Search: the fold is ignored — a staging member still matches and is
    // selectable while the query narrows.
    apply_action(&mut a, Action::SearchOpen);
    for c in "stg_finance__fiscal".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    assert!(
        a.active_list()
            .models
            .iter()
            .any(|m| m.name == "stg_finance__fiscal_years"),
        "a folded group's member still surfaces in search"
    );
    assert!(
        a.select_by_unique_id("model.jaffle_finance.stg_finance__fiscal_years"),
        "a folded member is selectable while searching"
    );
    // Closing the search restores the fold view.
    apply_action(&mut a, Action::SearchCancel);
    assert_eq!(
        a.active_list().len(),
        34,
        "the fold view restores when search closes"
    );
    assert!(a.folded_groups.contains("staging"));
}

#[test]
fn search_confirm_inside_a_folded_group_unfolds_just_that_group() {
    let mut a = app();
    a.toggle_group_fold("staging");
    a.toggle_group_fold("marts");
    // Search surfaces the folded staging member; confirming it lands the
    // pick — the explicit selection beats the fold pref: STAGING unfolds,
    // the unrelated marts fold survives.
    apply_action(&mut a, Action::SearchOpen);
    for c in "stg_finance__budgets".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    apply_action(&mut a, Action::SearchConfirm);
    assert!(
        !a.folded_groups.contains("staging"),
        "the confirmed match's group unfolded"
    );
    assert!(
        a.folded_groups.contains("marts"),
        "the fold the user did NOT pick stays folded"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.jaffle_finance.stg_finance__budgets"),
        "the confirmed match is rooted"
    );
}

#[test]
fn search_cancel_keeps_the_origins_fold() {
    let mut a = app();
    a.toggle_group_fold("staging");
    // Root is inside folded staging (detached). A search CANCEL must put
    // everything back — never unfold the origin's group.
    apply_action(&mut a, Action::SearchOpen);
    for c in "zz_nomatch".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    apply_action(&mut a, Action::SearchCancel);
    assert!(
        a.folded_groups.contains("staging"),
        "cancel mutates no fold state"
    );
}

#[test]
fn header_click_toggles_the_fold_not_the_selection() {
    let mut a = app();
    a.click_target(
        &HitTarget::ListHeader("marts".to_string()),
        MouseButton::Left,
        (5, 5),
    );
    assert!(
        a.folded_groups.contains("marts"),
        "a header click folds its group"
    );
    assert!(
        a.active_list().models.iter().all(|m| {
            let g = a
                .active_list()
                .group_of_model(
                    a.active_list()
                        .models
                        .iter()
                        .position(|n| n.unique_id == m.unique_id)
                        .unwrap(),
                )
                .map(|(k, _)| k != "marts")
                .unwrap_or(true);
            g
        }),
        "marts members left the list"
    );
    // A second click on the (still present) header unfolds.
    a.click_target(
        &HitTarget::ListHeader("marts".to_string()),
        MouseButton::Left,
        (5, 5),
    );
    assert!(!a.folded_groups.contains("marts"), "re-click unfolds");
    assert_eq!(a.active_list().len(), 45);
}

#[test]
fn folding_every_group_empties_the_list_and_names_the_fold() {
    let mut a = app();
    for layer in ["staging", "intermediate", "marts", "utilities"] {
        a.toggle_group_fold(layer);
    }
    assert!(a.active_list().is_empty(), "every group folded → no models");
    // The headers survive — row space is all headers now.
    assert_eq!(a.active_list().row_count(), 4);
    assert_eq!(
        a.list_empty_reason().as_deref(),
        Some("all groups folded — press - to expand"),
        "the stamp names the fold, not a phantom filter"
    );
}

#[test]
fn apply_prefs_restores_folded_groups_and_prunes_dead_keys() {
    let mut a = app();
    let prefs = crate::persist::PersistedState {
        folded_groups: ["staging", "no_such_layer"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };
    a.apply_prefs(&prefs);
    assert!(
        a.folded_groups.contains("staging"),
        "a live fold key restores"
    );
    assert!(
        !a.folded_groups.contains("no_such_layer"),
        "a stale fold key is pruned (no phantom fold)"
    );
    assert_eq!(a.active_list().len(), 34);
}

#[test]
fn restored_all_fold_then_dash_reopens_the_roots_group() {
    // Persisted all-groups-folded restore, then `-`: the cursor has no model
    // under it (the list is empty), so `-` falls back to the ROOT's group —
    // the only `-` path that can re-OPEN a fold (§10's 畳む / 開く).
    let mut a = app();
    let prefs = crate::persist::PersistedState {
        folded_groups: ["staging", "intermediate", "marts", "utilities"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        last_selected: Some("model.jaffle_finance.pos_txn".to_string()),
        tabs: vec![crate::persist::PersistedTab {
            root: "model.jaffle_finance.pos_txn".to_string(),
            view: LineageView::default(),
        }],
        active_tab: Some(0),
        ..Default::default()
    };
    a.apply_prefs(&prefs);
    assert_eq!(a.folded_groups.len(), 4, "all folds restored");
    assert_eq!(a.active_list().len(), 0, "folded view is empty");
    apply_action(&mut a, Action::ToggleGroupFold);
    assert!(
        !a.folded_groups.contains("utilities"),
        "`-` re-opened the root's folded group"
    );
    assert_eq!(a.folded_groups.len(), 3);
    assert_eq!(
        a.list_sel_uid().as_deref(),
        Some("model.jaffle_finance.pos_txn"),
        "the cursor re-seats on the now-visible root"
    );
}

#[test]
fn fold_survives_the_untested_filter_toggle() {
    // sample_manifest (not the `app()` fixture — all its models are tested):
    // staging holds 1 untested model (stg_two_configs), marts holds 4.
    let mut a = sample_app();
    let group = "staging";
    let hidden_uids: Vec<String> = a
        .model_list
        .models
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            a.model_list
                .group_of_model(*i)
                .is_some_and(|(k, _)| k == group)
        })
        .filter(|(_, m)| coverage_gap(m))
        .map(|(_, m)| m.unique_id.clone())
        .collect();
    assert_eq!(
        hidden_uids.len(),
        1,
        "the premise — staging holds exactly one untested model"
    );
    // Root on a Marts untested model (survives T, outside the fold); park the
    // cursor in staging and fold it.
    a.select_by_unique_id("model.sample.dim_customers");
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::ToggleGroupFold);
    assert!(a.folded_groups.contains(group), "staging is folded");

    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::Untested);
    assert!(
        a.folded_groups.contains(group),
        "the fold key survives the filter rebuild"
    );
    assert!(
        a.active_list().models.iter().all(coverage_gap),
        "every visible model is untested"
    );
    assert_eq!(
        a.active_list().len(),
        5 - hidden_uids.len(),
        "the folded group's untested members stay hidden under T"
    );
    for uid in &hidden_uids {
        assert!(
            a.active_list().models.iter().all(|m| &m.unique_id != uid),
            "{uid} is folded out of the filtered view"
        );
    }
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.dim_customers"),
        "the root re-resolves by uid inside the filtered+folded view"
    );
    // Toggling T off restores the full view — WITH the fold still applied.
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::All);
    assert!(a.folded_groups.contains(group), "still folded after T off");
    assert!(
        a.active_list()
            .models
            .iter()
            .all(|m| !hidden_uids.contains(&m.unique_id)),
        "the fold still applies to the restored full view"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.dim_customers"),
        "the root is still dim_customers by uid"
    );
}

#[test]
fn fold_survives_the_bookmark_filter_and_re_resolves_by_uid() {
    let mut a = app();
    // Bookmark a staging model, then fold staging: under `*` the only
    // candidate lives inside the fold — the narrowed view is empty, and the
    // folded header still reports its (filtered) membership.
    let kept = "model.jaffle_finance.stg_payment__shoppers";
    a.select_by_unique_id(kept);
    apply_action(&mut a, Action::BookmarkToggle);
    apply_action(&mut a, Action::ToggleGroupFold);
    assert!(a.folded_groups.contains("staging"), "staging folded");

    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert_eq!(a.list_filter, ListFilter::Bookmarked);
    assert!(
        a.folded_groups.contains("staging"),
        "the fold key survives the `*` rebuild"
    );
    assert_eq!(
        a.active_list().len(),
        0,
        "the sole bookmark is folded out of selection space"
    );
    match &a.active_list().rows[0] {
        DisplayRow::Header { layer, count } => {
            assert_eq!(layer, "staging");
            assert_eq!(
                *count, 1,
                "the header reports the folded members of the bookmarked base"
            );
        }
        other => panic!("the staging header survives, got {other:?}"),
    }
    // Unfold while `*` is live: the bookmarked model surfaces, and the
    // detached root re-resolves onto it by uid.
    a.toggle_group_fold("staging");
    assert_eq!(a.active_list().len(), 1, "the bookmark reappears");
    assert_eq!(
        a.list_sel_uid().as_deref(),
        Some(kept),
        "the cursor lands on the only (bookmarked) model"
    );
}

#[test]
fn fold_survives_sort_cycle_and_selection_re_resolves_by_uid() {
    let mut a = app();
    // Root on a stable marts model; park the CURSOR in staging (index 0) and
    // fold it. Cycling every sort rebuilds `model_list` + both view caches —
    // the fold key must keep hiding the group in the new order, and the root
    // must re-resolve by uid each time.
    a.select_by_unique_id(FCT);
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::ToggleGroupFold);
    assert!(a.folded_groups.contains("staging"), "staging folded");
    for expected in [SortMode::Downstream, SortMode::Tests, SortMode::Layer] {
        apply_action(&mut a, Action::SortCycle);
        assert_eq!(a.sort, expected);
        assert!(
            a.folded_groups.contains("staging"),
            "the fold key survives {expected:?}"
        );
        assert!(
            a.active_list()
                .models
                .iter()
                .all(|m| !m.name.starts_with("stg_")),
            "staging stays hidden in the {expected:?} order"
        );
        assert!(
            a.active_list()
                .rows
                .iter()
                .any(|r| matches!(r, DisplayRow::Header { layer, .. } if layer == "staging")),
            "the folded staging header still draws in {expected:?}"
        );
        assert_eq!(
            a.selected_unique_id().as_deref(),
            Some(FCT),
            "the root re-resolves by uid under {expected:?}"
        );
    }
}

#[test]
fn folded_model_still_opens_as_a_tab() {
    let mut a = app();
    // Fold staging, then open a staging model as a tab: the model is folded
    // out of SELECTION space but still exists in the dag (is_rootable) —
    // middle-click / `O` must open it, not toast `can't open a model`.
    // (`stg_payment__shoppers`, not the boot root `stg_finance__budgets` —
    // an already-open model dedupes into its own tab.)
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::ToggleGroupFold);
    assert!(a.folded_groups.contains("staging"));
    let uid = "model.jaffle_finance.stg_payment__shoppers";
    assert!(
        a.active_list().models.iter().all(|m| m.unique_id != uid),
        "folded out of selection space"
    );
    assert!(
        a.open_node_as_tab(uid),
        "a folded model opens as a tab — it renders in the lineage"
    );
    assert_eq!(a.tab_count(), 3);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(uid),
        "the new tab roots on the folded model (the §2 detach)"
    );
    // A genuinely non-model node still refuses: the toast names its kind.
    let source_uid = a
        .dag
        .nodes()
        .values()
        .find(|n| n.resource_type == "source")
        .map(|n| n.unique_id.clone())
        .expect("the fixture has a source");
    assert!(
        !a.open_node_as_tab(&source_uid),
        "a source still can't anchor a tab"
    );
}
