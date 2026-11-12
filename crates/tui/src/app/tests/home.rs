use super::*;

// ---- the fixed home tab + home screen ---------------------------------

#[test]
fn home_tab_is_fixed_unclosable_stateless_and_survives_reload() {
    let mut a = app();
    assert!(a.tabs()[0].home, "index 0 is the home tab");
    assert!(!a.home_active(), "the boot lands on the seeded model tab");

    apply_action(&mut a, Action::HomeGo);
    assert!(a.home_active());
    assert!(matches!(a.mode, Mode::Home), "home is the idle layer there");
    assert!(a.tab().root.is_none(), "home carries no root");
    assert!(a.selected_unique_id().is_none());
    assert!(a.tab().back.is_empty() && a.tab().lineage_cursor.is_none());

    // `X` refuses — the home cell can never leave index 0.
    apply_action(&mut a, Action::TabClose);
    assert_eq!(a.tab_count(), 2);
    assert!(a.tabs()[0].home);
    assert_eq!(a.take_notice().as_deref(), Some("can't close the home tab"));

    // set_root can't be tricked into rooting it either.
    a.set_root(Some(
        "model.jaffle_finance.stg_finance__budgets".to_string(),
    ));
    assert!(a.tab().root.is_none());

    // A reload keeps it while dropping dead model tabs (home always lives —
    // the strip can never be empty).
    a.restore_tabs(
        vec![TabState {
            root: Some("model.jaffle_finance.ghost".to_string()),
            ..TabState::default()
        }],
        Some(0),
    );
    a.reload().expect("reload ok");
    assert_eq!(a.tab_count(), 1);
    assert!(a.tabs()[0].home && a.home_active());
}

#[test]
fn restore_strips_a_second_home_flag() {
    // A malformed restore can't inject a second home tab — the fixed cell is
    // re-seeded at index 0 and every incoming flag is normalised off.
    let mut a = app();
    a.restore_tabs(
        vec![
            TabState::home(),
            TabState {
                root: Some(a.active_list().model_at(0).unwrap().unique_id.clone()),
                ..TabState::default()
            },
        ],
        Some(0),
    );
    assert_eq!(a.tabs().iter().filter(|t| t.home).count(), 1);
    assert!(a.tabs()[0].home && !a.tabs()[1].home);
}

#[test]
fn tilde_lands_on_home_and_digits_skip_it() {
    let mut a = app();
    let (root0, m1, _m2) = three_models(&a);
    a.open_node_as_tab(&m1);

    apply_action(&mut a, Action::HomeGo);
    assert!(a.home_active());
    assert_eq!(
        a.home_resume.as_deref(),
        Some(m1.as_str()),
        "the outgoing model root becomes the Resume row"
    );

    // The digits address MODEL tabs — `1` is tabs[1], `2` tabs[2]; home is
    // reachable only through `~` / its cell / cycling.
    apply_action(&mut a, Action::TabActivate(1));
    assert_eq!(a.selected_unique_id().as_deref(), Some(root0.as_str()));
    apply_action(&mut a, Action::HomeGo);
    apply_action(&mut a, Action::TabActivate(2));
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));
    apply_action(&mut a, Action::HomeGo);
    apply_action(&mut a, Action::TabActivate(9));
    assert!(a.home_active(), "out-of-range digits are a no-op");
}

#[test]
fn home_data_sections_order_and_flat_selectable() {
    let mut a = app();
    let (_root0, m1, _m2) = three_models(&a);
    apply_action(&mut a, Action::BookmarkToggle); // marks the seeded root
    a.open_node_as_tab(&m1); // recent becomes [m1, root0]
    a.enter_home();

    let d = a.home_data();
    let titles: Vec<&str> = d.sections.iter().map(|s| s.title).collect();
    assert_eq!(titles.first(), Some(&"Resume"), "Resume heads the page");
    assert_eq!(titles.last(), Some(&"Actions"), "Actions always exists");
    let pos = |t: &str| titles.iter().position(|x| x == &t);
    // The fixed order: Resume → Recent → Bookmarks → Health → Last run →
    // Diff → Attention → Actions (Health always renders; Last run / Diff
    // only with their data sources — absent under this fixture).
    assert!(
        pos("Recent").unwrap() < pos("Bookmarks").unwrap(),
        "fixed order: Recent before Bookmarks: {titles:?}"
    );
    let health = pos("Health").expect("Health always renders");
    assert!(
        pos("Bookmarks").unwrap() < health && health < pos("Attention").unwrap(),
        "Health sits between Bookmarks and Attention: {titles:?}"
    );
    assert!(
        pos("Attention").unwrap() < pos("Actions").unwrap(),
        "Attention before Actions: {titles:?}"
    );
    // The info sections are non-selectable: header + info rows only, so a
    // click/cursor can never land on them.
    for s in &d.sections {
        if matches!(s.title, "Health" | "Last run" | "Diff") {
            assert!(s.rows.is_empty(), "{} carries info, not rows", s.title);
            assert!(!s.info.is_empty(), "{} renders at least one line", s.title);
        }
    }
    assert_eq!(
        d.info_flat.len(),
        d.sections.iter().map(|s| s.info.len()).sum::<usize>(),
        "info_flat is the sections' info flattened"
    );
    // `selectable` IS the sections' rows flattened — the cursor/click
    // indexing can't drift from what's drawn.
    let flat: Vec<&HomeItem> = d
        .sections
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| &r.item))
        .collect();
    assert_eq!(flat, d.selectable.iter().collect::<Vec<_>>());
    match &d.selectable[0] {
        HomeItem::Model { uid } => assert_eq!(uid, &m1, "Resume = the outgoing tab's root"),
        other => panic!("the Resume row is a model: {other:?}"),
    }
}

