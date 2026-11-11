use super::*;

// ============================================================================
// TestBackend rendering
// ============================================================================

/// Concatenate the buffer into one string per row, then all rows joined by '\n'.
pub(super) fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

/// Concatenate a single buffer row into a string (no trailing newline). Used by
/// the lineage-title tests, which must scan ONLY the title row (row 0) — the
/// status bar also echoes the selected name, so a whole-buffer scan false-counts.
pub(super) fn row_text(buffer: &Buffer, y: u16) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for x in 0..area.width {
        if let Some(cell) = buffer.cell((x, y)) {
            out.push_str(cell.symbol());
        }
    }
    out
}

/// The hints a Selection/list-focused frame shows — the same call the
/// event loop makes (`App::status_hints` needs an App; these helpers render
/// from a bare list, so the pure generator is called directly with the
/// matching context).
pub(super) fn list_hints() -> Vec<dbtl::action::StatusHint> {
    dbtl::action::status_hints(
        dbtl::action::ModeKind::Selection,
        dbtl::ui::Focus::List,
        false,
        false,
    )
}

/// Render the initial state into a `width x height` buffer.
pub(super) fn render(width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let hints = list_hints();
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.hints = &hints;
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    terminal.backend().buffer().clone()
}

#[test]
pub(super) fn all_four_headers_visible_in_tall_buffer() {
    // Tall buffer (height >= 55) so all 49 list rows render without scrolling;
    // at initial selection (index 0, offset 0) the utilities header would
    // otherwise be off-screen on a normal-height terminal.
    let buffer = render(100, 60);
    let text = buffer_to_string(&buffer);
    // Headers render title-cased with a leading rule glyph (display-only; the
    // lowercase grouping key is unchanged — see model_list.rs struct tests).
    for header in ["Staging", "Intermediate", "Marts", "Utilities"] {
        assert!(
            text.contains(header),
            "header {header:?} must be visible in tall buffer"
        );
    }
}

#[test]
pub(super) fn known_model_name_visible() {
    let buffer = render(100, 60);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("stg_finance__budgets"),
        "the initially selected model name must be visible"
    );
}

#[test]
pub(super) fn selected_row_is_visually_distinguished() {
    // Normal-size buffer is fine here.
    let buffer = render(100, 30);
    let text = buffer_to_string(&buffer);

    // (1) Marker-based: the selected row carries the "> " marker; no other
    // model row should. We check the marker appears exactly once.
    let marker_lines = text.lines().filter(|l| l.contains("> ")).count();
    assert_eq!(
        marker_lines, 1,
        "exactly one row should carry the selection marker"
    );

    // (2) Style-based: at least one cell carries the REVERSED modifier (the
    // selected row), proving style distinction is also present.
    let area = buffer.area;
    let mut reversed_cells = 0;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                if cell.modifier.contains(Modifier::REVERSED) {
                    reversed_cells += 1;
                }
            }
        }
    }
    assert!(
        reversed_cells > 0,
        "selected row must carry a distinguishing (REVERSED) style"
    );
}

#[test]
pub(super) fn status_help_line_present() {
    let buffer = render(100, 30);
    let text = buffer_to_string(&buffer);
    // The contextual hints (BINDINGS-generated) render on the left of the
    // status row.
    assert!(
        text.contains("j/k move") && text.contains("? help"),
        "the hint row must be present: {text:?}"
    );
    // The selected node's name is echoed in the lineage pane title.
    assert!(
        text.contains("stg_finance__budgets"),
        "the selected node name stays visible"
    );
}

#[test]
pub(super) fn hidden_list_pane_renders_lineage_full_width() {
    // Cmd-b / Ctrl-b hides the model list: the lineage pane must span the whole
    // screen width and nothing of the list (headers, selection marker) renders.
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let mut state = UiState::new(list.len());
    state.toggle_list_pane(); // hide
    let ctx = RenderCtx::new(&list, &state, None);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);

    // The lineage pane's border now owns both screen edges of the top row.
    assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((99, 0)).unwrap().symbol(), "╮");
    // No list content: group headers and the selection marker are gone (the
    // status line still echoes the selected unique_id, so match the header's
    // "name (count)" shape, not a bare model name).
    assert!(
        !text.contains("staging ("),
        "list group headers must not render while hidden"
    );
    assert!(
        !text.lines().any(|l| l.contains("> ")),
        "no list selection marker while hidden"
    );
}

