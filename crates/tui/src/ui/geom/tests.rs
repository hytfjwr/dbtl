use super::*;
use crate::app::{HomeData, HomeInfo, HomeItem, HomeRow, HomeSection, HomeTone};

/// A minimal `HomeData` (no Dag needed — it's plain display data) with
/// one section of `rows` selectable model rows.
fn home_fixture(rows: usize) -> HomeData {
    let make_row = |i: usize| HomeRow {
        item: HomeItem::Model {
            uid: format!("model.p.m{i}"),
        },
        label: format!("m{i}"),
        detail: String::new(),
        tone: HomeTone::Normal,
    };
    HomeData {
        project: "p".to_string(),
        source: "manifest m.json".to_string(),
        scope: "project",
        info_flat: vec![HomeInfo::Text(format!("{rows} models"))],
        search: None,
        sections: vec![HomeSection {
            title: "Recent",
            info: vec![HomeInfo::Text(format!("{rows} models"))],
            rows: (0..rows).map(make_row).collect(),
            tail: None,
        }],
        selectable: (0..rows).map(|i| make_row(i).item).collect(),
    }
}

#[test]
fn home_lines_orders_chrome_then_sections() {
    // §3's fixed plan: title, pad, search bar, pad — then one header
    // per section followed by its info content rows and flat-indexed
    // selectable rows, and the fixed `Tip` line closes the plan
    // (search replaces it all).
    let lines = home_lines(&home_fixture(2));
    assert_eq!(
        lines,
        vec![
            HomeLine::Title,
            HomeLine::Pad,
            HomeLine::SearchBar,
            HomeLine::Pad,
            HomeLine::SectionHeader("Recent"),
            HomeLine::Info(0),
            HomeLine::Row(0),
            HomeLine::Row(1),
            HomeLine::Pad,
            HomeLine::Tip,
        ]
    );
}

#[test]
fn home_window_clips_and_counts_dropped_rows() {
    // A 7-row window into an 11-line plan keeps the top chrome and folds
    // the overflow into `+N more` (the count names every dropped row —
    // the one it replaced included).
    let plan = home_lines(&home_fixture(3)); // 11 lines
    let w = home_window(&plan, 7, 0);
    assert_eq!(w.len(), 7);
    assert_eq!(w.last(), Some(&HomeLine::More(5)), "5 of the 11 lines clip");
    // Scrolling to the cursor's line shows the tail instead.
    let w = home_window(&plan, 7, 1);
    assert_eq!(w.first(), Some(&HomeLine::Pad));
    assert_eq!(w.last(), Some(&HomeLine::More(4)));
    // `home_line_of` maps a flat row index to its plan position — the
    // ensure-visible input the loop scrolls to.
    assert_eq!(home_line_of(&plan, 0), 6);
    assert_eq!(home_line_of(&plan, 2), 8);
    assert_eq!(home_line_of(&plan, 99), 10, "stale cursors land at the end");
}

#[test]
fn home_hit_routes_rows_and_drops_chrome() {
    // The body band under home routes `Row(i)` cells to `HomeRow(i)`;
    // title/health/headers/pads/footer are dead cells.
    let data = home_fixture(2);
    let plan = home_lines(&data);
    let area = Rect::new(0, 0, 40, 12);
    let list = crate::model_list::build_model_list(
        &crate::load_dag(HIT_FIXTURE).expect("fixture loads"),
        crate::SortMode::Layer,
    );
    let g = HitGeometry {
        panes: pane_layout(
            area,
            &PaneConfig {
                tab_bar: true,
                ..cfg()
            },
        ),
        tab_bar: None,
        home: Some(home_window(&plan, 9, 0)),
        list: &list,
        list_offset: 0,
        list_selected: 0,
        layout: None,
        scroll_x: 0,
        scroll_y: 0,
        minimap: false,
        overlay: None,
        status: None,
        title: None,
    };
    // Body starts under the tab strip (row 1): plan line 0 (Title) is
    // dead, line 6 (Row 0) hits, line 7 (Row 1) hits; the window's last
    // line collapses the still-hidden Pad+Tip into `More` (inert).
    assert_eq!(hit_target(&g, 2, 1), None, "the title line is inert");
    assert_eq!(hit_target(&g, 2, 7), Some(HitTarget::HomeRow(0)));
    assert_eq!(hit_target(&g, 2, 8), Some(HitTarget::HomeRow(1)));
    assert_eq!(hit_target(&g, 2, 9), None, "the +N more line is inert");
    // The search bar, headers, and info content rows never click.
    assert_eq!(hit_target(&g, 2, 3), None, "the search bar is inert");
    assert_eq!(hit_target(&g, 2, 5), None, "the section header is inert");
    assert_eq!(hit_target(&g, 2, 6), None, "the info row is inert");
}

fn cfg() -> PaneConfig {
    PaneConfig::default()
}

#[test]
fn pane_layout_default_is_list_34_plus_lineage() {
    // §1: the list defaults to 34 columns regardless of terminal width —
    // NOT a ratio (the old 40/60 split is gone).
    for w in [80u16, 100, 160, 220] {
        let area = Rect::new(0, 0, w, 30);
        let p = pane_layout(area, &cfg());
        assert_eq!(p.list.width, DEFAULT_LIST_WIDTH, "{w}: list is 34");
        assert_eq!(p.list, Rect::new(0, 0, 34, 29), "{w}: list spans the body");
        assert_eq!(
            p.lineage,
            Rect::new(34, 0, w - 34, 29),
            "{w}: lineage takes the rest"
        );
        assert_eq!(p.inspector.width, 0, "{w}: inspector hidden by default");
        assert_eq!(p.tab_bar.height, 0, "{w}: tab bar hidden by default");
        assert_eq!(
            p.status,
            Rect::new(0, 29, w, 1),
            "{w}: status is the last row"
        );
    }
}

