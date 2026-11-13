use super::*;

#[test]
fn page_down_and_up_move_the_list_selection_by_ten_clamped() {
    let mut a = app();
    assert_eq!(a.ui_state.selected(), 0);
    apply_action(&mut a, Action::PageDown);
    assert_eq!(a.ui_state.selected(), 10, "page down = +10");
    apply_action(&mut a, Action::PageUp);
    assert_eq!(a.ui_state.selected(), 0, "page up = -10");
    apply_action(&mut a, Action::PageUp);
    assert_eq!(a.ui_state.selected(), 0, "clamped at the top");
    for _ in 0..20 {
        apply_action(&mut a, Action::PageDown);
    }
    assert_eq!(
        a.ui_state.selected(),
        a.model_list.len() - 1,
        "clamped at the bottom"
    );
}

#[test]
fn modal_page_scroll_and_home_end_record_intent() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    apply_action(&mut a, Action::SqlOpen);
    assert!(matches!(a.mode, Mode::Sql(_)), "SQL modal open");
    apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
    let scroll = |m: &Mode| match m {
        Mode::Sql(sv) => sv.scroll,
        other => panic!("expected Sql mode, got {other:?}"),
    };
    assert_eq!(scroll(&a.mode), 10, "page = 10 lines");
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    assert_eq!(scroll(&a.mode), 11, "line scroll still 1");
    apply_action(&mut a, Action::DetailScrollPage(Direction::Up));
    assert_eq!(scroll(&a.mode), 1, "page up = -10 saturating");
    apply_action(&mut a, Action::DetailScrollEnd);
    assert_eq!(
        scroll(&a.mode),
        usize::MAX,
        "End records MAX for the loop clamp"
    );
    apply_action(&mut a, Action::DetailScrollHome);
    assert_eq!(scroll(&a.mode), 0, "Home rewinds to the top");
    // The same arms drive the help overlay.
    apply_action(&mut a, Action::DetailClose);
    apply_action(&mut a, Action::HelpToggle);
    apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
    assert!(
        matches!(a.mode, Mode::Help(ref h) if h.scroll == 10),
        "help pages too"
    );
}

#[test]
fn gap_next_and_prev_cycle_through_untested_models() {
    // The big fixture has zero untested MODELS, so it pins the no-op side;
    // the sample manifest (dim_customers / agg_country_orders /
    // fct_customer_history / cll_demo untested) exercises the cycle itself.
    let mut full = app();
    apply_action(&mut full, Action::GapNext);
    assert_eq!(full.ui_state.selected(), 0, "no gaps -> selection unmoved");

    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sample_manifest.json"
    );
    let mut a = App::new(
        load_dag(sample).expect("sample loads"),
        PathBuf::from(sample),
    );
    let gaps: Vec<usize> = a
        .model_list
        .models
        .iter()
        .enumerate()
        .filter(|(_, m)| crate::coverage_gap(m))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(gaps.len(), 7, "sample manifest has seven untested models");
    apply_action(&mut a, Action::GapNext);
    let first = a.ui_state.selected();
    assert!(gaps.contains(&first), "n lands on a coverage gap");
    apply_action(&mut a, Action::GapNext);
    let second = a.ui_state.selected();
    assert!(gaps.contains(&second) && second != first, "n cycles onward");
    apply_action(&mut a, Action::GapPrev);
    assert_eq!(a.ui_state.selected(), first, "N walks back");
}

#[test]
fn bookmark_cycle_back_walks_bookmarks_in_reverse() {
    let mut a = app();
    // Bookmark models at indices 3 and 7, then cycle backward from 0.
    for i in [3usize, 7] {
        a.ui_state.set_selected(i);
        apply_action(&mut a, Action::BookmarkToggle);
    }
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::BookmarkCycleBack);
    assert_eq!(
        a.ui_state.selected(),
        7,
        "backward wraps to the last bookmark"
    );
    apply_action(&mut a, Action::BookmarkCycleBack);
    assert_eq!(
        a.ui_state.selected(),
        3,
        "backward again hits the earlier one"
    );
    apply_action(&mut a, Action::BookmarkCycle);
    assert_eq!(a.ui_state.selected(), 7, "forward cycles onward");
}

