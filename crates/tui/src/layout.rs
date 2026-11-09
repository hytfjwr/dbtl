//! Pure, ratatui-independent lineage layout.
//!
//! `layout(subgraph) -> Layout` produces a rectangular [`CharGrid`] of the
//! *entire* lineage graph (never clipped to a viewport) plus structured maps
//! (`columns`, `rects`, `selected_coord`). Invariant checks run against the
//! maps, NOT against CharGrid text, which would false-positive on shared
//! prefixes like `stg_payment__suppliers` vs `…supplier_departments`.

use std::collections::{BTreeMap, HashMap};

use crate::Subgraph;

/// Which glyph repertoire the lineage grid (and the surrounding UI chrome) is
/// drawn with.
///
/// `Unicode` is the pretty default (`╭ ─ │ ▶`). Every one of those glyphs is
/// **East-Asian-Ambiguous width**, though: terminals configured (or
/// font-fallback-forced) to render ambiguous characters 2 cells wide — common
/// in CJK setups — desync ratatui's 1-cell buffer model into doubled/ghosted
/// borders. `Ascii` (`+ - | >`) is unambiguous on every terminal, so the binary
/// probes the live terminal at startup and falls back to it automatically
/// (overridable with `--unicode` / `--ascii`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GlyphMode {
    /// Unicode box-drawing boxes/connectors — the default look.
    #[default]
    Unicode,
    /// Pure-ASCII boxes/connectors — immune to ambiguous-width rendering.
    Ascii,
}

/// How much room each node box takes in the lineage diagram.
///
/// `Comfortable` (the default) is the classic 3-row box (materialization tag in
/// the top border, `tests:N` in the bottom border). `Compact` collapses each
/// node to a single `│name│` row (no tag / tests labels), fitting roughly twice
/// as many nodes per screen — the big-graph overview mode. Geometry-only:
/// the glyph repertoire still comes from [`GlyphMode`], and emphasis still
/// covers exactly the name cells in both densities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Density {
    /// 3-row boxes with the tag / tests borders — the detailed default.
    #[default]
    Comfortable,
    /// 1-row `│name│` nodes — the dense overview.
    Compact,
}

/// The concrete glyph set for one [`GlyphMode`]: box borders, connector lines,
/// and the arrowhead. Corners double as the connector turn glyphs (`╮ ╯ ╰ ╭`
/// in Unicode; all `+` in ASCII).
struct BoxGlyphs {
    tl: char,
    tr: char,
    bl: char,
    br: char,
    h: char,
    v: char,
    arrow: char,
}

impl BoxGlyphs {
    fn for_mode(mode: GlyphMode) -> &'static BoxGlyphs {
        match mode {
            GlyphMode::Unicode => &UNICODE_GLYPHS,
            GlyphMode::Ascii => &ASCII_GLYPHS,
        }
    }
}

static UNICODE_GLYPHS: BoxGlyphs = BoxGlyphs {
    // Rounded arc corners (U+256D..U+2570): the same East-Asian-Ambiguous block
    // as the right-angle corners (ASCII mode already covers the 2-cell-width
    // terminals), matching the rounded pane chrome.
    tl: '╭',
    tr: '╮',
    bl: '╰',
    br: '╯',
    h: '─',
    v: '│',
    arrow: '▶',
};

static ASCII_GLYPHS: BoxGlyphs = BoxGlyphs {
    tl: '+',
    tr: '+',
    bl: '+',
    br: '+',
    h: '-',
    v: '|',
    arrow: '>',
};

/// How a node's label cells are classified for render-layer styling. Carried as
/// a parallel per-cell array on [`CharGrid`] (NOT a glyph change), so it never
/// affects grid width, `to_text()`, or `emphasis_regions()` — the golden diamond
/// exact-match stays byte-identical.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MaterializationClass {
    /// Connector cells, padding, or an unstyled node.
    #[default]
    Plain,
    Table,
    View,
    Incremental,
    Ephemeral,
    /// A model whose materialization is unknown / missing.
    OtherModel,
    Source,
    Seed,
    Snapshot,
    /// A declared downstream consumer (dashboard / notebook / …) — a terminator
    /// leaf on the right edge of the diagram.
    Exposure,
}

