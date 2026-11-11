use super::*;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn state(n: usize) -> UiState {
    UiState::new(n)
}

#[test]
fn j_and_down_are_equivalent_increment() {
    let mut s = state(45);
    assert_eq!(
        handle_key(&mut s, press(KeyCode::Char('j'))),
        KeyOutcome::Continue
    );
    assert_eq!(s.selected(), 1);
    handle_key(&mut s, press(KeyCode::Down));
    assert_eq!(s.selected(), 2);
}

#[test]
fn k_and_up_are_equivalent_decrement() {
    let mut s = state(45);
    s.selected = 5;
    handle_key(&mut s, press(KeyCode::Char('k')));
    assert_eq!(s.selected(), 4);
    handle_key(&mut s, press(KeyCode::Up));
    assert_eq!(s.selected(), 3);
}

#[test]
fn clamp_at_top_does_not_panic_or_underflow() {
    let mut s = state(45);
    assert_eq!(s.selected(), 0);
    handle_key(&mut s, press(KeyCode::Char('k')));
    assert_eq!(s.selected(), 0, "k at top stays at 0");
    handle_key(&mut s, press(KeyCode::Up));
    assert_eq!(s.selected(), 0, "up at top stays at 0");
}

#[test]
fn clamp_at_bottom_does_not_panic_or_overflow() {
    let mut s = state(45);
    s.selected = 44;
    handle_key(&mut s, press(KeyCode::Char('j')));
    assert_eq!(s.selected(), 44, "j at bottom stays at 44");
    handle_key(&mut s, press(KeyCode::Down));
    assert_eq!(s.selected(), 44, "down at bottom stays at 44");
}

#[test]
fn g_and_capital_g_jump_to_ends() {
    let mut s = state(45);
    s.selected = 20;
    handle_key(&mut s, press(KeyCode::Char('G')));
    assert_eq!(s.selected(), 44);
    handle_key(&mut s, press(KeyCode::Char('g')));
    assert_eq!(s.selected(), 0);
}

#[test]
fn tab_toggles_focus() {
    let mut s = state(45);
    assert_eq!(s.focus(), Focus::List);
    handle_key(&mut s, press(KeyCode::Tab));
    assert_eq!(s.focus(), Focus::RightPane);
    handle_key(&mut s, press(KeyCode::Tab));
    assert_eq!(s.focus(), Focus::List);
}

#[test]
fn toggle_list_pane_pins_focus_and_round_trips() {
    let mut s = state(45);
    assert!(s.list_visible(), "the list starts visible");

    // Hide: the lineage is the only pane left, so focus is forced there…
    s.toggle_list_pane();
    assert!(!s.list_visible());
    assert_eq!(s.focus(), Focus::RightPane, "hiding pins focus to lineage");
    // …and Tab cannot focus the invisible list.
    handle_key(&mut s, press(KeyCode::Tab));
    assert_eq!(s.focus(), Focus::RightPane, "Tab is a no-op while hidden");
    // List-side movement keys are no-ops too (move_* guard on List focus).
    handle_key(&mut s, press(KeyCode::Char('G')));
    assert_eq!(s.selected(), 0, "no list nav while the list is hidden");

    // Show: focus hands back to the list.
    s.toggle_list_pane();
    assert!(s.list_visible());
    assert_eq!(s.focus(), Focus::List, "showing focuses the list again");
}

#[test]
fn right_pane_focus_makes_jk_noop() {
    let mut s = state(45);
    s.selected = 10;
    handle_key(&mut s, press(KeyCode::Tab)); // focus -> right
    assert_eq!(s.focus(), Focus::RightPane);
    handle_key(&mut s, press(KeyCode::Char('j')));
    handle_key(&mut s, press(KeyCode::Char('k')));
    handle_key(&mut s, press(KeyCode::Char('g')));
    handle_key(&mut s, press(KeyCode::Char('G')));
    assert_eq!(s.selected(), 10, "no list nav when right pane focused");
}

// ---- lineage pane 2-axis scroll (wheel pan + loop follow only) ----

#[test]
fn movement_keys_never_pan_the_lineage_viewport() {
    // The keys moved the viewport in the old design; now they move the
    // App-level lineage cursor (which this pure layer never sees), so the
    // scroll offsets must stay untouched under EITHER focus.
    let mut s = state(45);
    s.selected = 5;
    for _ in 0..2 {
        for k in [
            KeyCode::Char('h'),
            KeyCode::Char('l'),
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Up,
        ] {
            handle_key(&mut s, press(k));
        }
        assert_eq!(s.lineage_scroll_x(), 0, "keys never pan horizontally");
        assert_eq!(s.lineage_scroll_y(), 0, "keys never pan vertically");
        handle_key(&mut s, press(KeyCode::Tab)); // 2nd round: right pane
    }
}

