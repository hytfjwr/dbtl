use super::*;

// ---- sibling collapse (`+N more`) ----

/// The acceptance fixture: `hub -> c00..c{kids-1}`, all models, plus an
/// optional `leaf -> leaf` edge under one child (the path-visibility case).
/// Built as a `RawManifest` so the collapse tests exercise the real
/// `Dag`/`Subgraph` plumbing, not a hand-rolled subgraph.
fn collapse_dag(kids: usize, leaf_parent: Option<&str>) -> Dag {
    use std::collections::HashMap;
    let mut node_map = HashMap::new();
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut edge = |p: &str, c: &str| {
        child_map
            .entry(p.to_string())
            .or_default()
            .push(c.to_string());
        parent_map
            .entry(c.to_string())
            .or_default()
            .push(p.to_string());
    };
    node_map.insert(
        "model.p.hub".to_string(),
        pr_model("hub", "marts", "select 0"),
    );
    for i in 0..kids {
        let uid = format!("model.p.c{i:02}");
        node_map.insert(
            uid.clone(),
            pr_model(&format!("c{i:02}"), "marts", "select 1"),
        );
        edge("model.p.hub", &uid);
    }
    if let Some(parent) = leaf_parent {
        node_map.insert(
            "model.p.leaf".to_string(),
            pr_model("leaf", "marts", "select 2"),
        );
        edge(parent, "model.p.leaf");
    }
    Dag::build(&crate::RawManifest {
        metadata: Default::default(),
        nodes: node_map,
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
    })
}

/// The collapse-acceptance `App`: the 20-sibling hub rooted, at an explicit
/// threshold (the fixture `app()` pins 0 — these tests are the opposite).
fn collapse_app(kids: usize, threshold: usize) -> App {
    collapse_app_with_leaf(kids, threshold, None)
}

fn collapse_app_with_leaf(kids: usize, threshold: usize, leaf_parent: Option<&str>) -> App {
    let mut a = App::new(
        collapse_dag(kids, leaf_parent),
        PathBuf::from("/p/target/manifest.json"),
    );
    a.set_collapse_threshold(threshold);
    assert!(a.select_by_unique_id("model.p.hub"), "hub is a list member");
    a
}

/// The single `+more` marker uid a collapsed hub view draws (fails the test
/// when zero or several markers exist — the hub shape guarantees exactly one).
fn the_marker(a: &App) -> String {
    let cv = a.collapsed_view();
    let markers: Vec<&String> = cv.groups.keys().collect();
    assert_eq!(markers.len(), 1, "exactly one marker group: {markers:?}");
    markers[0].clone()
}

#[test]
fn collapse_contracts_oversized_siblings_into_one_marker() {
    // Structural-map assertion (columns/rects), per the repo convention.
    let a = collapse_app(20, 8);
    let lay = a.styled_lineage_layout().expect("hub view lays out");
    assert_eq!(
        lay.rects.len(),
        1 + 8 + 1,
        "hub + 8 kept siblings + the marker"
    );
    assert_eq!(lay.columns["model.p.hub"], 0);
    for i in 0..8 {
        let uid = format!("model.p.c{i:02}");
        assert_eq!(
            lay.columns.get(&uid).copied(),
            Some(1),
            "kept sibling {uid} in the children column"
        );
    }
    for i in 8..20 {
        let uid = format!("model.p.c{i:02}");
        assert!(!lay.rects.contains_key(&uid), "hidden sibling {uid}");
    }
    let marker = the_marker(&a);
    assert_eq!(
        marker, "model.p.c08+more",
        "marker derives from the default run's lead member (members[threshold])"
    );
    assert_eq!(
        lay.columns.get(&marker).copied(),
        Some(1),
        "marker occupies the column it collapsed"
    );
    // The marker stacks AFTER the last kept sibling (uid order in-column).
    assert!(lay.rects[&marker].y > lay.rects["model.p.c07"].y);
    // Its box label is the pseudo-node's name — real geometry input.
    let cv = a.collapsed_view();
    let marker_node = cv
        .subgraph
        .nodes
        .iter()
        .find(|n| n.unique_id == marker)
        .expect("marker is a node");
    assert_eq!(marker_node.name, "+12 more");
    // Semantic surfaces are untouched: the rooted subgraph keeps all 20.
    assert_eq!(a.lineage_subgraph().nodes.len(), 21);
}