#[test]
fn home_move_clamps_never_wraps_and_open_dedupes() {
    let mut a = app();
    let (root0, _m1, _m2) = three_models(&a);
    a.enter_home();
    let n = a.home_data().selectable.len();
    assert!(n > 0, "the home always has rows (Actions)");

    for _ in 0..n + 4 {
        apply_action(&mut a, Action::HomeMove(Direction::Down));
    }
    assert_eq!(a.home_sel, n - 1, "j clamps at the last row");
    for _ in 0..n + 4 {
        apply_action(&mut a, Action::HomeMove(Direction::Up));
    }
    assert_eq!(a.home_sel, 0, "k clamps at the first row");

    // Enter on the Resume row re-activates the already-open seeded tab —
    // the dedupe, not a fresh tab.
    apply_action(&mut a, Action::HomeOpen);
    assert!(!a.home_active());
    assert_eq!(a.selected_unique_id().as_deref(), Some(root0.as_str()));
    assert_eq!(a.tab_count(), 2);
}

#[test]
fn home_command_rows_run_and_unavailable_ones_are_filtered() {
    let mut a = app();
    a.enter_home();
    let stats_i = a
        .home_data()
        .selectable
        .iter()
        .position(|it| {
            matches!(
                it,
                HomeItem::Command {
                    action: Action::StatsOpen,
                }
            )
        })
        .expect("the stats row is runnable");
    apply_action(&mut a, Action::HomePick(stats_i));
    assert!(
        matches!(a.mode, Mode::Stats(_)),
        "the command ran over home"
    );
    // And the modal closes back to home — the idle-mode rule.
    apply_action(&mut a, Action::DetailClose);
    assert!(matches!(a.mode, Mode::Home));

    // The no-unusable-rows rule: `D` without a baseline is NOT a row at
    // all — the shared `unavailable_reason` filters it at build time, so a
    // drawn command can never toast instead of running.
    assert!(
        !a.home_data().selectable.iter().any(|it| matches!(
            it,
            HomeItem::Command {
                action: Action::DiffOpen,
            }
        )),
        "the baseline-less diff row is filtered out of Actions"
    );
}