#[test]
pub(super) fn explicit_list_width_places_the_splitter_deterministically() {
    // The saved width is COLUMNS, not a ratio — the boundary strip lands
    // on the two border columns the pure layout reports, so a RenderCtx with
    // a fixed width draws deterministically.
    let render_at = |saved: u16| -> Buffer {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let list = fixture_list();
        let mut state = UiState::new(list.len());
        state.set_pane_width(dbtl::ui::Splitter::List, saved, 100);
        let ctx = RenderCtx::new(&list, &state, None);
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
        terminal.backend().buffer().clone()
    };
    // Default 34: the list's `╮│` and the lineage's `╭│` sit at 33 | 34.
    // (Row 25 is below the unscrolled scrollbar thumb and the scroll markers'
    // border midpoints; row 0's corners are never overdrawn.)
    let buffer = render_at(34);
    assert_eq!(buffer.cell((33, 0)).unwrap().symbol(), "╮");
    assert_eq!(buffer.cell((34, 0)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((33, 25)).unwrap().symbol(), "│");
    assert_eq!(buffer.cell((34, 25)).unwrap().symbol(), "│");
    // Saved 20: the strip follows — 19 | 20.
    let buffer = render_at(20);
    assert_eq!(buffer.cell((19, 0)).unwrap().symbol(), "╮");
    assert_eq!(buffer.cell((20, 0)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((20, 25)).unwrap().symbol(), "│");
    // Saved 90 clamps to the 60%-of-terminal cap at 100 cols: 59 | 60.
    let buffer = render_at(90);
    assert_eq!(buffer.cell((59, 0)).unwrap().symbol(), "╮");
    assert_eq!(buffer.cell((60, 0)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((60, 25)).unwrap().symbol(), "│");
}

#[test]
pub(super) fn splitter_hover_hint_and_accent_strip_render() {
    // Hovering a boundary restamps its 2 border columns accent and shows
    // the hint in the status row (the loop supplies both ctx inputs; draw
    // just paints them).
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let transient = vec![dbtl::action::StatusHint {
        key: "drag to resize · double-click to reset".to_string(),
        desc: String::new(),
        action: None,
        prio: 0,
    }];
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.splitter_highlight = Some(dbtl::ui::Splitter::List);
    ctx.hints = &transient;
    let accent = ctx.theme.accent;
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    // Every body row of the strip carries the accent fg (top corner and a
    // mid row are representative; the strip spans the whole body).
    assert_eq!(buffer.cell((33, 0)).unwrap().fg, accent, "top corner");
    assert_eq!(buffer.cell((34, 10)).unwrap().fg, accent, "mid strip");
    // The hint is a status-bar segment on the last row.
    let status = row_text(&buffer, 29);
    assert!(
        status.contains("drag to resize · double-click to reset"),
        "the hover hint renders in the status row: {status:?}"
    );
}

#[test]
pub(super) fn help_overlay_renders_keybindings_from_keymap() {
    // The ? overlay is generated from the keymap (single source of truth), so a
    // representative binding description must render.
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mode = dbtl::Mode::Help(dbtl::HelpView::new(dbtl::Mode::Selection));
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.mode = &mode;
    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw help overlay must not panic");
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("Keybindings"), "help overlay title present");
    assert!(
        text.contains(concat!("dbtl v", env!("CARGO_PKG_VERSION"))),
        "help title carries the crate version"
    );
    assert!(text.contains("quit"), "the quit binding is documented");
    assert!(
        text.contains("open structure / re-root to cursor"),
        "the Enter/structure binding is documented (derived from the keymap)"
    );
}

#[test]
pub(super) fn filter_tag_renders_in_list_title_and_status_segment() {
    // Drive the real path: bookmark + filter, then wire RenderCtx like the
    // loop does (bare label; title and segment add their own brackets).
    use dbtl::app::{apply_action, App};
    use dbtl::ui::StatusSegments;
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::BookmarkToggle);
    apply_action(&mut app, Action::ToggleBookmarkFilter);
    assert_eq!(app.active_list().len(), 1);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.filter_label = app.list_filter_label();
        ctx.segments = StatusSegments {
            diff: None,
            run: None,
            filter: app.list_filter_label(),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let buffer = terminal.backend().buffer().clone();
    let title = row_text(&buffer, 0);
    assert!(
        title.contains("[marked]"),
        "list title carries the filter tag: {title}"
    );
    let status = row_text(&buffer, buffer.area.height - 1);
    assert!(
        status.contains("[marked]") && !status.contains("[[marked]]"),
        "status segment shows the tag exactly once-bracketed: {status}"
    );
}

#[test]
pub(super) fn detail_modal_renders_structure_via_real_path() {
    // Drive the real path: build the App, select a known model, open the detail
    // modal, then render. The modal must show the structure (type/columns) drawn
    // from the Dag side maps — without RenderCtx holding a Dag.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::DetailOpen);

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("detail modal render must not panic");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("pos_txn"), "modal titled with the model name");
    assert!(
        text.contains("type:"),
        "structure shows the materialization line"
    );
    assert!(
        text.contains("columns"),
        "structure shows the columns section"
    );
    assert!(text.contains("table"), "pos_txn's materialization is shown");
    // The blast-radius line: pos_txn has 0 downstream / 7 upstream (the SAME
    // closures the fixture freezes in `closure_upstream_only_pos_txn`).
    assert!(
        text.contains("impact: 0 downstream / 7 upstream"),
        "structure shows the blast-radius line"
    );
    assert!(
        text.contains("path:   models/utilities/pos_prep/pos_txn.sql"),
        "structure shows the model's file path"
    );
}

#[test]
pub(super) fn detail_modal_impact_line_targets_the_cursored_node_not_the_root() {
    // The critical case: when the lineage pane is focused and the cursor sits on a
    // non-root SOURCE, opening the structure modal must count THAT source's blast
    // radius, not the root's. The shoppers source has 2 downstream / 0 upstream.
    use dbtl::app::{apply_action, App};
    use dbtl::ui::HitTarget;
    use dbtl::{Action, Focus, Mode};
    use ratatui::crossterm::event::MouseButton;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // Root a model whose upstream INCLUDES the shoppers source.
    app.select_by_unique_id("model.jaffle_finance.int_shoppers__combined");
    app.ui_state.set_focus(Focus::RightPane);
    // Put the lineage cursor onto the shoppers source (the deterministic path the
    // event loop uses for a click on a non-selectable node — the single click
    // moves the cursor there rather than re-rooting).
    let src = "source.jaffle_finance.dev_lake_jaffle_payment.shoppers";
    app.click_target(
        &HitTarget::LineageNode(src.to_string()),
        MouseButton::Left,
        (10, 10),
    );
    assert_eq!(
        app.lineage_cursor_uid().as_deref(),
        Some(src),
        "cursor reached the shoppers source"
    );
    apply_action(&mut app, Action::DetailOpen);
    // A source is non-selectable, so DetailOpen opens its modal (never re-roots).
    assert!(
        matches!(app.mode, Mode::Detail(_)),
        "source opens its modal"
    );

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains("impact: 2 downstream / 0 upstream"),
        "the modal counts the CURSORED source, not the root model"
    );
}

