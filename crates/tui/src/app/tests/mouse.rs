use super::*;

// ---- Issue 01: HitTarget resolution + button-aware click dispatch ----
//
// `hit_target` is the pure half (the event loop builds `HitGeometry` from the
// same pane/scroll state it draws with); `App::click_target` is the domain
// half (button-aware dispatch over the resolved target).

#[test]
fn hit_target_classifies_the_list_pane() {
    let a = sample_app();
    let area = Rect::new(0, 0, 160, 44);
    let lay = a.styled_lineage_layout();
    let g = hit_geom(&a, lay.as_deref(), area);
    let inner = pane_interior(g.panes.list);

    // Display row 0 is a group header; the row after it is the first model.
    let layer = match &a.active_list().rows[0] {
        DisplayRow::Header { layer, .. } => layer.clone(),
        other => panic!("row 0 must be a header, got {other:?}"),
    };
    assert_eq!(
        hit_target(&g, inner.x + 2, inner.y),
        Some(HitTarget::ListHeader(layer))
    );
    assert_eq!(
        hit_target(&g, inner.x + 2, inner.y + 1),
        Some(HitTarget::ListRow(0)),
        "the payload is the MODEL index, not the display row"
    );

    // Scrolled near the end, interior cells below the last row are ListBlank.
    let g_bottom = HitGeometry {
        list_offset: a.active_list().rows.len() - 1,
        ..hit_geom(&a, lay.as_deref(), area)
    };
    assert_eq!(
        hit_target(&g_bottom, inner.x + 2, inner.y + 3),
        Some(HitTarget::ListBlank)
    );
}

#[test]
fn hit_target_classifies_the_status_line_boundaries_and_dead_zones() {
    let a = sample_app();
    let area = Rect::new(0, 0, 160, 44);
    let lay = a.styled_lineage_layout();
    let g = hit_geom(&a, lay.as_deref(), area);

    assert_eq!(
        hit_target(&g, area.width / 2, area.height - 1),
        Some(HitTarget::StatusBar)
    );
    // The divider is two columns: the list's right border and the lineage
    // pane's left border — both report the LIST splitter (§5).
    assert_eq!(
        hit_target(&g, g.panes.list.x + g.panes.list.width - 1, 5),
        Some(HitTarget::PaneBorder(Splitter::List))
    );
    assert_eq!(
        hit_target(&g, g.panes.lineage.x, 5),
        Some(HitTarget::PaneBorder(Splitter::List))
    );
    // A pane's top border row is chrome, not a target.
    assert_eq!(hit_target(&g, 5, g.panes.list.y), None);
    // Outside the frame entirely → no surface.
    assert_eq!(hit_target(&g, area.width, area.height / 2), None);
}

#[test]
fn hit_target_maps_lineage_nodes_centered_and_scrolled() {
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.fct_orders");
    let lay = a.styled_lineage_layout().expect("a layout exists");
    // A wide frame centres the (smaller) diagram inside the lineage pane.
    let area = Rect::new(0, 0, 200, 60);
    let g = hit_geom(&a, Some(&lay), area);
    let inner = pane_interior(g.panes.lineage);
    let content = lineage_content_rect(inner, lay.grid.width(), lay.grid.height());
    assert!(
        content.x > inner.x || content.y > inner.y,
        "the fixture diagram must be centered for this test"
    );

    // Centered (scroll 0): a node's top-left grid cell maps through the pad.
    let (uid, r) = lay.rects.iter().next().expect("nodes exist");
    assert_eq!(
        hit_target(&g, content.x + r.x as u16, content.y + r.y as u16),
        Some(HitTarget::LineageNode(uid.clone()))
    );

    // Scrolled: grid coordinate = screen - content origin + scroll offset.
    let (uid2, r2) = lay.rects.iter().max_by_key(|(_, r)| r.x).expect("nodes");
    let scroll_x = 2usize;
    assert!(r2.x >= scroll_x, "picked node must still be visible");
    let g_scrolled = HitGeometry {
        scroll_x,
        ..hit_geom(&a, Some(&lay), area)
    };
    assert_eq!(
        hit_target(
            &g_scrolled,
            content.x + (r2.x - scroll_x) as u16,
            content.y + r2.y as u16
        ),
        Some(HitTarget::LineageNode(uid2.clone()))
    );

    // The centering pad and empty diagram cells are LineageBlank, never a node.
    if content.x > inner.x {
        assert_eq!(
            hit_target(&g, inner.x, content.y),
            Some(HitTarget::LineageBlank),
            "the left pad margin reports blank"
        );
    }
    // A grid cell no rect covers, inside the content window.
    let blank = (inner.y..inner.y + inner.height)
        .flat_map(|y| (inner.x..inner.x + inner.width).map(move |x| (x, y)))
        .find(|&(x, y)| {
            content.contains(ratatui::layout::Position { x, y })
                && !lay.rects.values().any(|r| {
                    let gx = (x - content.x) as usize;
                    let gy = (y - content.y) as usize;
                    gx >= r.x && gx < r.x + r.width && gy >= r.y && gy < r.y + r.height
                })
        })
        .expect("a diagram has empty cells around its nodes");
    assert_eq!(
        hit_target(&g, blank.0, blank.1),
        Some(HitTarget::LineageBlank)
    );
}

