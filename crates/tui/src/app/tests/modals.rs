use super::*;

// ---- SQL preview / stats dashboard modals ----

#[test]
fn sql_open_snapshots_raw_code() {
    // A manifest-loaded model carries raw_code; SqlOpen snapshots it into the
    // Mode payload (no Dag in the render layer). pos_txn has clean ASCII SQL.
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => {
            assert_eq!(sv.model_id, "model.jaffle_finance.pos_txn");
            assert_eq!(sv.name, "pos_txn");
            assert!(
                !sv.sql.is_empty() && !sv.sql.starts_with("(no SQL"),
                "real raw_code was snapshotted: {:?}",
                &sv.sql[..sv.sql.len().min(40)]
            );
            assert_eq!(sv.scroll, 0);
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);
    assert!(matches!(a.mode, Mode::Selection), "DetailClose closes Sql");
}

#[test]
fn sql_open_source_shows_placeholder() {
    // A source has no raw_code → the modal shows a placeholder, not a panic.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    // Move the cursor onto an upstream source.
    let src = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("source."))
        .min()
        .cloned()
        .expect("fct has an upstream source");
    a.tab_mut().lineage_cursor = Some(src.clone());
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => {
            assert_eq!(sv.model_id, src);
            assert!(
                sv.sql.starts_with("(no SQL"),
                "source SQL is a placeholder: {}",
                sv.sql
            );
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
}

#[test]
fn sql_open_respects_focus() {
    // Lineage focus previews the CURSOR; list focus previews the selection.
    let mut a = app();
    a.select_by_unique_id(FCT);
    a.ui_state.set_focus(Focus::RightPane);
    let up_model = a
        .dag
        .upstream(FCT)
        .iter()
        .filter(|u| u.starts_with("model."))
        .min()
        .cloned()
        .expect("fct has an upstream model");
    a.tab_mut().lineage_cursor = Some(up_model.clone());
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.model_id, up_model, "lineage focus previews the cursor"),
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);

    // List focus: the cursor is ignored; the selection (the root) is previewed.
    a.ui_state.set_focus(Focus::List);
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.model_id, FCT, "list focus previews the selection"),
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
}

/// A single-model `Dag` whose node carries raw AND manifest `compiled_code`
/// — the `c`-toggle scenario, no filesystem involved (`Dag::build` is pure).
fn compiled_app(raw: &str, compiled: Option<&str>) -> App {
    let mut node = pr_model("m", "marts", raw);
    node.compiled_code = compiled.map(str::to_string);
    node.original_file_path = Some("models/marts/m.sql".into());
    let dag = pr_dag(vec![("model.p.m", node)], &[], vec![]);
    let mut a = App::new(dag, PathBuf::from("/p/target/manifest.json"));
    a.select_by_unique_id("model.p.m");
    a
}