#[test]
pub(super) fn detail_modal_renders_column_counts_cursor_and_inferred_tag() {
    // Column-level lineage: the structure modal's column rows carry
    // `↑u ↓d` ONE-HOP edge-count badges and an h/l cursor; a model with no
    // declared columns renders the resolver's inferred names under a [from
    // SQL] tag. `cll_demo` is the predictable case: every output has
    // upstream=4 / down=0 (direct edges, not the transitive closure).
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    const SAMPLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let mut app = App::new(
        load_dag(SAMPLE).expect("sample manifest loads"),
        std::path::PathBuf::from(SAMPLE),
    );
    app.select_by_unique_id("model.sample.cll_demo");
    apply_action(&mut app, Action::DetailOpen);

    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(
        text.contains("columns (4) [from SQL]:"),
        "inferred-column header tag, got: {text}"
    );
    assert!(
        text.contains("> n_orders"),
        "cursor marker on the first row: {text}"
    );
    assert!(
        text.contains("↑4 ↓0"),
        "one-hop edge-count badges on the rows: {text}"
    );

    // h/l move the ROW cursor (a selection), so the marker follows.
    apply_action(
        &mut app,
        dbtl::Action::DetailColumnMove(dbtl::Direction::Down),
    );
    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(text.contains("> order_id"), "cursor moved down: {text}");
    assert!(!text.contains("> n_orders"), "marker left the first row");
}

