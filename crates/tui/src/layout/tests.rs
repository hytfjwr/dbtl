use super::*;
use crate::{ColumnEdgeKind, Edge, NodeInfo, Subgraph};

/// Build a NodeInfo with a model resource type and a name == unique_id tail.
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

/// Asymmetric three-node graph `a->b, a->c, b->c`, selected = c. The
/// LONGEST-PATH discriminator: column(c) must be 2 (a->b->c), not 1 (which
/// BFS shortest distance a->c would give, colliding c with b in column 1).
fn asymmetric_three() -> Subgraph {
    let mut nodes = vec![model("a", "a"), model("b", "b"), model("c", "c")];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    Subgraph {
        selected: "c".to_string(),
        nodes,
        edges: vec![edge("a", "b"), edge("a", "c"), edge("b", "c")],
    }
}

#[test]
fn longest_path_puts_c_in_column_2_not_1() {
    // The decisive longest-path-vs-BFS test.
    let lay = layout(&asymmetric_three());
    assert_eq!(lay.columns["a"], 0, "source a is column 0");
    assert_eq!(lay.columns["b"], 1, "b one hop from a");
    assert_eq!(
        lay.columns["c"], 2,
        "longest path a->b->c puts c in column 2 (BFS shortest would give 1)"
    );
}

#[test]
fn all_edges_are_strictly_right_going() {
    // Every edge has column(child) > column(parent) — no horizontal,
    // no backward edges. Assert on the columns map (truth source).
    let sg = asymmetric_three();
    let lay = layout(&sg);
    for e in &sg.edges {
        let pc = lay.columns[&e.parent];
        let cc = lay.columns[&e.child];
        assert!(
            cc > pc,
            "edge {}->{} not right-going: col {pc} -> {cc}",
            e.parent,
            e.child
        );
    }
}

#[test]
fn labels_do_not_overlap() {
    // Node rects are pairwise non-intersecting (truth source: rects).
    let lay = layout(&asymmetric_three());
    let rects: Vec<(&String, &NodeRect)> = lay.rects.iter().collect();
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            assert!(
                !rects[i].1.intersects(rects[j].1),
                "rects of {} and {} overlap: {:?} vs {:?}",
                rects[i].0,
                rects[j].0,
                rects[i].1,
                rects[j].1
            );
        }
    }
}

#[test]
fn layout_is_deterministic_bit_identical() {
    // Determinism: two layouts of the same subgraph are bit-identical
    // (the grid, including emphasis). Catches HashSet order leakage.
    let sg = asymmetric_three();
    let a = layout(&sg);
    let b = layout(&sg);
    assert_eq!(a.grid, b.grid, "grids must be bit-identical");
    assert_eq!(a.columns, b.columns);
    assert_eq!(a.rects, b.rects);
}

#[test]
fn selected_node_is_uniquely_emphasized() {
    // Exactly one emphasis region, spelling the selected node's name.
    let lay = layout(&asymmetric_three());
    let regions = lay.grid.emphasis_regions();
    assert_eq!(regions.len(), 1, "exactly one emphasis region");
    assert_eq!(
        regions[0].2, "c",
        "the region spells the selected node name"
    );
}

#[test]
fn empty_subgraph_is_safe() {
    let sg = Subgraph {
        selected: "x".to_string(),
        nodes: vec![],
        edges: vec![],
    };
    let lay = layout(&sg);
    assert_eq!(lay.grid.width(), 1);
    assert!(lay.selected_coord.is_none());
    assert!(lay.grid.emphasis_regions().is_empty());
}

// ---- blit / anchor / clamp (2-axis scroll) ----

#[test]
fn clamp_offset_bounds_and_saturates() {
    // Grid bigger than view: offset clamps to grid-view.
    assert_eq!(clamp_offset(100, 30, 80), 70, "clamp to grid-view max");
    assert_eq!(clamp_offset(100, 30, 50), 50, "in-range desired unchanged");
    // Grid smaller than view: only valid offset is 0.
    assert_eq!(clamp_offset(10, 30, 5), 0, "grid fits => offset 0");
    // Equal: offset 0.
    assert_eq!(clamp_offset(30, 30, 9), 0);
}