#[test]
fn sql_modal_compiled_toggle_flips_text_and_resets_scroll() {
    let mut a = compiled_app("select {{ ref('x') }}", Some("select 42 /* expanded */"));
    apply_action(&mut a, Action::SqlOpen);
    match &a.mode {
        Mode::Sql(sv) => {
            // Both variants rode in the same open-time snapshot (Dag-free
            // render), and the modal opens on RAW.
            assert_eq!(sv.compiled.as_deref(), Some("select 42 /* expanded */"));
            assert!(!sv.compiled_stale, "manifest compiled_code is never stale");
            assert!(!sv.show_compiled);
            assert_eq!(sv.active_sql(), "select {{ ref('x') }}");
            assert_eq!(sv.variant_label(), "raw");
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    // Wander the scroll, then toggle: the display flips and the scroll resets
    // (raw and compiled are different texts; a carried offset is arbitrary).
    if let Mode::Sql(sv) = &mut a.mode {
        sv.scroll = 5;
    }
    apply_action(&mut a, Action::SqlToggleCompiled);
    match &a.mode {
        Mode::Sql(sv) => {
            assert!(sv.show_compiled);
            assert_eq!(sv.active_sql(), "select 42 /* expanded */");
            assert_eq!(sv.variant_label(), "compiled");
            assert_eq!(sv.scroll, 0, "toggle resets the scroll");
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::SqlToggleCompiled);
    match &a.mode {
        Mode::Sql(sv) => {
            assert!(!sv.show_compiled);
            assert_eq!(sv.variant_label(), "raw");
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
}

#[test]
fn sql_toggle_without_compiled_only_notices() {
    // `c` on a raw-only node: no mode change, a one-shot notice explains why
    // (the fixture manifest has no target/compiled/ dir beside it).
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::SqlOpen);
    apply_action(&mut a, Action::SqlToggleCompiled);
    match &a.mode {
        Mode::Sql(sv) => {
            assert!(!sv.show_compiled && sv.variant_label() == "raw");
            assert_eq!(sv.compiled, None);
        }
        m => panic!("expected Mode::Sql, got {m:?}"),
    }
    assert_eq!(
        a.take_notice().as_deref(),
        Some("no compiled SQL for this node (run dbt compile)")
    );
}

#[test]
fn sql_toggle_ignores_other_modes() {
    // The arm is scoped to Mode::Sql — a stray dispatch elsewhere is a no-op.
    let mut a = app();
    apply_action(&mut a, Action::SqlToggleCompiled);
    assert_eq!(a.mode, Mode::Selection);
    assert_eq!(a.take_notice(), None);
}

#[test]
fn sql_variant_label_names_the_three_states() {
    use crate::action::SqlView;
    let base = SqlView {
        model_id: "m".into(),
        name: "m".into(),
        sql: "raw".into(),
        compiled: None,
        compiled_stale: false,
        show_compiled: false,
        path: None,
        scroll: 0,
    };
    assert_eq!(base.variant_label(), "raw");
    // Compiled flag without a compiled payload can never claim compiled mode.
    let sv = SqlView {
        show_compiled: true,
        ..base.clone()
    };
    assert_eq!(sv.variant_label(), "raw");
    let sv = SqlView {
        compiled: Some("c".into()),
        show_compiled: true,
        ..base.clone()
    };
    assert_eq!(sv.variant_label(), "compiled");
    let sv = SqlView {
        compiled_stale: true,
        ..sv
    };
    assert_eq!(sv.variant_label(), "compiled, stale");
    // …and the fallback keeps the raw text reachable.
    let sv = SqlView {
        compiled: Some("c".into()),
        show_compiled: true,
        ..base.clone()
    };
    assert_eq!(sv.active_sql(), "c");
}

#[test]
fn yank_compiled_sql_copies_the_expansion() {
    let mut a = compiled_app("select raw", Some("select compiled"));
    let out = apply_action(&mut a, Action::YankCompiledSql);
    match &out.effects[..] {
        [Effect::Yank(t)] => assert_eq!(t, "select compiled"),
        other => panic!("expected one Yank effect, got {other:?}"),
    }
    assert_eq!(
        a.take_notice().as_deref(),
        Some("Copied compiled SQL"),
        "fresh copy notice"
    );
}

#[test]
fn sql_modal_opens_on_compiled_when_raw_is_blank() {
    // `raw_code: ""` or whitespace-only is no better than absent — a
    // compiled-only node opens already flipped to [compiled] (there is
    // nothing to look at in [raw]).
    for blank in ["", "  \n\t  "] {
        let mut a = compiled_app(blank, Some("select compiled"));
        apply_action(&mut a, Action::SqlOpen);
        match &a.mode {
            Mode::Sql(sv) => {
                assert!(sv.show_compiled, "blank raw {blank:?} should auto-flip");
                assert_eq!(sv.active_sql(), "select compiled");
                assert_eq!(sv.variant_label(), "compiled");
            }
            m => panic!("expected Mode::Sql, got {m:?}"),
        }
    }
}

#[test]
fn yank_compiled_sql_marks_a_stale_copy() {
    // A stale expansion still copies — the notice flags it. Only
    // `ingest_compiled` can produce `stale: true` (manifest `compiled_code`
    // never is), so this one needs a real target/ tree.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let stamp = |p: &std::path::Path, secs_ago: u64| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago))
            .unwrap();
    };
    let cfile = root.join("target/compiled/p/models/marts/m.sql");
    std::fs::create_dir_all(cfile.parent().unwrap()).unwrap();
    std::fs::write(&cfile, "select compiled").unwrap();
    stamp(&cfile, 3600); // expansion is old…
    let src = root.join("models/marts/m.sql");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, "select raw").unwrap();
    stamp(&src, 60); // …than the source → stale

    let mut node = pr_model("m", "marts", "select raw");
    node.original_file_path = Some("models/marts/m.sql".into());
    node.package_name = Some("p".into());
    let mut dag = pr_dag(vec![("model.p.m", node)], &[], vec![]);
    dag.ingest_compiled(&root.join("target/compiled"), Some(root));
    assert!(
        dag.compiled("model.p.m").unwrap().stale,
        "the stamped setup must yield a stale entry"
    );

    let mut a = App::new(dag, PathBuf::from("/p/target/manifest.json"));
    a.select_by_unique_id("model.p.m");
    let out = apply_action(&mut a, Action::YankCompiledSql);
    match &out.effects[..] {
        [Effect::Yank(t)] => assert_eq!(t, "select compiled"),
        other => panic!("expected one Yank effect, got {other:?}"),
    }
    assert_eq!(
        a.take_notice().as_deref(),
        Some("Copied compiled SQL (stale)")
    );
}

#[test]
fn yank_compiled_sql_without_compiled_notices_and_copies_nothing() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let out = apply_action(&mut a, Action::YankCompiledSql);
    assert!(
        out.effects.is_empty(),
        "no Yank effect when there is nothing to copy"
    );
    assert_eq!(
        a.take_notice().as_deref(),
        Some("no compiled SQL for this node (run dbt compile)")
    );
    // `yC` stays raw-only — the new key did not move it.
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let out = apply_action(&mut a, Action::YankSql);
    match &out.effects[..] {
        [Effect::Yank(t)] => {
            assert_eq!(
                t.as_str(),
                a.dag.raw_code("model.jaffle_finance.pos_txn").unwrap()
            )
        }
        other => panic!("expected one Yank effect, got {other:?}"),
    }
}

#[test]
fn sql_and_yank_compiled_keys_dispatch() {
    use crate::dispatch;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
    // `c` inside the SQL modal is the raw/compiled toggle…
    let sql_mode = Mode::Sql(crate::action::SqlView {
        model_id: "m".into(),
        name: "m".into(),
        sql: String::new(),
        compiled: None,
        compiled_stale: false,
        show_compiled: false,
        path: None,
        scroll: 0,
    });
    assert_eq!(
        dispatch(&sql_mode, press(KeyCode::Char('c'))),
        Some(Action::SqlToggleCompiled)
    );
    // …and `ys` inside the yank prefix copies the compiled SQL.
    assert_eq!(
        dispatch(
            &Mode::Yank { back: None, sel: 0 },
            press(KeyCode::Char('s'))
        ),
        Some(Action::YankCompiledSql)
    );
}

#[test]
fn stats_open_computes_dashboard() {
    let mut a = app();
    apply_action(&mut a, Action::StatsOpen);
    let sv = match &a.mode {
        Mode::Stats(sv) => sv.clone(),
        m => panic!("expected Mode::Stats, got {m:?}"),
    };
    // Coverage base == coverage_summary's (model|seed|snapshot): 45+7+1 = 53,
    // and the sole gap in the fixture is the untested snapshot.
    assert_eq!(
        sv.testable_total, 53,
        "fixture: 45 models + 7 seeds + 1 snapshot"
    );
    assert_eq!(
        sv.untested_testable, 1,
        "fixture: only the snapshot is untested"
    );
    assert_eq!(
        (sv.testable_tested, sv.testable_total),
        a.coverage_summary(),
        "dashboard coverage == the lens/status coverage_summary (single source)"
    );
    let rt = |k: &str| {
        sv.by_resource_type
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, n)| *n)
    };
    assert_eq!(rt("source"), Some(38), "fixture: 38 sources");
    assert_eq!(rt("seed"), Some(7), "fixture: 7 seeds");
    assert_eq!(rt("snapshot"), Some(1), "fixture: 1 snapshot");
    assert_eq!(
        sv.testable_tested + sv.untested_testable,
        sv.testable_total,
        "tested + untested == testable_total"
    );
    assert!(sv.top_hubs.len() <= 5, "at most 5 hubs");
    // Hubs are degree-desc, then unique_id-asc — verify the ordering.
    for w in sv.top_hubs.windows(2) {
        assert!(
            w[0].2 > w[1].2 || (w[0].2 == w[1].2 && w[0].0 <= w[1].0),
            "hubs sorted by degree desc then unique_id asc"
        );
    }

    // --- transitive_hubs: top-5 by downstream-closure size over ALL nodes,
    // count desc then unique_id asc (the seed/source siblings tie at 17, so
    // `seed.* < source.*` and the four pos_* payment sources order by name;
    // pos_shp @17 drops off the cap). Derived by running against the fixture
    // and verified against `Dag::downstream`, not guessed. The leader counts
    // the fixture's two exposures (both downstream of fct, which it feeds):
    // the Dag closure is the raw graph truth, unlike the chip's split counts.
    assert_eq!(
        sv.transitive_hubs,
        vec![
            ("source_datetime_policy".to_string(), 22),
            ("pos_prod_aws_store_master".to_string(), 17),
            ("pos_cat".to_string(), 17),
            ("pos_pay".to_string(), 17),
            ("pos_rcv".to_string(), 17),
        ],
        "top-5 transitive blast-radius hubs (count desc, unique_id tie-break)"
    );
    // Cross-check the leader against the live closure (reuse, not a magic number).
    assert_eq!(
        a.dag
            .downstream("seed.jaffle_finance.source_datetime_policy")
            .len(),
        22,
        "leader's transitive downstream closure size"
    );

    // --- orphans + violations: the committed fixture is clean on both (no
    // disconnected model, no backward layer edge). The cap/listing render is
    // exercised with hand-built StatsView literals in the overlay tests.
    assert_eq!(
        sv.orphan_models,
        Vec::<String>::new(),
        "fixture has no orphans"
    );
    assert_eq!(
        sv.layer_violations,
        Vec::<(String, String)>::new(),
        "fixture has no layer violations (matches layer_violation_edges)"
    );

    // Deterministic: a second compute is bit-identical.
    assert_eq!(a.compute_stats_view(), sv, "compute_stats_view is stable");
}