#[test]
pub(super) fn detail_modal_renders_column_badges_and_relationship_targets() {
    // pos_ord declares `id` (unique+not_null -> `[NN U]`) and
    // `lg_id` (not_null+relationships -> `[NN R]`); the tests section resolves
    // the `to:` target and Tab lands the h/l cursor on the jumpable row.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.pos_ord");
    apply_action(&mut app, Action::DetailOpen);

    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(
        text.contains("[NN U]"),
        "the PK pair badges on `id`: {text}"
    );
    assert!(
        text.contains("[NN R]"),
        "not_null+relationships badges on `lg_id`: {text}"
    );
    // The relationships `to:` target resolves to the kept source's name.
    assert!(
        text.contains("relationships(lg_id) -> pos_pay"),
        "the resolved target row: {text}"
    );

    // Tab moves the h/l cursor onto the jumpable row — the `> ` marker leaves
    // the columns section for the test row.
    apply_action(&mut app, Action::DetailSectionToggle);
    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(
        text.contains("> relationships(lg_id) -> pos_pay"),
        "the cursor sits on the jumpable row: {text}"
    );
    assert!(
        !text.contains("> id"),
        "the column cursor is inactive: {text}"
    );

    // Enter on a SOURCE target can't re-root (sources aren't selectable) —
    // it notices and the modal stays open.
    apply_action(&mut app, Action::ColumnFocusEnter);
    assert_eq!(
        app.take_notice().as_deref(),
        Some("cannot re-root to a non-model node")
    );
    assert!(
        matches!(app.mode, dbtl::Mode::Detail(_)),
        "the modal stays open on a non-model target"
    );
}

#[test]
pub(super) fn stats_modal_renders_column_coverage() {
    // the column-coverage gauge line reaches the buffer — the same
    // numbers the docs README quotes.
    let sv = stats_view_with(0, 0, 0);
    let text = buffer_to_string(&render_stats_modal(sv, 100, 40));
    assert!(
        text.contains("Column coverage (declared columns):"),
        "the column-coverage header: {text}"
    );
    assert!(
        text.contains("12/20 columns tested (60%)"),
        "the column-coverage counts: {text}"
    );
}