/// A subgraph wide and tall enough to need scrolling in both axes.
fn wide_chain() -> Subgraph {
    // a->b->c->d->e->f (6 columns) plus side nodes stacked in column 1.
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let mut nodes: Vec<NodeInfo> = names.iter().map(|n| model(n, n)).collect();
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    let edges = vec![
        edge("a", "b"),
        edge("b", "c"),
        edge("c", "d"),
        edge("d", "e"),
        edge("e", "f"),
        // g, h are extra children of a so column 1 stacks (vertical extent).
        edge("a", "g"),
        edge("a", "h"),
    ];
    Subgraph {
        selected: "d".to_string(),
        nodes,
        edges,
    }
}

#[test]
fn blit_returns_exact_viewport_size() {
    let lay = layout(&wide_chain());
    let v = blit(&lay.grid, 0, 0, 10, 4);
    assert_eq!(v.width(), 10);
    assert_eq!(v.height(), 4);
}

#[test]
fn anchor_keeps_selected_node_visible_after_clamp() {
    // Centering then clamping must keep the selected box's top-left INSIDE
    // the viewport; and when the viewport is at least as large as the box,
    // the emphasized label is drawn into the cut (not merely arithmetically
    // "in range").
    let lay = wide_chain_layout();
    let rect = lay.selected_rect.expect("selected rect");
    let (bw, bh) = (rect.width, rect.height);
    let (sx, sy) = (rect.x, rect.y);
    for &(vw, vh) in &[
        (1usize, 1usize),
        (2, 2),
        (bw, bh),
        (bw + 4, bh + 2),
        (80, 20),
    ] {
        let (ox, oy) = anchor_offset(lay.selected_rect, &lay.grid, vw, vh);
        assert!(
            sx >= ox && sx < ox + vw,
            "box x {sx} not in [{ox},{}) (vw={vw})",
            ox + vw
        );
        assert!(
            sy >= oy && sy < oy + vh,
            "box y {sy} not in [{oy},{}) (vh={vh})",
            oy + vh
        );
        let view = blit(&lay.grid, ox, oy, vw, vh); // never panics
        if vw >= bw && vh >= bh {
            assert!(
                !view.emphasis_regions().is_empty(),
                "viewport {vw}x{vh} (>= box {bw}x{bh}) must contain the emphasized label"
            );
        }
    }
}

fn wide_chain_layout() -> Layout {
    layout(&wide_chain())
}

#[test]
fn anchor_keeps_full_long_box_visible() {
    // A LONG label makes a wide box; when the viewport is at least the box
    // width, the WHOLE box [sx, sx+w) must fit (midpoint-centering, not
    // start-centering). Narrower viewports must at least show the box start.
    let long = "a_very_long_selected_model_name"; // 31 chars
    let mut nodes = vec![model("up", "up"), model("zz", long)];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    let sg = Subgraph {
        selected: "zz".to_string(),
        nodes,
        edges: vec![edge("up", "zz")],
    };
    let lay = layout(&sg);
    let rect = lay.selected_rect.expect("selected rect");
    let (sx, w) = (rect.x, rect.width);
    assert!(
        w >= long.chars().count() + 2,
        "the box wraps the label + borders"
    );
    for view_w in [w, w + 2, w + 10] {
        let (ox, _oy) = anchor_offset(lay.selected_rect, &lay.grid, view_w, 3);
        assert!(
            sx >= ox && sx + w <= ox + view_w,
            "full box [{sx},{}) must fit viewport [{ox},{}) (view_w={view_w})",
            sx + w,
            ox + view_w
        );
    }
    for view_w in [(w / 2) + 1, w - 1] {
        let (ox, _oy) = anchor_offset(lay.selected_rect, &lay.grid, view_w, 3);
        assert!(
            sx >= ox && sx < ox + view_w,
            "box start {sx} must be visible in [{ox},{}) (view_w={view_w})",
            ox + view_w
        );
    }
}