#[test]
fn sql_and_stats_scroll_through_detail_scroll() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    // SQL modal: DetailScroll steps sv.scroll; up saturates at 0.
    apply_action(&mut a, Action::SqlOpen);
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.scroll, 2),
        m => panic!("expected Sql, got {m:?}"),
    }
    for _ in 0..5 {
        apply_action(&mut a, Action::DetailScroll(Direction::Up));
    }
    match &a.mode {
        Mode::Sql(sv) => assert_eq!(sv.scroll, 0, "scroll up saturates at 0"),
        m => panic!("expected Sql, got {m:?}"),
    }
    apply_action(&mut a, Action::DetailClose);

    // Stats modal: same plumbing.
    apply_action(&mut a, Action::StatsOpen);
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    match &a.mode {
        Mode::Stats(sv) => assert_eq!(sv.scroll, 1),
        m => panic!("expected Stats, got {m:?}"),
    }
}

#[test]
fn feature_toggle_stubs_are_safe_noops() {
    // The 5 view/pref toggles must never quit, emit effects, or leave
    // Selection mode — and the SELECTED MODEL (by unique_id, not raw index:
    // SortCycle legitimately reorders the list) must survive them all.
    let mut a = app();
    a.select_by_unique_id(FCT);
    for action in [
        Action::CycleLens,
        Action::BookmarkToggle,
        Action::BookmarkCycle,
        Action::SortCycle,
        Action::ToggleMinimap,
        Action::PaneNarrow,
        Action::PaneWiden,
        Action::PaneWidthsReset,
    ] {
        let out = apply_action(&mut a, action);
        assert!(!out.quit && out.effects.is_empty(), "{action:?} is a no-op");
        assert!(
            matches!(a.mode, Mode::Selection),
            "{action:?} keeps Selection"
        );
    }
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(FCT),
        "no toggle moved the selection off the selected model"
    );
}