#[test]
fn enter_on_marker_toggles_expand_and_recollapse() {
    let mut a = collapse_app(20, 8);
    a.ui_state.set_focus(Focus::RightPane);
    let marker = the_marker(&a);
    a.tab_mut().lineage_cursor = Some(marker.clone());
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(marker.as_str()),
        "the marker is a valid cursor position"
    );

    // Enter: expands the group — every member drawn, behind a `-12 less`.
    let out = apply_action(&mut a, Action::DetailOpen);
    assert!(out.effects.is_empty());
    assert!(
        matches!(a.mode, Mode::Selection),
        "a marker Enter toggles, never opens the modal"
    );
    let lay = a.styled_lineage_layout().expect("expanded layout");
    assert_eq!(lay.rects.len(), 1 + 20 + 1, "hub + 20 members + the toggle");
    for i in 0..20 {
        let uid = format!("model.p.c{i:02}");
        assert!(lay.rects.contains_key(&uid), "expanded shows {uid}");
    }
    // Same marker uid (the keep-set is unchanged), now labelled `-M less`.
    let cv = a.collapsed_view();
    let less = cv
        .subgraph
        .nodes
        .iter()
        .find(|n| n.unique_id == marker)
        .expect("the toggle marker stays");
    assert_eq!(less.name, "-12 less");

    // Enter on `-12 less`: collapses back to 8 kept + `+12 more`.
    apply_action(&mut a, Action::DetailOpen);
    let lay = a.styled_lineage_layout().expect("re-collapsed layout");
    assert_eq!(lay.rects.len(), 1 + 8 + 1, "collapsed again");
    assert!(!lay.rects.contains_key("model.p.c15"));
}

#[test]
fn marker_cursor_survives_walking_off_a_revealed_member() {
    // Regression: the marker uid derives from the group's default run lead
    // (`members[threshold]`), so parking the cursor on a revealed member (a
    // priority-keep that reshuffles the hidden tail) then walking onto the
    // marker must keep the cursor there — before the fix, the marker's uid
    // changed and the cursor silently snapped back to the root.
    let mut a = collapse_app(20, 8);
    a.ui_state.set_focus(Focus::RightPane);
    let marker = the_marker(&a);
    a.tab_mut().lineage_cursor = Some(marker.clone());
    apply_action(&mut a, Action::DetailOpen); // expand the group

    // Park on a revealed member deep in the tail.
    a.tab_mut().lineage_cursor = Some("model.p.c15".to_string());
    assert_eq!(
        the_marker(&a),
        marker,
        "a priority member reshuffles the hidden run but never the marker uid"
    );

    // Walk Up until the marker: the cursor must stay on it (the uid is still
    // a live node of the recomputed view — no snap-back to the root).
    for _ in 0..20 {
        if a.lineage_cursor_uid().as_deref() == Some(marker.as_str()) {
            break;
        }
        let before = a.lineage_cursor_uid();
        a.move_lineage_cursor(Direction::Up);
        assert_ne!(
            a.lineage_cursor_uid(),
            before,
            "walked past the marker or stalled at {before:?}"
        );
    }
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(marker.as_str()),
        "cursor parked on the marker survives the keep-set flip"
    );
}

#[test]
fn yank_column_lineage_falls_back_to_the_traced_column() {
    // `y l` from Selection mid-trace has no `pending_col` (no modal cursor),
    // but the trace names a column — yank ITS downstream closure instead of
    // dead-ending on "no column selected".
    let mut a = sample_app();
    a.column_focus_enter(ColumnRef::new(STG, "id"));
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankColumnLineage);
    let text = match &out.effects[..] {
        [Effect::Yank(t)] => t.clone(),
        e => panic!("expected the yank effect, got {e:?}"),
    };
    assert!(
        text.contains("order_id") || text.contains("cll_demo"),
        "the traced column's closure made the clipboard: {text}"
    );

    // Unfocused, the fallback is gone — the pick-a-column notice returns.
    let mut a = sample_app();
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::YankColumnLineage);
    assert!(out.effects.is_empty());
    assert!(a.take_notice().is_some(), "the no-column notice survives");
}

