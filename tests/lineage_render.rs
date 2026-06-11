//! At real terminal sizes (80x24, 100x30), selecting the big fan-out model
//! `fct_subscription_process` must render the selected node's full label and its
//! nearest connectors INSIDE the lineage pane. Render through the same path
//! production uses (layout -> anchor -> draw) and scan ONLY the lineage pane's
//! interior Rect: the pane title and bottom status both echo the selected name,
//! so a whole-buffer `contains(name)` is a guaranteed false positive. Judge
//! visibility by emphasis regions, not cell counts: require exactly one maximal
//! run of REVERSED-styled cells in the interior whose spelling equals the name.

use dbtl::layout::{layout, Layout};
use dbtl::model_list::{build_model_list, ModelList, SortMode};
use dbtl::ui::{clip_edges, draw, handle_key, pane_interior, pane_rects, RenderCtx, UiState};
use dbtl::{load_dag, Dag};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::Terminal;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");
const FCT: &str = "model.jaffle_finance.fct_subscription_process";
const FCT_NAME: &str = "fct_subscription_process";

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fixture manifest must load")
}

/// Find the selectable model index of `unique_id` in the assembled list.
fn model_index_of(list: &ModelList, unique_id: &str) -> usize {
    (0..list.len())
        .find(|&i| list.model_at(i).unwrap().unique_id == unique_id)
        .unwrap_or_else(|| panic!("{unique_id} must be a selectable model"))
}

/// Reproduce the event loop's pre-draw setup for a given selection and terminal
/// size: build the layout, size the lineage viewport via the SAME geometry the
/// draw uses, and anchor the lineage scroll on the selection (first-frame anchor).
fn setup(width: u16, height: u16, selected_index: usize) -> (Buffer, Rect, Layout, UiState) {
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    let mut state = UiState::new(list.len());
    // Drive `j` selected_index times to stay on the real key path (keeps us
    // honest that selection is reachable).
    for _ in 0..selected_index {
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
    }
    assert_eq!(
        state.selected(),
        selected_index,
        "selection reached via j keys"
    );

    let node = list.model_at(state.selected()).unwrap();
    let lay = layout(&dag.subgraph(&node.unique_id));

    let area = Rect::new(0, 0, width, height);
    let interior = pane_interior(pane_rects(area, true).lineage);
    state.anchor_lineage(
        lay.selected_rect,
        &lay.grid,
        interior.width as usize,
        interior.height as usize,
    );

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let ctx = RenderCtx::new(&list, &state, Some(&lay));
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    let buffer = terminal.backend().buffer().clone();
    (buffer, interior, lay, state)
}

/// Maximal contiguous horizontal runs of REVERSED-styled cells WITHIN `interior`
/// only, as spelled strings. This is the buffer-side analog of
/// `CharGrid::emphasis_regions`, restricted to the lineage pane so the title /
/// status echoes can't leak in.
fn emphasis_regions_in(buffer: &Buffer, interior: Rect) -> Vec<String> {
    let mut regions = Vec::new();
    for y in interior.y..interior.y + interior.height {
        let mut x = interior.x;
        while x < interior.x + interior.width {
            let reversed = buffer
                .cell((x, y))
                .map(|c| c.modifier.contains(Modifier::REVERSED))
                .unwrap_or(false);
            if reversed {
                let mut s = String::new();
                while x < interior.x + interior.width
                    && buffer
                        .cell((x, y))
                        .map(|c| c.modifier.contains(Modifier::REVERSED))
                        .unwrap_or(false)
                {
                    s.push_str(buffer.cell((x, y)).unwrap().symbol());
                    x += 1;
                }
                regions.push(s);
            } else {
                x += 1;
            }
        }
    }
    regions
}

