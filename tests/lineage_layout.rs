//! Lineage-layout tests for the PURE `layout()` function (no terminal).
//!
//! The golden freezes the visual contract on a tiny hand-built diamond; the
//! real-manifest tests assert structural invariants against `layout()`'s
//! structured maps (`columns`/`rects`), NOT CharGrid text scanning, because the
//! fct/pos subgraphs contain shared-prefix names (`stg_payment__suppliers` vs
//! `…supplier_departments`) that would make a textual `contains` false-positive.

use std::collections::HashSet;

use dbtl::layout::{layout, layout_mode, GlyphMode, Layout};
use dbtl::{load_dag, Dag, Edge, NodeInfo, Subgraph};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fixture manifest must load")
}

// ============================================================================
// Golden diamond exact-match (visual contract frozen)
// ============================================================================

fn model(id: &str, name: &str) -> NodeInfo {
    NodeInfo {
        unique_id: id.to_string(),
        name: name.to_string(),
        resource_type: "model".to_string(),
        path: Some(format!("staging/{name}.sql")),
        ..Default::default()
    }
}

fn edge(parent: &str, child: &str) -> Edge {
    Edge {
        parent: parent.to_string(),
        child: child.to_string(),
    }
}

/// The contract's diamond topology: a->b, a->c, b->d, c->d, selected = d.
/// col(a)=0, col(b)=col(c)=1, col(d)=2. b/c stacked in column 1; d the
/// upstream-heavy mart on the right.
fn diamond() -> Subgraph {
    let mut nodes = vec![
        model("a", "a"),
        model("b", "b"),
        model("c", "c"),
        model("d", "d"),
    ];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    Subgraph {
        selected: "d".to_string(),
        nodes,
        edges: vec![
            edge("a", "b"),
            edge("a", "c"),
            edge("b", "d"),
            edge("c", "d"),
        ],
    }
}

#[test]
fn golden_diamond_exact_match() {
    // FROZEN visual contract (generated from layout(), then pinned). Each node is
    // a 3-row Unicode box with the materialization tag in the top border (the
    // synthetic `model()` helper leaves materialization unset → tag "Model"). The
    // grid is rectangular; trailing spaces are significant. Properties locked:
    //   (i)   column order a | b,c | d (left=upstream, right=downstream)
    //   (ii)  all edges right-going (──▶ / corners, no backward/horizontal)
    //   (iii) b and c stacked vertically and non-overlapping
    //   (iv)  d uniquely emphasized (its name cell; asserted via emphasis_regions)
    //   (v)   all four edges a->b, a->c, b->d, c->d connected by box-drawing runs
    //   (vi)  the vertical turn "bus" routes one cell LEFT of the arrowhead, so a
    //         space always separates it from a box corner (no `│╭`/`│╰` that read
    //         as broken boxes) — the arrowhead `▶` is the only glyph touching a box
    // Width 30, 7 rows (GUTTER=3 → compact `──▶` connectors). Trailing spaces on
    // rows 4-7 are SIGNIFICANT. Corners are the rounded arcs (`╭╮╰╯`), matching
    // the rounded pane chrome; connector turns reuse the same corner glyphs.
    let expected = concat!(
        "╭Model─╮   ╭Model─╮   ╭Model─╮\n",
        "│ a    │──▶│ b    │──▶│ d    │\n",
        "╰──────╯ │ ╰──────╯ │ ╰──────╯\n",
        "         │          │         \n",
        "         │ ╭Model─╮ │         \n",
        "         ╰▶│ c    │─╯         \n",
        "           ╰──────╯           ",
    );
    let lay = layout(&diamond());
    assert_eq!(
        lay.grid.to_text(),
        expected,
        "golden diamond grid mismatch:\n--- got ---\n{}\n--- want ---\n{}",
        lay.grid.to_text(),
        expected
    );

    // (iv) Emphasis: exactly one region, spelling the selected node "d".
    let regions = lay.grid.emphasis_regions();
    assert_eq!(
        regions.len(),
        1,
        "exactly one emphasis region in the diamond"
    );
    assert_eq!(
        regions[0].2, "d",
        "the emphasized region spells the selected name"
    );
}