#[test]
fn path_edges_follow_cursor_and_edge_styles_dim_off_path() {
    // Cursor home: no path edges, and the styled layout leaves every connector
    // cell default (the frozen lens-off render). Cursor off-root: the path's
    // edges are canonical (parent, child) keys present in the layout's
    // edge_cells; their cells get the on_path band and every other edge dims.
    let mut a = app();
    a.select_by_unique_id(FCT);
    assert!(
        a.lineage_path_edges().is_empty(),
        "cursor home: no path edges"
    );
    let lay = a.styled_lineage_layout().expect("non-empty lineage");
    let plain_cell = lay.edge_cells.values().flatten().next().copied();
    if let Some((x, y)) = plain_cell {
        assert_eq!(
            lay.grid.attr_at(x, y),
            crate::CellAttr::default(),
            "cursor home: connectors stay Plain"
        );
    }

    a.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut a, Action::MoveLeft);
    let path_edges = a.lineage_path_edges();
    assert!(
        !path_edges.is_empty(),
        "off-root cursor produces path edges"
    );
    let lay = a.styled_lineage_layout().expect("non-empty lineage");
    for key in &path_edges {
        let cells = lay
            .edge_cells
            .get(key)
            .unwrap_or_else(|| panic!("path edge {key:?} exists in edge_cells"));
        for &(x, y) in cells {
            let attr = lay.grid.attr_at(x, y);
            assert!(attr.on_path, "path edge cell ({x},{y}) carries the band");
            assert!(!attr.dimmed, "path edge cell is not dimmed");
        }
    }
    // Some off-path edge exists in the FCT subgraph and dims. Skip cells shared
    // with a path edge (siblings into one child share its arrowhead cell, and
    // the path attr deliberately wins those).
    let path_set: std::collections::HashSet<_> = path_edges.iter().collect();
    let path_cells: std::collections::HashSet<(usize, usize)> = path_edges
        .iter()
        .filter_map(|k| lay.edge_cells.get(k))
        .flatten()
        .copied()
        .collect();
    let (x, y) = lay
        .edge_cells
        .iter()
        .filter(|(k, _)| !path_set.contains(k))
        .flat_map(|(_, cells)| cells)
        .find(|c| !path_cells.contains(c))
        .copied()
        .expect("an off-path connector cell exists");
    assert!(lay.grid.attr_at(x, y).dimmed, "off-path connector dims");
}

