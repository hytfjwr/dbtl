use super::*;

// ---- lineage cursor: spatial movement, focus routing, Enter commit ----

#[test]
fn lineage_cursor_moves_spatially_under_right_pane_focus() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    let lay = crate::layout(&a.lineage_subgraph());
    let root_col = lay.columns[FCT];
    assert!(root_col > 0, "fct has upstream columns");

    // h: exactly one column upstream per keypress, down to column 0.
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&cur], root_col - 1, "h = one column left");
    for expect in (0..root_col - 1).rev() {
        apply_action(&mut a, Action::MoveLeft);
        let c = a.lineage_cursor_uid().unwrap();
        assert_eq!(lay.columns[&c], expect, "each h steps one column");
    }
    let edge = a.lineage_cursor_uid().unwrap();
    apply_action(&mut a, Action::MoveLeft);
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(edge.as_str()),
        "h at the most-upstream column is a no-op"
    );

    // j/k: step through column 0's stack (sources/seeds — always ≥2 for
    // fct) and round-trip.
    let mut col: Vec<(String, usize)> = lay
        .rects
        .iter()
        .filter(|(uid, _)| lay.columns[uid.as_str()] == 0)
        .map(|(uid, r)| (uid.clone(), r.y))
        .collect();
    col.sort_by_key(|(_, y)| *y);
    assert!(col.len() >= 2, "fixture: column 0 stacks nodes");
    let i = col.iter().position(|(uid, _)| *uid == edge).unwrap();
    if i + 1 < col.len() {
        apply_action(&mut a, Action::MoveDown);
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(col[i + 1].0.as_str()),
            "j = next node down the column"
        );
        apply_action(&mut a, Action::MoveUp);
    } else {
        apply_action(&mut a, Action::MoveUp);
        assert_eq!(
            a.lineage_cursor_uid().as_deref(),
            Some(col[i - 1].0.as_str()),
            "k = previous node up the column"
        );
        apply_action(&mut a, Action::MoveDown);
    }
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(edge.as_str()),
        "j/k round-trips"
    );

    // l: one column back toward downstream.
    apply_action(&mut a, Action::MoveRight);
    let back = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&back], 1, "l = one column right");

    // The rooted selection never moved while the cursor walked.
    assert_eq!(a.selected_unique_id().as_deref(), Some(FCT));
}

#[test]
fn lineage_cursor_routing_display_subgraph_and_reset() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    // List focus: movement keys drive the LIST selection; the cursor stays home.
    let before = a.ui_state.selected();
    apply_action(&mut a, Action::MoveDown);
    assert_eq!(
        a.ui_state.selected(),
        before + 1,
        "list focus moves the list"
    );
    assert_eq!(
        a.lineage_cursor_uid(),
        a.selected_unique_id(),
        "cursor home = the selection"
    );

    // Lineage focus: the cursor walks and the DISPLAY subgraph re-selects it…
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur.as_str(), FCT, "the cursor left the root");
    assert_eq!(
        a.lineage_display_subgraph().selected,
        cur,
        "display subgraph selects (→ emphasizes, anchors) the cursor"
    );
    // …while ROOT semantics (exports, matches, title) stay on the selection.
    assert_eq!(a.lineage_subgraph().selected, FCT);

    // z (recenter) sends the cursor home.
    apply_action(&mut a, Action::Recenter);
    assert_eq!(a.lineage_cursor_uid().as_deref(), Some(FCT));
}

#[test]
fn toggle_list_pane_pins_focus_and_movement_drives_the_lineage_cursor() {
    let mut a = app();
    a.select_by_unique_id(FCT);

    // Hide the list: focus is pinned to the lineage pane…
    apply_action(&mut a, Action::ToggleListPane);
    assert!(!a.ui_state.list_visible());
    assert_eq!(a.ui_state.focus(), Focus::RightPane);
    // …so the movement keys route to the lineage CURSOR, never the hidden
    // list (the selection — the lineage root — must not move).
    apply_action(&mut a, Action::MoveLeft);
    assert_ne!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "h walks the cursor upstream while the list is hidden"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "the rooted selection stays put"
    );

    // Show it again: the list pane takes focus back.
    apply_action(&mut a, Action::ToggleListPane);
    assert!(a.ui_state.list_visible());
    assert_eq!(a.ui_state.focus(), Focus::List);
}

