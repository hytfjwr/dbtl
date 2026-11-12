use super::*;

/// One column row inside a node box (the column-focus view): the related
/// column's normalized name, the edge KIND that produced it within the traced
/// set (the 1-cell mark resolves against the CURRENT glyph mode at draw time
/// — `>` for the traced column overrides it), and two flags driving the row's
/// own render attr — `faint` dims it (the column's column edges never saw an
/// [`crate::Confidence::Exact`] link, so the row is an inferred name) and
/// `target` paints the on-path band (this row IS the traced column).
/// `Eq`-safe plain data: computed Dag-side, consumed Dag-free here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColLine {
    /// The column's normalized name (the text after the mark).
    pub name: String,
    /// The heaviest edge kind incident to the column — kept as the KIND, not
    /// a baked char, so a glyph-mode change re-resolves the mark glyph.
    pub kind: ColumnEdgeKind,
    /// Dim the row (inferred-only provenance).
    pub faint: bool,
    /// The traced column — painted with the on-path band + the `>` mark.
    pub target: bool,
}

impl ColLine {
    /// The 1-cell row mark in `mode`: `>` for the traced column, else the
    /// edge-kind glyph — `·` passthrough is East-Asian Ambiguous (width varies
    /// by terminal), so ASCII mode draws `-` instead; every other mark is
    /// already plain ASCII.
    pub(super) fn mark(&self, mode: GlyphMode) -> char {
        if self.target {
            return '>';
        }
        match self.kind {
            ColumnEdgeKind::Passthrough => match mode {
                GlyphMode::Unicode => '·',
                GlyphMode::Ascii => '-',
            },
            ColumnEdgeKind::FilterOrJoin => '%',
            ColumnEdgeKind::Rename => '~',
            ColumnEdgeKind::Transform => '*',
        }
    }
}

/// A node's placement rectangle on the [`CharGrid`]: the full BOX rect (3 rows
/// plus one per [`ColLine`] in [`Density::Comfortable`], 1 row in
/// [`Density::Compact`]). Consumed by the non-intersection check, viewport
/// anchoring/follow (which shows the whole box via `height`), and the mouse
/// hit-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRect {
    /// Left cell x (inclusive).
    pub x: usize,
    /// Top cell y (inclusive).
    pub y: usize,
    /// Number of cells wide (== label char count).
    pub width: usize,
    /// Number of cells tall.
    pub height: usize,
}

impl NodeRect {
    /// One-past-the-right cell x.
    pub(super) fn right(&self) -> usize {
        self.x + self.width
    }
    /// One-past-the-bottom cell y.
    fn bottom(&self) -> usize {
        self.y + self.height
    }
    /// Whether two rects overlap in any cell (used for the label-non-overlap
    /// invariant; should always be false for distinct nodes).
    pub fn intersects(&self, other: &NodeRect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// A pure character grid: a rectangular `height x width` matrix of display
/// chars plus a parallel per-cell emphasis flag. Row-major. No ratatui types.
///
/// The grid is always rectangular (every row padded to `width` with spaces), so
/// the golden exact-match can treat trailing spaces as significant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharGrid {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) cells: Vec<char>,
    emphasis: Vec<bool>,
    /// Parallel per-cell render attributes (materialization class + tests flag).
    /// Default `Plain`/false everywhere; stamped over label cells by
    /// [`Layout::apply_node_styles`]. `to_text()` / `emphasis_regions()` never
    /// read this, so the golden exact-match is unaffected.
    pub(super) attr: Vec<CellAttr>,
}

impl CharGrid {
    /// A `width x height` grid filled with spaces, no emphasis, default attrs.
    pub(super) fn new(width: usize, height: usize) -> Self {
        CharGrid {
            width,
            height,
            cells: vec![' '; width * height],
            emphasis: vec![false; width * height],
            attr: vec![CellAttr::default(); width * height],
        }
    }

    /// Grid width in cells.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Grid height in cells.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The char at `(x, y)`, or `' '` when out of range.
    pub fn char_at(&self, x: usize, y: usize) -> char {
        if x >= self.width || y >= self.height {
            return ' ';
        }
        self.cells[y * self.width + x]
    }

    /// The emphasis flag at `(x, y)`, or `false` when out of range.
    pub fn emphasis_at(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        self.emphasis[y * self.width + x]
    }

    /// The render attributes at `(x, y)`, or the default when out of range.
    pub fn attr_at(&self, x: usize, y: usize) -> CellAttr {
        if x >= self.width || y >= self.height {
            return CellAttr::default();
        }
        self.attr[y * self.width + x]
    }

    /// Set the render attributes at `(x, y)` (no-op when out of range). Used by
    /// [`Layout::apply_node_styles`] to stamp materialization/tests over a label.
    pub(super) fn set_attr(&mut self, x: usize, y: usize, attr: CellAttr) {
        if x < self.width && y < self.height {
            self.attr[y * self.width + x] = attr;
        }
    }