#[test]
fn anchor_clamps_to_top_left_when_grid_fits() {
    // Viewport >= grid: offset is (0,0) and the whole grid shows.
    let lay = layout(&asymmetric_three());
    let (ox, oy) = anchor_offset(
        lay.selected_rect,
        &lay.grid,
        lay.grid.width() + 10,
        lay.grid.height() + 10,
    );
    assert_eq!((ox, oy), (0, 0), "grid fits => no scroll");
}

#[test]
fn anchor_empty_is_origin() {
    let grid = CharGrid::new(1, 1);
    assert_eq!(anchor_offset(None, &grid, 10, 10), (0, 0));
}

#[test]
fn blit_preserves_emphasis_of_the_cut() {
    // Cut exactly the selected node's NAME cell (inside its box: one border +
    // one padding space in from the box's top-left) and confirm the emphasis
    // survives the blit.
    let lay = layout(&asymmetric_three());
    let rect = lay.selected_rect.unwrap();
    let (nx, ny) = (rect.x + 2, rect.y + 1);
    let view = blit(&lay.grid, nx, ny, 1, 1);
    assert!(
        view.emphasis_at(0, 0),
        "the selected name cell stays emphasized"
    );
    assert_eq!(view.char_at(0, 0), 'c', "and it is the name's first char");
}

// ---- CellAttr: apply_node_styles + blit propagation ----

#[test]
fn apply_node_styles_is_golden_safe_and_deterministic() {
    // Styling changes ONLY the attr array — never the chars or emphasis — so
    // to_text() and emphasis_regions() are byte-identical to the unstyled
    // grid (the golden diamond contract). And it is order-independent.
    let lay0 = layout(&asymmetric_three());
    let text_before = lay0.grid.to_text();
    let emph_before = lay0.grid.emphasis_regions();

    let styles = BTreeMap::from([
        (
            "a".to_string(),
            CellAttr {
                class: MaterializationClass::Source,
                ..Default::default()
            },
        ),
        (
            "b".to_string(),
            CellAttr {
                class: MaterializationClass::View,
                ..Default::default()
            },
        ),
        (
            "c".to_string(),
            CellAttr {
                class: MaterializationClass::Table,
                ..Default::default()
            },
        ),
    ]);

    let mut lay = layout(&asymmetric_three());
    lay.apply_node_styles(&styles);
    assert_eq!(
        lay.grid.to_text(),
        text_before,
        "styling must not change text"
    );
    assert_eq!(
        lay.grid.emphasis_regions(),
        emph_before,
        "styling must not change emphasis"
    );

    // Applying the same styles to a fresh layout yields a bit-identical grid.
    let mut lay2 = layout(&asymmetric_three());
    lay2.apply_node_styles(&styles);
    assert_eq!(lay.grid, lay2.grid, "apply_node_styles is deterministic");

    // The attr actually landed on the node's label cell (c at its rect).
    let rc = lay.rects["c"];
    assert_eq!(
        lay.grid.attr_at(rc.x, rc.y).class,
        MaterializationClass::Table
    );
    let rb = lay.rects["b"];
    assert_eq!(
        lay.grid.attr_at(rb.x, rb.y).class,
        MaterializationClass::View,
        "b's class stamped"
    );
}

#[test]
fn blit_preserves_attr_of_the_cut() {
    // The attr array survives a blit at a non-zero offset (so the renderer
    // sees materialization classes in the viewport window). The lens tint and
    // the two overlay bools (dimmed / on_path) ride alongside `class` and
    // survive too.
    let mut lay = layout(&asymmetric_three());
    lay.apply_node_styles(&BTreeMap::from([(
        "c".to_string(),
        CellAttr {
            class: MaterializationClass::Incremental,
            lens: LensTint::Warn,
            dimmed: true,
            on_path: true,
        },
    )]));
    let rc = lay.rects["c"];
    let view = blit(&lay.grid, rc.x, rc.y, 1, 1);
    let a = view.attr_at(0, 0);
    assert_eq!(a.class, MaterializationClass::Incremental);
    assert_eq!(a.lens, LensTint::Warn, "the lens tint survives the blit");
    assert!(a.dimmed, "the dimmed overlay bool survives the blit");
    assert!(a.on_path, "the on_path overlay bool survives the blit");
    // A cell with no styling stays default after blit (all four fields).
    let plain = blit(&lay.grid, 0, 1, 1, 1); // a connector/blank row
    assert_eq!(plain.attr_at(0, 0), CellAttr::default());
}

