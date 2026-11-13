use super::*;

/// Horizontal cells between adjacent column boxes (the connector gutter). Kept
/// compact — just enough for a short horizontal run plus the `>` arrowhead — so a
/// multi-column diagram is narrower and more often fits the pane without
/// scrolling (`-->` instead of a wide `---->`). Fixed so the golden is stable.
pub(super) const GUTTER: usize = 3;

/// Height of a node box: top border + label row + bottom border.
pub(super) const BOX_H: usize = 3;

/// Blank rows between stacked boxes in the same column (room for connectors).
pub(super) const ROW_GAP: usize = 1;

/// The display label for a node: its `name` (short, human-facing) — or, for a
/// versioned model, the `name.vseg` spelling [`crate::list_name`] shares with
/// `ls`/the list/docs (same-name versions must not draw identical boxes).
/// Sources keep their `name` too; that is acceptable for the PoC (collisions
/// are vanishingly rare in lineage subgraphs and the rect map disambiguates by
/// unique_id).
pub(super) fn label_of(node: &crate::NodeInfo) -> String {
    crate::list_name(&node.unique_id, &node.name)
}

/// The materialization tag shown at the top-left of a node's box (`+Tag---+`),
/// derived from the node's classification so the tag and the colour class share
/// one source of truth.
pub(super) fn tag_of(node: &crate::NodeInfo) -> &'static str {
    MaterializationClass::classify(&node.resource_type, node.materialized.as_deref()).tag()
}

/// The `tests:N` label embedded right-aligned in a node box's bottom border, or
/// `None` when the node has no tests. Derived from `NodeInfo::test_count` (the
/// pre-prune tests side count carried onto the node for exactly this purpose),
/// so tests stay visible in the lineage without ever entering the topology.
pub(super) fn tests_label_of(node: &crate::NodeInfo) -> Option<String> {
    (node.test_count > 0).then(|| format!("tests:{}", node.test_count))
}

/// The total width (in cells) of a node's box: the label with one space of
/// padding each side, widened if needed so the materialization tag fits in the
/// top border (and the `tests:N` label in the bottom border) with at least one
/// `-` to spare, plus the two side borders.
pub(super) fn box_width_of(node: &crate::NodeInfo) -> usize {
    let label_w = label_of(node).chars().count();
    let tag_w = tag_of(node).chars().count();
    let tests_w = tests_label_of(node).map_or(0, |t| t.chars().count());
    let inner = (label_w + 2).max(tag_w + 1).max(tests_w + 1);
    inner + 2
}

/// The box metrics for a [`Density`]: `(rows per box, connector-attach row
/// offset within the box)`. Comfortable = the classic 3-row box attaching at
/// its middle row; Compact = a 1-row node attaching at its only row.
pub(super) fn box_metrics(density: Density) -> (usize, usize) {
    match density {
        Density::Comfortable => (BOX_H, 1),
        Density::Compact => (1, 0),
    }
}

/// The box width for a node under a [`Density`]: the full bordered box in
/// Comfortable; just `│` + name + `│` in Compact (no tag / tests labels, so
/// no widening for them either).
pub(super) fn box_width_of_density(node: &crate::NodeInfo, density: Density) -> usize {
    match density {
        Density::Comfortable => box_width_of(node),
        Density::Compact => label_of(node).chars().count() + 2,
    }
}

/// The minimum box width a node's column rows demand: the interior is
/// `{mark} {name}` (a 2-cell prefix + the widest related column name) plus the
/// two side borders. `0` when the node has no column rows — every
/// non-column-focus layout then gets the classic widths, byte-identical.
pub(super) fn col_width_of(cols: &[ColLine]) -> usize {
    cols.iter()
        .map(|c| c.name.chars().count())
        .max()
        .map_or(0, |w| w + 4)
}

/// Stamp a node's COMPACT representation: a single `│name│` row (the side
/// borders in `g`'s glyph set). Only the name cells carry the emphasis flag,
/// so the emphasis region spells exactly the node's name — the same contract
/// as the 3-row [`draw_box`].
pub(super) fn draw_compact_box(
    grid: &mut CharGrid,
    g: &BoxGlyphs,
    rect: NodeRect,
    label: &str,
    emph: bool,
) {
    let (bx, by, w) = (rect.x, rect.y, rect.width);
    if w < 2 {
        return;
    }
    grid.put(bx, by, g.v);
    grid.put(bx + w - 1, by, g.v);
    for (i, ch) in label.chars().enumerate() {
        if 1 + i < w - 1 {
            grid.put_emph(bx + 1 + i, by, ch, emph);
        }
    }
}