#[test]
fn pane_layout_clamps_saved_width_to_min_and_cap() {
    // A saved width below the floor lifts to it; above the 60% cap drops
    // to it (the cap is terminal-width-relative, unlike the floor).
    let area = Rect::new(0, 0, 100, 30);
    let low = pane_layout(
        area,
        &PaneConfig {
            list_width: 4,
            ..cfg()
        },
    );
    assert_eq!(low.list.width, MIN_LIST_WIDTH, "saved 4 -> floor 18");
    let high = pane_layout(
        area,
        &PaneConfig {
            list_width: 200,
            ..cfg()
        },
    );
    assert_eq!(high.list.width, 60, "saved 200 -> cap 60% of 100");
    // A bigger terminal raises the cap with it.
    let wide = pane_layout(
        Rect::new(0, 0, 200, 30),
        &PaneConfig {
            list_width: 200,
            ..cfg()
        },
    );
    assert_eq!(wide.list.width, 120, "cap tracks the terminal");
}

#[test]
fn pane_layout_squeezes_displayed_width_below_floor_when_narrow() {
    // §5: the saved value is untouched while the DISPLAYED width shrinks.
    // Below §12's 80-column band edge the LIST never sits inline — the
    // squeeze can't demonstrate on it (its only surface there is the
    // drawer), so the INSPECTOR carries the below-floor demo: body 55,
    // saved 42 → 50%-cap 27 → floor 30, then the deficit squeezes the
    // displayed width to 15 — below the floor, the saved value intact.
    let cfg = PaneConfig {
        inspector_visible: true,
        ..cfg()
    };
    let p = pane_layout(Rect::new(0, 0, 55, 30), &cfg);
    assert_eq!(p.list.width, 0, "§12's band hides the inline list");
    assert_eq!(p.inspector.width, 15, "55 - 40 = a 15-column inspector");
    assert_eq!(p.lineage.width, MIN_LINEAGE_WIDTH);
    assert_eq!(cfg.inspector_width, 42, "the saved width never changes");
}

#[test]
fn terminal_too_small_boundary() {
    // §12's usable floor: EITHER axis below it gates the app.
    let small = |w: u16, h: u16| terminal_too_small(Rect::new(0, 0, w, h));
    assert!(small(49, 12), "one column under the width floor");
    assert!(small(50, 11), "one row under the height floor");
    assert!(small(0, 0) && small(1, 30), "degenerate areas count too");
    assert!(!small(50, 12), "exactly the floor is usable");
    assert!(!small(200, 60), "a comfortable terminal is usable");
}

#[test]
fn pane_layout_narrow_band_hides_inline_list() {
    // §12: below 80 columns the list never sits inline — even with the
    // pref on — because the drawer is its only surface there. The pref's
    // saved width is not consulted at all (not even squeezed).
    for w in [50u16, 60, 79] {
        let p = pane_layout(Rect::new(0, 0, w, 30), &cfg());
        assert_eq!(p.list.width, 0, "{w}: the band zeroes the inline list");
        assert_eq!(p.lineage.width, w, "{w}: lineage takes the whole body");
        assert_eq!(p.list_splitter, None, "{w}: no boundary to grab");
    }
    // Exactly 80 restores the inline pane — the band edge is `<`, not `<=`.
    let p = pane_layout(Rect::new(0, 0, LIST_AUTO_HIDE_W, 30), &cfg());
    assert_eq!(p.list.width, DEFAULT_LIST_WIDTH, "80: inline list returns");
}

#[test]
fn pane_layout_drawer_only_floats_in_the_band() {
    // §12: `min(34, 60%)` wide, the body's full height, over its left edge.
    let open = PaneConfig {
        list_drawer: true,
        ..cfg()
    };
    let p = pane_layout(Rect::new(0, 0, 70, 30), &open);
    // body is 29 rows (30 - status); min(34, 60% of 70 = 42) = 34.
    assert_eq!(p.list_drawer, Some(Rect::new(0, 0, 34, 29)));
    // The 60% cap binds before the 34-col default on the narrowest bands.
    let p = pane_layout(Rect::new(0, 0, 50, 30), &open);
    assert_eq!(
        p.list_drawer,
        Some(Rect::new(0, 0, 30, 29)),
        "60% of 50 = 30 < 34"
    );
    // Above the band the flag is inert — the same `true` resolves None.
    let p = pane_layout(Rect::new(0, 0, LIST_AUTO_HIDE_W, 30), &open);
    assert_eq!(p.list_drawer, None, "80+: no drawer, inline list instead");
    assert_eq!(p.list.width, DEFAULT_LIST_WIDTH);
    // And a closed drawer allocates nothing anywhere.
    let p = pane_layout(Rect::new(0, 0, 70, 30), &cfg());
    assert_eq!(p.list_drawer, None);
}

