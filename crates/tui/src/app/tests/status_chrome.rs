use super::*;

// ---- Issue 07: clickable status hints/chips + title chrome -----------------
//
// §8/§6's chrome targets dispatch through `click_target` exactly like the
// pane targets: a hint runs its own `Action`, a chip the §8 table's, a crumb
// replays `history_back`, the `[{v}]`/`[lens:…]` tags ResetView/CycleLens.

#[test]
fn click_target_status_hint_dispatches_its_action_and_none_is_inert() {
    let mut a = sample_app();
    let o = a.click_target(
        &HitTarget::StatusHint(Some(Action::SearchOpen)),
        MouseButton::Left,
        (0, 0),
    );
    assert!(
        matches!(a.mode, Mode::Search(_)),
        "a hint click runs the hint's action like its key would"
    );
    assert!(!o.quit && o.effects.is_empty());
    // A transient readout (splitter drag, prio-only text) carries no action —
    // clicking it does nothing.
    a.mode = Mode::Selection;
    a.click_target(&HitTarget::StatusHint(None), MouseButton::Left, (0, 0));
    assert!(matches!(a.mode, Mode::Selection), "action:None is inert");
}

#[test]
fn click_target_status_chips_follow_the_chip_table() {
    use crate::ui::StatusChip;
    // §8's chip click table, driven through the production seam.
    let mut a = sample_app();

    a.click_target(
        &HitTarget::StatusChip(StatusChip::Notify),
        MouseButton::Left,
        (0, 0),
    );
    assert!(
        matches!(a.mode, Mode::Notifications(_)),
        "!N opens the centre"
    );
    a.mode = Mode::Selection;

    a.click_target(
        &HitTarget::StatusChip(StatusChip::Focus),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(
        a.ui_state.focus(),
        crate::ui::Focus::RightPane,
        "the focus chip toggles"
    );
    a.click_target(
        &HitTarget::StatusChip(StatusChip::Focus),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(a.ui_state.focus(), crate::ui::Focus::List);

    // The coverage chip picks its lens directly (SetLens), and toggles OFF
    // again when the lens is already on.
    a.click_target(
        &HitTarget::StatusChip(StatusChip::Coverage),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(a.ui_state.lens(), crate::ui::LineageLens::Coverage);
    a.click_target(
        &HitTarget::StatusChip(StatusChip::Coverage),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(a.ui_state.lens(), crate::ui::LineageLens::Off);

    // The view chip resets the rooted view.
    a.tab_mut().lineage_view = crate::app::LineageView {
        upstream: true,
        downstream: false,
        depth: Some(2),
    };
    a.click_target(
        &HitTarget::StatusChip(StatusChip::View),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(a.tab().lineage_view, crate::app::LineageView::default());

    // The inert readouts report `None` — a click is a no-op, not a panic.
    for chip in [
        StatusChip::Impact,
        StatusChip::Scope,
        StatusChip::Position,
        StatusChip::Filter,
    ] {
        let before = a.selected_unique_id();
        a.click_target(&HitTarget::StatusChip(chip), MouseButton::Left, (0, 0));
        assert_eq!(a.selected_unique_id(), before, "{chip:?} is inert");
        assert!(matches!(a.mode, Mode::Selection));
    }
}

#[test]
fn click_target_title_crumb_replays_history_back() {
    // §6: a breadcrumb entry's click = `b` pressed its step count — the SAME
    // history mechanism, no second model.
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.stg_orders");
    a.jump_to("model.sample.fct_orders");
    a.jump_to("model.sample.cll_demo");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.cll_demo")
    );
    // Crumb steps 1 = the direct parent (one history_back).
    a.click_target(&HitTarget::TitleCrumb(1), MouseButton::Left, (0, 0));
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.fct_orders")
    );
    // Steps 2 = two backs — lands on the origin selection.
    a.jump_to("model.sample.cll_demo");
    a.click_target(&HitTarget::TitleCrumb(2), MouseButton::Left, (0, 0));
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.stg_orders")
    );
}

#[test]
fn click_target_title_tags_reset_view_and_cycle_lens() {
    let mut a = sample_app();
    // `[{v}]` resets the rooted view.
    a.tab_mut().lineage_view = crate::app::LineageView {
        upstream: true,
        downstream: false,
        depth: Some(3),
    };
    a.click_target(&HitTarget::TitleView, MouseButton::Left, (0, 0));
    assert_eq!(a.tab().lineage_view, crate::app::LineageView::default());
    // `[lens:…]` cycles the lens (the same arm `t` dispatches).
    assert_eq!(a.ui_state.lens(), crate::ui::LineageLens::Off);
    a.click_target(&HitTarget::TitleLens, MouseButton::Left, (0, 0));
    assert_eq!(a.ui_state.lens(), crate::ui::LineageLens::Coverage);
}