#[test]
fn cursor_moves_onto_the_marker_like_any_node() {
    let mut a = collapse_app(20, 8);
    a.ui_state.set_focus(Focus::RightPane);
    // Column 1 stacks c00..c07 then the marker (uid order): Down from the last
    // kept sibling lands on the marker; Down again stays (column bottom).
    a.tab_mut().lineage_cursor = Some("model.p.c07".to_string());
    a.move_lineage_cursor(Direction::Down);
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some("model.p.c08+more"),
        "down from the last kept sibling lands on the marker"
    );
    a.move_lineage_cursor(Direction::Down);
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some("model.p.c08+more"),
        "the marker is the column bottom"
    );
    // The marker is a real movement node horizontally too: Left reaches the
    // hub (the only column-0 box), Right returns into the children column.
    a.move_lineage_cursor(Direction::Left);
    assert_eq!(a.lineage_cursor_uid().as_deref(), Some("model.p.hub"));
    a.move_lineage_cursor(Direction::Right);
    assert!(
        a.lineage_cursor_uid().is_some(),
        "back into the children column"
    );
}

#[test]
fn lineage_search_auto_expands_a_hidden_match() {
    let mut a = collapse_app(20, 8);
    a.ui_state.set_focus(Focus::RightPane); // `/` targets the lineage pane
                                            // Pre-condition: c15 hides behind the marker.
    assert!(!a
        .styled_lineage_layout()
        .unwrap()
        .rects
        .contains_key("model.p.c15"));

    apply_action(&mut a, Action::SearchOpen);
    for c in "c15".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    assert_eq!(
        a.current_lineage_match().as_deref(),
        Some("model.p.c15"),
        "the query lands on the hidden member"
    );
    let lay = a.styled_lineage_layout().expect("revealed layout");
    assert!(
        lay.rects.contains_key("model.p.c15"),
        "the hidden match's group expanded — the match is drawn"
    );
    // The reveal is expansion state, not search state: it survives a cancel.
    apply_action(&mut a, Action::SearchCancel);
    let lay = a.styled_lineage_layout().expect("post-search layout");
    assert!(
        lay.rects.contains_key("model.p.c15"),
        "the revealed member stays expanded after the search closes"
    );
}

#[test]
fn keep_priority_bookmarks_and_path_nodes() {
    // A bookmarked tail member sorts past the cut but stays visible.
    let mut a = collapse_app(20, 8);
    a.bookmarks.insert("model.p.c15".to_string());
    let lay = a.styled_lineage_layout().expect("layout");
    assert!(
        lay.rects.contains_key("model.p.c15"),
        "a bookmarked member is never hidden"
    );
    // kept = the threshold slots, priority-first: {c15, c00..c06} (c15 takes a
    // slot inside the first 8, so c07 spills into the marker tail).
    assert_eq!(lay.rects.len(), 1 + 8 + 1);
    assert!(!lay.rects.contains_key("model.p.c07"), "c07 fills the tail");

    // A hidden member on the root↔cursor path stays visible: cursor the leaf
    // under c15, and the path hub → c15 → leaf keeps c15 drawn.
    let mut a = collapse_app_with_leaf(20, 8, Some("model.p.c15"));
    a.ui_state.set_focus(Focus::RightPane);
    a.tab_mut().lineage_cursor = Some("model.p.leaf".to_string());
    let lay = a.styled_lineage_layout().expect("layout");
    assert!(
        lay.rects.contains_key("model.p.c15"),
        "a path member is never hidden behind its marker"
    );
    assert!(lay.rects.contains_key("model.p.leaf"));
}