#[test]
fn wheel_scrolls_list_and_lineage_focus_independently() {
    let mut s = state(45);
    s.wheel_list(true); // down one notch
    assert_eq!(
        s.selected(),
        WHEEL_STEP,
        "wheel down moves the selection a notch"
    );
    s.wheel_list(false);
    assert_eq!(s.selected(), 0, "wheel up returns, clamped at 0");
    // Lineage wheel nudges scroll_y regardless of focus (List focus here).
    s.wheel_lineage(true);
    assert_eq!(s.lineage_scroll_y(), WHEEL_STEP);
    s.wheel_lineage(false);
    assert_eq!(s.lineage_scroll_y(), 0);
    // Horizontal wheel pans scroll_x the same way, saturating at 0.
    s.wheel_lineage_x(true);
    assert_eq!(s.lineage_scroll_x(), WHEEL_STEP);
    s.wheel_lineage_x(false);
    assert_eq!(s.lineage_scroll_x(), 0);
    s.wheel_lineage_x(false);
    assert_eq!(s.lineage_scroll_x(), 0, "saturates at the left edge");
    // Empty list: wheel is a safe no-op.
    let mut empty = state(0);
    empty.wheel_list(true);
    assert_eq!(empty.selected(), 0);
}

#[test]
fn clamp_lineage_bounds_both_axes() {
    // Wheel pan records saturating intent; the loop's clamp bounds it.
    let mut s = state(45);
    s.focus = Focus::RightPane;
    for _ in 0..100 {
        s.wheel_lineage(true); // down intent (y only; x is set directly)
    }
    s.lineage_scroll_x = 1000;
    s.clamp_lineage(20, 10, 8, 4);
    assert_eq!(s.lineage_scroll_x(), 20 - 8, "x clamped to grid-view");
    assert_eq!(s.lineage_scroll_y(), 10 - 4, "y clamped to grid-view");
    s.clamp_lineage(5, 3, 8, 4);
    assert_eq!(s.lineage_scroll_x(), 0);
    assert_eq!(s.lineage_scroll_y(), 0);
}

// ---- ensure_lineage_visible: minimal 2D scroll-follow for the cursor ----