#[test]
fn lineage_extreme_jumps_send_the_cursor_to_first_and_last_columns() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let lay = crate::layout(&a.lineage_subgraph());
    let max_col = lay.columns.values().max().copied().unwrap();
    assert!(max_col >= 2, "fixture lineage spans 3+ columns");
    apply_action(&mut a, Action::LineageRightmost);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&cur], max_col, "L lands in the last column");
    apply_action(&mut a, Action::LineageLeftmost);
    let cur = a.lineage_cursor_uid().unwrap();
    assert_eq!(lay.columns[&cur], 0, "H lands in the first column");
}

#[test]
fn untested_filter_narrows_the_list_and_toggles_off() {
    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sample_manifest.json"
    );
    let mut a = App::new(
        load_dag(sample).expect("sample loads"),
        PathBuf::from(sample),
    );
    let full = a.model_list.len();
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::Untested);
    assert_eq!(a.active_list().len(), 7, "only the seven untested models");
    assert!(
        a.active_list().models.iter().all(crate::coverage_gap),
        "every visible model is untested"
    );
    assert_eq!(
        a.ui_state.model_count(),
        7,
        "selection space follows the filtered view"
    );
    assert_eq!(a.list_filter_label(), Some("untested"));
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::All, "second press toggles off");
    assert_eq!(a.active_list().len(), full, "full list restored");
    assert_eq!(a.list_filter_label(), None);
}

#[test]
fn linted_filter_keeps_finding_models_and_rebuilds_on_config() {
    let mut a = app();
    // The expected membership: model-list entries whose uid carries a
    // finding in the shared run_rules set.
    let linted: std::collections::HashSet<String> = a
        .lint_findings()
        .iter()
        .map(|f| f.node_uid.clone())
        .collect();
    let want: Vec<String> = a
        .model_list
        .models
        .iter()
        .filter(|m| linted.contains(&m.unique_id))
        .map(|m| m.unique_id.clone())
        .collect();
    assert!(!want.is_empty(), "precondition: fixture has linted models");

    // Selecting a linted model survives the toggle: apply_list_filter
    // re-resolves the uid inside the filtered list.
    a.select_by_unique_id(&want[0]);
    apply_action(&mut a, Action::ToggleLintFilter);
    assert_eq!(a.list_filter, ListFilter::Linted);
    assert_eq!(a.list_filter_label(), Some("linted"));
    let got: Vec<&str> = a
        .active_list()
        .models
        .iter()
        .map(|m| m.unique_id.as_str())
        .collect();
    assert_eq!(
        got,
        want.iter().map(String::as_str).collect::<Vec<_>>(),
        "exactly the finding models, in list order"
    );
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(want[0].as_str()),
        "selection re-resolved by uid into the linted view"
    );

    // Disabling every rule empties the finding set → set_lint_config's
    // apply_list_filter rebuild empties the live view.
    let mut cfg = crate::LintConfig::default();
    for r in crate::LintRule::ALL {
        cfg.rules.insert(r, crate::RuleEnabled::Off);
    }
    a.set_lint_config(cfg);
    assert!(
        a.active_list().is_empty(),
        "no findings → empty linted view"
    );
    assert_eq!(
        a.list_empty_reason().as_deref(),
        Some("no linted models"),
        "the filter names what it removed"
    );
    // Re-enabling restores the same membership.
    a.set_lint_config(crate::LintConfig::default());
    assert_eq!(a.active_list().len(), want.len());
    apply_action(&mut a, Action::ToggleLintFilter);
    assert_eq!(a.list_filter, ListFilter::All, "second press toggles off");
}

#[test]
fn linted_filter_is_exclusive_and_search_narrows_on_top() {
    let mut a = app();
    apply_action(&mut a, Action::ToggleLintFilter);
    assert_eq!(a.list_filter, ListFilter::Linted);
    // Another persistent filter replaces, never stacks.
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::Untested);
    apply_action(&mut a, Action::ToggleLintFilter);
    assert_eq!(a.list_filter, ListFilter::Linted);
    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert_eq!(a.list_filter, ListFilter::Bookmarked);
    apply_action(&mut a, Action::ToggleLintFilter);
    assert_eq!(a.list_filter, ListFilter::Linted);

    // A list search narrows FROM the linted view: a query matching a full-
    // list clean model still only yields finding models.
    let linted: std::collections::HashSet<String> = a
        .lint_findings()
        .iter()
        .map(|f| f.node_uid.clone())
        .collect();
    let name = a.active_list().models[0].name.clone();
    apply_action(&mut a, Action::SearchOpen);
    for c in name.chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    assert!(
        !a.active_list().is_empty()
            && a.active_list()
                .models
                .iter()
                .all(|m| linted.contains(m.unique_id.as_str())),
        "search narrows inside the linted view, not over the full list"
    );
    apply_action(&mut a, Action::SearchCancel);
    assert!(
        a.active_list()
            .models
            .iter()
            .all(|m| linted.contains(&m.unique_id)),
        "closing the search restores the linted view"
    );
}

