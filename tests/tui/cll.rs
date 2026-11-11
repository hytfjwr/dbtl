use super::*;

// ============================================================================
// Column-level lineage (Issue 09): focus view, status chip, title suffix,
// column modal — rendered through the real draw() pipeline.
// ============================================================================

/// The CLL-demo fixture: `raw.orders → stg_orders → fct_orders → cll_demo`.
pub(super) fn cll_sample_app() -> dbtl::app::App {
    const SAMPLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let dag = load_dag(SAMPLE).expect("sample fixture loads");
    dbtl::app::App::new(dag, std::path::PathBuf::from(SAMPLE))
}

/// Enter column focus on `cll_demo.order_id` through the real gesture path:
/// select the model, open its structure modal, move the column cursor off the
/// first inferred column, and commit with the focus action.
pub(super) fn focus_order_id(app: &mut dbtl::app::App) {
    use dbtl::app::apply_action;
    use dbtl::Action;
    app.select_by_unique_id("model.sample.cll_demo");
    apply_action(app, Action::DetailOpen);
    // col_names[0] is "n_orders" (sorted); one h/l step lands on "order_id".
    apply_action(app, Action::DetailColumnMove(dbtl::Direction::Down));
    apply_action(app, Action::ColumnFocusEnter);
    assert!(app.column_status().is_some(), "the focus engaged");
}

/// Mirror the loop's re-anchor on `ColumnFocusEnter`: centre the traced
/// model's box in a `w x h` frame (the event loop does this via `force_anchor`
/// + `anchor_lineage` — the test harness owns its own scroll state).
pub(super) fn anchor_on_traced(app: &mut dbtl::app::App, w: u16, h: u16) {
    let lay = app.styled_lineage_layout().expect("focused layout");
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    let inner = dbtl::ui::pane_interior(
        dbtl::ui::pane_layout(area, &dbtl::ui::PaneConfig::default()).lineage,
    );
    app.ui_state.anchor_lineage(
        lay.selected_rect,
        &lay.grid,
        inner.width as usize,
        inner.height as usize,
    );
}

#[test]
pub(super) fn column_focus_draws_column_rows_status_chip_and_title_suffix() {
    // The focused lineage: each box lists the trace's columns with edge-kind
    // marks (`>` the traced column), the status chip reads
    // `column:model.col ↑N ↓M`, and the lineage title carries ` [col:name]`.
    // Exercised at the two canonical sizes.
    let mut app = cll_sample_app();
    focus_order_id(&mut app);
    for (w, h) in [(80u16, 24u16), (100, 30)] {
        anchor_on_traced(&mut app, w, h);
        let buffer = render_app(&app, w, h);
        let text = buffer_to_string(&buffer);
        assert!(
            text.contains("> order_id"),
            "{w}x{h}: traced column row inside cll_demo's box: {text}"
        );
        assert!(
            text.contains("~ id") || text.contains("% id"),
            "{w}x{h}: an upstream column row inside a parent box: {text}"
        );
        // The status chip and title suffix (composed into the title's suffix
        // slot — `[lens]` stays empty under the Off lens, so `[col:` rides it).
        assert!(
            text.contains("column:cll_demo.order_id"),
            "{w}x{h}: the column chip renders: {text}"
        );
        assert!(
            text.contains("[col:order_id]"),
            "{w}x{h}: the title suffix names the column: {text}"
        );
    }
}

#[test]
pub(super) fn columns_modal_renders_the_trace_tree_and_its_cursor() {
    // `K` over an active focus opens the column modal: section headers, the
    // traced column's row (the cursor), and upstream/downstream entries.
    use dbtl::app::apply_action;
    use dbtl::Action;
    let mut app = cll_sample_app();
    focus_order_id(&mut app);
    apply_action(&mut app, Action::ColumnsOpen);
    let buffer = render_app(&app, 100, 30);
    let text = buffer_to_string(&buffer);
    for needle in [
        "Column lineage - cll_demo.order_id",
        "upstream (",
        "this column:",
        "stg_orders.id",
        "Enter jump",
    ] {
        assert!(
            text.contains(needle),
            "columns modal shows {needle:?}: {text}"
        );
    }
}

#[test]
pub(super) fn column_focus_esc_returns_to_the_rooted_render() {
    use dbtl::app::apply_action;
    use dbtl::Action;
    let mut app = cll_sample_app();
    focus_order_id(&mut app);
    apply_action(&mut app, Action::ColumnFocusExit);
    let text = buffer_to_string(&render_app(&app, 80, 24));
    assert!(
        !text.contains("> order_id"),
        "the column rows are gone after Esc: {text}"
    );
    assert!(
        !text.contains("column:cll_demo.order_id"),
        "the chip is gone after Esc: {text}"
    );
}