#[test]
fn stale_lineage_cursor_falls_back_to_the_root() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft); // cursor = an upstream node
    let cur = a.lineage_cursor_uid().unwrap();
    assert_ne!(cur.as_str(), FCT);
    // Dropping the upstream side removes the cursor's node from the subgraph.
    apply_action(&mut a, Action::ToggleUpstream);
    assert!(
        !a.lineage_subgraph().contains(&cur),
        "precondition: the cursor's node was dropped from the view"
    );
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "a stale cursor falls back to the root"
    );
    assert_eq!(a.lineage_display_subgraph().selected, FCT);
}

#[test]
fn reload_sends_the_cursor_home() {
    // Reload restores the selection BY ID, so the loop's selection-change
    // chokepoint never fires — reload itself must re-home the cursor or a
    // pre-reload cursor would survive into the rebuilt graph.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    assert_ne!(a.lineage_cursor_uid().as_deref(), Some(FCT));
    a.reload().expect("reload ok");
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "reload re-homes the cursor to the (restored) root"
    );
}

#[test]
fn click_reroots_models_moves_cursor_for_sources_and_rehomes_on_root() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    // Click an upstream MODEL: re-root + cursor home.
    let up_model = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("model."))
        .min()
        .cloned()
        .unwrap();
    node_commit(&mut a, &up_model);
    assert_eq!(a.selected_unique_id().as_deref(), Some(up_model.as_str()));
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(up_model.as_str()),
        "cursor is home on the new root"
    );

    // Click a SOURCE: no re-root, the CURSOR moves there.
    a.select_by_unique_id(FCT);
    let src = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("source."))
        .min()
        .cloned()
        .unwrap();
    node_click(&mut a, &src);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "clicking a source never re-roots"
    );
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(src.as_str()),
        "the cursor moved to the clicked source"
    );

    // Click the CURRENT ROOT: the cursor homes onto it (the same uid —
    // `lineage_cursor_uid` reads the root whether parked or `None`).
    node_click(&mut a, FCT);
    assert_eq!(a.selected_unique_id().as_deref(), Some(FCT));
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "clicking the root re-homes the cursor"
    );
}

#[test]
fn enter_commits_cursor_reroot_for_models_structure_for_sources() {
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);

    // (a) cursor == root: Enter opens the root's structure (unchanged).
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.model_id, FCT),
        m => panic!("expected Detail for the root, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);

    // (b) cursor on an upstream MODEL: Enter re-roots (same as a click),
    // recording history.
    let up_model = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("model."))
        .min()
        .cloned()
        .expect("fct has an upstream model");
    a.tab_mut().lineage_cursor = Some(up_model.clone());
    apply_action(&mut a, Action::DetailOpen);
    assert!(
        matches!(a.mode, Mode::Selection),
        "a re-root stays in Selection mode"
    );
    assert_eq!(a.selected_unique_id().as_deref(), Some(up_model.as_str()));
    a.history_back();
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "the Enter re-root recorded history (b returns)"
    );

    // (c) cursor on a SOURCE (not list-selectable): Enter opens ITS
    // structure modal instead, and the root stays put.
    a.select_by_unique_id(FCT);
    let src = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("source."))
        .min()
        .cloned()
        .expect("fct has an upstream source");
    a.tab_mut().lineage_cursor = Some(src.clone());
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.model_id, src);
            assert_eq!(
                Some(dv.name.as_str()),
                a.dag.get(&src).map(|n| n.name.as_str()),
                "the modal is titled with the source's name"
            );
        }
        m => panic!("expected Detail for the source, got {m:?}"),
    }
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "a non-model cursor never re-roots"
    );
}

#[test]
fn detail_open_snapshots_payload_from_side_maps() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::DetailOpen);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.model_id, "model.jaffle_finance.pos_txn");
            assert_eq!(dv.name, "pos_txn");
            assert_eq!(dv.detail.materialized.as_deref(), Some("table"));
            assert!(
                !dv.detail.columns.is_empty(),
                "columns came from the Dag side map"
            );
        }
        _ => panic!("expected Detail mode"),
    }
    apply_action(&mut a, Action::DetailClose);
    assert!(
        matches!(a.mode, Mode::Selection),
        "Esc/close returns to Selection"
    );
}