#[test]
fn golden_diamond_ascii_exact_match() {
    // The SAME diamond drawn with GlyphMode::Ascii — the fallback for terminals
    // that render East-Asian-Ambiguous characters 2 cells wide (where the
    // Unicode golden above would ghost). Same geometry cell-for-cell; only the
    // glyph repertoire differs (`+ - | >`), and every cell is plain ASCII.
    let expected = concat!(
        "+Model-+   +Model-+   +Model-+\n",
        "| a    |-->| b    |-->| d    |\n",
        "+------+ | +------+ | +------+\n",
        "         |          |         \n",
        "         | +Model-+ |         \n",
        "         +>| c    |-+         \n",
        "           +------+           ",
    );
    let lay = layout_mode(&diamond(), GlyphMode::Ascii);
    assert_eq!(
        lay.grid.to_text(),
        expected,
        "golden ASCII diamond mismatch:\n--- got ---\n{}\n--- want ---\n{}",
        lay.grid.to_text(),
        expected
    );
    assert!(
        lay.grid.to_text().is_ascii(),
        "ASCII mode must emit only ASCII glyphs"
    );

    // Geometry is mode-independent: identical structured maps both modes.
    let uni = layout(&diamond());
    assert_eq!(lay.columns, uni.columns, "columns identical across modes");
    assert_eq!(lay.rects, uni.rects, "rects identical across modes");
    assert_eq!(
        lay.grid.emphasis_regions(),
        uni.grid.emphasis_regions(),
        "emphasis identical across modes"
    );
}

#[test]
fn golden_diamond_grid_is_rectangular() {
    // Every row must be the same width (padded), so the exact-match's trailing
    // spaces are well-defined.
    let lay = layout(&diamond());
    let w = lay.grid.width();
    for y in 0..lay.grid.height() {
        assert_eq!(
            lay.grid.row_string(y).chars().count(),
            w,
            "row {y} not padded to grid width {w}"
        );
    }
}

#[test]
fn golden_downstream_only_root_a() {
    // Downstream-only topology via a SELECTABLE model (the diamond root `a`). a
    // has no upstream, two downstream (b, c) then d. col(a)=0 so a is the
    // leftmost; all edges still right-going; a uniquely emphasized.
    let mut nodes = vec![
        model("a", "a"),
        model("b", "b"),
        model("c", "c"),
        model("d", "d"),
    ];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    let sg = Subgraph {
        selected: "a".to_string(),
        nodes,
        edges: vec![
            edge("a", "b"),
            edge("a", "c"),
            edge("b", "d"),
            edge("c", "d"),
        ],
    };
    let lay = layout(&sg);
    assert_eq!(lay.columns["a"], 0, "downstream-only root is column 0");
    assert_all_edges_right_going(&lay, &sg);
    let regions = lay.grid.emphasis_regions();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].2, "a", "selected root a uniquely emphasized");
}

// ============================================================================
// Real-manifest structural invariants (map-based, NOT text scanning)
// ============================================================================

/// Assert every edge is strictly right-going on the columns map.
fn assert_all_edges_right_going(lay: &Layout, sg: &Subgraph) {
    for e in &sg.edges {
        let pc = lay.columns[&e.parent];
        let cc = lay.columns[&e.child];
        assert!(
            cc > pc,
            "edge {}->{} not right-going: column {pc} -> {cc}",
            e.parent,
            e.child
        );
    }
}

/// The five structural invariants, all checked against `layout()`'s structured
/// maps, parameterized by the selected node id and its expected
/// upstream/downstream counts (frozen via jq).
fn check_structural_invariants(dag: &Dag, selected: &str, expect_up: usize, expect_down: usize) {
    let sg = dag.subgraph(selected);
    // Sanity: the subgraph size matches the frozen lineage counts.
    assert_eq!(
        dag.upstream(selected).len(),
        expect_up,
        "{selected} upstream count"
    );
    assert_eq!(
        dag.downstream(selected).len(),
        expect_down,
        "{selected} downstream count"
    );

    let lay = layout(&sg);

    // 1. Every subgraph node present EXACTLY once: rects has one entry per node,
    //    and there are exactly |nodes| rects (truth source = rects map).
    assert_eq!(
        lay.rects.len(),
        sg.nodes.len(),
        "{selected}: every node must have exactly one rect"
    );
    for node in &sg.nodes {
        assert!(
            lay.rects.contains_key(&node.unique_id),
            "{selected}: node {} missing a rect",
            node.unique_id
        );
        assert!(
            lay.columns.contains_key(&node.unique_id),
            "{selected}: node {} missing a column",
            node.unique_id
        );
    }

    // 2. Selected node present and uniquely emphasized: exactly one emphasis
    //    region whose spelling == the selected node's display name.
    let sel_name = dag
        .get(selected)
        .map(|n| n.name.clone())
        .expect("selected node info");
    let regions = lay.grid.emphasis_regions();
    assert_eq!(
        regions.len(),
        1,
        "{selected}: exactly one emphasis region (got {regions:?})"
    );
    assert_eq!(
        regions[0].2, sel_name,
        "{selected}: emphasis region must spell the selected node name"
    );

    // 3 & 4. Column order / right-going edges: for every edge parent->child,
    //    column(parent) < column(child) AND rect(parent).x < rect(child).x
    //    (upstream left, downstream right). Both the logical column map and the
    //    pixel-x rect map confirm it.
    assert_all_edges_right_going(&lay, &sg);
    for e in &sg.edges {
        let px = lay.rects[&e.parent].x;
        let cx = lay.rects[&e.child].x;
        assert!(
            px < cx,
            "{selected}: edge {}->{} not left-to-right in x: {px} -> {cx}",
            e.parent,
            e.child
        );
    }

    // 5. Label non-overlap: all node rects pairwise non-intersecting.
    let rects: Vec<&dbtl::NodeRect> = lay.rects.values().collect();
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            assert!(
                !rects[i].intersects(rects[j]),
                "{selected}: two node rects overlap: {:?} vs {:?}",
                rects[i],
                rects[j]
            );
        }
    }

    // Determinism cross-check: a second layout is bit-identical.
    let lay2 = layout(&sg);
    assert_eq!(
        lay.grid, lay2.grid,
        "{selected}: layout must be deterministic"
    );
}

