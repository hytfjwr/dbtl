use super::*;

// ---- tabs -------------------------------------------------------

#[test]
fn tab_actions_open_dedupe_close_and_cycle() {
    let mut a = app();
    let (_root0, m1, m2) = three_models(&a);
    assert_eq!(
        a.tab_count(),
        2,
        "the app boots with home + one seeded model tab"
    );

    // `O` on the list selection: fresh tab right of the active one, activated.
    a.ui_state.set_selected(1);
    apply_action(&mut a, Action::TabOpen);
    assert_eq!(a.tab_count(), 3);
    assert_eq!(a.active_tab(), 2);
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));

    // `O` on an already-open model dedupes — activates, never appends.
    apply_action(&mut a, Action::TabOpen);
    assert_eq!(a.tab_count(), 3);

    // A different selection grows the strip.
    a.ui_state.set_selected(2);
    apply_action(&mut a, Action::TabOpen);
    assert_eq!(a.tab_count(), 4);
    assert_eq!(a.selected_unique_id().as_deref(), Some(m2.as_str()));

    // `{`/`}` cycle one tab, wrapping at both ends — home (index 0) is a
    // tab like any other in the cycle.
    apply_action(&mut a, Action::TabNext);
    assert_eq!(a.active_tab(), 0);
    assert!(a.home_active(), "the wrap-around lands on home");
    apply_action(&mut a, Action::TabPrev);
    assert_eq!(a.active_tab(), 3);
    assert_eq!(a.selected_unique_id().as_deref(), Some(m2.as_str()));

    // `1`..=`9` activate MODEL tabs by number — `1` is tabs[1] (home is 0);
    // out-of-range is a no-op.
    apply_action(&mut a, Action::TabActivate(1));
    assert_eq!(a.active_tab(), 1);
    apply_action(&mut a, Action::TabActivate(9));
    assert_eq!(a.active_tab(), 1, "position 9 doesn't exist — no-op");

    // `X` on a mid-strip tab: the RIGHT neighbour takes over.
    a.activate_tab(1);
    apply_action(&mut a, Action::TabClose);
    assert_eq!(a.tab_count(), 3);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(m1.as_str()),
        "the right neighbour takes over a mid-strip close"
    );

    // `X` on the LAST tab: the LEFT neighbour takes over.
    apply_action(&mut a, Action::TabActivate(2));
    apply_action(&mut a, Action::TabClose);
    assert_eq!(a.tab_count(), 2);
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));

    // Closing the LAST model tab lands on home (the home landing replaces
    // "the last tab refuses").
    apply_action(&mut a, Action::TabClose);
    assert_eq!(a.tab_count(), 1);
    assert!(a.home_active(), "the last model tab's close lands on home");

    // The home tab itself refuses to close — with a toast.
    apply_action(&mut a, Action::TabClose);
    assert_eq!(a.tab_count(), 1);
    assert_eq!(a.take_notice().as_deref(), Some("can't close the home tab"));
}

#[test]
fn tab_switch_restores_each_tabs_own_state() {
    let mut a = app();
    let (_root0, m1, _m2) = three_models(&a);

    // Give the seeded tab (index 1 — index 0 is home) a distinct view,
    // cursor, history, expansion, anchor, column.
    a.tab_mut().lineage_view.upstream = false;
    a.tab_mut().back = vec![m1.clone()];
    a.tab_mut().forward = vec![_root0.clone()];
    a.tab_mut().collapse_expanded.insert("group-x".to_string());
    a.tab_mut().path_anchor = Some(m1.clone());
    a.tab_mut().lineage_cursor = Some(m1.clone());

    // Opening a second tab gets the FRESH defaults, not tab 0's state.
    a.open_node_as_tab(&m1);
    assert!(a.tab().lineage_view.upstream, "fresh tab: default view");
    assert!(a.tab().back.is_empty() && a.tab().forward.is_empty());
    assert!(a.tab().collapse_expanded.is_empty());
    assert!(a.tab().path_anchor.is_none());
    assert!(a.tab().lineage_cursor.is_none());
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));

    // Switch back: the seeded tab's state is intact.
    a.activate_tab(1);
    assert!(
        !a.tab().lineage_view.upstream,
        "the seeded tab's view survived"
    );
    assert_eq!(a.tab().back, vec![m1.clone()]);
    assert_eq!(a.tab().forward, vec![_root0]);
    assert!(a.tab().collapse_expanded.contains("group-x"));
    assert_eq!(a.tab().path_anchor.as_deref(), Some(m1.as_str()));
    assert_eq!(a.tab().lineage_cursor.as_deref(), Some(m1.as_str()));
    assert!(a.take_tab_switch(), "the switch flags the chokepoint");
    assert!(!a.take_tab_switch(), "the flag is one-shot");
}