#[test]
fn hit_target_minimap_swallows_the_diagram_below_it() {
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.fct_orders");
    let lay = a.styled_lineage_layout().expect("a layout exists");
    // A small frame: the diagram overflows the pane, so the minimap draws.
    let area = Rect::new(0, 0, 80, 24);
    let inner = pane_interior(pane_layout(area, &a.ui_state.pane_config()).lineage);
    let content = lineage_content_rect(inner, lay.grid.width(), lay.grid.height());
    let mm = minimap_rect(inner, lay.grid.width(), lay.grid.height(), 0, 0, true)
        .expect("an overflowing diagram draws the minimap");
    // `minimap_visible` defaults off — this geometry has the inset ON.
    let g = HitGeometry {
        minimap: true,
        ..hit_geom(&a, Some(&lay), area)
    };

    // Every cell inside the inset reports Minimap — never the node below.
    assert_eq!(hit_target(&g, mm.x, mm.y), Some(HitTarget::Minimap));
    assert_eq!(
        hit_target(&g, mm.x + mm.width - 1, mm.y + mm.height - 1),
        Some(HitTarget::Minimap)
    );

    // Prove the swallow: scroll a REAL node's centre under the inset's centre.
    let (_, r) = lay.rects.iter().max_by_key(|(_, r)| r.x).expect("nodes");
    let cx = r.x + r.width / 2;
    let cy = r.y + r.height / 2;
    // `content` can start below `mm` (vertical pad) — keep the math signed.
    let want_x = i64::from(mm.x) - i64::from(content.x) + i64::from(mm.width / 2);
    let want_y = i64::from(mm.y) - i64::from(content.y) + i64::from(mm.height / 2);
    let g_under = HitGeometry {
        minimap: true,
        scroll_x: (cx as i64 - want_x).max(0) as usize,
        scroll_y: (cy as i64 - want_y).max(0) as usize,
        ..hit_geom(&a, Some(&lay), area)
    };
    assert_eq!(
        hit_target(&g_under, mm.x + mm.width / 2, mm.y + mm.height / 2),
        Some(HitTarget::Minimap),
        "a node dragged under the inset still reports Minimap"
    );

    // With the inset off, the same cell falls back to the diagram surface.
    let g_off = HitGeometry {
        minimap: false,
        ..hit_geom(&a, Some(&lay), area)
    };
    assert!(!matches!(
        hit_target(&g_off, mm.x, mm.y),
        Some(HitTarget::Minimap)
    ));
}

