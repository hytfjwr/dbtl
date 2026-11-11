use super::*;

// ---- node context menu + nested overlay returns -------------------
//
// `a` (or a right-click — `App::click_target` covers that path above) opens the
// BINDINGS-derived context menu for the node under the cursor / modal; the menu
// and the `y` which-key stack over a modal and restore it on close.

#[test]
fn menu_open_from_selection_targets_the_focus_node() {
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.stg_orders");
    apply_action(&mut a, Action::MenuOpen);
    let ms = menu_state(&a);
    assert_eq!(ms.target, "model.sample.stg_orders");
    assert_eq!(ms.anchor, crate::action::MenuAnchor::Node);
    assert!(ms.back.is_none(), "nothing stacked under a Selection menu");
    // The first selectable row is highlighted on open.
    assert!(matches!(
        ms.items[ms.selected],
        crate::action::MenuEntry::Item { .. }
    ));
}

#[test]
fn menu_open_over_detail_and_sql_targets_the_modal_node() {
    // List selection sits on B; the modal shows A. The `a` menu acts on A —
    // the same hostile case the yank-stash tests cover.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::MenuOpen);
    let ms = menu_state(&a);
    assert_eq!(
        ms.target, "model.sample.stg_orders",
        "the menu acts on the modal's node, not the list selection"
    );
    assert!(
        matches!(ms.back.as_deref(), Some(Mode::Detail(_))),
        "the structure modal rides in `back`"
    );
    // Esc closes the menu and restores the stacked modal — the
    // nested-overlay return rule.
    apply_action(&mut a, Action::DetailClose);
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.model_id, "model.sample.stg_orders"),
        m => panic!("expected the Detail modal back, got {m:?}"),
    }

    // Same for the SQL modal — `s` previews the SELECTION (focus_target), so
    // open it while stg_orders is selected, then move the selection away.
    apply_action(&mut a, Action::DetailClose); // close Detail first
    a.select_by_unique_id("model.sample.stg_orders");
    apply_action(&mut a, Action::SqlOpen);
    assert!(matches!(a.mode, Mode::Sql(_)), "s opens the SQL modal");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::MenuOpen);
    assert_eq!(menu_state(&a).target, "model.sample.stg_orders");
    apply_action(&mut a, Action::DetailClose);
    assert!(
        matches!(a.mode, Mode::Sql(_)),
        "Esc returns to the SQL modal"
    );
}

#[test]
fn menu_move_wraps_and_never_lands_on_separators() {
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.stg_orders");
    apply_action(&mut a, Action::MenuOpen);
    // Count the selectable rows, then walk a full lap + 1: every landing is an
    // Item, and the last step wraps back to the start.
    let items = menu_state(&a)
        .items
        .iter()
        .filter(|e| matches!(e, crate::action::MenuEntry::Item { .. }))
        .count();
    assert!(items >= 3, "the fixture menu has several items");
    let first = menu_state(&a).selected;
    for _ in 0..items {
        apply_action(&mut a, Action::MenuMove(Direction::Down));
        let ms = menu_state(&a);
        assert!(
            matches!(ms.items[ms.selected], crate::action::MenuEntry::Item { .. }),
            "cursor must never sit on a separator"
        );
    }
    assert_eq!(
        menu_state(&a).selected,
        first,
        "a full lap wraps to the start"
    );
    apply_action(&mut a, Action::MenuMove(Direction::Up));
    assert_ne!(menu_state(&a).selected, first, "up wraps backward too");
}

#[test]
fn menu_confirm_runs_the_highlighted_item_and_restores_back() {
    // Open the menu over the structure modal and confirm the FIRST item — the
    // layout's head row is the structure-view Enter ("Structure view" here,
    // since the modal's node IS the root) → runs DetailOpen on the modal's
    // node, restoring a fresh Detail mode.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    apply_action(&mut a, Action::MenuOpen);
    let labels = menu_labels(&menu_state(&a).items);
    assert_eq!(labels[0], "Structure view", "Enter on the rooted node");
    let out = apply_action(&mut a, Action::MenuConfirm);
    assert!(
        matches!(a.mode, Mode::Detail(_)),
        "Enter re-opens the structure view on the target, got {:?}",
        a.mode
    );
    assert!(out.effects.is_empty());
}

#[test]
fn menu_invoke_moves_the_selection_to_the_target_then_runs() {
    // Menu opened on A while the selection sits on B: running "Copy name"
    // yanks A's name — the menu's contract is that items act on THEIR node.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    a.select_by_unique_id("model.sample.fct_orders");
    apply_action(&mut a, Action::MenuOpen);
    let idx = menu_state(&a)
        .items
        .iter()
        .position(
            |e| matches!(e, crate::action::MenuEntry::Item { label, .. } if label == "Copy name"),
        )
        .expect("Copy name exists for a model");
    let out = apply_action(&mut a, Action::MenuInvoke(idx));
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "stg_orders", "the item acted on the menu's node");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.stg_orders"),
        "execution moves the selection to the menu target"
    );
    // The stacked modal it came from is restored after the run.
    assert!(matches!(a.mode, Mode::Detail(_)), "back restored");
}