fn rect(x: usize, y: usize, w: usize, h: usize) -> crate::NodeRect {
    crate::NodeRect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[test]
fn ensure_lineage_visible_is_a_noop_when_rect_already_shows() {
    // The anti-"viewport pans on every keypress" property: a visible rect
    // must not move the window at all (NOT a re-center).
    let mut s = state(45);
    s.lineage_scroll_x = 10;
    s.lineage_scroll_y = 4;
    s.ensure_lineage_visible(rect(12, 5, 6, 3), 100, 50, 20, 10);
    assert_eq!((s.lineage_scroll_x(), s.lineage_scroll_y()), (10, 4));
}

#[test]
fn ensure_lineage_visible_scrolls_minimally_toward_each_edge() {
    let mut s = state(45);
    // Right/bottom overflow: align the rect's far edge with the view edge.
    s.ensure_lineage_visible(rect(30, 20, 10, 3), 100, 50, 20, 10);
    assert_eq!(s.lineage_scroll_x(), 30 + 10 - 20, "right edge aligned");
    assert_eq!(s.lineage_scroll_y(), 20 + 3 - 10, "bottom edge aligned");
    // Left/top overflow: align the rect's near edge.
    s.ensure_lineage_visible(rect(5, 2, 4, 3), 100, 50, 20, 10);
    assert_eq!(s.lineage_scroll_x(), 5, "left edge aligned");
    assert_eq!(s.lineage_scroll_y(), 2, "top edge aligned");
}

#[test]
fn ensure_lineage_visible_prefers_start_edge_for_oversized_rects_and_clamps() {
    let mut s = state(45);
    // A rect wider than the viewport: the start (left) edge wins.
    s.ensure_lineage_visible(rect(40, 0, 30, 3), 100, 50, 20, 10);
    assert_eq!(s.lineage_scroll_x(), 40, "start edge wins for wide rects");
    // A rect at the grid's far corner: result stays within clamp bounds.
    s.ensure_lineage_visible(rect(95, 48, 5, 2), 100, 50, 20, 10);
    assert_eq!(s.lineage_scroll_x(), 100 - 20, "x clamped to grid-view");
    assert_eq!(s.lineage_scroll_y(), 50 - 10, "y clamped to grid-view");
    // Degenerate zero-size viewport: never panics, offsets clamp to 0.
    s.ensure_lineage_visible(rect(0, 0, 1, 1), 1, 1, 0, 0);
    assert_eq!(s.lineage_scroll_x(), 0);
    assert_eq!(s.lineage_scroll_y(), 0);
}

#[test]
fn anchor_lineage_centers_and_clamps() {
    use crate::layout::{layout, NodeRect};
    use crate::{Edge, NodeInfo, Subgraph};
    let n = |id: &str| NodeInfo {
        unique_id: id.into(),
        name: id.into(),
        resource_type: "model".into(),
        path: Some(format!("staging/{id}.sql")),
        ..Default::default()
    };
    let mut nodes = vec![n("a"), n("b"), n("c")];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    let sg = Subgraph {
        selected: "c".into(),
        nodes,
        edges: vec![
            Edge {
                parent: "a".into(),
                child: "b".into(),
            },
            Edge {
                parent: "b".into(),
                child: "c".into(),
            },
        ],
    };
    let lay = layout(&sg);
    let mut s = state(1);
    let (vw, vh) = (5usize, 1usize);
    s.anchor_lineage(lay.selected_rect, &lay.grid, vw, vh);
    let (sx, _sy) = lay.selected_coord.unwrap();
    let ox = s.lineage_scroll_x();
    assert!(
        sx >= ox && sx < ox + vw,
        "selected x {sx} in [{ox},{})",
        ox + vw
    );
    let _r = NodeRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
}

#[test]
fn lens_cycle_is_off_coverage_heat_layer_violation_wrap() {
    // Default Off, and one cycle_lens() from default == the old coverage lens
    // (muscle memory). Full cycle wraps back to Off.
    let mut s = state(45);
    assert_eq!(s.lens(), LineageLens::Off, "default lens is Off");
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::Coverage, "one `t` == coverage");
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::DegreeHeat);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::Layer);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::LayerViolation);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::Lint);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::Diff);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::RunStatus);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::RunTime);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::ColumnImpact);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::PrimaryKey);
    s.cycle_lens();
    assert_eq!(s.lens(), LineageLens::Off, "wraps back to Off");
}

#[test]
fn q_signals_quit() {
    let mut s = state(45);
    assert_eq!(
        handle_key(&mut s, press(KeyCode::Char('q'))),
        KeyOutcome::Quit
    );
}

#[test]
fn ctrl_c_signals_quit() {
    let mut s = state(45);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(handle_key(&mut s, ctrl_c), KeyOutcome::Quit);
}

#[test]
fn empty_list_navigation_is_safe() {
    let mut s = state(0);
    handle_key(&mut s, press(KeyCode::Char('j')));
    handle_key(&mut s, press(KeyCode::Char('k')));
    handle_key(&mut s, press(KeyCode::Char('G')));
    assert_eq!(s.selected(), 0);
}

// ---- scroll-follow (ensure_visible), display-row space ----

/// Display-row index of model `i` in the 49-row fixture layout. Mirrors
/// `ModelList::row_of_model`.
fn model_row(i: usize) -> usize {
    match i {
        0..=10 => i + 1,
        11..=18 => i + 2,
        19..=27 => i + 3,
        _ => i + 4,
    }
}
const TOTAL_ROWS: usize = 49;

/// First-in-group reveal rows (header above). Mirrors `reveal_row_of_model`.
fn reveal_row(i: usize) -> usize {
    match i {
        0 | 11 | 19 | 28 => model_row(i) - 1,
        _ => model_row(i),
    }
}

fn follow(s: &mut UiState, height: usize) {
    let sel = s.selected();
    s.ensure_visible(height, reveal_row(sel), model_row(sel), TOTAL_ROWS);
}

#[test]
fn scroll_index_zero_keeps_top_visible() {
    let mut s = state(45);
    s.offset = 30;
    s.selected = 0;
    follow(&mut s, 10);
    let row = model_row(0);
    let off = s.offset();
    assert!(
        off <= row && row < off + 10,
        "model 0 (row {row}) in [{off},{})",
        off + 10
    );
    assert_eq!(off, 0, "selection at the top snaps the window to the top");
}