#[test]
fn tested_node_shows_tests_label_in_bottom_border() {
    // A node with tests renders `tests:N` right-aligned in its bottom
    // border (with one border cell after it), in BOTH glyph modes; an
    // untested node's bottom border stays plain. The label never widens
    // the emphasis region (it is border, not label).
    let mut nodes = vec![model("a", "a"), model("b", "b")];
    nodes[1].test_count = 2;
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    let sg = Subgraph {
        selected: "a".to_string(),
        nodes,
        edges: vec![edge("a", "b")],
    };
    for mode in [GlyphMode::Unicode, GlyphMode::Ascii] {
        let lay = layout_mode(&sg, mode);
        let rb = lay.rects["b"];
        let bottom: String = (rb.x..rb.x + rb.width)
            .map(|x| lay.grid.char_at(x, rb.y + 2))
            .collect();
        assert!(
            bottom.contains("tests:2"),
            "{mode:?}: bottom border carries the tests label, got {bottom:?}"
        );
        assert!(
            rb.width >= "tests:2".len() + 3,
            "{mode:?}: box widened to fit the label"
        );
        let ra = lay.rects["a"];
        let a_bottom: String = (ra.x..ra.x + ra.width)
            .map(|x| lay.grid.char_at(x, ra.y + 2))
            .collect();
        assert!(
            !a_bottom.contains("tests"),
            "{mode:?}: untested node has a plain bottom border"
        );
        // Emphasis still spells exactly the selected name.
        let regions = lay.grid.emphasis_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].2, "a");
    }
}

#[test]
fn edge_cells_are_recorded_and_edge_styles_are_golden_safe() {
    // Every drawn edge records a non-empty connector run ending in its
    // arrowhead cell; stamping edge attrs changes ONLY the attr array
    // (never text/emphasis — the golden contract, like node styles); and
    // the recording is deterministic across runs.
    let sg = asymmetric_three();
    let lay0 = layout(&sg);
    let text_before = lay0.grid.to_text();
    for e in &sg.edges {
        let key = (e.parent.clone(), e.child.clone());
        let cells = &lay0.edge_cells[&key];
        assert!(!cells.is_empty(), "edge {key:?} records its run");
        let c = lay0.rects[&e.child];
        assert!(
            cells.contains(&(c.x - 1, c.y + 1)),
            "edge {key:?} records its arrowhead cell"
        );
    }
    assert_eq!(
        layout(&sg).edge_cells,
        lay0.edge_cells,
        "edge_cells deterministic"
    );

    let mut lay = layout(&sg);
    let key = ("a".to_string(), "b".to_string());
    lay.apply_edge_styles(&BTreeMap::from([(
        key.clone(),
        CellAttr {
            on_path: true,
            ..Default::default()
        },
    )]));
    assert_eq!(
        lay.grid.to_text(),
        text_before,
        "edge styling never changes text"
    );
    for &(x, y) in &lay.edge_cells[&key] {
        assert!(lay.grid.attr_at(x, y).on_path, "attr landed on ({x},{y})");
    }
    // An unstyled edge's cells stay default.
    let other = ("a".to_string(), "c".to_string());
    if let Some(&(x, y)) = lay.edge_cells[&other]
        .iter()
        .find(|c| !lay.edge_cells[&key].contains(c))
    {
        assert_eq!(lay.grid.attr_at(x, y), CellAttr::default());
    }
}