#[test]
fn an_items_own_key_invokes_it_directly_via_dispatch() {
    use crate::dispatch;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);

    let mut a = sample_app();
    a.select_by_unique_id("model.sample.stg_orders");
    apply_action(&mut a, Action::MenuOpen);
    // The "SQL preview" item carries BINDINGS' `s` row — pressing `s` inside
    // the menu dispatches MenuInvoke on ITS index, not the cursored one.
    let (idx, _) = menu_state(&a)
        .items
        .iter()
        .enumerate()
        .find(|(_, e)| matches!(e, crate::action::MenuEntry::Item { label, .. } if label == "SQL preview"))
        .expect("SQL preview exists for a model");
    assert_eq!(
        dispatch(&a.mode, press(KeyCode::Char('s'))),
        Some(Action::MenuInvoke(idx))
    );
    let out = apply_action(&mut a, Action::MenuInvoke(idx));
    assert!(out.effects.is_empty());
    assert!(matches!(a.mode, Mode::Sql(_)), "s opened the SQL preview");
}

#[test]
fn menu_entries_omit_unsupported_items_and_orphan_separators() {
    let a = sample_app();
    // A seed has no SQL in the side-map: its menu drops both SQL rows, and
    // the separator that only separated them must not go adjacent/leading.
    let entries = a.menu_entries("seed.sample.country_codes");
    assert!(
        !entries.is_empty(),
        "a Dag node still has universal actions"
    );
    let labels = menu_labels(&entries);
    assert!(
        !labels
            .iter()
            .any(|l| *l == "SQL preview" || *l == "Copy raw SQL"),
        "a seed has no SQL rows, got {labels:?}"
    );
    // No leading / trailing / adjacent separators survive the filtering.
    assert!(!matches!(
        entries.first(),
        Some(crate::action::MenuEntry::Separator)
    ));
    assert!(!matches!(
        entries.last(),
        Some(crate::action::MenuEntry::Separator)
    ));
    assert!(!entries.windows(2).any(|w| matches!(
        (&w[0], &w[1]),
        (
            crate::action::MenuEntry::Separator,
            crate::action::MenuEntry::Separator
        )
    )));
}

#[test]
fn menu_open_for_a_stale_or_marker_target_notices_without_boxing() {
    let mut a = sample_app();
    assert!(
        a.menu_entries("+8 more").is_empty(),
        "a non-Dag uid yields no rows"
    );
    // `a` on a marker target can never produce the phantom box — the focus
    // target itself is a real node here, so drive the stale path directly.
    a.open_menu("+8 more", crate::action::MenuAnchor::Node);
    assert_eq!(a.mode, Mode::Selection);
    assert_eq!(a.take_notice().as_deref(), Some("no actions for this node"));
}

#[test]
fn menu_enter_label_is_context_sensitive() {
    // On the rooted node the Enter row says "Structure view"; on a different
    // node it honestly says "Re-root here" — the label can't lie about what
    // Enter does.
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.fct_orders");
    let rooted = a.menu_entries("model.sample.fct_orders");
    let other = a.menu_entries("model.sample.stg_orders");
    let first_label = |entries: &Vec<crate::action::MenuEntry>| match &entries[0] {
        crate::action::MenuEntry::Item { label, .. } => label.clone(),
        _ => panic!("first entry must be an item"),
    };
    assert_eq!(first_label(&rooted), "Structure view");
    assert_eq!(first_label(&other), "Re-root here");
}

#[test]
fn detail_yank_esc_returns_to_the_modal() {
    // Structure -> y -> Esc: the which-key's close restores the modal it
    // stacked over (the `back` slot), not the bare panes.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    apply_action(&mut a, Action::YankPrefix);
    match &a.mode {
        Mode::Yank { back, .. } => assert!(
            matches!(back.as_deref(), Some(Mode::Detail(_))),
            "the modal is stacked in `back`"
        ),
        m => panic!("expected Mode::Yank, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose); // Esc on the which-key
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.model_id, "model.sample.stg_orders"),
        m => panic!("expected the Detail modal back, got {m:?}"),
    }
    // …and a second Esc closes the modal itself.
    apply_action(&mut a, Action::DetailClose);
    assert_eq!(a.mode, Mode::Selection);
}

#[test]
fn menu_opened_over_the_whichkey_keeps_the_outer_destination() {
    // `a` while `y` is up over a modal: the menu takes the which-key's back
    // (the modal), so Esc on the menu still lands on the modal — the
    // intermediate which-key does not become a mode of its own.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    apply_action(&mut a, Action::YankPrefix);
    a.open_menu("model.sample.stg_orders", crate::action::MenuAnchor::Node);
    match &a.mode {
        Mode::Menu(ms) => assert!(
            matches!(ms.back.as_deref(), Some(Mode::Detail(_))),
            "the menu inherits the which-key's outer destination"
        ),
        m => panic!("expected Mode::Menu, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);
    assert!(
        matches!(a.mode, Mode::Detail(_)),
        "Esc returns to the modal, skipping the dead which-key"
    );
}