impl MaterializationClass {
    /// Classify a node from its resource type and (for models) materialization —
    /// the single source of truth for the (resource_type, materialized) → class
    /// rule. Sources/seeds/snapshots classify by type; a model with an unknown or
    /// missing materialization falls back to `OtherModel`.
    pub fn classify(resource_type: &str, materialized: Option<&str>) -> Self {
        match resource_type {
            "source" => MaterializationClass::Source,
            "seed" => MaterializationClass::Seed,
            "snapshot" => MaterializationClass::Snapshot,
            "exposure" => MaterializationClass::Exposure,
            "model" => match materialized {
                Some("table") => MaterializationClass::Table,
                Some("view") => MaterializationClass::View,
                Some("incremental") => MaterializationClass::Incremental,
                Some("ephemeral") => MaterializationClass::Ephemeral,
                _ => MaterializationClass::OtherModel,
            },
            _ => MaterializationClass::Plain,
        }
    }

    /// The short display tag shown in a node box's top border (`+Tag---+`).
    /// Exhaustive over the enum, so adding a variant is a compile error here and
    /// the tag can never silently drift from the colour class.
    pub fn tag(self) -> &'static str {
        match self {
            MaterializationClass::Table => "Table",
            MaterializationClass::View => "View",
            MaterializationClass::Incremental => "Incremental",
            MaterializationClass::Ephemeral => "Ephemeral",
            MaterializationClass::OtherModel => "Model",
            MaterializationClass::Source => "Source",
            MaterializationClass::Seed => "Seed",
            MaterializationClass::Snapshot => "Snapshot",
            MaterializationClass::Exposure => "Exposure",
            MaterializationClass::Plain => "Node",
        }
    }
}

/// A SEMANTIC lineage-lens tint, set by `App::lineage_styles` for the ACTIVE lens
/// only. Layout stays ratatui-free, so this names an *intent* (Warn / heat bucket
/// / which layer / a layer-rank violation) and `ui::lineage::attr_style` maps each
/// to a concrete `Color`. `None` (the default) leaves the materialization class
/// colour showing through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LensTint {
    /// No tint — the class colour shows.
    #[default]
    None,
    /// Coverage lens: a testable resource with zero tests (the old `untested`).
    Warn,
    /// Degree-heat lens, low / mid / high transitive-downstream buckets.
    HeatLow,
    HeatMid,
    HeatHigh,
    /// Layer lens, one tint per dbt logical layer.
    LayerStaging,
    LayerIntermediate,
    LayerMarts,
    LayerUtilities,
    LayerOther,
    /// Layer-violation lens: incident to a marts→staging-style backward edge.
    Violation,
    /// Diff lens: node added vs the `--diff` baseline.
    DiffAdd,
    /// Diff lens: node modified vs the `--diff` baseline.
    DiffMod,
}

/// Per-cell render attributes, orthogonal to the char and the emphasis flag:
/// the materialization `class` (→ a colour) plus render-only overlay state that
/// COMPOSES with it. Tests are shown as data only (the `tests:N` label in the
/// bottom border, from `NodeInfo::test_count`), never as a style — an underline
/// across the box's border glyphs reads as doubled lines. A `struct` (not an
/// enum) so the orthogonal overlays can ride alongside `class`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttr {
    pub class: MaterializationClass,
    /// The active lineage lens's tint for this node (`None` = no tint, the class
    /// colour shows). Render-only; set by `App::lineage_styles` for whichever lens
    /// is selected (→ a foreground that wins over the class colour, but loses to
    /// `dimmed`).
    pub lens: LensTint,
    /// Focus dim: the cursor is off-root and this node is NOT on the root↔cursor
    /// path. Render-only; a dim foreground that wins over both the lens tint and
    /// the class colour, so the path nodes stand out.
    pub dimmed: bool,
    /// On the root↔cursor lineage path (automatic when the cursor is off-root).
    /// Render-only; an orthogonal background band so it composes with the
    /// foreground (lens / class / dim) above.
    pub on_path: bool,
}