#[test]
pub(super) fn sql_modal_titles_the_active_variant() {
    // End-to-end through the REAL draw path against the compiled fixture:
    // `s` opens on [raw] with the toggle hint, `c` flips to [compiled] showing
    // the manifest's expansion, and forcing `compiled_stale` yields
    // [compiled, stale] (the mtime side of stale is covered by the core and
    // fixture tests — this asserts the title chrome reaches the buffer).
    use dbtl::app::{apply_action, App};
    use dbtl::{Action, Mode};
    const COMPILED: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/compiled_project/target/manifest.json"
    );
    let dag = load_dag(COMPILED).expect("compiled fixture loads");
    let mut app = App::new(dag, std::path::PathBuf::from(COMPILED));
    app.select_by_unique_id("model.compiled_demo.mart_one");
    apply_action(&mut app, Action::SqlOpen);

    // 140 cols: the modal is 80% of the area, so the full title (name + path +
    // tag + hints) fits without clipping.
    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(text.contains("[raw]"), "raw title tag renders: {text}");
    assert!(
        text.contains("c: raw/compiled"),
        "toggle hint renders when compiled exists: {text}"
    );
    assert!(
        text.contains("ref('stg_one')"),
        "the raw text (unexpanded Jinja) is shown: {text}"
    );

    apply_action(&mut app, Action::SqlToggleCompiled);
    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(text.contains("[compiled]"), "compiled title tag: {text}");
    assert!(
        text.contains("demo.main.stg_one"),
        "the expanded SQL is shown: {text}"
    );

    if let Mode::Sql(sv) = &mut app.mode {
        sv.compiled_stale = true;
    }
    let text = buffer_to_string(&render_app(&app, 140, 40));
    assert!(
        text.contains("[compiled, stale]"),
        "stale title tag: {text}"
    );
}

/// Render a `Mode::Stats(sv)` modal over the base panes into a buffer (the same
/// `draw` path the event loop drives, with the modal data carried in the Mode
/// payload — no `Dag` in `RenderCtx`).
pub(super) fn render_stats_modal(sv: dbtl::StatsView, width: u16, height: u16) -> Buffer {
    use dbtl::Mode;
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mode = Mode::Stats(sv);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.mode = &mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("stats modal render must not panic");
    }
    terminal.backend().buffer().clone()
}

/// A `StatsView` literal with `n_orphans` orphan names, `n_viol` layer
/// violations, and `n_lint` lint findings on one rule — built to exercise the
/// render caps without a Dag.
pub(super) fn stats_view_with(n_orphans: usize, n_viol: usize, n_lint: usize) -> dbtl::StatsView {
    dbtl::StatsView {
        project: "proj".into(),
        by_resource_type: vec![("model".into(), 8), ("source".into(), 3)],
        by_materialization: vec![("table".into(), 5), ("view".into(), 3)],
        testable_total: 10,
        testable_tested: 9,
        col_declared: 20,
        col_tested: 12,
        top_hubs: vec![("model.proj.a".into(), "a".into(), 6)],
        transitive_hubs: vec![("a".into(), 12), ("b".into(), 4)],
        orphan_models: (0..n_orphans).map(|i| format!("orphan_{i:02}")).collect(),
        layer_violations: (0..n_viol)
            .map(|i| (format!("mart_{i:02}"), format!("stg_{i:02}")))
            .collect(),
        lint_findings: if n_lint == 0 {
            vec![]
        } else {
            vec![dbtl::LintWorklist {
                rule: "undocumented_models".to_string(),
                severity: dbtl::Severity::Warn,
                names: (0..n_lint).map(|i| format!("nodoc_{i:02}")).collect(),
            }]
        },
        lint_errors: 0,
        lint_warnings: n_lint,
        critical_path: vec!["src_a".into(), "stg_a".into(), "mart_a".into()],
        critical_path_runtime: None,
        untested_testable: 1,
        zero_downstream_models: 2,
        no_description_models: 4,
        cll_fallbacks: 0,
        scroll: 0,
    }
}