#[test]
fn toggle_density_reshapes_the_cached_layout_and_the_yank() {
    // `v` flips the density: the cached styled layout rebuilds 1-row boxes
    // (density is part of the cache key) and the ASCII yank IS the pane's
    // diagram, so it follows. Toggling back restores the 3-row geometry.
    let mut a = app();
    a.select_by_unique_id(FCT);
    let tall = a.styled_lineage_layout().expect("lineage");
    assert_eq!(tall.rects[FCT].height, 3, "default: comfortable boxes");
    let tall_yank = a.lineage_ascii().expect("yank");

    let out = apply_action(&mut a, Action::ToggleDensity);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.ui_state.density(), crate::Density::Compact);
    let flat = a.styled_lineage_layout().expect("lineage");
    assert_eq!(flat.rects[FCT].height, 1, "compact: 1-row boxes");
    assert!(
        flat.grid.height() < tall.grid.height(),
        "compact grid is shorter"
    );
    let flat_yank = a.lineage_ascii().expect("yank");
    assert_ne!(flat_yank, tall_yank, "the yank follows the density");
    assert_eq!(flat_yank, flat.grid.to_text(), "yank IS the pane's diagram");

    apply_action(&mut a, Action::ToggleDensity);
    assert_eq!(a.ui_state.density(), crate::Density::Comfortable);
    assert_eq!(
        a.styled_lineage_layout().expect("lineage").rects[FCT].height,
        3,
        "toggling back restores comfortable geometry"
    );
}

