use super::*;

// ============================================================================
// --diff baseline: the diff summary modal + the status diff chip
// ============================================================================

/// Render a `Mode::Diff(dv)` modal over the base panes (same shape as
/// [`render_stats_modal`]: payload in the Mode, no `Dag` in `RenderCtx`).
pub(super) fn render_diff_modal(dv: dbtl::DiffView, width: u16, height: u16) -> Buffer {
    use dbtl::Mode;
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mode = Mode::Diff(dv);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.mode = &mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("diff modal render must not panic");
    }
    terminal.backend().buffer().clone()
}

#[test]
pub(super) fn diff_modal_lists_sections_with_counts_marks_and_more_cap() {
    let dv = dbtl::DiffView {
        baseline: "main/target/manifest.json".into(),
        scope: dbtl::PackageScope::Root,
        added: (0..12)
            .map(|i| (format!("new_model_{i:02}"), "model".to_string()))
            .collect(),
        removed: vec![("old_model".into(), "model".into())],
        modified: vec![("fct_orders".into(), "materialized: view -> table".into())],
        edges_added: vec![("stg_a".into(), "fct_orders".into())],
        edges_removed: vec![],
        column_changes: vec![],
        pr: dbtl::PrImpact::default(),
        scroll: 0,
    };
    let text = buffer_to_string(&render_diff_modal(dv, 100, 60));
    assert!(text.contains("Diff vs baseline"), "modal title");
    assert!(
        text.contains("baseline: main/target/manifest.json"),
        "names the baseline source"
    );
    assert!(
        text.contains("scope: root project only"),
        "the modal states the scope like the export, got: {text}"
    );
    assert!(
        text.contains("Added (12):"),
        "added header counts the FULL list"
    );
    assert!(text.contains("+ new_model_00 (model)"), "added row mark");
    assert!(
        text.contains("+ new_model_11 (model)"),
        "the listing is UNCAPPED - this modal is the only complete diff surface"
    );
    assert!(
        text.contains("~ fct_orders: materialized: view -> table"),
        "modified row carries the reasons"
    );
    assert!(text.contains("- old_model (model)"), "removed row mark");
    assert!(
        text.contains("+ stg_a -> fct_orders"),
        "edge rows use the ASCII arrow"
    );
    assert!(
        text.contains("Edges removed (0):"),
        "empty section still counts"
    );
}

#[test]
pub(super) fn diff_modal_with_no_changes_says_so() {
    let dv = dbtl::DiffView {
        baseline: "base".into(),
        ..Default::default()
    };
    let text = buffer_to_string(&render_diff_modal(dv, 100, 30));
    assert!(
        text.contains("no changes vs the baseline"),
        "a clean diff is stated, not an empty wall of zeros"
    );
}

#[test]
pub(super) fn diff_modal_scope_line_matches_the_export() {
    // Under --include-packages the modal prints the same `scope:` line the
    // `D`→`e` export writes — screen and file must not disagree about what
    // the numbers cover.
    let dv = dbtl::DiffView {
        baseline: "base".into(),
        scope: dbtl::PackageScope::All,
        ..Default::default()
    };
    let text = buffer_to_string(&render_diff_modal(dv, 100, 30));
    assert!(
        text.contains("scope: all packages (metrics count the root project only)"),
        "the all-scope label, got: {text}"
    );
}