    /// Write `c` at `(x, y)` (no-op when out of range). Used for connectors;
    /// connectors are stamped before labels so a label always wins its cells.
    pub(super) fn put(&mut self, x: usize, y: usize, c: char) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = c;
        }
    }

    /// Write `c` at `(x, y)` with an emphasis flag (no-op when out of range).
    pub(super) fn put_emph(&mut self, x: usize, y: usize, c: char, emph: bool) {
        if x < self.width && y < self.height {
            let idx = y * self.width + x;
            self.cells[idx] = c;
            self.emphasis[idx] = emph;
        }
    }

    /// Render one row as a `width`-char string (trailing spaces significant).
    pub fn row_string(&self, y: usize) -> String {
        if y >= self.height {
            return " ".repeat(self.width);
        }
        let start = y * self.width;
        self.cells[start..start + self.width].iter().collect()
    }

    /// The whole grid as a newline-joined string, every row padded to `width`.
    /// Trailing spaces are significant (matches the golden exact-match).
    pub fn to_text(&self) -> String {
        (0..self.height)
            .map(|y| self.row_string(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Maximal contiguous horizontal runs of emphasized cells, as
    /// `(y, x_start, spelled_string)`. The contract requires exactly one
    /// region whose spelling equals the selected node's name — counting
    /// *regions*, not cells (a multi-cell label is one region).
    pub fn emphasis_regions(&self) -> Vec<(usize, usize, String)> {
        let mut regions = Vec::new();
        for y in 0..self.height {
            let mut x = 0;
            while x < self.width {
                if self.emphasis_at(x, y) {
                    let start = x;
                    let mut s = String::new();
                    while x < self.width && self.emphasis_at(x, y) {
                        s.push(self.char_at(x, y));
                        x += 1;
                    }
                    regions.push((y, start, s));
                } else {
                    x += 1;
                }
            }
        }
        regions
    }
}

/// The full result of laying out a subgraph: the grid plus structured truth
/// maps for invariant checks and viewport anchoring.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The entire lineage graph as a rectangular char grid (never viewport-clipped).
    pub grid: CharGrid,
    /// `unique_id -> longest-path column` (logical). Truth source for the
    /// right-going-edge invariant.
    pub columns: HashMap<String, usize>,
    /// `unique_id -> label rectangle` on the grid. Truth source for
    /// present-exactly-once, column order (x), and label non-overlap.
    pub rects: HashMap<String, NodeRect>,
    /// The selected node's label start coordinate `(x, y)` on the grid (for
    /// viewport anchoring), or `None` when the subgraph is empty.
    pub selected_coord: Option<(usize, usize)>,
    /// The selected node's full label rectangle on the grid, or `None` when the
    /// subgraph is empty. Anchoring uses the rect's WIDTH so the whole (often
    /// long) label fits in the viewport, not just its start cell.
    pub selected_rect: Option<NodeRect>,
    /// `(parent, child) -> the connector cells this edge actually drew`
    /// (segments, corners, and its arrowhead). A cell first claimed by an
    /// earlier (sorted-order) edge is attributed to THAT edge only, so a shared
    /// bus or crossing never double-counts. Lets [`Layout::apply_edge_styles`]
    /// re-style a specific edge's run without the
    /// renderer re-deriving connector routes.
    pub edge_cells: HashMap<(String, String), Vec<(usize, usize)>>,
    /// The interior cells of column rows (the column-focus view's per-column
    /// text lines inside node boxes), where `draw_box` stamped per-ROW attrs
    /// (`faint`/`target` from [`ColLine`]). [`Layout::apply_node_styles`] skips these
    /// cells so a node's box-level attr never flattens a column row's own
    /// styling. Empty whenever no column rows were drawn (every non-focus
    /// layout — the skip is then a no-op and styling is byte-identical).
    pub col_cells: HashSet<(usize, usize)>,
}

impl Layout {
    /// Stamp per-cell render attributes over each node's label cells, from a
    /// `unique_id -> CellAttr` map. An app-side POST-PASS (called after
    /// [`layout`], never inside it), so `layout`'s arity and the golden/
    /// determinism tests are untouched. The source is a [`BTreeMap`] and node
    /// rects are pairwise disjoint, so the result is order-independent and
    /// deterministic. Unknown ids are ignored.
    pub fn apply_node_styles(&mut self, styles: &BTreeMap<String, CellAttr>) {
        for (uid, attr) in styles {
            if let Some(&rect) = self.rects.get(uid) {
                for dy in 0..rect.height {
                    for dx in 0..rect.width {
                        // Column-row interiors keep their per-ROW attr (faint /
                        // target, stamped by `draw_box`); the box-level node
                        // attr must not flatten them.
                        if self.col_cells.contains(&(rect.x + dx, rect.y + dy)) {
                            continue;
                        }
                        self.grid.set_attr(rect.x + dx, rect.y + dy, *attr);
                    }
                }
            }
        }
    }

    /// Stamp per-cell render attributes over each edge's connector cells, from a
    /// `(parent, child) -> CellAttr` map — the edge twin of
    /// [`apply_node_styles`](Layout::apply_node_styles), and the same contract:
    /// an app-side post-pass, attr-only (never a glyph), deterministic (a
    /// `BTreeMap` source). Unknown edges are ignored.
    ///
    /// Cell lists are NOT fully disjoint: siblings into one child share that
    /// child's arrowhead cell. Stamping is therefore two-phase — non-`on_path`
    /// attrs first, `on_path` attrs second — so a shared cell always resolves
    /// to the PATH attr regardless of key order (the path band must never be
    /// dimmed away by an off-path sibling).
    pub fn apply_edge_styles(&mut self, styles: &BTreeMap<(String, String), CellAttr>) {
        let phases = [false, true];
        for phase in phases {
            for (key, attr) in styles.iter().filter(|(_, a)| a.on_path == phase) {
                if let Some(cells) = self.edge_cells.get(key) {
                    for &(x, y) in cells {
                        self.grid.set_attr(x, y, *attr);
                    }
                }
            }
        }
    }
}