#[test]
fn hit_target_drawer_routes_interior_chrome_and_exterior() {
    let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
    let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
    let g = HitGeometry {
        panes: pane_layout(
            Rect::new(0, 0, 70, 24),
            &PaneConfig {
                list_drawer: true,
                ..cfg()
            },
        ),
        tab_bar: None,
        list: &list,
        list_offset: 0,
        list_selected: 0,
        home: None,
        layout: None,
        scroll_x: 0,
        scroll_y: 0,
        minimap: false,
        overlay: None,
        status: None,
        title: None,
    };
    // The drawer floats at x 0..34 over the body's 23 rows; its interior
    // (x 1..33, y 1..22) resolves EXACTLY like the inline list's — header
    // on row 0, the first model on row 1 (the same display-row space).
    let layer = match &list.rows[0] {
        DisplayRow::Header { layer, .. } => layer.clone(),
        other => panic!("row 0 must be a header, got {other:?}"),
    };
    assert_eq!(hit_target(&g, 2, 1), Some(HitTarget::ListHeader(layer)));
    assert_eq!(hit_target(&g, 2, 2), Some(HitTarget::ListRow(0)));
    // Its border is inert chrome — the click neither acts nor dismisses.
    for (x, y) in [(0, 2), (33, 2), (2, 0), (2, 22)] {
        assert_eq!(
            hit_target(&g, x, y),
            Some(HitTarget::ModalInterior),
            "{x},{y} on the drawer frame"
        );
    }
    // Everything outside dismisses: the covered lineage cells, the
    // (zero-height) tab row's band, and the status line alike.
    for (x, y) in [(40, 2), (40, 0), (40, 23), (0, 23)] {
        assert_eq!(
            hit_target(&g, x, y),
            Some(HitTarget::ModalExterior),
            "{x},{y} outside the drawer"
        );
    }
    // A covering overlay still outranks it — the modal surface wins.
    let g = HitGeometry {
        overlay: Some(Rect::new(10, 5, 30, 10)),
        ..HitGeometry {
            panes: g.panes,
            tab_bar: None,
            list: &list,
            list_offset: 0,
            list_selected: 0,
            home: None,
            layout: None,
            scroll_x: 0,
            scroll_y: 0,
            minimap: false,
            overlay: None,
            status: None,
            title: None,
        }
    };
    assert_eq!(
        hit_target(&g, 12, 6),
        Some(HitTarget::OverlayRow(0)),
        "a real modal's row beats the drawer"
    );
    assert_eq!(hit_target(&g, 2, 2), Some(HitTarget::ModalExterior));
}

#[test]
fn pane_layout_hidden_list_gives_lineage_the_full_body() {
    let area = Rect::new(0, 0, 100, 30);
    let hidden = pane_layout(
        area,
        &PaneConfig {
            list_visible: false,
            ..cfg()
        },
    );
    assert_eq!(hidden.list.width, 0, "hidden list is zero-width");
    assert_eq!(
        hidden.lineage,
        Rect::new(0, 0, 100, 29),
        "lineage takes the whole body (status line excluded)"
    );
    assert_eq!(hidden.list_splitter, None, "no boundary to grab");
    assert_eq!(
        hidden.status,
        Rect::new(0, 29, 100, 1),
        "status line is unaffected"
    );
}

#[test]
fn pane_layout_splitter_is_the_adjacent_two_border_columns() {
    // §5's strip: the list's right border column plus the lineage's left
    // border column, spanning the whole body — either side grabs.
    let p = pane_layout(Rect::new(0, 0, 100, 30), &cfg());
    assert_eq!(
        p.list_splitter,
        Some(Rect::new(33, 0, 2, 29)),
        "the 2-column strip between the panes"
    );
    // The strip's cells resolve to the List boundary, not to a pane
    // interior — verified against hit_target in the app tests.
}

#[test]
fn pane_layout_inspector_takes_the_right_columns() {
    // Issue 04's pane, driven through the same config: a 160-col terminal
    // shows list 34 + inspector 42 + the lineage in between, with a second
    // splitter on the lineage|inspector edge.
    let p = pane_layout(
        Rect::new(0, 0, 160, 30),
        &PaneConfig {
            inspector_visible: true,
            ..cfg()
        },
    );
    assert_eq!(p.list.width, 34);
    assert_eq!(p.inspector, Rect::new(118, 0, 42, 29));
    assert_eq!(p.lineage, Rect::new(34, 0, 84, 29));
    assert_eq!(p.inspector_splitter, Some(Rect::new(117, 0, 2, 29)));
    assert_eq!(p.list_splitter, Some(Rect::new(33, 0, 2, 29)));
}

#[test]
fn pane_layout_tab_bar_claims_a_row_above_the_body() {
    // Issue 03's strip: with the flag on, the body starts one row lower —
    // status is untouched.
    let p = pane_layout(
        Rect::new(0, 0, 100, 30),
        &PaneConfig {
            tab_bar: true,
            ..cfg()
        },
    );
    assert_eq!(p.tab_bar, Rect::new(0, 0, 100, 1));
    assert_eq!(p.list.y, 1, "the body moves down one row");
    assert_eq!(p.list.height, 28);
    assert_eq!(p.status, Rect::new(0, 29, 100, 1));
    assert_eq!(p.list_splitter, Some(Rect::new(33, 1, 2, 28)));
}

/// A strip label set `tab_bar_layout` can pack predictably: cell width =
/// ` {label} × ` = chars + 4.
fn labels(ns: &[&str]) -> Vec<String> {
    ns.iter().map(|s| s.to_string()).collect()
}

const HIT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/manifest.json"
);