#[test]
fn expansion_resets_on_re_root_and_survives_same_root() {
    let mut a = collapse_app(20, 8);
    a.ui_state.set_focus(Focus::RightPane);
    a.tab_mut().lineage_cursor = Some(the_marker(&a));
    apply_action(&mut a, Action::DetailOpen); // expand
    assert_eq!(a.styled_lineage_layout().unwrap().rects.len(), 22);

    // Re-root: the expansions are per-root state — the new root recomputes.
    assert!(a.select_by_unique_id("model.p.c05"));
    // Rooted on c05, the hub is UPSTREAM: its own 20-child group is not in
    // this subgraph at all — but the expansion key must not linger.
    assert!(
        a.collapsed_view().groups.is_empty(),
        "no group under the new root"
    );

    // ...and back: the hub's group is collapsed again (state was dropped).
    assert!(a.select_by_unique_id("model.p.hub"));
    let lay = a.styled_lineage_layout().expect("back at the hub");
    assert_eq!(lay.rects.len(), 1 + 8 + 1, "expansions reset on re-root");
}

#[test]
fn exports_keep_the_semantic_graph_but_ascii_mirrors_the_screen() {
    // Policy: Mermaid/DOT emit every real node (no markers); the ASCII yank is
    // screen-equivalent — the pane's diagram, `+M more` markers included.
    let a = collapse_app(20, 8);
    let mermaid = a.lineage_mermaid().expect("mermaid");
    assert!(mermaid.contains("c19"), "mermaid keeps hidden members");
    assert!(!mermaid.contains("+more"), "no marker in mermaid");
    let dot = a.lineage_dot().expect("dot");
    assert!(dot.contains("model.p.c19"), "dot keeps hidden members");
    assert!(!dot.contains("+more"), "no marker in dot");
    let ascii = a.lineage_ascii().expect("ascii");
    assert!(
        ascii.contains("+12 more"),
        "screen-equivalent ascii draws the marker:\n{ascii}"
    );
    assert!(
        !ascii.contains("c19"),
        "hidden members stay hidden in ascii"
    );
}