#[test]
fn gap_next_under_the_lint_lens_cycles_finding_models() {
    // n/N are lens-aware: under Lint they walk the finding models instead of
    // the coverage gaps, scoped to the active list and wrapping.
    let mut a = app();
    a.ui_state.set_lens(LineageLens::Lint);
    let linted: std::collections::HashSet<String> = a
        .lint_findings()
        .iter()
        .map(|f| f.node_uid.clone())
        .collect();
    let targets: Vec<usize> = a
        .model_list
        .models
        .iter()
        .enumerate()
        .filter(|(_, m)| linted.contains(m.unique_id.as_str()))
        .map(|(i, _)| i)
        .collect();
    assert!(targets.len() >= 2, "fixture has multiple linted models");

    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::GapPrev);
    assert_eq!(
        a.ui_state.selected(),
        *targets.last().unwrap(),
        "N wraps back to the last linted model"
    );
    apply_action(&mut a, Action::GapNext);
    assert_eq!(
        a.ui_state.selected(),
        targets[0],
        "n wraps forward to the first"
    );
    apply_action(&mut a, Action::GapNext);
    let second = a.ui_state.selected();
    assert!(
        targets.contains(&second) && second != targets[0],
        "n cycles onward through the linted models"
    );

    // Off the Lint lens the keys are the coverage-gap cycle again — the big
    // fixture has no untested models, so n is a no-op there.
    a.ui_state.set_lens(LineageLens::Off);
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::GapNext);
    assert_eq!(
        a.ui_state.selected(),
        0,
        "outside the Lint lens n is the untested cycle"
    );

    // Scoped to the active list: under the linted filter the cycle only
    // lands on members of that view (all of which are linted).
    a.ui_state.set_lens(LineageLens::Lint);
    apply_action(&mut a, Action::ToggleLintFilter);
    apply_action(&mut a, Action::GapNext);
    let uid = a.selected_unique_id().expect("cycle landed somewhere");
    assert!(
        linted.contains(uid.as_str()),
        "the scoped cycle lands inside the filtered view"
    );
    apply_action(&mut a, Action::ToggleLintFilter);

    // No findings → a no-op under the lens, and the palette reason names it.
    let mut cfg = crate::LintConfig::default();
    for r in crate::LintRule::ALL {
        cfg.rules.insert(r, crate::RuleEnabled::Off);
    }
    a.set_lint_config(cfg);
    a.ui_state.set_selected(0);
    apply_action(&mut a, Action::GapNext);
    assert_eq!(a.ui_state.selected(), 0, "no findings → no-op");
    assert_eq!(
        a.unavailable_reason(Action::GapNext).as_deref(),
        Some("no lint findings in the list"),
        "the lens-aware reason names the missing set"
    );
    a.ui_state.set_lens(LineageLens::Off);
    assert_eq!(
        a.unavailable_reason(Action::GapNext).as_deref(),
        Some("no untested models"),
        "off the lens the reason names the coverage gaps again"
    );
}

#[test]
fn bookmark_filter_tracks_toggles_and_search_narrows_on_top() {
    let mut a = app();
    let kept = "model.jaffle_finance.stg_payment__shoppers";
    a.select_by_unique_id(kept);
    apply_action(&mut a, Action::BookmarkToggle);
    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert_eq!(a.active_list().len(), 1, "only the bookmarked model");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some(kept),
        "selection re-resolved by id into the filtered view"
    );
    // A list search narrows FROM the bookmarked view: a query matching
    // many full-list models still shows only the bookmarked one.
    apply_action(&mut a, Action::SearchOpen);
    for c in "stg".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    assert_eq!(a.active_list().len(), 1, "search composes with the filter");
    apply_action(&mut a, Action::SearchCancel);
    // Un-bookmarking the row under the live Bookmarked filter empties it.
    apply_action(&mut a, Action::BookmarkToggle);
    assert_eq!(a.active_list().len(), 0, "un-bookmark leaves the view");
    // Toggling the OTHER filter replaces, not stacks.
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::Untested);
    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert_eq!(a.list_filter, ListFilter::Bookmarked);
}