#[test]
fn shared_channel_corner_is_claimed_by_every_edge_into_the_child() {
    // Two parents on rows DIFFERENT from their shared child both route
    // through the same channel column at the child's attach row. The
    // child-side corner cell must appear in BOTH edges' cell lists (like
    // the shared arrowhead), so the two-phase apply_edge_styles can give
    // the path attr the final say regardless of edge sort order.
    // Column 0 stacks a (y0), b (y4), z (y8); child c sits at y0 in column
    // 1 — a->c is same-row, b->c and z->c both need the channel.
    let mut nodes = vec![
        model("a", "a"),
        model("b", "b"),
        model("c", "c"),
        model("z", "z"),
    ];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    let sg = Subgraph {
        selected: "c".to_string(),
        nodes,
        edges: vec![edge("a", "c"), edge("b", "c"), edge("z", "c")],
    };
    let lay = layout(&sg);
    let rc = lay.rects["c"];
    let corner = (rc.x - 2, rc.y + 1); // channel column, child attach row
    for parent in ["b", "z"] {
        let key = (parent.to_string(), "c".to_string());
        assert!(
            lay.edge_cells[&key].contains(&corner),
            "{parent}->c claims the shared child-side corner {corner:?}"
        );
    }
    // And the path attr wins the shared corner even when the path edge
    // sorts AFTER the off-path edge (z->c is last in sort order).
    let mut lay = layout(&sg);
    lay.apply_edge_styles(&BTreeMap::from([
        (
            ("b".to_string(), "c".to_string()),
            CellAttr {
                dimmed: true,
                ..Default::default()
            },
        ),
        (
            ("z".to_string(), "c".to_string()),
            CellAttr {
                on_path: true,
                ..Default::default()
            },
        ),
    ]));
    let attr = lay.grid.attr_at(corner.0, corner.1);
    assert!(attr.on_path, "the path attr wins the shared corner");
    assert!(!attr.dimmed, "the shared corner is never left dimmed");
}

#[test]
fn compact_density_collapses_boxes_to_one_row() {
    // Compact: every rect is 1 row tall and `name+2` wide; the emphasis
    // still spells exactly the selected name; the connector attaches at the
    // node's own row (the arrowhead sits left of the child at its row);
    // geometry is identical across glyph modes; and the default-density
    // entry points are untouched (layout_mode == layout_density Comfortable).
    let sg = asymmetric_three();
    for mode in [GlyphMode::Unicode, GlyphMode::Ascii] {
        let lay = layout_density(&sg, mode, Density::Compact);
        for (uid, r) in &lay.rects {
            assert_eq!(r.height, 1, "{uid} is one row in compact");
            let name_len = sg
                .nodes
                .iter()
                .find(|n| &n.unique_id == uid)
                .unwrap()
                .name
                .chars()
                .count();
            assert_eq!(r.width, name_len + 2, "{uid} is |name| wide");
        }
        let regions = lay.grid.emphasis_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].2, "c", "emphasis spells the selected name");
        let rc = lay.rects["c"];
        let g = BoxGlyphs::for_mode(mode);
        assert_eq!(lay.grid.char_at(rc.x - 1, rc.y), g.arrow, "arrow at row");
        assert_eq!(lay.grid.char_at(rc.x, rc.y), g.v, "side border glyph");
    }
    assert_eq!(
        layout_density(&sg, GlyphMode::Unicode, Density::Compact).rects,
        layout_density(&sg, GlyphMode::Ascii, Density::Compact).rects,
        "compact geometry identical across glyph modes"
    );
    assert_eq!(
        layout_mode(&sg, GlyphMode::Unicode).grid,
        layout_density(&sg, GlyphMode::Unicode, Density::Comfortable).grid,
        "layout_mode IS the Comfortable density"
    );
}

#[test]
fn unstyled_layout_has_default_attrs() {
    // layout() alone never stamps attrs — every cell is Plain.
    let lay = layout(&asymmetric_three());
    for y in 0..lay.grid.height() {
        for x in 0..lay.grid.width() {
            assert_eq!(lay.grid.attr_at(x, y), CellAttr::default());
        }
    }
}

// ---- column rows (Issue 09's layout_columns) ----