/// The nested-hub collapse `App`: `hub → m00..m15` plus `m10 → d00..d15` —
/// two collapsible groups, the second's parent hidden behind the first's
/// marker.
fn nested_hub_app(threshold: usize) -> App {
    use std::collections::HashMap;
    let mut node_map = HashMap::new();
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut edge = |p: &str, c: &str| {
        child_map
            .entry(p.to_string())
            .or_default()
            .push(c.to_string());
        parent_map
            .entry(c.to_string())
            .or_default()
            .push(p.to_string());
    };
    node_map.insert(
        "model.p.hub".to_string(),
        pr_model("hub", "marts", "select 0"),
    );
    for i in 0..16 {
        let m = format!("model.p.m{i:02}");
        let d = format!("model.p.d{i:02}");
        node_map.insert(
            m.clone(),
            pr_model(&format!("m{i:02}"), "marts", "select 1"),
        );
        node_map.insert(
            d.clone(),
            pr_model(&format!("d{i:02}"), "marts", "select 2"),
        );
        edge("model.p.hub", &m);
        edge("model.p.m10", &d);
    }
    let dag = Dag::build(&crate::RawManifest {
        metadata: Default::default(),
        nodes: node_map,
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
    let mut a = App::new(dag, PathBuf::from("/p/target/manifest.json"));
    a.set_collapse_threshold(threshold);
    assert!(a.select_by_unique_id("model.p.hub"), "hub is a list member");
    a
}

#[test]
fn expanded_nested_marker_stays_wired_through_the_hidden_parents_marker() {
    // hub → m00..m15 collapses behind `m08+more`; the hidden m10 owns the
    // d-group `d08+more`. Expanding that marker (the Enter path) must keep it
    // wired to the OUTER marker — before the fix, the `-M less` edge hung off
    // the missing m10 and the marker dropped to column 1, connector-less.
    let mut a = nested_hub_app(8);
    a.ui_state.set_focus(Focus::RightPane);
    let lay = a.styled_lineage_layout().expect("collapsed layout");
    assert_eq!(
        lay.columns.get("model.p.d08+more").copied(),
        Some(2),
        "the nested group's marker draws under the outer marker"
    );

    // Enter on the nested marker expands it in place.
    a.tab_mut().lineage_cursor = Some("model.p.d08+more".to_string());
    apply_action(&mut a, Action::DetailOpen);
    let lay = a.styled_lineage_layout().expect("expanded layout");
    assert!(
        lay.rects.contains_key("model.p.d00"),
        "expansion draws the members"
    );
    assert_eq!(
        lay.columns.get("model.p.d08+more").copied(),
        Some(2),
        "the -M less marker stays in its column — no orphan"
    );
    let cv = a.collapsed_view();
    let es: std::collections::BTreeSet<(&str, &str)> = cv
        .subgraph
        .edges
        .iter()
        .map(|e| (e.parent.as_str(), e.child.as_str()))
        .collect();
    assert!(
        es.contains(&("model.p.m08+more", "model.p.d08+more")),
        "the marker edge resolves through the hidden parent's marker: {es:?}"
    );
    assert!(
        !es.iter().any(|(p, _)| *p == "model.p.m10"),
        "no edge dangles to a hidden node: {es:?}"
    );
}

#[test]
fn modal_yank_ascii_does_not_inherit_the_current_roots_keep_set() {
    // The `y`-over-modal base is rooted on the modal's node (`pending_node`).
    // When that subject is NOT the pane's root — e.g. Enter on a source opens
    // the modal without re-rooting — the current root's cursor/path keep-alive
    // must not leak into its collapse: the foreign root keeps only its own
    // subject. `gp → hub → m00..m15`: rooted on gp, the hub's group collapses.
    use std::collections::HashMap;
    let mut node_map = HashMap::new();
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut edge = |p: &str, c: &str| {
        child_map
            .entry(p.to_string())
            .or_default()
            .push(c.to_string());
        parent_map
            .entry(c.to_string())
            .or_default()
            .push(p.to_string());
    };
    node_map.insert(
        "model.p.gp".to_string(),
        pr_model("gp", "marts", "select 0"),
    );
    node_map.insert(
        "model.p.hub".to_string(),
        pr_model("hub", "marts", "select 0"),
    );
    edge("model.p.gp", "model.p.hub");
    for i in 0..16 {
        let m = format!("model.p.m{i:02}");
        node_map.insert(
            m.clone(),
            pr_model(&format!("m{i:02}"), "marts", "select 1"),
        );
        edge("model.p.hub", &m);
    }
    let dag = Dag::build(&crate::RawManifest {
        metadata: Default::default(),
        nodes: node_map,
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
    let mut a = App::new(dag, PathBuf::from("/p/target/manifest.json"));
    a.set_collapse_threshold(8);
    assert!(a.select_by_unique_id("model.p.hub"));
    a.ui_state.set_focus(Focus::RightPane);
    // Under the CURRENT root (hub) the cursor keeps the tail member drawn.
    a.tab_mut().lineage_cursor = Some("model.p.m15".to_string());
    assert!(a
        .styled_lineage_layout()
        .unwrap()
        .rects
        .contains_key("model.p.m15"));

    // The modal-yank base (pending_node = gp ≠ the pane root) keeps only its
    // own subject: m15 goes back behind the marker.
    a.pending_node = Some("model.p.gp".to_string());
    a.mode = Mode::Yank { back: None, sel: 0 };
    let ascii = a.lineage_ascii().expect("ascii");
    assert!(
        ascii.contains("+8 more"),
        "the group still collapses under the modal root:\n{ascii}"
    );
    assert!(
        !ascii.contains("m15"),
        "the pane root's cursor keep-alive must not leak into the foreign \
         root's export:\n{ascii}"
    );
}

#[test]
fn click_marker_toggles_and_parks_the_cursor_and_s_notices() {
    let mut a = collapse_app(20, 8);
    a.ui_state.set_focus(Focus::RightPane);
    let marker = the_marker(&a);
    // A double-click (the commit) toggles the group AND parks the cursor on
    // the marker — the same focus-the-click behaviour a node commit gets.
    node_commit(&mut a, &marker);
    assert_eq!(
        a.lineage_cursor_uid().as_deref(),
        Some(marker.as_str()),
        "the marker click takes the cursor"
    );
    assert_eq!(
        a.styled_lineage_layout().unwrap().rects.len(),
        1 + 20 + 1,
        "the click expanded the group"
    );
    // `s` on a collapse marker: a notice, never a preview modal.
    apply_action(&mut a, Action::SqlOpen);
    assert!(
        matches!(a.mode, Mode::Selection),
        "no sql preview for collapse chrome"
    );
    assert_eq!(
        a.take_notice().as_deref(),
        Some("no SQL for a collapse marker (Enter toggles it)")
    );
    // A second commit collapses back.
    node_commit(&mut a, &marker);
    assert_eq!(
        a.styled_lineage_layout().unwrap().rects.len(),
        1 + 8 + 1,
        "the second click collapsed the group"
    );
}

// ---------------------------------------------------------------------------
// per-column test badges, the tests-section cursor + relationships
// jump, and the PrimaryKey lens.
// ---------------------------------------------------------------------------

/// A tiny Dag: `orders` is PK-guaranteed (unique + not_null on `id`) and
/// carries a relationships test into `customers`; `customers` has only a
/// lone not_null (no guarantee → the lens tints it); `loose` has no tests at
/// all. `orders` declares 4 columns so the badge set differs per row.
fn pk_dag() -> Dag {
    use crate::{RawManifest, RawNode, RawTestMetadata};
    use std::collections::HashMap;
    let cols = |names: &[&str]| {
        names
            .iter()
            .map(|n| (n.to_string(), serde_json::json!({"name": n})))
            .collect::<serde_json::Map<_, _>>()
    };
    let mut nodes = HashMap::new();
    nodes.insert(
        "model.p.orders".to_string(),
        RawNode {
            name: "orders".into(),
            resource_type: "model".into(),
            path: Some("marts/orders.sql".into()),
            columns: cols(&["id", "customer_id", "status", "total"]),
            ..Default::default()
        },
    );
    nodes.insert(
        "model.p.customers".to_string(),
        RawNode {
            name: "customers".into(),
            resource_type: "model".into(),
            path: Some("marts/customers.sql".into()),
            columns: cols(&["id", "name"]),
            ..Default::default()
        },
    );
    nodes.insert(
        "model.p.loose".to_string(),
        RawNode {
            name: "loose".into(),
            resource_type: "model".into(),
            path: Some("marts/loose.sql".into()),
            ..Default::default()
        },
    );
    let mut test = |uid: &str, kind: &str, attached: &str, col: Option<&str>| {
        nodes.insert(
            uid.to_string(),
            RawNode {
                name: uid.into(),
                resource_type: "test".into(),
                attached_node: Some(attached.into()),
                column_name: col.map(str::to_string),
                test_metadata: Some(RawTestMetadata {
                    name: Some(kind.into()),
                    ..Default::default()
                }),
                depends_on: crate::RawDependsOn {
                    macros: vec![],
                    nodes: vec![attached.to_string()],
                },
                ..Default::default()
            },
        );
    };
    test("test.p.u_id", "unique", "model.p.orders", Some("id"));
    test("test.p.nn_id", "not_null", "model.p.orders", Some("id"));
    test(
        "test.p.av",
        "accepted_values",
        "model.p.orders",
        Some("status"),
    );
    test("test.p.nn_c", "not_null", "model.p.customers", Some("id"));
    // The relationships test: attached to orders.customer_id, depends_on
    // carries BOTH ends (the attached node + the `to:` target), `field:` the
    // referenced column.
    nodes.insert(
        "test.p.rel".to_string(),
        RawNode {
            name: "rel".into(),
            resource_type: "test".into(),
            attached_node: Some("model.p.orders".into()),
            column_name: Some("customer_id".into()),
            test_metadata: Some(RawTestMetadata {
                name: Some("relationships".into()),
                kwargs: Some(serde_json::Map::from_iter([(
                    "field".to_string(),
                    serde_json::Value::String("id".to_string()),
                )])),
            }),
            depends_on: crate::RawDependsOn {
                macros: vec![],
                nodes: vec![
                    "model.p.orders".to_string(),
                    "model.p.customers".to_string(),
                ],
            },
            ..Default::default()
        },
    );
    Dag::build(&RawManifest {
        metadata: Default::default(),
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::from([(
            "model.p.orders".to_string(),
            vec!["model.p.customers".to_string()],
        )]),
        child_map: HashMap::from([(
            "model.p.customers".to_string(),
            vec!["model.p.orders".to_string()],
        )]),
        macros: HashMap::new(),
    })
}

fn pk_app() -> App {
    App::new(pk_dag(), PathBuf::from("/tmp/x/target/manifest.json"))
}

#[test]
fn pk_lens_tints_only_models_without_a_guarantee() {
    let mut a = pk_app();
    // `customers` carries only a lone not_null — no guarantee → Violation.
    assert_eq!(
        tint_of(&mut a, LineageLens::PrimaryKey, "model.p.customers"),
        LensTint::Violation,
        "the not_null-only model is violation-tinted"
    );
    // `orders` has unique+not_null on `id` → guaranteed → no tint.
    assert_eq!(
        tint_of(&mut a, LineageLens::PrimaryKey, "model.p.orders"),
        LensTint::None,
        "the guaranteed model keeps its class colour"
    );
    // The fixture agrees: the two known PK-missing models tint, a guaranteed
    // marts model does not — the SAME set `missing_primary_key_tests` flags.
    let mut a = app();
    assert_eq!(
        tint_of(
            &mut a,
            LineageLens::PrimaryKey,
            "model.jaffle_finance.int_shoppers__combined"
        ),
        LensTint::Violation,
        "a fixture PK-missing model tints"
    );
    assert_eq!(
        tint_of(
            &mut a,
            LineageLens::PrimaryKey,
            "model.jaffle_finance.dim_delivery_lanes"
        ),
        LensTint::None,
        "a fixture PK-guaranteed model does not"
    );
}

#[test]
fn cycle_lens_never_skips_primary_key() {
    // PrimaryKey is data-independent: with NO lint findings, diff baseline,
    // run_results, or column focus, the reducer's CycleLens arm still lands
    // on it — it is not in the skippable set.
    let mut a = pk_app();
    for _ in 0..=10 {
        apply_action(&mut a, Action::CycleLens);
        if a.ui_state.lens() == LineageLens::PrimaryKey {
            break;
        }
    }
    assert_eq!(a.ui_state.lens(), LineageLens::PrimaryKey);
    apply_action(&mut a, Action::CycleLens);
    assert_eq!(a.ui_state.lens(), LineageLens::Off, "then wraps to Off");
}

#[test]
fn detail_section_toggle_lands_only_on_jumpable_tests() {
    let mut a = pk_app();
    open_detail(&mut a, "model.p.orders");
    // orders' tests sort by kind: accepted_values(0), not_null(1),
    // relationships(2), unique(3) — only the relationships row is jumpable.
    apply_action(&mut a, Action::DetailSectionToggle);
    match &a.mode {
        Mode::Detail(dv) => {
            assert_eq!(dv.test_index, 2, "Tab lands on the relationships row");
            assert_eq!(
                dv.tests[dv.test_index].ref_uid.as_deref(),
                Some("model.p.customers")
            );
        }
        m => panic!("expected Detail, got {m:?}"),
    }
    // h/l stays put — the only jumpable row is the cursor's own.
    apply_action(&mut a, Action::DetailColumnMove(Direction::Down));
    apply_action(&mut a, Action::DetailColumnMove(Direction::Up));
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.test_index, 2, "clamped at the single jumpable row"),
        m => panic!("expected Detail, got {m:?}"),
    }
    // Tab again hands the cursor back to the columns section.
    apply_action(&mut a, Action::DetailSectionToggle);
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.test_index, usize::MAX),
        m => panic!("expected Detail, got {m:?}"),
    }
}

#[test]
fn detail_enter_on_test_row_jumps_to_the_relationships_target() {
    let mut a = pk_app();
    open_detail(&mut a, "model.p.orders");
    apply_action(&mut a, Action::DetailSectionToggle);
    apply_action(&mut a, Action::ColumnFocusEnter);
    assert_eq!(a.mode, Mode::Selection, "the modal closes on the jump");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.p.customers"),
        "Enter re-roots to the `to:` target"
    );
}

#[test]
fn detail_tab_notices_when_no_jumpable_test_exists() {
    let mut a = pk_app();
    open_detail(&mut a, "model.p.loose");
    apply_action(&mut a, Action::DetailSectionToggle);
    match &a.mode {
        Mode::Detail(dv) => assert_eq!(dv.test_index, usize::MAX, "cursor stays on columns"),
        m => panic!("expected Detail, got {m:?}"),
    }
    assert_eq!(
        a.take_notice().as_deref(),
        Some("no relationship test targets on this node")
    );
}