#[test]
pub(super) fn stats_modal_gauge_has_graded_fill_over_dark_track() {
    // High coverage (9/10 = 90%) → an OK-graded fill over a SURFACE_HI dark
    // track, both made of bg-styled spaces (no glyphs). Scan the buffer for a
    // cell with the graded fill bg and a cell with the track bg, proving the
    // two-span gauge rendered (coordinate-free, per the advisor).
    use dbtl::ui::theme;
    let buffer = render_stats_modal(stats_view_with(0, 0, 0), 100, 40);
    // Scan ONLY the coverage row (the line carrying "tested") so the green fill
    // can't be confused with the green transitive-hub mini-bar elsewhere.
    let area = buffer.area;
    let cov_y = (0..area.height)
        .find(|&y| {
            (0..area.width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains("tested")
        })
        .expect("a coverage row containing 'tested' must render");
    let mut saw_fill = false;
    let mut saw_track = false;
    for x in 0..area.width {
        if let Some(c) = buffer.cell((x, cov_y)) {
            if c.bg == theme::OK {
                saw_fill = true;
            }
            if c.bg == theme::SURFACE_HI {
                saw_track = true;
            }
        }
    }
    assert!(
        saw_fill,
        "coverage gauge fill (OK grade @ 90%) on the coverage row"
    );
    assert!(
        saw_track,
        "coverage gauge dark track (SURFACE_HI) on the coverage row"
    );
}

#[test]
pub(super) fn stats_modal_low_coverage_gauge_is_red() {
    use dbtl::ui::theme;
    let mut sv = stats_view_with(0, 0, 0);
    sv.testable_tested = 2; // 2/10 = 20% → danger grade
    let buffer = render_stats_modal(sv, 100, 40);
    let red = (0..buffer.area.height).any(|y| {
        (0..buffer.area.width).any(|x| buffer.cell((x, y)).map(|c| c.bg) == Some(theme::DANGER))
    });
    assert!(red, "low coverage (<40%) grades the gauge fill DANGER red");
}

#[test]
pub(super) fn stats_modal_shows_new_sections_and_more_cap() {
    // 12 orphans + 11 violations + 13 lint findings → all three worklists hit
    // the cap-10 + "+K more". Tall buffer so the whole (unscrolled) dashboard
    // fits the modal interior.
    let buffer = render_stats_modal(stats_view_with(12, 11, 13), 100, 120);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("Orphans (12)"),
        "orphans header with full count"
    );
    assert!(
        text.contains("Layer violations (11)"),
        "violations header with full count"
    );
    assert!(
        text.contains("Lint findings (13: 0 errors, 13 warnings):"),
        "lint worklist header carries the severity split"
    );
    assert!(
        text.contains("undocumented_models (13):"),
        "per-rule lint sub-list with count"
    );
    assert!(
        text.contains("Hubs (transitive"),
        "transitive-hubs section present"
    );
    assert!(
        text.contains("+2 more"),
        "orphans overflow rolls into +K more"
    );
    assert!(
        text.contains("+1 more"),
        "violations overflow rolls into +K more"
    );
    assert!(
        text.contains("+3 more"),
        "lint worklist overflow rolls into +K more"
    );
    assert!(
        text.contains("mart_00 -> stg_00"),
        "violation rows use the ASCII arrow"
    );
    // Critical path: header carries the depth, hops render as the indented
    // staircase (each name one space further right).
    assert!(
        text.contains("Critical path (depth 3)"),
        "critical-path header with depth"
    );
    assert!(text.contains("  src_a"), "first hop at base indent");
    assert!(text.contains("   stg_a"), "second hop one space deeper");
    assert!(text.contains("    mart_a"), "third hop two spaces deeper");
}

#[test]
pub(super) fn palette_modal_echoes_query_highlights_selection_and_shows_key_labels() {
    // Drive the real path: open the palette, type a narrowing query, step the
    // cursor, then render. The modal must echo the query, reverse the selected
    // row, and show the dim key labels (right column derived from the keymap).
    use dbtl::app::{apply_action, App};
    use dbtl::{Action, Direction};
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "toggle".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    apply_action(&mut app, Action::SearchMove(Direction::Down));

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("palette modal render must not panic");
    }
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);

    // (1) Title + the live query are echoed.
    assert!(text.contains("commands"), "palette titled 'commands'");
    assert!(
        text.contains("> toggle"),
        "the live query is echoed on the query line"
    );
    // (2) A candidate help text and its key label render (e.g. the minimap toggle
    // is bound to `M`, the downstream toggle to `d`). "toggle" is a subsequence of
    // several Selection-command help strings, so at least one must show.
    assert!(
        text.contains("toggle minimap") || text.contains("toggle downstream"),
        "a matched command's help text renders:\n{text}"
    );
    // (3) The selected row carries the REVERSED style (selection cue).
    let area = buffer.area;
    let mut reversed = 0;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                if cell.modifier.contains(Modifier::REVERSED) {
                    reversed += 1;
                }
            }
        }
    }
    assert!(reversed > 0, "the selected palette row is REVERSED");
}