/// Column 0 stacks a/b/z (all -> c in column 1): the fixture for
/// running-y stacking — a taller box must push its column-mates down.
fn stacked_parents() -> Subgraph {
    let mut nodes = vec![
        model("a", "a"),
        model("b", "b"),
        model("c", "c"),
        model("z", "z"),
    ];
    nodes.sort_by(|x, y| x.unique_id.cmp(&y.unique_id));
    Subgraph {
        selected: "c".to_string(),
        nodes,
        edges: vec![edge("a", "c"), edge("b", "c"), edge("z", "c")],
    }
}

#[test]
fn column_rows_grow_height_width_and_record_col_cells() {
    // A box gains one row per related column, the box widens to the
    // longest `{mark} {name}` row, the row interiors are recorded in
    // `col_cells`, and the traced column reads `>`.
    let sg = asymmetric_three();
    let cols = BTreeMap::from([(
        "c".to_string(),
        vec![
            ColLine {
                name: "order_id".into(),
                kind: ColumnEdgeKind::Rename,
                faint: false,
                target: true,
            },
            ColLine {
                name: "a_much_longer_column_name".into(),
                kind: ColumnEdgeKind::Transform,
                faint: true,
                target: false,
            },
        ],
    )]);
    let lay = layout_columns(&sg, GlyphMode::Unicode, Density::Comfortable, &cols);
    let rc = lay.rects["c"];
    assert_eq!(rc.height, 5, "3-row box + 2 column rows");
    assert_eq!(
        rc.width,
        "a_much_longer_column_name".chars().count() + 4,
        "the widest column row sets the box width"
    );
    // The classic boxes are untouched.
    assert_eq!(lay.rects["a"].height, 3);
    assert_eq!(lay.rects["b"].height, 3);
    // The column rows draw `{mark} {name}` inside the box.
    let text = lay.grid.to_text();
    assert!(text.contains("> order_id"), "traced row: {text}");
    assert!(
        text.contains("* a_much_longer_column_name"),
        "transform row: {text}"
    );
    // col_cells covers exactly the row interiors (between the side
    // borders), for each of the two rows.
    for dy in [2usize, 3] {
        for dx in 1..rc.width - 1 {
            assert!(
                lay.col_cells.contains(&(rc.x + dx, rc.y + dy)),
                "interior cell ({},{}) recorded",
                rc.x + dx,
                rc.y + dy
            );
        }
        assert!(
            !lay.col_cells.contains(&(rc.x, rc.y + dy)),
            "the side border is NOT a column cell"
        );
    }
    // Row attrs were stamped at draw time: `>` row on_path, `*` row dimmed.
    assert!(lay.grid.attr_at(rc.x + 1, rc.y + 2).on_path);
    assert!(lay.grid.attr_at(rc.x + 1, rc.y + 3).dimmed);
}

#[test]
fn column_rows_stack_with_running_y() {
    // `a` gets 2 extra rows in column 0's stack: b and z must move down by
    // that much (running y, not a fixed stride).
    let sg = stacked_parents();
    let classic = layout(&sg);
    let cols = BTreeMap::from([(
        "a".to_string(),
        vec![
            ColLine {
                name: "x".into(),
                kind: ColumnEdgeKind::Passthrough,
                faint: false,
                target: false,
            },
            ColLine {
                name: "y".into(),
                kind: ColumnEdgeKind::Passthrough,
                faint: false,
                target: false,
            },
        ],
    )]);
    let lay = layout_columns(&sg, GlyphMode::Unicode, Density::Comfortable, &cols);
    let dy = 2usize;
    assert_eq!(lay.rects["a"].height, 5);
    for uid in ["b", "z"] {
        assert_eq!(
            lay.rects[uid].y,
            classic.rects[uid].y + dy,
            "{uid} pushed down by a's column rows"
        );
    }
    // The child in the NEXT column is unaffected.
    assert_eq!(lay.rects["c"].y, classic.rects["c"].y);
    // Grid grew to the tallest stacked column's bottom.
    assert_eq!(lay.grid.height(), classic.grid.height() + dy);
}