/// The per-box text payload [`draw_box`] stamps: the label row, the top-border
/// tag, the bottom-border `tests:N` label, and the related-column rows
/// ([`ColLine`]s — empty for every non-column-focus layout, the classic shape).
pub(super) struct BoxBody<'a> {
    pub(super) label: &'a str,
    pub(super) tag: &'a str,
    pub(super) tests: Option<&'a str>,
    pub(super) cols: &'a [ColLine],
}

/// Stamp a node's box into the grid: a `3 + body.cols.len()`-row box (in `g`'s
/// glyph set) with the materialization tag embedded in the top border, the
/// single-line label on the second row, one `│{mark} {name}…│` row per
/// [`ColLine`] under it (empty for every non-column-focus layout — the box is
/// then the classic 3-row shape, byte-identical), and the optional `tests:N`
/// label right-aligned in the bottom border. Only the LABEL cells carry the
/// emphasis flag, so the emphasis region spells exactly the node's name.
///
/// Each column row's INTERIOR cells are recorded in `col_cells` and stamped
/// with the row's own attr (`faint` → dimmed, `target` → the on-path band) at
/// draw time; [`Layout::apply_node_styles`] skips them so the box-level node
/// attr (class / lens / focus dim) never flattens a row's own styling. The
/// rows' SIDE borders stay un-recorded, so they inherit the node's class colour
/// like every other border cell.
pub(super) fn draw_box(
    grid: &mut CharGrid,
    g: &BoxGlyphs,
    mode: GlyphMode,
    rect: NodeRect,
    body: &BoxBody<'_>,
    emph: bool,
    col_cells: &mut HashSet<(usize, usize)>,
) {
    let BoxBody {
        label,
        tag,
        tests,
        cols,
    } = *body;
    let (bx, by, w) = (rect.x, rect.y, rect.width);
    if w < 2 {
        return;
    }
    let inner = w - 2;

    // Top border: tl + tag + h… + tr
    grid.put(bx, by, g.tl);
    grid.put(bx + w - 1, by, g.tr);
    let tag_chars: Vec<char> = tag.chars().collect();
    for i in 0..inner {
        grid.put(bx + 1 + i, by, tag_chars.get(i).copied().unwrap_or(g.h));
    }

    // Middle row: v + ' ' + label + padding + v (label cells carry emphasis).
    grid.put(bx, by + 1, g.v);
    grid.put(bx + w - 1, by + 1, g.v);
    for i in 0..inner {
        grid.put(bx + 1 + i, by + 1, ' ');
    }
    for (i, ch) in label.chars().enumerate() {
        if 1 + i < inner {
            grid.put_emph(bx + 1 + 1 + i, by + 1, ch, emph);
        }
    }

    // Column rows (column-focus only): `│{mark} {name}…│`, one per related
    // column, between the label row and the bottom border.
    for (i, cl) in cols.iter().enumerate() {
        let ry = by + 2 + i;
        grid.put(bx, ry, g.v);
        grid.put(bx + w - 1, ry, g.v);
        let row_attr = CellAttr {
            dimmed: cl.faint,
            on_path: cl.target,
            ..CellAttr::default()
        };
        for j in 0..inner {
            let cx = bx + 1 + j;
            grid.put(cx, ry, ' ');
            grid.set_attr(cx, ry, row_attr);
            col_cells.insert((cx, ry));
        }
        // `{mark} {name}`, left-trimmed into the interior (the mark column is
        // flush at bx+1 so edge kinds scan vertically down the box). The mark
        // resolves against THIS draw's glyph mode.
        for (j, ch) in format!("{} {}", cl.mark(mode), cl.name).chars().enumerate() {
            if j < inner {
                grid.put(bx + 1 + j, ry, ch);
            }
        }
    }

    // Bottom border: bl + h… tests:N h + br (the tests label right-aligned with
    // one trailing border cell, so it cannot be misread as the NEXT box's tag).
    let bb = by + 2 + cols.len();
    grid.put(bx, bb, g.bl);
    grid.put(bx + w - 1, bb, g.br);
    for i in 0..inner {
        grid.put(bx + 1 + i, bb, g.h);
    }
    if let Some(t) = tests {
        let t_chars: Vec<char> = t.chars().collect();
        // box_width_of guarantees inner >= len + 1, so start lands inside.
        let start = (bx + w - 1).saturating_sub(1 + t_chars.len());
        for (i, ch) in t_chars.iter().enumerate() {
            if start + i > bx {
                grid.put(start + i, bb, *ch);
            }
        }
    }
}