#[test]
fn compact_density_keeps_cursor_movement_and_path_highlight_working() {
    // The spatial cursor and the path/edge highlight read the CACHED layout's
    // geometry, which reshapes under compact density (1-row rects, attach row
    // = the node's own row): h must still step one column upstream, and the
    // path edges must still resolve to drawn connector cells carrying the band.
    let mut a = app();
    a.select_by_unique_id(FCT);
    apply_action(&mut a, Action::ToggleDensity);
    a.ui_state.set_focus(Focus::RightPane);

    let lay = a.styled_lineage_layout().expect("lineage");
    let root_col = lay.columns[FCT];
    assert!(root_col > 0, "fct has upstream columns");
    apply_action(&mut a, Action::MoveLeft);
    let cur = a.lineage_cursor_uid().expect("cursor");
    let lay = a.styled_lineage_layout().expect("lineage");
    assert_eq!(
        lay.columns[&cur],
        root_col - 1,
        "h steps one column upstream in compact too"
    );
    assert_eq!(lay.rects[&cur].height, 1, "compact rects stay 1-row");
    let path_edges = a.lineage_path_edges();
    assert!(!path_edges.is_empty(), "the path exists in compact mode");
    for key in &path_edges {
        let cells = &lay.edge_cells[key];
        assert!(!cells.is_empty(), "compact connectors recorded for {key:?}");
        for &(x, y) in cells {
            assert!(lay.grid.attr_at(x, y).on_path, "band on compact connector");
        }
    }
}

#[test]
fn layer_bands_label_unanimous_model_columns_only() {
    // Every band's tint must agree with the Layer lens's per-node tint for the
    // models in that column (the shared layer_tint mapping), bands are sorted
    // by x (BTreeMap by column), and a band never appears for a column whose
    // models disagree — or that holds no models at all.
    let mut a = app();
    a.select_by_unique_id(FCT);
    let lay = a.styled_lineage_layout().expect("lineage");
    let bands = a.layer_bands(&lay);
    assert!(
        !bands.is_empty(),
        "the fct subgraph has unanimous model columns"
    );
    let xs: Vec<usize> = bands.iter().map(|b| b.x).collect();
    let mut sorted = xs.clone();
    sorted.sort_unstable();
    assert_eq!(xs, sorted, "bands come in column order");
    for band in &bands {
        // Find the column's models and check unanimity against the band label.
        let col_uids: Vec<&String> = lay
            .rects
            .iter()
            .filter(|(_, r)| r.x == band.x)
            .map(|(uid, _)| uid)
            .collect();
        let model_layers: Vec<&str> = col_uids
            .iter()
            .filter_map(|uid| a.dag.get(uid))
            .filter(|n| n.resource_type == "model")
            .map(|n| crate::model_list::first_dir(n).unwrap_or("other"))
            .collect();
        assert!(
            !model_layers.is_empty(),
            "a band implies models in the column"
        );
        assert!(
            model_layers.iter().all(|l| *l == band.label),
            "band {band:?} must be unanimous over {model_layers:?}"
        );
    }
    // Determinism: two computations are identical.
    assert_eq!(bands, a.layer_bands(&lay));
}

#[test]
fn overlay_scroll_acts_only_inside_an_overlay() {
    let mut a = app();
    // In Selection mode, an overlay-scroll action is a no-op (no panic).
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    assert!(matches!(a.mode, Mode::Selection));
    // In Help mode it advances the help scroll.
    apply_action(&mut a, Action::HelpToggle);
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    match a.mode {
        Mode::Help(ref h) => assert_eq!(h.scroll, 2),
        _ => panic!("expected Help mode"),
    }
    // Up saturates at 0.
    for _ in 0..5 {
        apply_action(&mut a, Action::DetailScroll(Direction::Up));
    }
    match a.mode {
        Mode::Help(ref h) => assert_eq!(h.scroll, 0, "scroll up saturates at 0"),
        _ => panic!("expected Help mode"),
    }
}