#[test]
fn tab_switch_stashes_and_restores_the_lineage_scroll() {
    let mut a = app();
    let (_root0, m1, _m2) = three_models(&a);

    a.ui_state.set_lineage_scroll(40, 7);
    a.open_node_as_tab(&m1);
    assert_eq!(
        a.tabs()[1].scroll,
        Some((40, 7)),
        "the outgoing tab stashed the viewport"
    );
    assert!(a.tab().scroll.is_none(), "a fresh tab has no stash");

    a.ui_state.set_lineage_scroll(3, 3);
    a.activate_tab(1);
    assert_eq!(
        (a.ui_state.lineage_scroll_x(), a.ui_state.lineage_scroll_y()),
        (40, 7),
        "the saved viewport restores verbatim"
    );
    assert_eq!(a.tabs()[2].scroll, Some((3, 3)));
}

#[test]
fn tab_open_on_a_non_model_toasts_instead() {
    let mut a = app();
    // A source uid resolves in the Dag but isn't a list model — `O`/`+`/
    // middle-click can't tab it; the shared gate toasts the resource kind.
    assert!(!a.open_node_as_tab("source.jaffle_finance.dev_lake_jaffle_payment.suppliers"));
    assert_eq!(a.tab_count(), 2);
    assert_eq!(
        a.take_notice().as_deref(),
        Some("can't open a source as a tab")
    );
    // An unknown uid toasts the generic kind.
    assert!(!a.open_node_as_tab("model.jaffle_finance.does_not_exist"));
    assert_eq!(
        a.take_notice().as_deref(),
        Some("can't open a node as a tab")
    );
}

#[test]
fn tab_strip_insertion_places_the_new_tab_right_of_the_active_one() {
    let mut a = app();
    let (root0, m1, m2) = three_models(&a);
    a.open_node_as_tab(&m1);
    a.activate_tab(1);
    a.open_node_as_tab(&m2);
    // [home, root0, m2, m1] — the new tab lands at active+1, not at the end
    // (the home tab at index 0 carries no root — the "" cell).
    let roots: Vec<&str> = a
        .tabs()
        .iter()
        .map(|t| t.root.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(roots, vec!["", root0.as_str(), m2.as_str(), m1.as_str()]);
    assert_eq!(a.active_tab(), 2);
}

#[test]
fn plus_palette_opens_the_model_search_for_a_new_tab() {
    let mut a = app();
    let (_root0, m1, _m2) = three_models(&a);
    // `+` on the strip is the loop's open_tab_palette: Palette mode, `@`
    // pre-seeded, new_tab armed.
    a.open_tab_palette();
    let Mode::Palette(p) = &a.mode else {
        panic!("+ must open the palette")
    };
    assert_eq!(p.query, "@");
    assert!(p.new_tab, "+ searches for a NEW tab's model");
    // Pick a candidate and confirm — the model opens in a new tab.
    let name = m1.rsplit('.').next().unwrap().to_string();
    let cand = a
        .palette_rows(&format!("@{name}"))
        .into_iter()
        .find(|row| matches!(row, crate::PaletteRow::Model { uid, .. } if uid == &m1))
        .expect("m1 is a candidate");
    assert!(matches!(cand, crate::PaletteRow::Model { name: n, .. } if n == name));
    a.mode = Mode::Palette(crate::action::PaletteState {
        query: format!("@{name}"),
        selected: 0,
        new_tab: true,
    });
    apply_action(&mut a, Action::SearchConfirm);
    assert_eq!(a.tab_count(), 3, "the palette pick opened a new tab");
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));
    assert!(matches!(a.mode, Mode::Selection), "the palette closed");
}