#[test]
fn click_target_left_click_moves_the_cursor_only() {
    let mut a = sample_app();
    let root0 = a.selected_unique_id().unwrap();
    let second = a.active_list().model_at(1).unwrap().unique_id.clone();

    // A list row: §2 T1 — a click picks the model exactly like a keypress:
    // re-root WITH the back-history push (`jump_to`, not bare `reroot_select`).
    a.click_target(&HitTarget::ListRow(1), MouseButton::Left, (0, 0));
    assert_eq!(a.selected_unique_id().as_deref(), Some(second.as_str()));
    assert_eq!(a.ui_state.focus(), Focus::List);
    assert_eq!(
        a.tab().back,
        vec![root0],
        "a list click lands on the tab's history like the key path"
    );

    // A lineage node: §6 M1 — a single left click moves the CURSOR only;
    // the re-root is the double-click's commit (`commit_lineage_node` +
    // `DetailOpen`, which `node_commit` simulates).
    a.select_by_unique_id("model.sample.fct_orders");
    a.click_target(
        &HitTarget::LineageNode("model.sample.stg_orders".to_string()),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.fct_orders"),
        "a single click never re-roots (M1)"
    );
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some("model.sample.stg_orders"),
        "the cursor moved onto the node"
    );
    assert_eq!(a.ui_state.focus(), Focus::RightPane);

    // Pane interior surfaces take their pane's focus (what they did before);
    // the minimap and chrome are fully inert.
    a.ui_state.set_focus(Focus::RightPane);
    let sel = a.selected_unique_id();
    a.click_target(
        &HitTarget::ListHeader("staging".to_string()),
        MouseButton::Left,
        (0, 0),
    );
    assert_eq!(a.ui_state.focus(), Focus::List);
    assert_eq!(a.selected_unique_id(), sel, "a header selects nothing");
    a.click_target(&HitTarget::Minimap, MouseButton::Left, (0, 0));
    assert_eq!(
        a.ui_state.focus(),
        Focus::List,
        "the minimap does literally nothing — focus doesn't even move"
    );
    assert_eq!(
        a.selected_unique_id(),
        sel,
        "the minimap swallows its click"
    );
    a.click_target(
        &HitTarget::PaneBorder(Splitter::List),
        MouseButton::Left,
        (0, 0),
    );
    a.click_target(&HitTarget::StatusBar, MouseButton::Left, (0, 0));
    assert_eq!(a.selected_unique_id(), sel, "chrome stays inert");
}

#[test]
fn hit_target_with_list_hidden_gives_everything_to_the_lineage() {
    let mut a = sample_app();
    a.select_by_unique_id("model.sample.fct_orders");
    let lay = a.styled_lineage_layout().expect("a layout exists");
    let area = Rect::new(0, 0, 160, 44);
    // `list_visible = false`: the list rect collapses to zero width and the
    // lineage pane takes the whole body — no divider exists.
    let g = HitGeometry {
        panes: pane_layout(
            area,
            &PaneConfig {
                list_visible: false,
                ..PaneConfig::default()
            },
        ),
        ..hit_geom(&a, Some(&lay), area)
    };
    // A cell inside the (former) list region is lineage interior now —
    // never a list target, and never the absent boundary. (x=0 is the
    // lineage pane's own border, so start at 5.)
    for x in [5, 30, 60] {
        let t = hit_target(&g, x, 5);
        assert!(
            matches!(
                t,
                Some(HitTarget::LineageNode(_)) | Some(HitTarget::LineageBlank)
            ),
            "x={x} should be lineage surface, got {t:?}"
        );
    }
    assert_eq!(
        hit_target(&g, area.width / 2, area.height - 1),
        Some(HitTarget::StatusBar),
        "the status line is unaffected"
    );
}

#[test]
fn hit_target_with_no_layout_reports_lineage_blank() {
    let a = sample_app();
    let area = Rect::new(0, 0, 160, 44);
    let g = hit_geom(&a, None, area);
    let inner = pane_interior(g.panes.lineage);
    // An undrawn diagram: every interior cell is LineageBlank, never a node.
    assert_eq!(
        hit_target(&g, inner.x + 5, inner.y + 5),
        Some(HitTarget::LineageBlank)
    );
}

#[test]
fn click_target_blank_surfaces_only_focus_and_modals_are_inert() {
    let mut a = sample_app();
    let sel = a.selected_unique_id();
    a.ui_state.set_focus(Focus::RightPane);

    a.click_target(&HitTarget::ListBlank, MouseButton::Left, (0, 0));
    assert_eq!(
        a.ui_state.focus(),
        Focus::List,
        "list blank takes list focus"
    );
    assert_eq!(a.selected_unique_id(), sel, "no selection change");

    a.click_target(&HitTarget::LineageBlank, MouseButton::Left, (0, 0));
    assert_eq!(
        a.ui_state.focus(),
        Focus::RightPane,
        "lineage blank takes lineage focus"
    );
    assert_eq!(a.selected_unique_id(), sel, "no selection change");

    // The overlay targets are unreachable from the panes (an overlay owns the
    // screen while present) and must be inert regardless — not even a focus
    // change.
    for t in [HitTarget::ModalInterior, HitTarget::ModalExterior] {
        a.click_target(&t, MouseButton::Left, (0, 0));
        assert_eq!(a.ui_state.focus(), Focus::RightPane, "{t:?} must be inert");
        assert_eq!(a.selected_unique_id(), sel, "{t:?} selects nothing");
    }
}

