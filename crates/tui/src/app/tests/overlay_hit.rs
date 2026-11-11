use super::*;

// ---- Issue 06: overlay hit-testing + geometry -------------------------------

#[test]
fn hit_target_overlay_owns_rows_frame_and_the_whole_exterior() {
    let a = sample_app();
    let area = Rect::new(0, 0, 160, 44);
    let overlay = Rect::new(30, 10, 60, 20);
    let g = HitGeometry {
        overlay: Some(overlay),
        ..hit_geom(&a, None, area)
    };
    let inner = pane_interior(overlay);
    // Interior cells are rows (0-based inside the border).
    assert_eq!(
        hit_target(&g, inner.x + 3, inner.y + 2),
        Some(HitTarget::OverlayRow(2))
    );
    assert_eq!(
        hit_target(&g, inner.x, inner.y + inner.height - 1),
        Some(HitTarget::OverlayRow((inner.height - 1) as usize))
    );
    // The frame (border/title) is inert chrome — not a row, not a dismissal.
    assert_eq!(
        hit_target(&g, overlay.x, overlay.y),
        Some(HitTarget::ModalInterior)
    );
    assert_eq!(
        hit_target(
            &g,
            overlay.x + overlay.width - 1,
            overlay.y + overlay.height - 1
        ),
        Some(HitTarget::ModalInterior)
    );
    // EVERYTHING outside is the click-outside target — panes, status bar,
    // the whole dimmed backdrop.
    assert_eq!(hit_target(&g, 1, 1), Some(HitTarget::ModalExterior));
    assert_eq!(
        hit_target(&g, area.width - 1, area.height - 1),
        Some(HitTarget::ModalExterior)
    );
}

#[test]
fn modal_rect_caps_width_at_120_and_centers() {
    use crate::ui::modal_rect;
    // min(80% of 160 = 128, 120) → 120; height = 80% of 44 ≈ 35.
    let r = modal_rect(Rect::new(0, 0, 160, 44));
    assert_eq!(r.width, 120, "the 120-col cap wins on wide terminals");
    assert_eq!(r.height, 35);
    assert_eq!(r.x, (160 - 120) / 2, "horizontally centred");
    assert_eq!(r.y, (44 - 35) / 2, "vertically centred");
    // 80% on a normal terminal: 100 → 80.
    let r = modal_rect(Rect::new(0, 0, 100, 40));
    assert_eq!((r.width, r.height), (80, 32));
    // Ultra-wide 240: still capped at 120, centred.
    let r = modal_rect(Rect::new(0, 0, 240, 60));
    assert_eq!(r.width, 120, "240-wide terminal still caps at 120");
    assert_eq!(r.x, (240 - 120) / 2);
    // Tiny terminals never panic and stay inside the area.
    let r = modal_rect(Rect::new(0, 0, 2, 2));
    assert!(r.width >= 1 && r.width <= 2);
    assert!(r.height >= 1 && r.height <= 2);
    assert!(area_contains(Rect::new(0, 0, 2, 2), r));
}

fn area_contains(area: Rect, r: Rect) -> bool {
    r.x >= area.x
        && r.y >= area.y
        && r.x + r.width <= area.x + area.width
        && r.y + r.height <= area.y + area.height
}

