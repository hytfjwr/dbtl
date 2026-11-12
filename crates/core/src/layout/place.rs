use super::*;

/// Longest-path column assignment over the subgraph edges.
///
/// `column(n) = max over parents p (column(p) + 1)`, sources = 0. Computed by
/// relaxation to a fixpoint (the subgraph is a DAG, so it converges in at most
/// `nodes` passes; we cap at `nodes+1` for safety). Returns every node's column
/// (nodes with no in-subgraph parent get 0).
///
/// `pub` for the sibling-collapse stage (`dbtl_tui::collapse`), which
/// groups same-parent siblings by the column this assigns.
pub fn longest_path_columns(sg: &Subgraph) -> HashMap<String, usize> {
    let mut col: HashMap<String, usize> = sg
        .nodes
        .iter()
        .map(|n| (n.unique_id.clone(), 0usize))
        .collect();

    // Relax until no change (bounded by node count for a DAG; guard anyway).
    let max_passes = sg.nodes.len() + 1;
    for _ in 0..max_passes {
        let mut changed = false;
        for edge in &sg.edges {
            let parent_col = *col.get(&edge.parent).unwrap_or(&0);
            let entry = col.entry(edge.child.clone()).or_insert(0);
            if *entry < parent_col + 1 {
                *entry = parent_col + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    col
}

/// Lay out a lineage subgraph with the default (Unicode) glyph set. The frozen
/// entry point most tests use; the event loop calls [`layout_mode`] with the
/// app's detected/forced [`GlyphMode`]. Geometry (columns/rects/emphasis) is
/// IDENTICAL across modes — only the border/connector glyphs differ.
pub fn layout(sg: &Subgraph) -> Layout {
    layout_mode(sg, GlyphMode::Unicode)
}

/// Lay out a lineage subgraph into a [`Layout`] (pure; no ratatui), drawing
/// with the given [`GlyphMode`]'s glyph set at the default (Comfortable)
/// [`Density`]. The frozen pre-density entry point — geometry is byte-stable.
pub fn layout_mode(sg: &Subgraph, mode: GlyphMode) -> Layout {
    layout_density(sg, mode, Density::Comfortable)
}

/// Lay out a lineage subgraph into a [`Layout`] (pure; no ratatui), drawing
/// with the given [`GlyphMode`]'s glyph set at the given [`Density`].
///
/// Empty subgraph -> a 1x1 blank grid and empty maps (callers render an empty
/// pane). Otherwise builds the longest-path columns, stacks each column's nodes
/// deterministically (by `unique_id`), positions columns left-to-right with a
/// fixed gutter, stamps connectors, then writes labels on top.
pub fn layout_density(sg: &Subgraph, mode: GlyphMode, density: Density) -> Layout {
    layout_impl(sg, mode, density, &BTreeMap::new())
}

/// Lay out a lineage subgraph like [`layout_density`], but draw each node's
/// RELATED columns as extra `│{mark} {name}…│` rows inside its box (the
/// column-focus view): `cols` maps `unique_id` → the node's related
/// column rows, in display order. Nodes absent from the map (and every node
/// when no focus is active) keep the classic 3-row shape.
///
/// Comfortable density only: a [`Density::Compact`] call (and an all-empty
/// `cols` map) delegates straight to [`layout_density`] — compact boxes have
/// no room for rows, and an empty map means "no focus", so the classic
/// geometry is byte-identical in both cases.
pub fn layout_columns(
    sg: &Subgraph,
    mode: GlyphMode,
    density: Density,
    cols: &BTreeMap<String, Vec<ColLine>>,
) -> Layout {
    if density == Density::Compact || cols.values().all(Vec::is_empty) {
        return layout_density(sg, mode, density);
    }
    layout_impl(sg, mode, density, cols)
}

/// The shared layout pipeline behind [`layout_density`] and [`layout_columns`].
/// `cols` is the column-focus row map (`unique_id` → related [`ColLine`]s);
/// empty for every classic layout, so the frozen golden paths are untouched.
fn layout_impl(
    sg: &Subgraph,
    mode: GlyphMode,
    density: Density,
    cols: &BTreeMap<String, Vec<ColLine>>,
) -> Layout {
    let g = BoxGlyphs::for_mode(mode);
    let (box_h, box_mid) = box_metrics(density);
    // Column rows are a Comfortable-only surface: Compact boxes are a single
    // row, so the map reads empty there regardless of what the caller passed.
    let empty_cols: BTreeMap<String, Vec<ColLine>> = BTreeMap::new();
    let cols = if density == Density::Compact {
        &empty_cols
    } else {
        cols
    };
    let col_rows = |uid: &str| -> &[ColLine] { cols.get(uid).map(Vec::as_slice).unwrap_or(&[]) };
    let box_w = |node: &crate::NodeInfo| -> usize {
        box_width_of_density(node, density).max(col_width_of(col_rows(&node.unique_id)))
    };
    let mut col_cells: HashSet<(usize, usize)> = HashSet::new();
    if sg.nodes.is_empty() {
        return Layout {
            grid: CharGrid::new(1, 1),
            columns: HashMap::new(),
            rects: HashMap::new(),
            selected_coord: None,
            selected_rect: None,
            edge_cells: HashMap::new(),
            col_cells,
        };
    }

    let columns = longest_path_columns(sg);
    let max_col = *columns.values().max().unwrap_or(&0);

    // Group node ids by column, each group sorted by unique_id (determinism).
    // sg.nodes is already unique_id-sorted, so pushing in order keeps columns
    // sorted without re-sorting.
    let mut col_nodes: Vec<Vec<&crate::NodeInfo>> = vec![Vec::new(); max_col + 1];
    for node in &sg.nodes {
        let c = *columns.get(&node.unique_id).unwrap_or(&0);
        col_nodes[c].push(node);
    }

    // Column width = the widest BOX in the column. Column x = cumulative prior
    // widths + gutters (the gutter carries the connector + arrowhead).
    let col_width: Vec<usize> = col_nodes
        .iter()
        .map(|nodes| nodes.iter().map(|n| box_w(n)).max().unwrap_or(1).max(1))
        .collect();
    let mut col_x: Vec<usize> = Vec::with_capacity(col_width.len());
    let mut x = 0usize;
    for (i, w) in col_width.iter().enumerate() {
        col_x.push(x);
        x += w;
        if i + 1 < col_width.len() {
            x += GUTTER;
        }
    }
    let grid_width = x;

    // Each node is a `box_h`-row box plus its column rows (column-focus only;
    // `col_rows` is empty otherwise, so heights stay the classic `box_h`).
    // Within a column, boxes stack top-down with ROW_GAP between — running y
    // rather than `i * stride` so a taller box pushes its column-mates down.
    // `rects` are the BOX rects (used for non-overlap, column order, anchoring,
    // and mouse hit-testing). Grid height = tallest stacked column's bottom.
    let mut rects: HashMap<String, NodeRect> = HashMap::new();
    let mut grid_height = 1usize;
    for (c, nodes) in col_nodes.iter().enumerate() {
        let mut y = 0usize;
        for node in nodes.iter() {
            let height = box_h + col_rows(&node.unique_id).len();
            rects.insert(
                node.unique_id.clone(),
                NodeRect {
                    x: col_x[c],
                    y,
                    width: box_w(node),
                    height,
                },
            );
            grid_height = grid_height.max(y + height);
            y += height + ROW_GAP;
        }
    }

    let mut grid = CharGrid::new(grid_width.max(1), grid_height);

    // ---- Stamp connectors (segments/corners first, arrowheads last) ----
    // For each edge parent->child, draw a connector from the parent box's right
    // edge to the child box's left edge, attaching at each box's MIDDLE row
    // (`rect.y + 1`). The glyphs come from the same set as the node boxes.
    // Crossings are acceptable. The arrowhead is a separate second pass so each
    // arrowhead wins its own child-entry cell even when siblings share a turn
    // column.
    let mut edge_cells: HashMap<(String, String), Vec<(usize, usize)>> = HashMap::new();
    for edge in &sg.edges {
        let (Some(p), Some(c)) = (rects.get(&edge.parent), rects.get(&edge.child)) else {
            continue;
        };
        let (p, c) = (*p, *c);
        // Cells THIS edge draws (claimed cells only — a cell already drawn by an
        // earlier sorted-order edge belongs to that edge), recorded for
        // `apply_edge_styles`. The arrowhead joins in pass 2 below.
        let mut cells: Vec<(usize, usize)> = Vec::new();
        let (py, cy) = (p.y + box_mid, c.y + box_mid); // connector-attach rows
        let from_x = p.right(); // one past the parent box's right border
        if c.x == 0 {
            continue;
        }
        let arrow_x = c.x - 1; // the arrowhead cell, immediately left of the child box
        if from_x > arrow_x {
            continue; // degenerate; shouldn't happen for right-going edges
        }
        if py == cy {
            // Same-row edge: a straight horizontal run into the arrowhead.
            for hx in from_x..arrow_x {
                if grid.char_at(hx, py) == ' ' {
                    grid.put(hx, py, g.h);
                    cells.push((hx, py));
                }
            }
            edge_cells.insert((edge.parent.clone(), edge.child.clone()), cells);
            continue;
        }
        // Different rows: route the VERTICAL turn one cell further left than the
        // arrowhead, in a dedicated gutter channel, so the vertical line never
        // abuts a box's corner. With it pressed against the box (`c.x - 1`),
        // stacked boxes read as broken (`|+` runs); the spare cell separates the
        // bus from every box in the column. GUTTER >= 3 guarantees the lead-in
        // cell exists for adjacent columns (`from_x <= c.x - 3`); the fallback to
        // the arrow cell only fires in the degenerate no-room case.
        let channel_x = if arrow_x > from_x {
            arrow_x - 1
        } else {
            arrow_x
        };
        // Horizontal leaving the parent at its attach row, up to the channel.
        for hx in from_x..channel_x {
            if grid.char_at(hx, py) == ' ' {
                grid.put(hx, py, g.h);
                cells.push((hx, py));
            }
        }
        // Vertical channel from the parent row to the child row, with a corner
        // glyph at each end (parent-side turns down/up; child-side turns back
        // toward the box / arrowhead — the box corner glyphs double as turns).
        // A cell already claimed by another edge's run is left as-is (crossings
        // are acceptable; the arrowhead still marks the entry).
        let (lo, hi) = if py < cy { (py, cy) } else { (cy, py) };
        for vy in lo..=hi {
            if grid.char_at(channel_x, vy) != ' ' {
                continue;
            }
            let ch = if vy == py {
                if cy > py {
                    g.tr
                } else {
                    g.br
                } // parent-side corner: left + down/up
            } else if vy == cy {
                if cy > py {
                    g.bl
                } else {
                    g.tl
                } // child-side corner: turn toward box
            } else {
                g.v
            };
            grid.put(channel_x, vy, ch);
            cells.push((channel_x, vy));
        }
        // Short horizontal from the channel's child-side corner into the arrowhead
        // (empty when the channel sits directly left of the arrowhead, GUTTER=3).
        for hx in (channel_x + 1)..arrow_x {
            if grid.char_at(hx, cy) == ' ' {
                grid.put(hx, cy, g.h);
                cells.push((hx, cy));
            }
        }
        // The child-side corner + its lead-in row are SHARED by every
        // different-row edge into this child (they all route through the same
        // channel column at the child's attach row) — claim them for THIS edge
        // even when another edge drew the glyph first, like the arrowhead in
        // pass 2. Without this, a path edge sorted AFTER an off-path sibling
        // would miss the shared corner and the path band would show a dimmed
        // notch there. (A crossing edge's glyph may also occupy the cell; the
        // band on it is the lesser artifact vs a gap in the path.)
        for sx in channel_x..arrow_x {
            if !cells.contains(&(sx, cy)) {
                cells.push((sx, cy));
            }
        }
        edge_cells.insert((edge.parent.clone(), edge.child.clone()), cells);
    }
    // Pass 2: arrowheads last, so each arrowhead wins its child-entry cell. The
    // arrowhead cell is attributed to ITS edge even when another edge's run was
    // first through it (the arrow glyph wins the cell, so the attr should too).
    // Deliberately independent of pass 1's continues: a degenerate edge that
    // drew no run still gets its arrowhead drawn AND recorded (glyph and attr
    // stay in lockstep); only a column-0 child (`c.x == 0` — impossible for a
    // right-going edge) yields no entry at all, which every consumer of
    // `edge_cells` tolerates by lookup (`get`), never by assumption.
    for edge in &sg.edges {
        let Some(c) = rects.get(&edge.child) else {
            continue;
        };
        if c.x > 0 {
            grid.put(c.x - 1, c.y + box_mid, g.arrow);
            edge_cells
                .entry((edge.parent.clone(), edge.child.clone()))
                .or_default()
                .push((c.x - 1, c.y + box_mid));
        }
    }

    // ---- Draw the node boxes on top (borders/tag/label win their cells) ----
    let selected_emph = sg.selected.clone();
    for node in &sg.nodes {
        let rect = rects[&node.unique_id];
        let emph = node.unique_id == selected_emph;
        match density {
            Density::Comfortable => {
                let tests = tests_label_of(node);
                draw_box(
                    &mut grid,
                    g,
                    mode,
                    rect,
                    &BoxBody {
                        label: label_of(node),
                        tag: tag_of(node),
                        tests: tests.as_deref(),
                        cols: col_rows(&node.unique_id),
                    },
                    emph,
                    &mut col_cells,
                );
            }
            Density::Compact => draw_compact_box(&mut grid, g, rect, label_of(node), emph),
        }
    }

    let selected_rect = rects.get(&sg.selected).copied();
    let selected_coord = selected_rect.map(|r| (r.x, r.y));

    Layout {
        grid,
        columns,
        rects,
        selected_coord,
        selected_rect,
        edge_cells,
        col_cells,
    }
}