#[test]
fn click_target_right_click_opens_the_node_menu_at_the_pointer() {
    // Issue 06: a right-click on a list row or a lineage node opens the
    // node context menu anchored at the POINTER cell; right-clicks
    // elsewhere stay inert (middle-clicks open tabs — covered below).
    let mut a = sample_app();
    let uid = a.active_list().model_at(1).unwrap().unique_id.clone();

    a.click_target(&HitTarget::ListRow(1), MouseButton::Right, (11, 7));
    match &a.mode {
        Mode::Menu(ms) => {
            assert_eq!(ms.target, uid, "the menu acts on the clicked row's node");
            assert_eq!(
                ms.anchor,
                crate::action::MenuAnchor::Cell { col: 11, row: 7 },
                "the box anchors at the pointer cell"
            );
            assert!(
                ms.items.iter().all(|e| matches!(
                    e,
                    crate::action::MenuEntry::Item { .. } | crate::action::MenuEntry::Separator
                )),
                "items are BINDINGS-resolved entries"
            );
        }
        m => panic!("expected Mode::Menu, got {m:?}"),
    }
    assert_eq!(a.ui_state.focus(), Focus::List);
    // The menu's Esc restores Selection (nothing stacked under it).
    apply_action(&mut a, Action::DetailClose);
    assert_eq!(a.mode, Mode::Selection);

    // Same for a lineage node — focus moves to the right pane, the menu
    // targets the clicked node.
    a.click_target(
        &HitTarget::LineageNode("model.sample.stg_orders".to_string()),
        MouseButton::Right,
        (40, 9),
    );
    match &a.mode {
        Mode::Menu(ms) => assert_eq!(ms.target, "model.sample.stg_orders"),
        m => panic!("expected Mode::Menu, got {m:?}"),
    }
    assert_eq!(a.ui_state.focus(), Focus::RightPane);
    apply_action(&mut a, Action::DetailClose);

    // Right-clicks on chrome/blank surfaces stay inert (middle-clicks are
    // not inert — they open tabs, covered by the test below).
    let initial = a.selected_unique_id();
    let initial_focus = a.ui_state.focus();
    a.click_target(
        &HitTarget::ListHeader("staging".to_string()),
        MouseButton::Right,
        (5, 3),
    );
    a.click_target(&HitTarget::Minimap, MouseButton::Right, (5, 3));
    a.click_target(&HitTarget::StatusBar, MouseButton::Right, (5, 3));
    assert_eq!(
        a.selected_unique_id(),
        initial,
        "non-node surfaces select nothing"
    );
    assert_eq!(
        a.ui_state.focus(),
        initial_focus,
        "inert clicks don't even re-focus a pane"
    );
    assert_eq!(a.mode, Mode::Selection, "no menu from inert targets");
}

#[test]
fn click_target_middle_click_opens_a_tab() {
    let mut a = sample_app();
    // §6 M4: middle-click on a list row or a lineage node opens that model
    // in a NEW tab (the `O` semantics — inserted right of the active tab
    // and activated).
    let row_uid = a.active_list().model_at(1).unwrap().unique_id.clone();
    a.click_target(&HitTarget::ListRow(1), MouseButton::Middle, (5, 3));
    assert_eq!(a.tab_count(), 3, "a row's middle-click opened a tab");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(row_uid.as_str()),
        "the new tab roots on the clicked model"
    );
    a.click_target(
        &HitTarget::LineageNode("model.sample.fct_orders".to_string()),
        MouseButton::Middle,
        (5, 3),
    );
    assert_eq!(a.tab_count(), 4, "a node's middle-click opened a tab");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.sample.fct_orders"),
    );
    // Middle-clicking an already-open model just activates its tab (T2
    // dedupe — the strip never grows a duplicate).
    a.click_target(
        &HitTarget::LineageNode("model.sample.fct_orders".to_string()),
        MouseButton::Middle,
        (5, 3),
    );
    assert_eq!(
        a.tab_count(),
        4,
        "an already-open model dedupes, not appends"
    );
}