#[test]
fn menu_rect_anchors_at_the_pointer_and_flips_inside_the_screen() {
    use crate::ui::menu_rect;
    let a = sample_app();
    let entries = a.menu_entries("model.sample.stg_orders");
    let ms = crate::action::MenuState {
        target: "model.sample.stg_orders".to_string(),
        name: "stg_orders".to_string(),
        items: entries,
        selected: 0,
        back: None,
        anchor: crate::action::MenuAnchor::Cell { col: 10, row: 5 },
    };
    let area = Rect::new(0, 0, 120, 40);
    let lin_inner = Rect::new(40, 1, 80, 38);
    // Mid-screen anchor: the box grows down-right of the cell.
    let r = menu_rect(area, lin_inner, None, 0, 0, &ms);
    assert!(area_contains(area, r));
    assert_eq!(r.x, 11, "anchored one cell right of the pointer");
    assert_eq!(r.y, 6, "anchored one cell below the pointer");

    // A right-edge anchor flips the box to the pointer's LEFT side.
    let ms_right = crate::action::MenuState {
        anchor: crate::action::MenuAnchor::Cell { col: 118, row: 5 },
        ..ms.clone()
    };
    let r = menu_rect(area, lin_inner, None, 0, 0, &ms_right);
    assert!(area_contains(area, r));
    assert!(
        r.x + r.width <= 119,
        "flipped left of the pointer, got {r:?}"
    );

    // A bottom-edge anchor flips the box UP above the pointer.
    let ms_bottom = crate::action::MenuState {
        anchor: crate::action::MenuAnchor::Cell { col: 10, row: 38 },
        ..ms.clone()
    };
    let r = menu_rect(area, lin_inner, None, 0, 0, &ms_bottom);
    assert!(area_contains(area, r));
    assert!(r.y + r.height <= 39, "flipped above the pointer, got {r:?}");

    // A corner anchor: both flips at once, still fully inside.
    let ms_corner = crate::action::MenuState {
        anchor: crate::action::MenuAnchor::Cell { col: 119, row: 39 },
        ..ms
    };
    let r = menu_rect(area, lin_inner, None, 0, 0, &ms_corner);
    assert!(area_contains(area, r));
}

#[test]
fn detail_yank_execute_returns_to_the_modal() {
    // Structure -> y -> y (copy unique_id): the action runs AND the modal it
    // stacked over is restored — the which-key is one-shot with a `back`.
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankId);
    assert!(
        out.effects.iter().any(|e| matches!(e, Effect::Yank(_))),
        "y yanks the uid"
    );
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.model_id, "model.sample.stg_orders"),
        m => panic!("expected the Detail modal back, got {m:?}"),
    }
}

#[test]
fn pointer_row_picks_move_the_overlay_cursor() {
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.stg_orders");
    // Yank which-key: YankPick moves the highlighted row.
    apply_action(&mut a, Action::YankPrefix);
    apply_action(&mut a, Action::YankPick(2));
    match &a.mode {
        Mode::Yank { sel, .. } => assert_eq!(*sel, 2),
        m => panic!("expected Mode::Yank, got {m:?}"),
    }
    // Menu: MenuSelect hovers a row without running it, skips separators.
    apply_action(&mut a, Action::DetailClose); // close which-key -> Selection
    apply_action(&mut a, Action::MenuOpen);
    let pick = menu_state(&a)
        .items
        .iter()
        .rposition(|e| matches!(e, crate::action::MenuEntry::Item { .. }))
        .expect("at least one item row");
    apply_action(&mut a, Action::MenuSelect(pick));
    assert_eq!(menu_state(&a).selected, pick);
    // A separator row refuses the cursor — hover must not land on a rule.
    let sep = menu_state(&a)
        .items
        .iter()
        .position(|e| matches!(e, crate::action::MenuEntry::Separator))
        .expect("a separator row");
    apply_action(&mut a, Action::MenuSelect(sep));
    assert_eq!(menu_state(&a).selected, pick);
}

#[test]
fn outside_click_action_is_the_same_as_esc() {
    // The event loop maps a ModalExterior click to `esc_action(mode)` — the
    // exact Action the Esc key dispatches. Assert that action really closes
    // a menu back to its stacked modal (the mouse == keyboard contract).
    let mut a = sample_app();
    open_detail(&mut a, "model.sample.stg_orders");
    apply_action(&mut a, Action::MenuOpen);
    let esc = crate::action::esc_action(&a.mode).expect("menu has an Esc row");
    apply_action(&mut a, esc);
    assert!(
        matches!(a.mode, Mode::Detail(_)),
        "outside-click action restores the modal"
    );
}