/// Whether any box-drawing glyph is adjacent to the selected node's label run in
/// the interior. The selected name renders INSIDE a box (Unicode by default), so
/// we look for a box border / top-border tag / attached connector glyph within a
/// small neighbourhood of the emphasized run.
fn box_glyph_near_selection(buffer: &Buffer, interior: Rect) -> bool {
    const BOX_GLYPHS: [&str; 7] = ["│", "─", "╭", "╮", "╰", "╯", "▶"];
    let x_end = interior.x + interior.width;
    let y_end = interior.y + interior.height;
    for y in interior.y..y_end {
        // Locate an emphasized run on this row.
        let mut x = interior.x;
        let mut run_start: Option<u16> = None;
        let mut run_end: Option<u16> = None;
        while x < x_end {
            let reversed = buffer
                .cell((x, y))
                .map(|c| c.modifier.contains(Modifier::REVERSED))
                .unwrap_or(false);
            if reversed {
                run_start.get_or_insert(x);
                run_end = Some(x);
            }
            x += 1;
        }
        if let (Some(s), Some(e)) = (run_start, run_end) {
            // Scan the run's row and the rows just above/below (the box borders
            // and tag), within a couple cells either side of the label.
            let lo_x = s.saturating_sub(2);
            let hi_x = (e + 2).min(x_end - 1);
            let lo_y = y.saturating_sub(1);
            let hi_y = (y + 1).min(y_end - 1);
            for yy in lo_y..=hi_y {
                for xx in lo_x..=hi_x {
                    if let Some(cell) = buffer.cell((xx, yy)) {
                        if BOX_GLYPHS.contains(&cell.symbol()) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn assert_selected_visible_at(width: u16, height: u16) {
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    let idx = model_index_of(&list, FCT);
    let (buffer, interior, _lay, _state) = setup(width, height, idx);

    let regions = emphasis_regions_in(&buffer, interior);
    assert_eq!(
        regions.len(),
        1,
        "{width}x{height}: exactly ONE emphasis region in the lineage pane interior, got {regions:?}"
    );
    assert_eq!(
        regions[0], FCT_NAME,
        "{width}x{height}: the emphasis region must spell the full selected node name"
    );
    assert!(
        box_glyph_near_selection(&buffer, interior),
        "{width}x{height}: the selected node's box/connector must be drawn around its label"
    );
}

#[test]
fn fct_selected_visible_at_80x24() {
    assert_selected_visible_at(80, 24);
}

#[test]
fn fct_selected_visible_at_100x30() {
    assert_selected_visible_at(100, 30);
}

#[test]
fn fct_label_fully_inside_interior_not_clipped() {
    // The FULL label (all 23 chars of fct_subscription_process) must render —
    // not a truncated prefix. The region length equals the name length.
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    let idx = model_index_of(&list, FCT);
    for (w, h) in [(80u16, 24u16), (100, 30)] {
        let (buffer, interior, _lay, _state) = setup(w, h, idx);
        let regions = emphasis_regions_in(&buffer, interior);
        assert_eq!(regions.len(), 1, "{w}x{h}: one region");
        assert_eq!(
            regions[0].chars().count(),
            FCT_NAME.chars().count(),
            "{w}x{h}: full label rendered, not clipped/truncated"
        );
    }
}

#[test]
fn cursor_keys_traverse_columns_and_highlight_follows() {
    // h moves the lineage CURSOR one column upstream per keypress (never a
    // viewport pan): drive the real App path (focus → apply_action) on the fct
    // fan-out, mirror the event loop's follow (`ensure_lineage_visible`), and
    // require (a) the cursor's box to be fully inside the viewport window after
    // each step and (b) the rendered emphasis to spell the CURSOR's name.
    use dbtl::app::{apply_action, App};
    use dbtl::{Action, Focus};

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id(FCT);
    app.ui_state.set_focus(Focus::RightPane);

    let area = Rect::new(0, 0, 100, 30);
    let interior = pane_interior(pane_rects(area, true).lineage);
    let (vw, vh) = (interior.width as usize, interior.height as usize);

    // First frame: anchor on the root (the cursor's home). The layout comes
    // from the SAME memoized producer the event loop draws from, so this test
    // exercises the production pipeline (cache key -> styles -> layout) too.
    let lay0 = app.styled_lineage_layout().expect("non-empty lineage");
    app.ui_state
        .anchor_lineage(lay0.selected_rect, &lay0.grid, vw, vh);
    let root_col = lay0.columns[FCT];
    assert!(root_col > 0, "fct has upstream columns to walk");

    // Walk to the most-upstream column, one column per keypress.
    let mut prev_col = root_col;
    for _ in 0..root_col {
        apply_action(&mut app, Action::MoveLeft);
        let cur = app.lineage_cursor_uid().expect("cursor exists");
        let lay = app.styled_lineage_layout().expect("non-empty lineage");
        assert_eq!(
            lay.columns[&cur],
            prev_col - 1,
            "each h moves exactly one column upstream"
        );
        prev_col = lay.columns[&cur];
        // Event-loop follow: minimal scroll, then the box must be fully shown.
        let rect = lay.selected_rect.expect("cursor rect");
        app.ui_state
            .ensure_lineage_visible(rect, lay.grid.width(), lay.grid.height(), vw, vh);
        let (ox, oy) = (
            app.ui_state.lineage_scroll_x(),
            app.ui_state.lineage_scroll_y(),
        );
        assert!(
            rect.x >= ox && rect.x + rect.width <= ox + vw,
            "cursor box horizontally inside the viewport"
        );
        assert!(
            rect.y >= oy && rect.y + rect.height <= oy + vh,
            "cursor box vertically inside the viewport"
        );
    }

    // At the upstream edge h is a no-op.
    let at_edge = app.lineage_cursor_uid();
    apply_action(&mut app, Action::MoveLeft);
    assert_eq!(
        app.lineage_cursor_uid(),
        at_edge,
        "h at the most-upstream column is a no-op"
    );

    // Render through the real draw path: the ONLY emphasized run inside the
    // lineage interior must spell the cursor's name (the highlight left the
    // root and followed the cursor).
    let cur = app.lineage_cursor_uid().unwrap();
    let cur_name = app.dag.get(&cur).expect("cursor node exists").name.clone();
    assert_ne!(cur, FCT, "the cursor walked off the root");
    let lay = app.styled_lineage_layout().expect("non-empty lineage");
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let ctx = RenderCtx::new(app.active_list(), &app.ui_state, Some(&lay));
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    let buffer = terminal.backend().buffer().clone();
    let regions = emphasis_regions_in(&buffer, interior);
    assert_eq!(regions.len(), 1, "exactly one emphasized run: {regions:?}");
    assert_eq!(
        regions[0], cur_name,
        "the emphasis spells the cursor's name, not the root's"
    );
}

#[test]
fn extreme_terminal_sizes_do_not_panic() {
    // Tiny and odd sizes: layout + anchor + draw must not panic, regardless of
    // whether the selected node ends up visible (it can't fit a 1x1 pane).
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    let idx = model_index_of(&list, FCT);
    for (w, h) in [(1u16, 1u16), (3, 3), (20, 5), (5, 20), (2, 2), (200, 60)] {
        let _ = setup(w, h, idx); // must simply not panic
    }
}

#[test]
fn scroll_markers_match_clipped_edges_on_the_border() {
    // A wide diagram (fct fan-out) at a narrow terminal must scroll, so the pure
    // `clip_edges` reports overflow — and the renderer must stamp a directional
    // marker on each clipped border edge (and ONLY those), at that edge's
    // midpoint. This is what turns a partially-drawn box at the pane edge from
    // "broken render" into "scroll this way".
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    let idx = model_index_of(&list, FCT);

    let (w, h) = (80u16, 24u16);
    let (buffer, interior, lay, state) = setup(w, h, idx);

    // The content window the renderer blits is min(grid, interior) per axis.
    let content_w = lay.grid.width().min(interior.width as usize);
    let content_h = lay.grid.height().min(interior.height as usize);
    let edges = clip_edges(
        lay.grid.width(),
        lay.grid.height(),
        state.lineage_scroll_x(),
        state.lineage_scroll_y(),
        content_w,
        content_h,
    );
    // Sanity: the big fan-out at 80x24 genuinely overflows horizontally.
    assert!(
        edges.right || edges.left,
        "precondition: fct at {w}x{h} should clip horizontally (edges={edges:?})"
    );

    // The bordered lineage pane rect (markers live on its border, not interior).
    // The top `^` is nudged to the title-free right end of the top border; the
    // others keep their edge midpoints (mirrors `stamp_scroll_markers`).
    let rect = pane_rects(Rect::new(0, 0, w, h), true).lineage;
    let mid_x = rect.x + rect.width / 2;
    let mid_y = rect.y + rect.height / 2;
    let right = rect.x + rect.width - 1;
    let bottom = rect.y + rect.height - 1;
    let sym = |x: u16, y: u16| {
        buffer
            .cell((x, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default()
    };

    assert_eq!(
        sym(rect.x, mid_y) == "◀",
        edges.left,
        "left marker iff left-clipped"
    );
    assert_eq!(
        sym(right, mid_y) == "▶",
        edges.right,
        "right marker iff right-clipped"
    );
    assert_eq!(
        sym(right - 1, rect.y) == "▲",
        edges.top,
        "top marker iff top-clipped"
    );
    assert_eq!(
        sym(mid_x, bottom) == "▼",
        edges.bottom,
        "bottom marker iff bottom-clipped"
    );
}

#[test]
fn no_scroll_markers_when_diagram_fits() {
    // A small lineage in a large pane fits entirely: `clip_edges` reports nothing,
    // and the renderer must draw NO scroll markers (no false "broken" affordance).
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);
    let idx = model_index_of(&list, "model.jaffle_finance.pos_txn");

    let (w, h) = (200u16, 60u16);
    let (buffer, interior, lay, state) = setup(w, h, idx);
    let content_w = lay.grid.width().min(interior.width as usize);
    let content_h = lay.grid.height().min(interior.height as usize);
    let edges = clip_edges(
        lay.grid.width(),
        lay.grid.height(),
        state.lineage_scroll_x(),
        state.lineage_scroll_y(),
        content_w,
        content_h,
    );
    assert!(
        !edges.any(),
        "precondition: pos_txn fits a {w}x{h} pane (edges={edges:?})"
    );

    // Markers only ever land on the four border midpoints; check exactly those
    // (a whole-rect scan would false-positive on the connector `▶` arrowheads
    // INSIDE the diagram, which are legitimate content, not chrome).
    let rect = pane_rects(Rect::new(0, 0, w, h), true).lineage;
    let mid_x = rect.x + rect.width / 2;
    let mid_y = rect.y + rect.height / 2;
    let right = rect.x + rect.width - 1;
    let bottom = rect.y + rect.height - 1;
    let markers = ["◀", "▶", "▲", "▼"];
    for (x, y, edge) in [
        (rect.x, mid_y, "left"),
        (right, mid_y, "right"),
        (right - 1, rect.y, "top"),
        (mid_x, bottom, "bottom"),
    ] {
        let s = buffer
            .cell((x, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default();
        assert!(
            !markers.contains(&s.as_str()),
            "no {edge} scroll marker expected when the diagram fits, found {s:?} at ({x},{y})"
        );
    }
}

#[test]
fn selection_change_reanchors_to_new_node() {
    // Switching selection must re-anchor so the NEW node is visible. Render fct,
    // then a small-lineage model (pos_txn), and confirm each renders its own
    // emphasized name in the interior (the follow-update requirement).
    let dag = fixture_dag();
    let list = build_model_list(&dag, SortMode::Layer);

    let fct_idx = model_index_of(&list, FCT);
    let (buf_fct, int_fct, _l1, _s1) = setup(100, 30, fct_idx);
    let r1 = emphasis_regions_in(&buf_fct, int_fct);
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0], FCT_NAME);

    let txn_idx = model_index_of(&list, "model.jaffle_finance.pos_txn");
    let (buf_txn, int_txn, _l2, _s2) = setup(100, 30, txn_idx);
    let r2 = emphasis_regions_in(&buf_txn, int_txn);
    assert_eq!(r2.len(), 1, "pos_txn: one emphasis region");
    assert_eq!(r2[0], "pos_txn", "re-anchored to the newly selected node");
}