#[test]
pub(super) fn new_tab_palette_titles_open_in_new_tab() {
    // The tab strip's `+` palette (`PaletteState::new_tab`) is titled for the
    // tab-open intent — not "commands" (a plain `@` query re-roots, keeping
    // the models title).
    use dbtl::app::App;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.mode = dbtl::Mode::Palette(dbtl::action::PaletteState {
        query: "@".to_string(),
        selected: 0,
        new_tab: true,
    });
    let buffer = render_app(&app, 100, 40);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("open in new tab"),
        "the + palette is titled for the tab-open intent:\n{text}"
    );

    app.mode = dbtl::Mode::Palette(dbtl::action::PaletteState {
        query: "@stg".to_string(),
        selected: 0,
        new_tab: false,
    });
    let buffer = render_app(&app, 100, 40);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains(" models "),
        "a plain @ query keeps the models title:\n{text}"
    );
}

#[test]
pub(super) fn palette_highlights_key_label_chars_when_only_the_key_matches() {
    // A row admitted by a KEY-LABEL match (not help) must show its match cue. Query
    // "cmd": the `Cmd-b/Ctrl-b -> show / hide the model list pane` row is admitted
    // by its key label (its help has no "cmd" subsequence). Step the cursor OFF that
    // row (the selected row keeps its reverse, suppressing the highlight cue), then
    // assert ITS key-label chars carry the yellow+bold cue while its help does not.
    use dbtl::action::palette_candidates;
    use dbtl::app::{apply_action, App};
    use dbtl::model_list::match_indices;
    use dbtl::{Action, Direction};

    // Precondition: row 0 of the "cmd" candidates is the key-label-only row.
    let cands = palette_candidates("cmd");
    let kl_row = cands.first().expect("a 'cmd' candidate");
    assert!(
        match_indices(kl_row.help, "cmd").is_empty()
            && !match_indices(&kl_row.key_label(), "cmd").is_empty(),
        "row 0 is admitted via its key label, not help text"
    );
    let help0 = kl_row.help; // the help text that must carry NO highlight

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "cmd".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    // Move selection off row 0 so the key-label row is NOT reversed.
    apply_action(&mut app, Action::SearchMove(Direction::Down));

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area;

    // Locate the key-label row inside the modal by its (unique) help text, then
    // scan ONLY that row for GOLD+bold (match highlight) cells — a whole-buffer
    // scan would catch the list pane's own accented layer headers outside the
    // modal.
    let mut row_y = None;
    for y in 0..area.height {
        if row_text(&buffer, y).contains(help0) {
            row_y = Some(y);
            break;
        }
    }
    let y = row_y.expect("the key-label row renders inside the modal");
    let mut highlighted = String::new();
    for x in 0..area.width {
        if let Some(cell) = buffer.cell((x, y)) {
            if cell.fg == dbtl::ui::theme::GOLD && cell.modifier.contains(Modifier::BOLD) {
                highlighted.push_str(cell.symbol());
            }
        }
    }
    // The matched key-label chars (C, m, d of "Cmd-b/Ctrl-b") are highlighted…
    let lower = highlighted.to_lowercase();
    assert!(
        lower.contains('c') && lower.contains('m') && lower.contains('d'),
        "the key-label match chars are highlighted GOLD+bold, got {highlighted:?}"
    );
    // …and NO char of the HELP text is highlighted (the help had no match). The help
    // "show / hide the model list pane" has chars (h, w) the key label lacks.
    assert!(
        !highlighted.contains('h') && !highlighted.contains('w'),
        "no help-text char is highlighted, got {highlighted:?}"
    );
}

#[test]
pub(super) fn draw_does_not_panic_on_tiny_buffer() {
    // Smoke: degenerate small terminals must not panic in draw.
    let backend = TestBackend::new(8, 4);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let ctx = RenderCtx::new(&list, &state, None);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw on tiny buffer must not panic");
}