#[test]
fn menu_items_act_on_non_model_targets() {
    // A seed has no list row — the selection can never land on it, so every
    // item must resolve through the armed stash (`pending_node`), not the
    // selection. Review: Copy name used to yank the selected model's name.
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.stg_orders");
    a.open_menu("seed.sample.country_codes", crate::action::MenuAnchor::Node);
    let items = menu_state(&a).items.clone();
    let invoke = |a: &mut App, label: &str| {
        let i = items
            .iter()
            .position(
                |e| matches!(e, crate::action::MenuEntry::Item { label: l, .. } if l == label),
            )
            .unwrap_or_else(|| panic!("menu has no {label:?} row: {items:?}"));
        apply_action(a, Action::MenuInvoke(i))
    };

    // Copy name -> the SEED's name, not the selected model's.
    let out = invoke(&mut a, "Copy name");
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "country_codes");

    // Copy unique_id -> the seed's uid.
    a.open_menu("seed.sample.country_codes", crate::action::MenuAnchor::Node);
    let items = menu_state(&a).items.clone();
    let i = items
        .iter()
        .position(|e| matches!(e, crate::action::MenuEntry::Item { label: l, .. } if l == "Copy unique_id"))
        .expect("Copy unique_id row");
    let out = apply_action(&mut a, Action::MenuInvoke(i));
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "seed.sample.country_codes");

    // Open in editor -> the seed's file, under the project root.
    a.open_menu("seed.sample.country_codes", crate::action::MenuAnchor::Node);
    let items = menu_state(&a).items.clone();
    let i = items
        .iter()
        .position(|e| matches!(e, crate::action::MenuEntry::Item { label: l, .. } if l == "Open in editor"))
        .expect("Open in editor row (seed has a file)");
    let out = apply_action(&mut a, Action::MenuInvoke(i));
    let path = match out.effects.as_slice() {
        [Effect::OpenEditor(p)] => p.clone(),
        other => panic!("expected one OpenEditor effect, got {other:?}"),
    };
    assert!(
        path.ends_with("seeds/country_codes.csv"),
        "editor must open the seed's file, got {path}"
    );

    // Bookmark is list-row-only — the seed's menu omits it.
    assert!(
        !items.iter().any(
            |e| matches!(e, crate::action::MenuEntry::Item { label: l, .. } if l == "Bookmark")
        ),
        "a non-model must not offer Bookmark"
    );

    // And "Structure view" is honest on a non-model: Enter opens ITS detail
    // modal (there is no re-root for seeds).
    assert_eq!(menu_labels(&items)[0], "Structure view");
    a.open_menu("seed.sample.country_codes", crate::action::MenuAnchor::Node);
    apply_action(&mut a, Action::MenuInvoke(0));
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.model_id, "seed.sample.country_codes"),
        m => panic!("expected Detail on the seed, got {m:?}"),
    }
}

#[test]
fn menu_invoke_on_a_model_still_reroots_then_acts() {
    // Model target: the selection move fires (reroot_select lands) AND the
    // stash makes the yank hit the same node — belt and braces.
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.fct_orders");
    a.open_menu("model.sample.stg_orders", crate::action::MenuAnchor::Node);
    let i = menu_state(&a)
        .items
        .iter()
        .position(
            |e| matches!(e, crate::action::MenuEntry::Item { label: l, .. } if l == "Copy name"),
        )
        .expect("Copy name row");
    let out = apply_action(&mut a, Action::MenuInvoke(i));
    let text = match out.effects.as_slice() {
        [Effect::Yank(t)] => t.clone(),
        other => panic!("expected one Yank effect, got {other:?}"),
    };
    assert_eq!(text, "stg_orders");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.stg_orders"),
        "the cursor move contract: selection lands on the menu's node"
    );
}
