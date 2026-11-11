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
    assert_eq!(gaps.len(), 5, "sample manifest has five untested models");
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
    assert_eq!(a.active_list().len(), 5, "only the five untested models");
    assert!(
        a.active_list().models.iter().all(crate::coverage_gap),
        "every visible model is untested"
    );
    assert_eq!(
        a.ui_state.model_count(),
        5,
        "selection space follows the filtered view"
    );
    assert_eq!(a.list_filter_label(), Some("untested"));
    apply_action(&mut a, Action::ToggleUntestedFilter);
    assert_eq!(a.list_filter, ListFilter::All, "second press toggles off");
    assert_eq!(a.active_list().len(), full, "full list restored");
    assert_eq!(a.list_filter_label(), None);
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