#[test]
fn at_query_in_the_plain_palette_still_reroots_the_active_tab() {
    let mut a = app();
    let (root0, m1, _m2) = three_models(&a);
    let name = m1.rsplit('.').next().unwrap();
    // Ctrl-p + `@<name>` Enter — a plain palette search (new_tab off): the
    // ACTIVE tab re-roots, no tab is created.
    a.mode = Mode::Palette(crate::action::PaletteState {
        query: format!("@{name}"),
        selected: 0,
        new_tab: false,
    });
    apply_action(&mut a, Action::SearchConfirm);
    assert_eq!(a.tab_count(), 2, "a re-root never grows the strip");
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));
    assert_eq!(
        a.tab().back,
        vec![root0],
        "the re-root pushed history on the active tab"
    );
}

#[test]
fn wheel_over_the_yank_menu_does_not_close_it() {
    // Review: a wheel scroll dispatched Action::DetailScroll into the yank menu
    // — a no-op scroll, but the one-shot close fired anyway and dropped the
    // menu. The scroll family is exempt now, like YankPick.
    let mut a = sample_app();
    apply_action(&mut a, Action::YankPrefix);
    for act in [
        Action::DetailScroll(Direction::Down),
        Action::DetailScroll(Direction::Up),
        Action::DetailScrollPage(Direction::Down),
        Action::DetailScrollHome,
        Action::DetailScrollEnd,
    ] {
        apply_action(&mut a, act);
        assert!(
            matches!(a.mode, Mode::Yank { .. }),
            "{act:?} must not close the yank menu"
        );
    }
}

#[test]
fn wheel_on_the_tab_strip_scrolls_the_strip_not_the_panes() {
    // The loop's wheel-on-strip arm calls `scroll_tab_bar` — the FIRST-VISIBLE
    // index moves and the active tab doesn't change.
    let mut a = app();
    let (root0, m1, m2) = three_models(&a);
    a.open_node_as_tab(&m1);
    a.open_node_as_tab(&m2);
    a.activate_tab(1);
    assert_eq!(a.selected_unique_id().as_deref(), Some(root0.as_str()));
    // Far right, then clamped back left — the scroll index saturates.
    for _ in 0..10 {
        a.scroll_tab_bar(1);
    }
    let maxed = a.tab_scroll();
    assert!(maxed <= a.tab_count());
    for _ in 0..10 {
        a.scroll_tab_bar(-1);
    }
    assert_eq!(a.tab_scroll(), 0);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(root0.as_str()),
        "scrolling the strip never switches tabs"
    );
}

#[test]
fn reload_drops_tabs_whose_roots_vanished() {
    let mut a = app();
    let (root0, m1, _m2) = three_models(&a);
    a.open_node_as_tab(&m1);
    // Plant a ghost-rooted tab — a root the manifest re-read won't carry
    // (same simulation style as the history/bookmark prune tests), plus a
    // source-rooted one: a surviving-but-non-model root can't re-root either.
    a.restore_tabs(
        vec![
            TabState {
                root: Some(root0.clone()),
                ..TabState::default()
            },
            TabState {
                root: Some(m1.clone()),
                ..TabState::default()
            },
            TabState {
                root: Some("model.jaffle_finance.ghost_root".to_string()),
                ..TabState::default()
            },
            TabState {
                root: Some("source.jaffle_finance.dev_lake_jaffle_payment.suppliers".to_string()),
                ..TabState::default()
            },
        ],
        Some(3),
    );
    assert_eq!(a.tab_count(), 5, "home + the four restored tabs");

    a.reload().expect("reload ok");
    let roots: Vec<&str> = a.tabs().iter().filter_map(|t| t.root.as_deref()).collect();
    assert_eq!(
        roots,
        vec![root0.as_str(), m1.as_str()],
        "the dead-root and non-model tabs dropped; the active index clamped"
    );
    assert_eq!(a.active_tab(), 2);

    // If EVERY model tab's root vanished, home carries the strip alone —
    // landing on the "where do I look next" screen IS the designed answer.
    a.restore_tabs(
        vec![TabState {
            root: Some("model.jaffle_finance.ghost_only".to_string()),
            ..TabState::default()
        }],
        Some(0),
    );
    a.reload().expect("reload ok");
    assert_eq!(a.tab_count(), 1, "home alone keeps the >=1-tab invariant");
    assert!(a.home_active(), "the lone survivor is the home tab");
}

