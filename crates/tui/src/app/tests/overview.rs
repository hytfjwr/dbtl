use super::*;

// ---- whole-graph overview (`w`) ----

#[test]
fn overview_toggle_shows_whole_graph_and_invalidates_caches() {
    let mut a = app();
    let rooted = a.subgraph_rc().nodes.len();
    assert!(
        rooted < a.dag.len(),
        "precondition: the default selection roots a partial subgraph"
    );

    let out = apply_action(&mut a, Action::ToggleOverview);
    assert!(!out.quit && out.effects.is_empty(), "a pure state toggle");
    assert_eq!(
        a.subgraph_rc().nodes.len(),
        a.dag.len(),
        "overview shows every node"
    );
    assert_eq!(
        a.subgraph_rc().selected,
        a.selected_unique_id().unwrap(),
        "the current selection is still the emphasised node"
    );
    assert_eq!(
        a.styled_lineage_layout()
            .expect("overview has a layout")
            .rects
            .len(),
        a.dag.len(),
        "every node gets a rect"
    );
    let note = a.take_notice().expect("entering the overview toasts");
    assert!(note.starts_with("overview: "), "got {note:?}");
    assert!(
        a.ui_state.minimap_visible(),
        "entering the overview turns the minimap on"
    );

    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(
        a.subgraph_rc().nodes.len(),
        rooted,
        "toggling back restores the rooted subgraph"
    );
}

#[test]
fn overview_gates_view_and_density_toggles() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    a.take_notice(); // drain the entry toast

    let view_before = a.tab().lineage_view.clone();
    let density_before = a.ui_state.density();
    for action in [
        Action::ToggleUpstream,
        Action::DepthDecrease,
        Action::ToggleDensity,
    ] {
        apply_action(&mut a, action);
        assert_eq!(
            a.tab().lineage_view,
            view_before,
            "the view filters never change in overview"
        );
        assert_eq!(
            a.ui_state.density(),
            density_before,
            "the density pref never changes in overview"
        );
        assert_eq!(
            a.take_notice().as_deref(),
            Some("view filters are off in overview (w to exit)")
        );
    }
}

#[test]
fn jump_to_drills_out_of_overview() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    assert!(a.ui_state.overview(), "overview is on");

    // The smallest model uid in the fixture, deterministically distinct from
    // the default selection.
    let target = "model.jaffle_finance.dim_delivery_lanes";
    assert_ne!(a.selected_unique_id().as_deref(), Some(target));
    a.jump_to(target);
    assert!(
        !a.ui_state.overview(),
        "a successful re-root drills out of the overview"
    );
    assert_eq!(a.selected_unique_id().as_deref(), Some(target));
}

#[test]
fn overview_forces_compact_and_ascii_yank_matches() {
    let mut a = app();
    assert_eq!(
        a.ui_state.density(),
        crate::Density::Comfortable,
        "precondition: default density"
    );
    let rooted_lines = a.lineage_ascii().expect("rooted yank").lines().count();

    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(
        a.ui_state.density(),
        crate::Density::Comfortable,
        "the user's density pref is untouched"
    );
    assert_eq!(
        a.effective_density(),
        crate::Density::Compact,
        "the overview forces compact rendering"
    );
    let overview_lines = a.lineage_ascii().expect("overview yank").lines().count();
    assert!(
        overview_lines > rooted_lines,
        "the whole-graph yank has more lines than the rooted one"
    );
}

#[test]
fn overview_suppresses_breadcrumb_and_path() {
    let mut a = app();
    a.jump_to(FCT); // builds back-history
    assert!(
        a.breadcrumb(usize::MAX).is_some(),
        "precondition: a breadcrumb exists"
    );

    apply_action(&mut a, Action::ToggleOverview);
    assert!(
        a.breadcrumb(usize::MAX).is_none(),
        "the breadcrumb is suppressed while the overview is on"
    );

    a.move_lineage_cursor(Direction::Right);
    assert!(
        a.lineage_path_set().is_empty(),
        "no root-cursor path while the overview is on"
    );
}

#[test]
fn overview_export_names_after_project() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    let out = apply_action(&mut a, Action::ExportLineage);
    let [Effect::WriteFile { path, .. }] = out.effects.as_slice() else {
        panic!("ExportLineage writes a file: {:?}", out.effects);
    };
    assert!(
        path.ends_with("_overview.txt"),
        "the overview export is named after the project, got {path:?}"
    );
}

#[test]
fn overview_search_matches_the_whole_graph() {
    let mut a = app();
    let root_sg = a.lineage_subgraph();
    // `lineage_matches` is a fuzzy subsequence match, so pick the smallest
    // (deterministic) outside-source name that ALSO fails to fuzzy-match
    // anything in the (tiny) rooted subgraph — a plain membership check isn't
    // enough to guarantee the rooted-view precondition below.
    let mut outside_names: Vec<String> = a
        .dag
        .nodes()
        .values()
        .filter(|n| n.resource_type == "source" && !root_sg.contains(&n.unique_id))
        .map(|n| n.name.clone())
        .collect();
    outside_names.sort();
    let outside_name = outside_names
        .into_iter()
        .find(|name| a.lineage_matches(name).is_empty())
        .expect("fixture has a source name the rooted view can't fuzzy-match");

    apply_action(&mut a, Action::ToggleOverview);
    assert!(
        !a.lineage_matches(&outside_name).is_empty(),
        "the overview reaches the whole graph"
    );
}