#[test]
fn tab_bar_layout_packs_whole_cells_then_plus_then_project() {
    // 100 cols, 3 short tabs, project name pinned right: zone_w = 100 -
    // 3 (plus) - 7 (project) = 90; cells "1 aa"=8, "2 bbb"=9, "3 c"=7.
    let strip = Rect::new(0, 0, 100, 1);
    let lay = tab_bar_layout(
        strip,
        &labels(&["1 aa", "2 bbb", "3 c"]),
        &[true; 3],
        1,
        0,
        "myproj",
        true,
    );
    assert_eq!(lay.scroll, 0);
    assert!(lay.left_arrow.is_none() && lay.right_arrow.is_none());
    assert_eq!(lay.tabs.len(), 3);
    assert_eq!(lay.tabs[0].rect, Rect::new(0, 0, 8, 1));
    assert_eq!(
        lay.tabs[0].close_x,
        Some(6),
        "the x sits 2 in from the right"
    );
    assert_eq!(lay.tabs[1].rect, Rect::new(8, 0, 9, 1));
    assert_eq!(lay.tabs[2].rect, Rect::new(17, 0, 7, 1));
    assert_eq!(lay.plus, Rect::new(90, 0, 3, 1), "+ pinned at strip end");
    assert_eq!(lay.project, Some(Rect::new(93, 0, 7, 1)));
    assert_eq!(lay.area, strip);
}

#[test]
fn tab_bar_layout_overflow_shows_arrows_and_whole_cells_only() {
    // 30 cols, no project (zone_w = 27): cells "1 alpha"=11, "2 beta"=10,
    // "3 gamma"=11, "4 delta"=11 — 11+10=21 fits under the `›`-adjusted
    // budget (26), +11 doesn't → `›`.
    let strip = Rect::new(0, 0, 30, 1);
    let all = labels(&["1 alpha", "2 beta", "3 gamma", "4 delta"]);
    let lay = tab_bar_layout(strip, &all, &[true; 4], 0, 0, "", true);
    assert!(lay.left_arrow.is_none(), "scrolled to the left edge");
    let arrow = lay.right_arrow.expect("tabs hide on the right");
    assert_eq!(arrow, Rect::new(26, 0, 1, 1), "› at the zone's last col");
    // Whole-cell packing under the arrow-adjusted budget.
    let drawn: Vec<usize> = lay.tabs.iter().map(|c| c.index).collect();
    assert_eq!(drawn, vec![0, 1]);
    let mut x = 0;
    for c in &lay.tabs {
        assert_eq!(c.rect.x, x);
        x += c.rect.width;
    }
    assert!(x <= 26, "cells stay left of the arrow");
}

#[test]
fn tab_bar_layout_ensure_active_pulls_the_active_tab_into_view() {
    let strip = Rect::new(0, 0, 30, 1);
    let all = labels(&["1 alpha", "2 beta", "3 gamma", "4 delta"]);
    // Without ensure, a stored scroll just replays — the active tab may
    // be out of the window (manual scrolls may look around).
    let lay = tab_bar_layout(strip, &all, &[true; 4], 3, 0, "", false);
    assert!(!lay.tabs.iter().any(|c| c.index == 3));
    // With ensure (a switch/structure change) the window bumps right
    // until the active tab is inside — and `‹` appears.
    let lay = tab_bar_layout(strip, &all, &[true; 4], 3, 0, "", true);
    assert!(lay.tabs.iter().any(|c| c.index == 3));
    assert!(lay.left_arrow.is_some(), "scrolled in from the left");
    assert_eq!(
        lay.left_arrow.unwrap(),
        Rect::new(0, 0, 1, 1),
        "‹ at the zone's first col"
    );
    assert_eq!(
        lay.tabs.last().unwrap().index,
        3,
        "the active tab is the window's tail"
    );
}

#[test]
fn tab_bar_layout_narrow_strip_sheds_project_before_plus() {
    // 20 cols: project can't coexist with 10 cols of tab zone — the name
    // drops, ` + ` stays pinned.
    let strip = Rect::new(0, 0, 20, 1);
    let lay = tab_bar_layout(
        strip,
        &labels(&["1 aa"]),
        &[true; 1],
        0,
        0,
        "longprojectname",
        true,
    );
    assert!(lay.project.is_none(), "the name sheds first");
    assert_eq!(lay.plus, Rect::new(17, 0, 3, 1));
    // An empty strip is a no-op (the bar hides below the size floor).
    let lay = tab_bar_layout(
        Rect::new(0, 0, 0, 1),
        &labels(&["1 aa"]),
        &[true; 1],
        0,
        0,
        "p",
        true,
    );
    assert!(lay.tabs.is_empty() && lay.plus.width == 0);
}

#[test]
fn hit_target_resolves_tab_cells_arrows_close_and_plus() {
    // 30 cols, four tabs → the same overflow scene as the layout test:
    // `›` at x=26, cells for tabs 0-2, ` + ` at x=27..30.
    let strip = Rect::new(0, 0, 30, 1);
    let all = labels(&["1 alpha", "2 beta", "3 gamma", "4 delta"]);
    let tb = tab_bar_layout(strip, &all, &[true; 4], 0, 0, "", true);
    let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
    let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
    let g = HitGeometry {
        panes: pane_layout(
            Rect::new(0, 0, 30, 10),
            &PaneConfig {
                tab_bar: true,
                ..cfg()
            },
        ),
        tab_bar: Some(tb),
        home: None,
        list: &list,
        list_offset: 0,
        list_selected: 0,
        layout: None,
        scroll_x: 0,
        scroll_y: 0,
        minimap: false,
        overlay: None,
        status: None,
        title: None,
    };
    assert_eq!(hit_target(&g, 1, 0), Some(HitTarget::Tab(0)));
    // The x cell: tab 0's close_x is rect.x + w - 2 = 9.
    assert_eq!(hit_target(&g, 9, 0), Some(HitTarget::TabClose(0)));
    assert_eq!(hit_target(&g, 15, 0), Some(HitTarget::Tab(1)));
    assert_eq!(hit_target(&g, 26, 0), Some(HitTarget::TabScrollRight));
    assert_eq!(hit_target(&g, 28, 0), Some(HitTarget::TabNew));
    // A point below the strip doesn't report a tab target.
    assert_ne!(hit_target(&g, 1, 3), Some(HitTarget::Tab(0)));
}