#[test]
fn layout_columns_degenerates_to_the_classic_layout() {
    // No rows at all (or Compact, where rows can't draw) => byte-identical
    // to the classic entry points.
    let sg = asymmetric_three();
    let with_empty = BTreeMap::from([("a".to_string(), Vec::new())]);
    for cols in [&BTreeMap::new(), &with_empty] {
        let lay = layout_columns(&sg, GlyphMode::Unicode, Density::Comfortable, cols);
        let classic = layout(&sg);
        assert_eq!(lay.grid, classic.grid, "empty map = classic grid");
        assert_eq!(lay.rects, classic.rects);
        assert!(lay.col_cells.is_empty());
    }
    let cols = BTreeMap::from([(
        "c".to_string(),
        vec![ColLine {
            name: "x".into(),
            kind: ColumnEdgeKind::Passthrough,
            faint: false,
            target: false,
        }],
    )]);
    let compact = layout_columns(&sg, GlyphMode::Unicode, Density::Compact, &cols);
    let classic_compact = layout_density(&sg, GlyphMode::Unicode, Density::Compact);
    assert_eq!(
        compact.grid, classic_compact.grid,
        "compact ignores column rows"
    );
    assert_eq!(compact.rects, classic_compact.rects);
    assert!(compact.col_cells.is_empty());
}

#[test]
fn column_rows_have_identical_geometry_in_both_glyph_modes() {
    // Unicode and ASCII differ ONLY in the glyphs stamped — rects, columns,
    // and the col_cells record are the same (the `·` vs `-` mark resolves
    // per draw from ColLine::kind + the draw's glyph mode).
    let sg = asymmetric_three();
    let cols = BTreeMap::from([(
        "c".to_string(),
        vec![ColLine {
            name: "order_id".into(),
            kind: ColumnEdgeKind::Passthrough,
            faint: false,
            target: true,
        }],
    )]);
    let uni = layout_columns(&sg, GlyphMode::Unicode, Density::Comfortable, &cols);
    let asc = layout_columns(&sg, GlyphMode::Ascii, Density::Comfortable, &cols);
    assert_eq!(uni.rects, asc.rects);
    assert_eq!(uni.columns, asc.columns);
    assert_eq!(uni.col_cells, asc.col_cells);
    assert_eq!(uni.grid.width(), asc.grid.width());
    assert_eq!(uni.grid.height(), asc.grid.height());
}

#[test]
fn apply_node_styles_preserves_column_row_attrs() {
    // A node's box-level attr (class / lens / focus dim) must never flatten
    // a column row's own `faint`/`target` attr — apply_node_styles skips
    // `col_cells` while still styling the rest of the box.
    let sg = asymmetric_three();
    let cols = BTreeMap::from([(
        "c".to_string(),
        vec![
            ColLine {
                name: "target_col".into(),
                kind: ColumnEdgeKind::Rename,
                faint: false,
                target: true,
            },
            ColLine {
                name: "inferred_col".into(),
                kind: ColumnEdgeKind::Passthrough,
                faint: true,
                target: false,
            },
        ],
    )]);
    let mut lay = layout_columns(&sg, GlyphMode::Unicode, Density::Comfortable, &cols);
    let node_attr = CellAttr {
        class: MaterializationClass::Ephemeral,
        lens: LensTint::HeatHigh,
        dimmed: false,
        on_path: false,
    };
    lay.apply_node_styles(&BTreeMap::from([("c".to_string(), node_attr)]));
    let rc = lay.rects["c"];
    // The box's own cells take the node attr (label row, borders).
    assert_eq!(lay.grid.attr_at(rc.x, rc.y), node_attr, "top-left border");
    assert_eq!(
        lay.grid.attr_at(rc.x + 1, rc.y + 1),
        node_attr,
        "label-row interior takes the node attr"
    );
    // Column-row interiors keep their per-row attr.
    let target = lay.grid.attr_at(rc.x + 1, rc.y + 2);
    assert!(target.on_path, "the target row keeps its band");
    assert_eq!(target.lens, LensTint::None, "the node lens didn't flatten");
    let inferred = lay.grid.attr_at(rc.x + 1, rc.y + 3);
    assert!(inferred.dimmed, "the inferred row stays dim");
    assert_eq!(inferred.lens, LensTint::None);
}