#[test]
fn critical_path_is_a_deterministic_source_rooted_chain() {
    let a = app();
    let sv = a.compute_stats_view();
    assert!(
        sv.critical_path.len() >= 4,
        "the fixture graph is at least 4 deep, got {:?}",
        sv.critical_path
    );
    // The chain start must have no parents (else its parent's chain would
    // be longer) and the end no children — both checked by name lookup.
    let by_name = |name: &str| {
        a.dag
            .nodes()
            .values()
            .filter(|n| n.name == name)
            .collect::<Vec<_>>()
    };
    let first = sv.critical_path.first().unwrap();
    assert!(
        by_name(first).iter().any(|n| n.direct_up == 0),
        "chain starts at a root: {first}"
    );
    let last = sv.critical_path.last().unwrap();
    assert!(
        by_name(last).iter().any(|n| n.direct_down == 0),
        "chain ends at a leaf: {last}"
    );
    // Deterministic: a second computation is identical.
    assert_eq!(sv.critical_path, a.compute_stats_view().critical_path);
}

#[test]
fn stats_dashboard_counts_name_match_fallbacks() {
    // One parseable model and one with unparsable raw_code: the dashboard's
    // aggregate count must line up with the resolver's per-model flag (the
    // detail modal's `[name-match]` tag and `dbtl columns`'s stderr count).
    let dag = pr_dag(
        vec![
            ("model.p.good", pr_model("good", "marts", "select 1 as id")),
            (
                "model.p.bad",
                pr_model("bad", "marts", "select from where (((broken"),
            ),
        ],
        &[],
        vec![],
    );
    let a = App::new(dag, PathBuf::from("/x/manifest.json"));
    let sv = a.compute_stats_view();
    assert_eq!(sv.cll_fallbacks, 1);
    assert!(a.cll.is_fallback("model.p.bad"));
    assert!(!a.cll.is_fallback("model.p.good"));
    // The sweep is memoized: a second computation is identical and cheap.
    assert_eq!(a.compute_stats_view().cll_fallbacks, 1);
}

#[test]
fn longest_chain_terminates_on_a_cyclic_manifest() {
    // A malformed manifest.json can carry a model-level cycle that survives
    // the prune (only test/operation nodes are dropped): a ⇄ b, plus a clean
    // tail b → c. Without the on-stack guard the memoized DFS recursed
    // forever and overflowed the stack as soon as the Stats dashboard (`i`)
    // computed the critical path.
    use crate::{RawManifest, RawNode};
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    let mut add = |id: &str, name: &str| {
        nodes.insert(
            id.to_string(),
            RawNode {
                name: name.into(),
                resource_type: "model".into(),
                ..Default::default()
            },
        );
    };
    add("model.p.a", "a");
    add("model.p.b", "b");
    add("model.p.c", "c");
    let child_map = HashMap::from([
        ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
        (
            "model.p.b".to_string(),
            vec!["model.p.a".to_string(), "model.p.c".to_string()],
        ),
    ]);
    let parent_map = HashMap::from([
        ("model.p.a".to_string(), vec!["model.p.b".to_string()]),
        ("model.p.b".to_string(), vec!["model.p.a".to_string()]),
        ("model.p.c".to_string(), vec!["model.p.b".to_string()]),
    ]);
    let dag = Dag::build(&RawManifest {
        unresolved_adjacent: Default::default(),
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
        docs: Default::default(),
    });
    let chain = super::analysis::longest_chain(&dag);
    // Sane: non-empty, no repeated node, every hop is a real kept edge.
    assert!(!chain.is_empty(), "a chain is still produced");
    let unique: std::collections::HashSet<&String> = chain.iter().collect();
    assert_eq!(unique.len(), chain.len(), "the chain never revisits a node");
    let edges: std::collections::HashSet<(String, String)> = dag
        .edges()
        .into_iter()
        .map(|(p, c)| {
            let name = |uid: &str| {
                dag.get(uid)
                    .map_or_else(|| uid.to_string(), |n| n.name.clone())
            };
            (name(&p), name(&c))
        })
        .collect();
    for hop in chain.windows(2) {
        assert!(
            edges.contains(&(hop[0].clone(), hop[1].clone())),
            "every hop is a real edge: {hop:?}"
        );
    }
    // Deterministic, smallest-uid-first: the DFS visits `a` first, so the
    // cycle breaks at the back-edge into `a` and the chain is a → b.
    assert_eq!(chain, vec!["a".to_string(), "b".to_string()]);
    // Deterministic: a second computation is identical.
    assert_eq!(chain, super::analysis::longest_chain(&dag));
}