/// A node's placement rectangle on the [`CharGrid`]: the full BOX rect (3 rows
/// in [`Density::Comfortable`], 1 row in [`Density::Compact`]). Consumed by the
/// non-intersection check, viewport anchoring/follow (which shows the whole
/// box via `height`), and the mouse hit-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRect {
    /// Left cell x (inclusive).
    pub x: usize,
    /// Top cell y (inclusive).
    pub y: usize,
    /// Number of cells wide (== label char count).
    pub width: usize,
    /// Number of cells tall (always 1 here).
    pub height: usize,
}

impl NodeRect {
    /// One-past-the-right cell x.
    fn right(&self) -> usize {
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
    width: usize,
    height: usize,
    cells: Vec<char>,
    emphasis: Vec<bool>,
    /// Parallel per-cell render attributes (materialization class + tests flag).
    /// Default `Plain`/false everywhere; stamped over label cells by
    /// [`Layout::apply_node_styles`]. `to_text()` / `emphasis_regions()` never
    /// read this, so the golden exact-match is unaffected.
    attr: Vec<CellAttr>,
}

impl CharGrid {
    /// A `width x height` grid filled with spaces, no emphasis, default attrs.
    fn new(width: usize, height: usize) -> Self {
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
    fn set_attr(&mut self, x: usize, y: usize, attr: CellAttr) {
        if x < self.width && y < self.height {
            self.attr[y * self.width + x] = attr;
        }
    }

    /// Write `c` at `(x, y)` (no-op when out of range). Used for connectors;
    /// connectors are stamped before labels so a label always wins its cells.
    fn put(&mut self, x: usize, y: usize, c: char) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = c;
        }
    }