#[test]
fn reload_follows_the_active_tab_across_left_of_active_prunes() {
    // Review: `retain` slides survivors left — a dead tab BELOW the active
    // one shifts the live tab's index. `active` must track it (the
    // `close_tab` rule), not just clamp into range.
    let mut a = app();
    let (root0, m1, _m2) = three_models(&a);
    // [dead, root0, m1] with root0 live at index 1.
    a.restore_tabs(
        vec![
            TabState {
                root: Some("model.jaffle_finance.ghost_root".to_string()),
                ..TabState::default()
            },
            TabState {
                root: Some(root0.clone()),
                ..TabState::default()
            },
            TabState {
                root: Some(m1.clone()),
                ..TabState::default()
            },
        ],
        Some(1),
    );
    a.reload().expect("reload ok");
    assert_eq!(a.active_tab(), 1, "root0 slid into index 1 (home is 0)");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(root0.as_str()),
        "the SAME tab stays active — not the one that slid under the index"
    );
}

#[test]
fn reload_prunes_non_model_ids_from_tab_history() {
    // A back/forward entry always names a model ROOT — a uid that survives
    // the reload as a NON-model (the dag keeps it) can never be a landing:
    // the prune uses `is_rootable`, not mere survival.
    let mut a = app();
    let (root0, m1, _m2) = three_models(&a);
    let source = "source.jaffle_finance.dev_lake_jaffle_payment.suppliers".to_string();
    assert!(
        a.dag.get(&source).is_some(),
        "the planted id survives reload"
    );
    a.restore_tabs(
        vec![TabState {
            root: Some(root0.clone()),
            back: vec![source.clone(), m1.clone()],
            forward: vec![source.clone(), m1.clone()],
            ..TabState::default()
        }],
        Some(0),
    );
    a.reload().expect("reload ok");
    assert_eq!(
        a.tab().back,
        vec![m1.clone()],
        "the surviving-but-non-model id is pruned from back"
    );
    assert_eq!(a.tab().forward, vec![m1], "same for forward");
}

#[test]
fn tabs_beyond_nine_open_unnumbered_and_off_the_digit_keys() {
    // No cap — positions 10+ draw without a `{n} ` prefix and sit outside
    // the `1`..=`9` key map (the BINDINGS rows stop at 9).
    let mut a = app();
    for i in 1..12 {
        let uid = a.active_list().model_at(i).unwrap().unique_id.clone();
        a.open_node_as_tab(&uid);
    }
    assert_eq!(a.tab_count(), 13, "home + the seeded tab + 11 opens");
    let labels = a.tab_labels();
    assert_eq!(
        labels[0], "\u{2302}",
        "index 0 is the home glyph, unnumbered"
    );
    assert!(labels[1].starts_with("1 "), "position 1 is numbered");
    assert!(labels[9].starts_with("9 "), "position 9 is numbered");
    assert!(
        !labels[10].starts_with("10 ") && !labels[10].starts_with("0 "),
        "position 10 carries no number: {:?}",
        labels[10]
    );
    // The active tab is position 13 — `9` activates the tab numbered 9
    // (index 9 — the label and the digit share the strip index).
    apply_action(&mut a, Action::TabActivate(9));
    assert_eq!(a.active_tab(), 9);
}

#[test]
fn selecting_in_the_list_re_roots_only_the_active_tab() {
    // The rule: picking a model in the list changes only the active tab's
    // root — the other tabs' roots are untouched by list navigation.
    let mut a = app();
    let (_root0, m1, m2) = three_models(&a);
    a.open_node_as_tab(&m1); // tab 2 roots m1
    a.activate_tab(1); // back to the seeded tab
                       // Move the list selection onto m2 (the production click arm's
                       // `reroot_select`; `j`/wheel adopt through the same select).
    a.reroot_select(&m2);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(m2.as_str()),
        "the active tab re-rooted"
    );
    assert_eq!(
        a.tabs()[2].root.as_deref(),
        Some(m1.as_str()),
        "the other tab's root is untouched"
    );
    // Switching to tab 2 shows ITS root — not the selection tab 1 made.
    a.activate_tab(2);
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));
}