#[test]
fn scroll_follows_selection_to_bottom_in_display_space() {
    let mut s = state(45);
    let h = 10;
    s.selected = 44;
    follow(&mut s, h);
    let row = model_row(44);
    let off = s.offset();
    assert!(
        off <= row && row < off + h,
        "model 44 (row {row}) in [{off},{})",
        off + h
    );
    assert_eq!(off, 39, "bottom-aligned window: total(49) - height(10)");
}

#[test]
fn scroll_step_by_step_keeps_selected_row_visible() {
    let mut s = state(45);
    let h = 10;
    for _ in 0..44 {
        handle_key(&mut s, press(KeyCode::Char('j')));
        follow(&mut s, h);
        let row = model_row(s.selected());
        let off = s.offset();
        assert!(
            off <= row && row < off + h,
            "model {} (row {row}) out of [{off},{})",
            s.selected(),
            off + h
        );
    }
    assert_eq!(s.selected(), 44);
}

#[test]
fn scroll_back_up_follows_and_clamps() {
    let mut s = state(45);
    let h = 10;
    s.selected = 44;
    follow(&mut s, h);
    assert_eq!(s.offset(), 39);
    for _ in 0..44 {
        handle_key(&mut s, press(KeyCode::Char('k')));
        follow(&mut s, h);
        let row = model_row(s.selected());
        let off = s.offset();
        assert!(
            off <= row && row < off + h,
            "model {} (row {row}) out of [{off},{}) going up",
            s.selected(),
            off + h
        );
    }
    assert_eq!(s.selected(), 0);
    assert_eq!(s.offset(), 0, "back at top, offset clamped to 0");
}

#[test]
fn ensure_visible_height_one_keeps_selected_row_and_no_panic() {
    let mut s = state(45);
    for sel in [0usize, 11, 28, 44] {
        s.selected = sel;
        follow(&mut s, 1);
        assert_eq!(
            s.offset(),
            model_row(sel),
            "height 1: window sits on the selected row"
        );
    }
}

#[test]
fn ensure_visible_height_zero_is_treated_as_one() {
    let mut s = state(45);
    s.selected = 44;
    s.ensure_visible(0, reveal_row(44), model_row(44), TOTAL_ROWS);
    assert_eq!(s.offset(), model_row(44), "height 0 behaves like height 1");
}

#[test]
fn ensure_visible_clamps_when_everything_fits() {
    let mut s = state(45);
    s.selected = 44;
    s.offset = 5;
    s.ensure_visible(100, reveal_row(44), model_row(44), TOTAL_ROWS);
    assert_eq!(s.offset(), 0, "when all rows fit there is no scrolling");
}

#[test]
fn first_in_group_selection_reveals_its_header() {
    let mut s = state(45);
    s.offset = 39;
    s.selected = 28;
    follow(&mut s, 10);
    let header = reveal_row(28);
    let model = model_row(28);
    let off = s.offset();
    assert!(
        off <= header,
        "header row {header} must be at/below offset {off}"
    );
    assert!(
        model < off + 10,
        "model row {model} must be inside [{off},{})",
        off + 10
    );
    assert_eq!(off, 31, "window snaps up to reveal the utilities header");
}

// ---- Responsive band: auto-hide verdict, drawer, focus pins ----

#[test]
fn inspector_policy_shown_at_resolves_the_auto_width_rule() {
    use InspectorPolicy::*;
    // The inspector rule: auto at >=160 columns only; the explicit
    // prefs answer for themselves at any width.
    assert!(Auto.shown_at(160));
    assert!(Auto.shown_at(400));
    assert!(!Auto.shown_at(159));
    assert!(!Auto.shown_at(0));
    assert!(On.shown_at(20), "on answers for itself");
    assert!(!Off.shown_at(300), "off answers for itself");
}

#[test]
fn list_shown_resolves_pref_and_drawer_across_the_band() {
    let mut s = state(45);
    assert!(s.list_shown(), "wide band: the pref shows the inline list");
    // Enter the band: the inline surface disappears (pref untouched).
    s.set_list_auto_hidden(true);
    assert!(s.list_visible(), "the PREF still says shown");
    assert!(!s.list_shown(), "but nothing list-shaped is on screen");
    // The drawer is the band's only list surface.
    s.set_list_drawer(true);
    assert!(s.list_shown(), "the open drawer IS the list in the band");
    s.set_list_drawer(false);
    assert!(!s.list_shown());
    // Leaving the band hands the verdict back to the pref — the stale
    // drawer flag is inert above 80.
    s.set_list_drawer(true);
    s.set_list_auto_hidden(false);
    assert!(s.list_shown(), "the pref rules above the band again");
    assert!(s.list_drawer(), "the flag survives (inert) for re-entry");
}