#[test]
fn invariants_upstream_only_pos_txn() {
    // pos_txn: 7 upstream / 0 downstream (selected sits at the right end).
    check_structural_invariants(&fixture_dag(), "model.jaffle_finance.pos_txn", 7, 0);
}

#[test]
fn invariants_both_fct_subscription_process() {
    // The big one: 27 upstream / 4 downstream (2 models + the fixture's 2
    // exposures riding behind them).
    check_structural_invariants(
        &fixture_dag(),
        "model.jaffle_finance.fct_subscription_process",
        27,
        4,
    );
}

#[test]
fn invariants_wide_fanout_pos_files_assignment() {
    // Wide downstream fan-out: 5 upstream / 16 downstream.
    check_structural_invariants(
        &fixture_dag(),
        "model.jaffle_finance.pos_files__assignment",
        5,
        16,
    );
}

#[test]
fn invariants_both_small_stg_payment_shoppers() {
    // Small both-direction node (2 upstream / 1 downstream).
    check_structural_invariants(
        &fixture_dag(),
        "model.jaffle_finance.stg_payment__shoppers",
        2,
        1,
    );
}

#[test]
fn invariants_downstream_only_source_subscriptions() {
    // Downstream-only via a SOURCE (0 upstream / 5 downstream). The model list
    // can only SELECT models, so the UI never reaches this source directly; this
    // test exercises that layout() is root-agnostic and does not break for a
    // pure-source root. The selected node sits at column 0 (left).
    let dag = fixture_dag();
    let src = "source.jaffle_finance.dev_lake_jaffle_payment.subscriptions";
    let sg = dag.subgraph(src);
    assert!(dag.upstream(src).is_empty(), "source has no upstream");
    assert_eq!(
        dag.downstream(src).len(),
        7,
        "source has 7 downstream (5 models + 2 exposures)"
    );

    let lay = layout(&sg);
    assert_eq!(
        lay.columns[src], 0,
        "downstream-only source is column 0 (left)"
    );
    assert_all_edges_right_going(&lay, &sg);

    // Present exactly once + uniquely emphasized.
    assert_eq!(lay.rects.len(), sg.nodes.len());
    let sel_name = dag.get(src).unwrap().name.clone();
    let regions = lay.grid.emphasis_regions();
    assert_eq!(regions.len(), 1, "exactly one emphasis region");
    assert_eq!(regions[0].2, sel_name, "emphasis spells the source name");
}

#[test]
fn fct_subgraph_has_expected_shape() {
    // Cross-check the frozen layout dimensions from the contract (jq-verified):
    // 32 nodes (incl. the 2 exposures), 9 columns (0..8 — the dashboard
    // exposure terminates one column right of the rightmost model), selected
    // fct in column 5, all 41 edges right-going with 9 having delta>1.
    let dag = fixture_dag();
    let fct = "model.jaffle_finance.fct_subscription_process";
    let sg = dag.subgraph(fct);
    assert_eq!(sg.nodes.len(), 32, "fct subgraph has 32 nodes");
    assert_eq!(sg.edges.len(), 41, "fct subgraph has 41 edges");

    let lay = layout(&sg);
    let max_col = *lay.columns.values().max().unwrap();
    assert_eq!(max_col, 8, "fct subgraph spans 9 columns (0..=8)");
    assert_eq!(lay.columns[fct], 5, "fct is in column 5");

    let delta_gt1: HashSet<(&str, &str)> = sg
        .edges
        .iter()
        .filter(|e| lay.columns[&e.child] > lay.columns[&e.parent] + 1)
        .map(|e| (e.parent.as_str(), e.child.as_str()))
        .collect();
    assert_eq!(
        delta_gt1.len(),
        9,
        "9 edges span more than one column (delta>1)"
    );
}