    /// Write `c` at `(x, y)` with an emphasis flag (no-op when out of range).
    fn put_emph(&mut self, x: usize, y: usize, c: char, emph: bool) {
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
    /// bus or crossing never double-counts. Lets [`apply_edge_styles`]
    /// (Layout::apply_edge_styles) re-style a specific edge's run without the
    /// renderer re-deriving connector routes.
    pub edge_cells: HashMap<(String, String), Vec<(usize, usize)>>,
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

/// Horizontal cells between adjacent column boxes (the connector gutter). Kept
/// compact — just enough for a short horizontal run plus the `>` arrowhead — so a
/// multi-column diagram is narrower and more often fits the pane without
/// scrolling (`-->` instead of a wide `---->`). Fixed so the golden is stable.
const GUTTER: usize = 3;

/// Height of a node box: top border + label row + bottom border.
const BOX_H: usize = 3;

/// Blank rows between stacked boxes in the same column (room for connectors).
const ROW_GAP: usize = 1;

/// The display label for a node: its `name` (short, human-facing). Sources keep
/// their `name` too; that is acceptable for the PoC (collisions are vanishingly
/// rare in lineage subgraphs and the rect map disambiguates by unique_id).
fn label_of(node: &crate::NodeInfo) -> &str {
    &node.name
}

/// The materialization tag shown at the top-left of a node's box (`+Tag---+`),
/// derived from the node's classification so the tag and the colour class share
/// one source of truth.
fn tag_of(node: &crate::NodeInfo) -> &'static str {
    MaterializationClass::classify(&node.resource_type, node.materialized.as_deref()).tag()
}

/// The `tests:N` label embedded right-aligned in a node box's bottom border, or
/// `None` when the node has no tests. Derived from `NodeInfo::test_count` (the
/// pre-prune tests side count carried onto the node for exactly this purpose),
/// so tests stay visible in the lineage without ever entering the topology.
fn tests_label_of(node: &crate::NodeInfo) -> Option<String> {
    (node.test_count > 0).then(|| format!("tests:{}", node.test_count))
}

/// The total width (in cells) of a node's box: the label with one space of
/// padding each side, widened if needed so the materialization tag fits in the
/// top border (and the `tests:N` label in the bottom border) with at least one
/// `-` to spare, plus the two side borders.
fn box_width_of(node: &crate::NodeInfo) -> usize {
    let label_w = label_of(node).chars().count();
    let tag_w = tag_of(node).chars().count();
    let tests_w = tests_label_of(node).map_or(0, |t| t.chars().count());
    let inner = (label_w + 2).max(tag_w + 1).max(tests_w + 1);
    inner + 2
}

/// The box metrics for a [`Density`]: `(rows per box, connector-attach row
/// offset within the box)`. Comfortable = the classic 3-row box attaching at
/// its middle row; Compact = a 1-row node attaching at its only row.
fn box_metrics(density: Density) -> (usize, usize) {
    match density {
        Density::Comfortable => (BOX_H, 1),
        Density::Compact => (1, 0),
    }
}

/// The box width for a node under a [`Density`]: the full bordered box in
/// Comfortable; just `│` + name + `│` in Compact (no tag / tests labels, so
/// no widening for them either).
fn box_width_of_density(node: &crate::NodeInfo, density: Density) -> usize {
    match density {
        Density::Comfortable => box_width_of(node),
        Density::Compact => label_of(node).chars().count() + 2,
    }
}

/// Stamp a node's COMPACT representation: a single `│name│` row (the side
/// borders in `g`'s glyph set). Only the name cells carry the emphasis flag,
/// so the emphasis region spells exactly the node's name — the same contract
/// as the 3-row [`draw_box`].
fn draw_compact_box(grid: &mut CharGrid, g: &BoxGlyphs, rect: NodeRect, label: &str, emph: bool) {
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

/// Stamp a node's box into the grid: a 3-row box (in `g`'s glyph set) with the
/// materialization tag embedded in the top border, the single-line label on the
/// middle row, and the optional `tests:N` label right-aligned in the bottom
/// border. Only the LABEL cells carry the emphasis flag, so the emphasis region
/// spells exactly the node's name.
fn draw_box(
    grid: &mut CharGrid,
    g: &BoxGlyphs,
    rect: NodeRect,
    label: &str,
    tag: &str,
    tests: Option<&str>,
    emph: bool,
) {
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

    // Bottom border: bl + h… tests:N h + br (the tests label right-aligned with
    // one trailing border cell, so it cannot be misread as the NEXT box's tag).
    grid.put(bx, by + 2, g.bl);
    grid.put(bx + w - 1, by + 2, g.br);
    for i in 0..inner {
        grid.put(bx + 1 + i, by + 2, g.h);
    }
    if let Some(t) = tests {
        let t_chars: Vec<char> = t.chars().collect();
        // box_width_of guarantees inner >= len + 1, so start lands inside.
        let start = (bx + w - 1).saturating_sub(1 + t_chars.len());
        for (i, ch) in t_chars.iter().enumerate() {
            if start + i > bx {
                grid.put(start + i, by + 2, *ch);
            }
        }
    }
}

/// Longest-path column assignment over the subgraph edges.
///
/// `column(n) = max over parents p (column(p) + 1)`, sources = 0. Computed by
/// relaxation to a fixpoint (the subgraph is a DAG, so it converges in at most
/// `nodes` passes; we cap at `nodes+1` for safety). Returns every node's column
/// (nodes with no in-subgraph parent get 0).
fn longest_path_columns(sg: &Subgraph) -> HashMap<String, usize> {
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
    let g = BoxGlyphs::for_mode(mode);
    let (box_h, box_mid) = box_metrics(density);
    if sg.nodes.is_empty() {
        return Layout {
            grid: CharGrid::new(1, 1),
            columns: HashMap::new(),
            rects: HashMap::new(),
            selected_coord: None,
            selected_rect: None,
            edge_cells: HashMap::new(),
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
        .map(|nodes| {
            nodes
                .iter()
                .map(|n| box_width_of_density(n, density))
                .max()
                .unwrap_or(1)
                .max(1)
        })
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

    // Each node is a `box_h`-row box. Within a column, box i sits at
    // y = i*(box_h+ROW_GAP). `rects` are the BOX rects (used for non-overlap,
    // column order, anchoring, and mouse hit-testing). Grid height = tallest
    // stacked column.
    let stride = box_h + ROW_GAP;
    let mut rects: HashMap<String, NodeRect> = HashMap::new();
    let mut max_rows = 0usize;
    for (c, nodes) in col_nodes.iter().enumerate() {
        for (i, node) in nodes.iter().enumerate() {
            rects.insert(
                node.unique_id.clone(),
                NodeRect {
                    x: col_x[c],
                    y: i * stride,
                    width: box_width_of_density(node, density),
                    height: box_h,
                },
            );
            max_rows = max_rows.max(i + 1);
        }
    }
    let grid_height = if max_rows == 0 {
        1
    } else {
        (max_rows - 1) * stride + box_h
    };

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
                    rect,
                    label_of(node),
                    tag_of(node),
                    tests.as_deref(),
                    emph,
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
    }
}

/// Clamp a desired scroll offset to `[0, grid_dim - view_dim]` for one axis.
///
/// When the grid is no larger than the viewport, the only valid offset is 0.
/// All arithmetic is saturating, so degenerate sizes never panic. This is the
/// single source of truth for both axes' scroll bounds.
pub fn clamp_offset(grid_dim: usize, view_dim: usize, desired: usize) -> usize {
    let max_offset = grid_dim.saturating_sub(view_dim);
    desired.min(max_offset)
}

/// Compute the initial viewport offset `(x, y)` that centers the selected
/// node's LABEL in a `view_w x view_h` viewport over `grid`, then clamps each
/// axis so the whole label is visible.
///
/// The horizontal anchor centers the label's *midpoint* (not its start cell),
/// so a long label (e.g. `fct_subscription_process`, 24 cells) fits entirely
/// when the viewport is at least as wide as the label — centering the start
/// would clip the tail. Two cheap edge-ensures cover grid boundaries and the
/// extreme case where the label is wider than the viewport (very long source
/// names / tiny panes): show the right edge, then prefer the start (names read
/// left-to-right) without ever panicking. Labels are one row tall, so the
/// vertical axis just centers the row and clamps.
///
/// `None` (empty subgraph) yields `(0, 0)`.
pub fn anchor_offset(
    selected_rect: Option<NodeRect>,
    grid: &CharGrid,
    view_w: usize,
    view_h: usize,
) -> (usize, usize) {
    let Some(rect) = selected_rect else {
        return (0, 0);
    };
    let grid_w = grid.width();
    let grid_h = grid.height();
    let (sx, w) = (rect.x, rect.width.max(1));

    // Horizontal: center the label's midpoint in the viewport.
    let mut off_x = clamp_offset(grid_w, view_w, (sx + w / 2).saturating_sub(view_w / 2));
    // Ensure the label's right edge is visible (no tail clipping).
    if sx + w > off_x + view_w {
        off_x = (sx + w).saturating_sub(view_w);
    }
    // Ensure the label's start is visible (wins for labels wider than view_w,
    // so the human-meaningful prefix shows; names read left-to-right).
    if sx < off_x {
        off_x = sx;
    }
    off_x = clamp_offset(grid_w, view_w, off_x);

    // Vertical: labels are 1 row tall, so centering the row + clamp suffices.
    let off_y = clamp_offset(grid_h, view_h, rect.y.saturating_sub(view_h / 2));

    (off_x, off_y)
}

/// Cut a `view_w x view_h` window out of `grid` at offset `(off_x, off_y)`,
/// returning a new [`CharGrid`] of exactly that size (padding with spaces where
/// the window extends past the grid). Pure (no ratatui), so what lands in the
/// viewport is testable without a terminal.
///
/// The whole grid is built first (`layout`) and only then sliced, so the
/// "compute window == draw window" guarantee holds in 2D.
pub fn blit(grid: &CharGrid, off_x: usize, off_y: usize, view_w: usize, view_h: usize) -> CharGrid {
    let mut out = CharGrid::new(view_w.max(1), view_h.max(1));
    for vy in 0..out.height {
        for vx in 0..out.width {
            let gx = off_x + vx;
            let gy = off_y + vy;
            if gx < grid.width && gy < grid.height {
                let c = grid.char_at(gx, gy);
                let e = grid.emphasis_at(gx, gy);
                out.put_emph(vx, vy, c, e);
                out.set_attr(vx, vy, grid.attr_at(gx, gy));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edge, NodeInfo, Subgraph};

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
}