#[test]
fn pane_layout_narrow_squeeze_prefers_lineage_floor_then_order() {
    // Inspector shown at 90 cols: desired 34 + 42 leaves lineage 14.
    // Pass 1 (honour floors): inspector 42->30, list 34->20 — every floor
    // kept. The saved values stay intact.
    let cfg = PaneConfig {
        inspector_visible: true,
        ..cfg()
    };
    let p = pane_layout(Rect::new(0, 0, 90, 30), &cfg);
    assert_eq!(
        (p.list.width, p.lineage.width, p.inspector.width),
        (20, 40, 30),
        "surplus shed in inspector->list order, floors held"
    );
    // 80 cols: floors can't all hold — the inspector's own floor breaks
    // first (the least essential pane), the list keeps 18. (80 is also
    // §12's band edge — the inline list still exists at exactly 80.)
    let p = pane_layout(Rect::new(0, 0, 80, 30), &cfg);
    assert_eq!(
        (p.list.width, p.lineage.width, p.inspector.width),
        (18, 40, 22),
        "inspector dips below its floor before the list does"
    );
    // 55 cols — inside §12's band: the inline list is gone entirely (its
    // only surface is the drawer), so the squeeze never touches it; the
    // inspector dips below its floor to 15 and the lineage keeps its 40.
    let p = pane_layout(Rect::new(0, 0, 55, 30), &cfg);
    assert_eq!(p.list.width, 0, "§12's band hides the inline list");
    assert_eq!((p.lineage.width, p.inspector.width), (40, 15));
    // Narrower still the inspector too is squeezed away.
    let p = pane_layout(Rect::new(0, 0, 50, 30), &cfg);
    assert_eq!(p.inspector.width, 10);
    assert_eq!(p.inspector_splitter, Some(Rect::new(39, 0, 2, 29)));
}

#[test]
fn pane_layout_max_cap_interacts_with_squeeze() {
    // A saved width past the cap on a narrow terminal: cap first, then the
    // squeeze decides (cap 60% of 80 = 48, then lineage floor shrinks it).
    // Uses 80 — the narrowest terminal where the inline list exists at all
    // under §12's band rule.
    let p = pane_layout(
        Rect::new(0, 0, 80, 30),
        &PaneConfig {
            list_width: 80,
            ..cfg()
        },
    );
    assert_eq!(p.list.width, 40, "80 - 40; the cap already held it to 48");
    assert_eq!(p.lineage.width, 40);
    // Inside the band the saved width is not consulted at all — a 200-col
    // pref displays exactly like the default: zero.
    let p = pane_layout(
        Rect::new(0, 0, 60, 30),
        &PaneConfig {
            list_width: 80,
            ..cfg()
        },
    );
    assert_eq!(p.list.width, 0, "§12's band overrides even a huge pref");
    assert_eq!(p.lineage.width, 60);
}

#[test]
fn pane_layout_degenerate_sizes_never_panic() {
    for (w, h) in [(1u16, 1u16), (3, 3), (10, 2), (40, 5), (18, 10)] {
        for tab_bar in [false, true] {
            for inspector in [false, true] {
                let p = pane_layout(
                    Rect::new(0, 0, w, h),
                    &PaneConfig {
                        tab_bar,
                        inspector_visible: inspector,
                        ..cfg()
                    },
                );
                // The panes tile the body without overlap or overflow.
                assert_eq!(
                    p.list.width + p.lineage.width + p.inspector.width,
                    w,
                    "{w}x{h}: panes tile the full body width"
                );
                assert_eq!(
                    p.list.x + p.list.width,
                    p.lineage.x,
                    "{w}x{h}: list|lineage abut"
                );
                assert_eq!(
                    p.lineage.x + p.lineage.width,
                    p.inspector.x,
                    "{w}x{h}: lineage|inspector abut"
                );
                assert!(
                    p.inspector.x + p.inspector.width <= w,
                    "{w}x{h}: inspector stays inside the body"
                );
            }
        }
    }
}

#[test]
fn clamp_widths_floor_and_terminal_cap() {
    assert_eq!(clamp_list_width(0, 100), 18);
    assert_eq!(clamp_list_width(34, 100), 34);
    assert_eq!(clamp_list_width(80, 100), 60, "60% cap");
    assert_eq!(clamp_inspector_width(0, 100), 30);
    assert_eq!(clamp_inspector_width(80, 100), 50, "50% cap");
    // Degenerate terminal: the floor wins over a cap below it.
    assert_eq!(clamp_list_width(34, 20), 18);
    assert_eq!(clamp_inspector_width(34, 20), 30);
}

#[test]
fn splitter_drag_width_tracks_the_grabbed_cell() {
    // List splitter: the dragged border lands under the pointer. Anchor is
    // the list's fixed left edge (x=0).
    assert_eq!(splitter_drag_width(Splitter::List, 50, 0, 0), 51);
    assert_eq!(splitter_drag_width(Splitter::List, 50, 1, 0), 50);
    // Grabbed the right-hand cell (the lineage's border): same edge
    // tracking, one column over.
    assert_eq!(splitter_drag_width(Splitter::List, 34, 1, 0), 34);
    // Inspector splitter: grows leftward from the terminal's right edge.
    assert_eq!(splitter_drag_width(Splitter::Inspector, 118, 1, 160), 42);
    assert_eq!(splitter_drag_width(Splitter::Inspector, 117, 0, 160), 42);
    assert_eq!(splitter_drag_width(Splitter::Inspector, 100, 0, 160), 59);
    // Out-of-range intent saturates at 0 for the caller to clamp.
    assert_eq!(splitter_drag_width(Splitter::List, 0, 1, 0), 0);
    assert_eq!(splitter_drag_width(Splitter::Inspector, 200, 0, 160), 0);
}