#[test]
fn home_health_splits_lint_by_severity_and_attention_tones_it() {
    // The fixture carries 2 error + 80 warning findings — the Health gauge
    // tail quotes both counts, and the Attention worklist renders the
    // error-severity rows Bad first, the warn rows Warn after.
    let mut a = app();
    a.enter_home();
    let d = a.home_data();
    let tail = d
        .sections
        .iter()
        .find(|s| s.title == "Health")
        .and_then(|s| s.info.first())
        .map(|i| match i {
            HomeInfo::Gauge { tail, .. } => tail.clone(),
            other => panic!("Health's first info row is the gauge: {other:?}"),
        })
        .expect("the Health section always renders its gauge");
    assert!(
        tail.contains("lint 2 errors, 80 warnings"),
        "the tail splits lint by severity: {tail}"
    );
    let att = d
        .sections
        .iter()
        .find(|s| s.title == "Attention")
        .expect("the fixture has attention rows");
    let lint: Vec<&HomeRow> = att
        .rows
        .iter()
        .filter(|r| r.detail.starts_with("lint "))
        .collect();
    assert!(!lint.is_empty(), "lint findings join the worklist");
    // Error rows tone Bad and lead the warn rows — the fixture's error
    // findings are the missing_primary_key_tests pair.
    let first_warn = lint
        .iter()
        .position(|r| r.detail.starts_with("lint warn: "));
    assert!(
        lint.iter()
            .take(first_warn.unwrap_or(lint.len()))
            .all(|r| r.tone == HomeTone::Bad && r.detail.starts_with("lint error: ")),
        "error rows lead and tone Bad: {lint:?}"
    );
    assert!(
        lint.iter()
            .any(|r| r.detail == "lint error: missing_primary_key_tests"),
        "the gating rule's row is present: {lint:?}"
    );
    if let Some(i) = first_warn {
        assert!(
            lint[i..]
                .iter()
                .all(|r| r.tone == HomeTone::Warn && r.detail.starts_with("lint warn: ")),
            "warn rows follow and tone Warn: {lint:?}"
        );
    }

    // A warn-severity finding reads `lint warn:` and tones Warn — a
    // staging→staging edge pinned to `staging_depends_on_staging` alone.
    // stg_b carries a test so the untested class can't claim it first (the
    // `seen` dedupe keeps a model's FIRST/worst row).
    let dag = pr_dag(
        vec![
            ("model.p.stg_a", pr_model("stg_a", "staging", "select 1")),
            ("model.p.stg_b", pr_model("stg_b", "staging", "select 1")),
            (
                "test.p.stg_b_unique",
                crate::RawNode {
                    name: "stg_b_unique".into(),
                    resource_type: "test".into(),
                    attached_node: Some("model.p.stg_b".to_string()),
                    test_metadata: Some(crate::RawTestMetadata {
                        name: Some("unique".into()),
                        kwargs: None,
                    }),
                    ..Default::default()
                },
            ),
        ],
        &[("model.p.stg_a", "model.p.stg_b")],
        vec![],
    );
    let mut b = App::new(dag, PathBuf::from("/p/manifest.json"));
    let mut cfg = crate::LintConfig::default();
    for r in crate::LintRule::ALL {
        cfg.severities.insert(r, None);
    }
    cfg.severities.insert(
        crate::LintRule::StagingDependsOnStaging,
        Some(crate::Severity::Warn),
    );
    b.set_lint_config(cfg);
    b.enter_home();
    let d = b.home_data();
    let att = d
        .sections
        .iter()
        .find(|s| s.title == "Attention")
        .expect("warn rows render");
    let row = att
        .rows
        .iter()
        .find(|r| r.detail.starts_with("lint warn: "))
        .expect("the warn-severity row");
    assert_eq!(row.tone, HomeTone::Warn);
    assert_eq!(row.detail, "lint warn: staging_depends_on_staging");
}

#[test]
fn home_search_filters_and_enter_opens_a_tab() {
    let mut a = app();
    a.enter_home();
    apply_action(&mut a, Action::SearchOpen);
    let Mode::Search(s) = &a.mode else {
        panic!("`/` opens a search over home")
    };
    assert_eq!(s.target, SearchTarget::Home);

    // An empty query lists every model; typing narrows by fuzzy match.
    let all = a.home_data().search.as_ref().unwrap().results.len();
    assert!(all >= 3, "the fixture has models to search");
    for c in "shoppers".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    let d = a.home_data();
    let results = &d.search.as_ref().unwrap().results;
    assert!(
        !results.is_empty() && results.len() < all,
        "the query narrowed the list"
    );
    let top = match &results[0].item {
        HomeItem::Model { uid } => uid.clone(),
        other => panic!("results are model rows: {other:?}"),
    };
    apply_action(&mut a, Action::SearchConfirm);
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(top.as_str()),
        "Enter opened the highlighted result as a tab"
    );
    assert!(!a.home_active());

    // Re-open and Esc: cancel restores nothing and lands back on home.
    apply_action(&mut a, Action::HomeGo);
    apply_action(&mut a, Action::SearchOpen);
    apply_action(&mut a, Action::SearchCancel);
    assert!(
        matches!(a.mode, Mode::Home),
        "Esc returns to home, not Selection"
    );
}

#[test]
fn home_row_clicks_open_like_enter() {
    let mut a = app();
    let (_root0, m1, _m2) = three_models(&a);
    a.open_node_as_tab(&m1);
    a.enter_home();
    let area = Rect::new(0, 0, 80, 24);
    let panes = pane_layout(area, &a.ui_state.pane_config());
    let body = panes.list.union(panes.lineage).union(panes.inspector);
    let data = a.home_data();
    let lines = crate::ui::home_window(&crate::ui::home_lines(&data), body.height as usize, 0);
    let mut g = hit_geom(&a, None, area);
    g.home = Some(lines.clone());
    // Chrome lines are inert — only drawn `Row(i)` cells click.
    for (i, l) in lines.iter().enumerate() {
        let t = hit_target(&g, body.x + 2, body.y + i as u16);
        match l {
            crate::ui::HomeLine::Row(flat) => {
                assert_eq!(t, Some(HitTarget::HomeRow(*flat)), "line {i} ({l:?})")
            }
            _ => assert_eq!(t, None, "line {i} ({l:?}) is dead chrome"),
        }
    }
    // A row click dispatches HomePick — the seeded root's tab activates.
    a.click_target(&HitTarget::HomeRow(0), MouseButton::Left, (0, 0));
    assert_eq!(a.selected_unique_id().as_deref(), Some(m1.as_str()));
    assert_eq!(a.tab_count(), 3, "dedupe activated, not appended");
}