#[test]
pub(super) fn diff_modal_renders_the_pr_impact_pack_sections() {
    // With changes present, the modal appends the reviewer-shaped PR impact pack
    // below the diff sections: blast radius, exposures, CI command, risk flags.
    let dv = dbtl::DiffView {
        baseline: "main".into(),
        modified: vec![("stg_a".into(), "SQL changed".into())],
        pr: dbtl::PrImpact {
            affected_models: 3,
            affected_marts: vec!["fct_x".into(), "fct_y".into()],
            affected_exposures: vec!["exec_dash (dashboard, owner: Finance)".into()],
            affected_owners: vec!["alice".into(), "bob".into()],
            ci_command: Some("dbt build --select stg_a+".into()),
            column_affected_models: 0,
            ci_command_columns: None,
            untested_changes: vec!["stg_a".into()],
            changed_hubs: vec![("stg_a".into(), 7)],
            new_layer_violations: vec![("fct_x".into(), "stg_z".into())],
        },
        ..Default::default()
    };
    let text = buffer_to_string(&render_diff_modal(dv, 100, 60));
    assert!(text.contains("PR impact pack"), "the pack header: {text}");
    assert!(text.contains("affected models: 3"), "{text}");
    assert!(text.contains("affected marts: fct_x, fct_y"), "{text}");
    assert!(text.contains("affected owners: alice, bob"), "{text}");
    assert!(text.contains("CI: dbt build --select stg_a+"), "{text}");
    assert!(text.contains("Affected exposures (1):"), "{text}");
    assert!(text.contains("Risk flags"), "{text}");
    assert!(text.contains("untested change: stg_a"), "{text}");
    assert!(text.contains("hub change: stg_a (7 downstream)"), "{text}");
    assert!(
        text.contains("new layer violation: fct_x -> stg_z"),
        "{text}"
    );
}

#[test]
pub(super) fn diff_modal_renders_the_column_changes_section() {
    // The CLL refinement sits between the diff sections and the PR pack:
    // one marked row per classified change (x breaking / ~ partial /
    // + non-breaking), dim detail rows for the dangling columns and the
    // exposures they reach.
    let dv = dbtl::DiffView {
        baseline: "main".into(),
        modified: vec![("stg_orders".into(), "columns changed".into())],
        column_changes: vec![
            dbtl::ColumnChange {
                node: "model.p.stg_orders".into(),
                node_name: "stg_orders".into(),
                column: "status".into(),
                renamed_from: None,
                kind: dbtl::ColumnChangeKind::Removed,
                class: dbtl::ColumnChangeClass::Breaking,
                type_from: None,
                type_to: None,
                affected_columns: vec!["fct_orders.status".into()],
                affected_models: vec!["fct_orders".into()],
                affected_marts: vec!["fct_orders".into()],
                affected_exposures: vec!["orders_dash (dashboard)".into()],
            },
            dbtl::ColumnChange {
                node: "model.p.stg_orders".into(),
                node_name: "stg_orders".into(),
                column: "order_status".into(),
                renamed_from: Some("status_old".into()),
                kind: dbtl::ColumnChangeKind::Renamed,
                class: dbtl::ColumnChangeClass::NonBreaking,
                type_from: None,
                type_to: None,
                affected_columns: vec![],
                affected_models: vec![],
                affected_marts: vec![],
                affected_exposures: vec![],
            },
        ],
        pr: dbtl::PrImpact {
            affected_models: 1,
            column_affected_models: 1,
            ci_command: Some("dbt build --select stg_orders+".into()),
            ci_command_columns: Some("dbt build --select fct_orders stg_orders".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let text = buffer_to_string(&render_diff_modal(dv, 100, 60));
    assert!(text.contains("Column changes (2):"), "{text}");
    assert!(
        text.contains("x stg_orders.status: removed [breaking]"),
        "the breaking row: {text}"
    );
    assert!(
        text.contains("cols: fct_orders.status"),
        "the dangling columns list: {text}"
    );
    assert!(
        text.contains("exposures: orders_dash (dashboard)"),
        "the reached exposure: {text}"
    );
    assert!(
        text.contains("+ stg_orders.status_old -> order_status: renamed [non-breaking]"),
        "a migrated rename prints old -> new: {text}"
    );
    assert!(
        text.contains("column-affected models: 1"),
        "the column-narrow count: {text}"
    );
    assert!(
        text.contains("CI columns: dbt build --select fct_orders stg_orders"),
        "the column-narrowed selector: {text}"
    );
}

#[test]
pub(super) fn status_diff_chip_renders_with_counts() {
    let list = fixture_list();
    let state = UiState::new(list.len());
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.segments = dbtl::ui::StatusSegments {
            diff: Some("diff +2 ~1 -3"),
            ..Default::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains("[diff +2 ~1 -3]"),
        "diff chip rendered: {text}"
    );
}