#[test]
fn hit_test_maps_clicks_to_nodes_across_scroll() {
    // Two single-cell-tall labels at known grid positions.
    let rects = HashMap::from([
        (
            "a".to_string(),
            NodeRect {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
        ),
        (
            "b".to_string(),
            NodeRect {
                x: 10,
                y: 4,
                width: 5,
                height: 1,
            },
        ),
    ]);
    let interior = Rect::new(2, 1, 40, 20); // pane interior offset from screen origin

    // Click on 'a' at grid (0,0) with no scroll: screen = interior origin.
    assert_eq!(hit_test(&rects, 0, 0, interior, 2, 1).as_deref(), Some("a"));
    // A blank grid cell → None.
    assert_eq!(hit_test(&rects, 0, 0, interior, 6, 1), None);
    // Click outside the interior → None.
    assert_eq!(hit_test(&rects, 0, 0, interior, 0, 0), None);
    // 'b' at grid (10,4): with scroll (8,3), it sits at screen
    // (interior.x + 10 - 8, interior.y + 4 - 3) = (4, 2). Round-trips.
    assert_eq!(hit_test(&rects, 8, 3, interior, 4, 2).as_deref(), Some("b"));
    // Same screen cell without scroll lands on empty grid → None.
    assert_eq!(hit_test(&rects, 0, 0, interior, 4, 2), None);
}

#[test]
fn clip_edges_flags_only_real_overflow() {
    // Grid fits the viewport (content == grid, offset 0): nothing clipped.
    assert_eq!(clip_edges(50, 10, 0, 0, 50, 10), ClipEdges::default());
    assert!(!clip_edges(50, 10, 0, 0, 50, 10).any());

    // Wider grid, scrolled into the middle: both horizontal edges clipped,
    // vertical fits.
    let e = clip_edges(100, 10, 20, 0, 60, 10);
    assert_eq!(
        (e.left, e.right, e.top, e.bottom),
        (true, true, false, false)
    );

    // Scrolled hard left: left edge flush (no left clip), right still cut.
    let e = clip_edges(100, 10, 0, 0, 60, 10);
    assert_eq!((e.left, e.right), (false, true));

    // Scrolled to the far right edge: right flush, left cut.
    let e = clip_edges(100, 10, 40, 0, 60, 10);
    assert_eq!((e.left, e.right), (true, false));

    // Taller grid scrolled down: top and bottom both clipped.
    let e = clip_edges(50, 100, 0, 30, 50, 20);
    assert_eq!((e.top, e.bottom), (true, true));
}

#[test]
fn scrollbar_thumb_none_when_content_fits() {
    // Exactly fits → no bar.
    assert_eq!(scrollbar_thumb(10, 0, 10, 0, 10), None);
    // Viewport larger than content → no bar.
    assert_eq!(scrollbar_thumb(5, 0, 10, 0, 10), None);
    // Degenerate zero-height track → no bar even when overflowing.
    assert_eq!(scrollbar_thumb(100, 0, 10, 0, 0), None);
}

#[test]
fn scrollbar_thumb_at_top_when_offset_zero() {
    let track_y0 = 1;
    let track_h = 10;
    let (top, len) = scrollbar_thumb(100, 0, 10, track_y0, track_h).expect("overflow → bar");
    assert_eq!(top, track_y0, "offset 0 → thumb flush at the track top");
    assert!(len >= 1, "thumb length is always at least 1");
    assert!(
        top + len <= track_y0 + track_h,
        "thumb stays inside the track"
    );
}

#[test]
fn scrollbar_thumb_flush_bottom_at_max_offset() {
    let track_y0 = 1;
    let track_h = 10;
    let total = 100;
    let viewport = 10;
    let max_off = total - viewport;
    let (top, len) =
        scrollbar_thumb(total, max_off, viewport, track_y0, track_h).expect("overflow → bar");
    assert_eq!(
        top + len,
        track_y0 + track_h,
        "at max offset the thumb is flush against the track bottom"
    );
}

#[test]
fn scrollbar_thumb_len_always_at_least_one() {
    // A huge total vs a 1-cell viewport would round the proportional length
    // to 0 — it must clamp up to 1 so the thumb is never invisible.
    let (_, len) = scrollbar_thumb(10_000, 0, 1, 0, 5).expect("overflow → bar");
    assert_eq!(len, 1, "tiny proportional thumb clamps to >= 1");
}

#[test]
fn scrollbar_thumb_offset_past_max_clamps() {
    let track_y0 = 0;
    let track_h = 8;
    let total = 50;
    let viewport = 10;
    // An offset beyond max_off must not push the thumb past the track bottom.
    let (top, len) =
        scrollbar_thumb(total, 9999, viewport, track_y0, track_h).expect("overflow → bar");
    assert_eq!(
        top + len,
        track_y0 + track_h,
        "out-of-range offset clamps to a flush-bottom thumb"
    );
}

#[test]
fn minimap_scroll_to_centres_the_clicked_block() {
    // 160×60 grid in a 16×6 inset at (80,2): sx = 10, sy = 10.
    let mm = Rect::new(80, 2, 16, 6);
    // Cell (0,0) covers grid [0,10) — its centre is (5,5); a 40×20 window
    // centred there clamps to scroll (0,0).
    assert_eq!(minimap_scroll_to(mm, 80, 2, 160, 60, 40, 20), (0, 0));
    // Cell (8,3) covers grid x [80,90), y [30,40) — centre (85,35) →
    // scroll = centre - view/2 = (65,25).
    assert_eq!(minimap_scroll_to(mm, 88, 5, 160, 60, 40, 20), (65, 25));
    // The last cell saturates toward the grid's far edge (the caller's
    // clamp_lineage lands it on the max offset — here it just can't go
    // below the centre-minus-half floor).
    assert_eq!(minimap_scroll_to(mm, 95, 7, 160, 60, 40, 20), (135, 45));
    // A scrub outside the rect keeps tracking (saturating left/top).
    assert_eq!(minimap_scroll_to(mm, 79, 1, 160, 60, 40, 20), (0, 0));
    assert_eq!(minimap_scroll_to(mm, 99, 9, 160, 60, 40, 20), (175, 65));
}

#[test]
fn pan_offset_tracks_the_pointer_one_to_one() {
    // Grab at (50,·) with scroll 10: drag left 5 → scroll 15 (content
    // follows the pointer); drag right 5 → scroll 5.
    assert_eq!(pan_offset(10, 50, 45), 15);
    assert_eq!(pan_offset(10, 50, 55), 5);
    // Past the diagram's home edge: saturate at 0 (the far edge is the
    // caller's clamp_lineage).
    assert_eq!(pan_offset(4, 50, 60), 0);
    assert_eq!(pan_offset(10, 50, 50), 10, "no movement → no change");
}

#[test]
fn tooltip_rect_hangs_below_right_and_flips_on_overflow() {
    let area = Rect::new(0, 0, 100, 40);
    let at = Position { x: 10, y: 10 };
    // Preferred: one cell below-right of the pointer cell.
    assert_eq!(
        tooltip_rect(at, 30, 6, area),
        Rect::new(11, 11, 30, 6),
        "below-right anchor"
    );
    // Right-edge overflow: the box lands LEFT of the cell.
    assert_eq!(
        tooltip_rect(Position { x: 95, y: 10 }, 30, 6, area),
        Rect::new(65, 11, 30, 6),
        "flipped left"
    );
    // Bottom-edge overflow: the box ends AT the cell's row.
    assert_eq!(
        tooltip_rect(Position { x: 10, y: 39 }, 30, 6, area),
        Rect::new(11, 33, 30, 6),
        "flipped up"
    );
    // Both edges: bottom-left of the cell.
    assert_eq!(
        tooltip_rect(Position { x: 95, y: 39 }, 30, 6, area),
        Rect::new(65, 33, 30, 6),
        "flipped both ways"
    );
    // A box bigger than the area shrinks to it and clamps inside.
    assert_eq!(
        tooltip_rect(Position { x: 50, y: 20 }, 200, 60, area),
        Rect::new(0, 0, 100, 40),
        "clamped into the area"
    );
    // Degenerate area never panics.
    let _ = tooltip_rect(Position { x: 0, y: 0 }, 5, 3, Rect::new(0, 0, 1, 1));
}

// ---- Issue 07: status-line + title chrome geometry ----

fn hint(key: &str, desc: &str, action: crate::Action, prio: u8) -> crate::action::StatusHint {
    crate::action::StatusHint {
        key: key.to_string(),
        desc: desc.to_string(),
        action: Some(action),
        prio,
    }
}

#[test]
fn status_layout_tiles_hints_left_and_chips_right() {
    use crate::Action as A;
    let hints = vec![
        hint("j/k", "move", A::MoveDown, 10),
        hint("?", "help", A::HelpToggle, 90),
    ];
    let chips = vec![
        (StatusChip::Impact, "[impact v1 ^2]".to_string()),
        (StatusChip::Focus, "[list]".to_string()),
    ];
    let l = status_layout(60, &hints, &chips);
    // Hints: " j/k move" = 9 cells, " ? help" = 7 — contiguous from x=0.
    assert_eq!(
        l.hints,
        vec![
            StatusSpan {
                x0: 0,
                x1: 9,
                target: HitTarget::StatusHint(Some(A::MoveDown)),
            },
            StatusSpan {
                x0: 9,
                x1: 16,
                target: HitTarget::StatusHint(Some(A::HelpToggle)),
            },
        ]
    );
    // Chips are a RIGHT-anchored block: " [impact v1 ^2]" = 15, " [list]"
    // = 7 → the 22-cell block occupies [38, 60).
    assert_eq!(
        l.chips,
        vec![
            StatusSpan {
                x0: 38,
                x1: 53,
                target: HitTarget::StatusChip(StatusChip::Impact),
            },
            StatusSpan {
                x0: 53,
                x1: 60,
                target: HitTarget::StatusChip(StatusChip::Focus),
            },
        ]
    );
}

#[test]
fn status_layout_drops_the_hint_tail_then_the_chip_tail() {
    use crate::Action as A;
    // §8's shed order: the hints' low-priority tail first, then — only
    // once the hints are gone — the chips' tail. The kept set is always
    // a PREFIX of each priority-sorted input.
    let hints = vec![
        hint("j/k", "move", A::MoveDown, 10),       // 9 cells
        hint("Enter", "open", A::DetailOpen, 20),   // 11
        hint("/", "search", A::SearchOpen, 30),     // 9
        hint("O", "new tab", A::TabOpen, 40),       // 10
        hint("Tab", "lineage", A::ToggleFocus, 50), // 12
        hint("?", "help", A::HelpToggle, 90),       // 7
    ];
    let chips = vec![
        (StatusChip::Impact, "[i]".to_string()),
        (StatusChip::Focus, "[f]".to_string()),
        (StatusChip::View, "[v]".to_string()),
    ];
    // 58 hint cells + 12 chip cells = 70 > 60: `? help` (7) sheds → 63
    // still over → `Tab lineage` (12) sheds → 51 fits.
    let l = status_layout(60, &hints, &chips);
    assert_eq!(l.hints.len(), 4, "the two tail hints shed");
    assert_eq!(l.chips.len(), 3, "every chip still fits");
    // At 30: all six hints must go (58 alone > 30), then chips shed to
    // fit — impact + focus (8 cells) survive, view drops.
    let l = status_layout(30, &hints, &chips);
    // 30 - hints 9+11+9=29 keeps 3 hints… recompute: 29+12=41>30 → drop
    // "help" (51→)… walk the prefix: the layout keeps the longest hint
    // prefix that leaves the chips their room.
    let kept_hints = l.hints.len();
    let hints_w: u16 = [9u16, 11, 9, 10, 12, 7][..kept_hints].iter().sum();
    let chips_w: u16 = l.chips.len() as u16 * 4;
    assert!(hints_w + chips_w <= 30, "kept prefix fits");
    assert_eq!(
        l.chips.first().map(|s| &s.target),
        Some(&HitTarget::StatusChip(StatusChip::Impact)),
        "impact survives longest — kept chips are a head-prefix"
    );
    // Degenerate: a width smaller than the impact chip alone keeps
    // nothing — never a panic, never a span past the line.
    let l = status_layout(3, &hints, &chips);
    assert!(l.hints.iter().all(|s| s.x1 <= 3) && l.chips.iter().all(|s| s.x1 <= 3));
}

#[test]
fn hit_target_reports_status_spans_then_plain_status_bar() {
    use crate::Action as A;
    let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
    let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
    let area = Rect::new(0, 0, 80, 24);
    let panes = pane_layout(
        area,
        &PaneConfig {
            tab_bar: false,
            ..cfg()
        },
    );
    let hints = vec![hint("j/k", "move", A::MoveDown, 10)];
    let chips = vec![(StatusChip::Focus, "[list]".to_string())];
    let status_l = status_layout(panes.status.width, &hints, &chips);
    let g = HitGeometry {
        panes,
        tab_bar: None,
        home: None,
        list: &list,
        list_offset: 0,
        list_selected: 0,
        layout: None,
        scroll_x: 0,
        scroll_y: 0,
        minimap: false,
        overlay: None,
        status: Some(&status_l),
        title: None,
    };
    let y = panes.status.y;
    // The first hint's span covers its leading-space cell at x=0.
    assert_eq!(
        hit_target(&g, 0, y),
        Some(HitTarget::StatusHint(Some(A::MoveDown)))
    );
    // The right-anchored chip: " [list]" = 7 cells → [73, 80).
    assert_eq!(
        hit_target(&g, 79, y),
        Some(HitTarget::StatusChip(StatusChip::Focus))
    );
    assert_eq!(
        hit_target(&g, 73, y),
        Some(HitTarget::StatusChip(StatusChip::Focus))
    );
    // The dead cells between the two regions stay plain StatusBar.
    assert_eq!(hit_target(&g, 40, y), Some(HitTarget::StatusBar));
}

#[test]
fn hit_target_reports_title_spans_between_the_corner_cells() {
    let dag = crate::load_dag(HIT_FIXTURE).expect("fixture loads");
    let list = crate::model_list::build_model_list(&dag, crate::SortMode::Layer);
    let panes = pane_layout(
        Rect::new(0, 0, 80, 24),
        &PaneConfig {
            tab_bar: false,
            ..cfg()
        },
    );
    // Title spans are RELATIVE to the first title cell (pane.x + 1):
    // a crumb entry at cells 1..4, the view tag at 20..24, lens at 25..34.
    let title_l = TitleLayout {
        text: " a > b > root         [v] [lens:x]".to_string(),
        spans: vec![
            TitleSpan {
                kind: TitleKind::Crumb(2),
                x0: 1,
                x1: 4,
            },
            TitleSpan {
                kind: TitleKind::View,
                x0: 20,
                x1: 24,
            },
            TitleSpan {
                kind: TitleKind::Lens,
                x0: 25,
                x1: 34,
            },
        ],
    };
    let g = HitGeometry {
        panes,
        tab_bar: None,
        home: None,
        list: &list,
        list_offset: 0,
        list_selected: 0,
        layout: None,
        scroll_x: 0,
        scroll_y: 0,
        minimap: false,
        overlay: None,
        status: None,
        title: Some(&title_l),
    };
    let row = panes.lineage.y;
    let x0 = panes.lineage.x + 1;
    assert_eq!(hit_target(&g, x0 + 2, row), Some(HitTarget::TitleCrumb(2)));
    assert_eq!(hit_target(&g, x0 + 21, row), Some(HitTarget::TitleView));
    assert_eq!(hit_target(&g, x0 + 30, row), Some(HitTarget::TitleLens));
    // An uncovered title cell (between the crumb and the view tag) is
    // inert chrome — None, never a stale span claim.
    assert_eq!(hit_target(&g, x0 + 10, row), None);
    // The border corner columns still belong to the splitters/chrome.
    assert_ne!(
        hit_target(&g, panes.lineage.x, row),
        Some(HitTarget::TitleCrumb(2))
    );
}