#[test]
fn band_entry_pins_list_focus_to_the_lineage() {
    let mut s = state(45);
    assert_eq!(s.focus(), Focus::List);
    // Shrinking into the band: a `Focus::List` left standing on a
    // surface that no longer exists pins to the lineage.
    s.set_list_auto_hidden(true);
    assert_eq!(
        s.focus(),
        Focus::RightPane,
        "a hidden list can't hold focus"
    );
    // The drawer opening in the band takes the focus — you open the
    // list surface to pick from it.
    s.set_list_drawer(true);
    assert_eq!(s.focus(), Focus::List);
    // Closing it gives the focus back (the pin's mirror image).
    s.set_list_drawer(false);
    assert_eq!(s.focus(), Focus::RightPane);
    // Above the band a stray open flag is fully inert — no steal.
    s.set_list_auto_hidden(false);
    s.set_list_drawer(true);
    assert_eq!(s.focus(), Focus::RightPane, "inert flag, inert focus");
    // The drawer pin never touches inspector focus either.
    s.set_inspector_shown(true);
    s.set_focus(Focus::Inspector);
    s.set_list_drawer(false);
    assert_eq!(s.focus(), Focus::Inspector);
}

#[test]
fn tab_ring_counts_the_drawer_as_the_list_stop() {
    let mut s = state(45);
    s.set_list_auto_hidden(true);
    s.set_list_drawer(true);
    assert_eq!(s.focus(), Focus::List);
    s.toggle_focus();
    assert_eq!(s.focus(), Focus::RightPane);
    // `Focus::Inspector` is skipped (pane hidden): the ring returns to
    // the drawer's list stop.
    s.toggle_focus();
    assert_eq!(s.focus(), Focus::List);
    // With the drawer closed the band's list stop vanishes entirely.
    s.set_list_drawer(false);
    s.toggle_focus();
    assert_eq!(s.focus(), Focus::RightPane, "nothing to land on — stays");
}

#[test]
fn toggles_in_the_band_move_the_drawer_never_the_pref() {
    let mut s = state(45);
    s.set_list_auto_hidden(true);
    // `toggle_list_pane` in the band — however it was dispatched (Ctrl-b
    // reroute, palette confirm, menu) — toggles the DRAWER, the only
    // list surface that can exist here; the persisted inline pref must
    // not move (a write here would silently change ≥80 behaviour).
    let pref = s.list_visible();
    s.set_focus(Focus::RightPane);
    s.toggle_list_pane();
    assert_eq!(s.list_visible(), pref, "the persisted pref is untouched");
    assert!(s.list_drawer(), "the drawer opened instead");
    assert_eq!(s.focus(), Focus::List, "the opened drawer takes focus");
    // The same toggle closes it and hands focus back.
    s.toggle_list_pane();
    assert_eq!(s.list_visible(), pref);
    assert!(!s.list_drawer());
    assert_eq!(s.focus(), Focus::RightPane);
    // And above the band the same call writes the pref, like always.
    s.set_list_auto_hidden(false);
    s.toggle_list_pane();
    assert_eq!(s.list_visible(), !pref, "the pref flips off-band");
}

#[test]
fn band_reentry_with_an_open_drawer_grabs_list_focus() {
    let mut s = state(45);
    s.set_list_auto_hidden(true);
    s.set_list_drawer(true);
    assert_eq!(s.focus(), Focus::List);
    // Leave the band with the drawer still flagged: the flag goes inert
    // and the user moves on while the wide panes are up.
    s.set_list_auto_hidden(false);
    s.set_focus(Focus::RightPane);
    // Re-entering the band re-materializes the stale-open drawer — that
    // IS a fresh open, so it takes list focus like `set_list_drawer`
    // does (a hidden-or-focused mismatch can't strand the surface).
    s.set_list_auto_hidden(true);
    assert!(s.list_shown(), "the stale flag re-shows the drawer");
    assert_eq!(s.focus(), Focus::List, "the re-open grabs list focus");
    // A drawer the user actually CLOSED before leaving stays closed.
    s.set_list_drawer(false);
    s.set_list_auto_hidden(false);
    s.set_focus(Focus::RightPane);
    s.set_list_auto_hidden(true);
    assert!(!s.list_shown());
    assert_eq!(s.focus(), Focus::RightPane, "no drawer, no steal");
}
