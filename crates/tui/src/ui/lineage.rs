//! The lineage pane renderer: blit the selected node's CharGrid at the current
//! 2-axis scroll offset and transcribe it into the pane interior, converting
//! per-cell emphasis to the REVERSED+BOLD style the list uses.
//!
//! Thin transcription only: no layout or scroll decisions here — the full grid
//! was built by `layout()` and the window chosen by `blit()` with `state`'s
//! scroll offset, so the computed window equals the drawn window.

use std::collections::BTreeSet;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::layout::{CellAttr, LensTint, MaterializationClass};
use crate::model_list::ModelList;

use super::geom::{TitleKind, TitleLayout, TitleSpan};
use super::{
    focus_border, selected_style, theme, title_style, Focus, HitTarget, LineageLens, UiState,
};

/// Draw the lineage pane for the selected model (or a blank pane when there is
/// no subgraph). `pub(crate)` so `render::draw` can compose it.
///
/// (Argument count: this is a private transcription helper fed straight from
/// `RenderCtx`'s fields — the public bundling already happened there, so a
/// second bundle struct would just mirror `RenderCtx`.)
/// One column's layer annotation for the lineage pane's bottom-border band:
/// the column's grid `x`/`width`, the human label (the dbt layer dir, e.g.
/// `staging`), and the SEMANTIC tint (the same `LensTint::Layer*` the Layer
/// lens paints nodes with, so the band and the boxes always agree on colour).
/// Produced Dag-side by `App::layer_bands` ONLY for columns whose models
/// unanimously share one layer — a mixed column shows no band rather than a
/// lie. Travels through `RenderCtx` (Dag-free: plain data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerBand {
    /// The column's left edge in grid coordinates.
    pub x: usize,
    /// The column's width (its widest box).
    pub width: usize,
    /// The layer label drawn into the border (clipped to the column width).
    pub label: String,
    /// The layer's lens tint (mapped to a colour by the render layer).
    pub tint: LensTint,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_lineage_pane(
    frame: &mut Frame,
    area: Rect,
    list: &ModelList,
    state: &UiState,
    lineage: Option<&crate::Layout>,
    search: Option<&str>,
    search_pos: Option<(usize, usize)>,
    view_label: Option<&str>,
    breadcrumb: Option<&str>,
    overview_title: Option<&str>,
    glyphs: crate::GlyphMode,
    minimap: bool,
    layer_bands: Option<&[LayerBand]>,
    col_suffix: Option<&str>,
    hover_node: Option<&str>,
    search_matches: &[String],
    hover: Option<&HitTarget>,
    t: &theme::Theme,
) {
    let chrome = super::chrome(glyphs);
    let sel_name = list
        .model_at(state.selected())
        .map(|n| n.name.as_str())
        .unwrap_or("-");
    let caret = chrome.caret;
    // The title layout is the SAME shape `hit_target` consulted for the
    // title row (`title_layout` is the single composer, so the clickable
    // crumb/view/lens spans exactly overlay the drawn text). The hovered
    // span — when the pointer sits on one — draws underlined, the affordance
    // cue for a clickable title part.
    let title = title_layout(
        sel_name,
        search.map(|q| (q, search_pos)),
        view_label,
        breadcrumb,
        overview_title,
        state.lens(),
        col_suffix,
        caret,
        area.width,
    );
    let hovered = hover.and_then(|h| match h {
        HitTarget::TitleCrumb(n) => Some(TitleKind::Crumb(*n)),
        HitTarget::TitleView => Some(TitleKind::View),
        HitTarget::TitleLens => Some(TitleKind::Lens),
        _ => None,
    });
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome.border)
        .border_style(focus_border(state, Focus::RightPane, t))
        .title(title_line(
            &title,
            title_style(state, Focus::RightPane, t),
            hovered,
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(lay) = lineage else {
        // No subgraph to draw: stamp the one-line reason on the first interior
        // row — the same display-only chrome the list pane's empty/hint rows
        // use (never a layout row).
        if inner.height > 0 && inner.width > 0 {
            let msg = if list.is_empty() {
                "no model selected"
            } else {
                "no lineage to show"
            };
            let row = Rect { height: 1, ..inner };
            frame.render_widget(Clear, row);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!(" {msg}"),
                    Style::default().fg(t.text_faint),
                )),
                row,
            );
        }
        return;
    };

    let view_w = inner.width as usize;
    let view_h = inner.height as usize;
    if view_w == 0 || view_h == 0 {
        return;
    }

    // Centre the diagram both axes via the SHARED geometry helper (the mouse
    // hit-test uses the same one, so clicks land on the right node). When the
    // diagram is larger than the pane, the content rect == the pane and the
    // scroll offset takes over.
    let target = super::lineage_content_rect(inner, lay.grid.width(), lay.grid.height());
    let content_w = target.width as usize;
    let content_h = target.height as usize;

    // Cut the window at the current scroll offset (same blit the tests use):
    // computed window == drawn window.
    let window = crate::blit(
        &lay.grid,
        state.lineage_scroll_x(),
        state.lineage_scroll_y(),
        content_w,
        content_h,
    );

    let emph_style = selected_style(t);
    let mut lines: Vec<Line> = Vec::with_capacity(view_h);
    for y in 0..window.height() {
        // Build the row as runs sharing the same (emphasis, attr): the selected
        // node is REVERSED+BOLD (emphasis wins over colour); other labels are
        // coloured by materialization; connectors/blanks (Plain) stay default.
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_key = (false, CellAttr::default());
        for x in 0..window.width() {
            let ch = window.char_at(x, y);
            let key = (window.emphasis_at(x, y), window.attr_at(x, y));
            if key != run_key && !run.is_empty() {
                spans.push(styled_run(&run, run_key, emph_style, t));
                run.clear();
            }
            run_key = key;
            run.push(ch);
        }
        if !run.is_empty() {
            spans.push(styled_run(&run, run_key, emph_style, t));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), target);

    // The pointer/search accents: every search-match node's box
    // frame, and the HOVERED node's frame plus the connector cells of its
    // direct edges — a style-only post-blit pass mapped through the same
    // scroll window the blit used. Glyphs, geometry, and CharGrid are all
    // untouched (it never reaches the layout cache keys), so the golden
    // fixtures and hit-test see nothing. Drawn before the minimap — the inset
    // is its own sketch and wins where the two overlap.
    stamp_accent_cells(
        frame,
        target,
        lay,
        state.lineage_scroll_x(),
        state.lineage_scroll_y(),
        hover_node,
        search_matches,
        t,
    );

    // A wide/tall diagram is scrolled, not broken: stamp a directional marker on
    // each clipped border edge so partially-drawn boxes read as "scroll this way"
    // (h/l, ↑/↓) rather than a corrupt render. Computed from the SAME content-rect
    // size that was blitted, so a marker appears exactly when cells are unshown.
    // Layer bands (Layer lens only — the loop passes `Some` exactly then):
    // each unanimous column's layer name, stamped into the BOTTOM border at
    // the column's x-range, scroll-following the content window. Drawn before
    // the scroll markers so a clipped-bottom marker wins its one cell.
    if let Some(bands) = layer_bands {
        stamp_layer_bands(
            frame,
            area,
            target,
            state.lineage_scroll_x(),
            content_w,
            bands,
            t,
        );
    }

    let edges = super::clip_edges(
        lay.grid.width(),
        lay.grid.height(),
        state.lineage_scroll_x(),
        state.lineage_scroll_y(),
        content_w,
        content_h,
    );
    stamp_scroll_markers(frame, area, edges, chrome, t);

    // Opt-in minimap inset (default OFF, so every existing lineage render is
    // untouched). `minimap_rect` is the single source of BOTH the draw guard
    // (enabled + diagram overflows + interior big enough) and the rect — the
    // mouse hit-test reads the same one, so a click under the inset can never
    // disagree with what was drawn. Drawn AFTER the blit + markers — it
    // overwrites a corner of the interior, which is acceptable for an opt-in
    // overview. Writes ONLY to the frame buffer (never CharGrid).
    if let Some(mm) = super::minimap_rect(
        inner,
        lay.grid.width(),
        lay.grid.height(),
        state.lineage_scroll_x(),
        state.lineage_scroll_y(),
        minimap,
    ) {
        draw_minimap(
            frame,
            mm,
            lay,
            state.lineage_scroll_x(),
            state.lineage_scroll_y(),
            content_w,
            content_h,
            chrome,
            t,
        );
    }
}

/// Stamp the lineage minimap into `mm` (the rect [`super::geom::minimap_rect`]
/// computed): a fixed-size occupancy sketch of the WHOLE grid in the top-right
/// of the lineage interior, with the current viewport drawn as a `mm_view`
/// rectangle. Pure frame-buffer writes (`buffer_mut`), never CharGrid — so the
/// layout goldens are untouched. Occupancy is a per-minimap-cell boolean OR
/// over `lay.rects` (order-independent ⇒ deterministic, no sort needed).
#[allow(clippy::too_many_arguments)]
fn draw_minimap(
    frame: &mut Frame,
    mm: Rect,
    lay: &crate::Layout,
    scroll_x: usize,
    scroll_y: usize,
    content_w: usize,
    content_h: usize,
    chrome: &super::Chrome,
    t: &theme::Theme,
) {
    let grid_w = lay.grid.width();
    let grid_h = lay.grid.height();
    let mm_w = mm.width;
    let mm_h = mm.height;
    if grid_w == 0 || grid_h == 0 || mm_w == 0 || mm_h == 0 {
        return;
    }
    let x0 = mm.x;
    let y0 = mm.y;

    // Grid cells per minimap cell (ceil, so the whole grid maps inside the inset).
    let sx = grid_w.div_ceil(mm_w as usize).max(1);
    let sy = grid_h.div_ceil(mm_h as usize).max(1);

    let node_style = Style::default().fg(t.text_faint);
    let view_style = Style::default().fg(t.gold).add_modifier(Modifier::BOLD);

    // 1. Occupancy: each minimap cell is `#` if ANY node rect overlaps the grid
    //    region it covers, else `.`. (Stamp `.` too, so the inset reads as a
    //    distinct sketch rather than leaking the underlying diagram cells.)
    let buf = frame.buffer_mut();
    for my in 0..mm_h {
        for mx in 0..mm_w {
            let gx0 = mx as usize * sx;
            let gx1 = gx0 + sx; // exclusive
            let gy0 = my as usize * sy;
            let gy1 = gy0 + sy; // exclusive
            let occupied = lay
                .rects
                .values()
                .any(|r| r.x < gx1 && gx0 < r.x + r.width && r.y < gy1 && gy0 < r.y + r.height);
            let sym = if occupied {
                chrome.mm_node
            } else {
                chrome.mm_empty
            };
            if let Some(cell) = buf.cell_mut((x0 + mx, y0 + my)) {
                cell.set_symbol(sym).set_style(node_style);
            }
        }
    }

    // 2. Viewport rectangle: map (scroll, content size) → minimap coords, clamp
    //    into the inset, force >=1x1 so a degenerate viewport still draws a `+`,
    //    then stamp the full perimeter.
    let map_x = |gx: usize| ((gx / sx).min(mm_w as usize - 1)) as u16;
    let map_y = |gy: usize| ((gy / sy).min(mm_h as usize - 1)) as u16;
    let vx0 = map_x(scroll_x);
    let vy0 = map_y(scroll_y);
    // The viewport spans [scroll, scroll+content); its last shown cell is
    // scroll+content-1 (saturating for a zero-size content guard).
    let vx1 = map_x(scroll_x + content_w.saturating_sub(1)).max(vx0);
    let vy1 = map_y(scroll_y + content_h.saturating_sub(1)).max(vy0);
    for vy in vy0..=vy1 {
        for vx in vx0..=vx1 {
            // Perimeter only: corners + edges of the viewport rect.
            if vx == vx0 || vx == vx1 || vy == vy0 || vy == vy1 {
                if let Some(cell) = buf.cell_mut((x0 + vx, y0 + vy)) {
                    cell.set_symbol(chrome.mm_view).set_style(view_style);
                }
            }
        }
    }
}

/// The grid cells the accent overlay paints: every uid's box
/// PERIMETER (top/bottom rows + left/right columns — the frame, not the label
/// interior), and for the hovered node additionally the connector cells of
/// its DIRECT edges. `edge_cells` keys an edge by `(parent, child)`, so a
/// node's incident runs are those keyed with the uid at either end. Pure over
/// `lay`'s maps (grid coordinates, deduped via `BTreeSet`), so the set is
/// deterministic and a unit test asserts the exact cells. A `+M more` marker
/// uid works too — its rect exists in `lay.rects` like any node's.
fn accent_cells(
    lay: &crate::Layout,
    hover_node: Option<&str>,
    search_matches: &[String],
) -> BTreeSet<(usize, usize)> {
    let mut cells = BTreeSet::new();
    let frame = |cells: &mut BTreeSet<_>, uid: &str| {
        let Some(r) = lay.rects.get(uid) else {
            return; // off-screen or hidden behind a collapse marker
        };
        if r.height == 1 {
            // A compact `│name│` box: the frame is the two border cells —
            // the name interior keeps its own (label) styling.
            cells.insert((r.x, r.y));
            cells.insert((r.x + r.width.saturating_sub(1), r.y));
            return;
        }
        for dx in 0..r.width {
            cells.insert((r.x + dx, r.y));
            cells.insert((r.x + dx, r.y + r.height - 1));
        }
        for dy in 0..r.height {
            cells.insert((r.x, r.y + dy));
            cells.insert((r.x + r.width.saturating_sub(1), r.y + dy));
        }
    };
    for uid in search_matches {
        frame(&mut cells, uid);
    }
    if let Some(h) = hover_node {
        frame(&mut cells, h);
        for ((parent, child), run) in &lay.edge_cells {
            if parent == h || child == h {
                cells.extend(run.iter().copied());
            }
        }
    }
    cells
}

/// Paint the accent overlay for [`accent_cells`]: fg → `t.accent` + BOLD on
/// each frame-buffer cell the grid->screen window maps (scrolled-out cells
/// drop). Style-only — `bg` and existing modifiers are preserved via the
/// cell's own style (an on-path band or the REVERSED selection cue survive
/// the accent), and no glyph ever moves, so this is safe to run on top of the
/// blit without perturbing the frozen renders.
#[allow(clippy::too_many_arguments)]
fn stamp_accent_cells(
    frame: &mut Frame,
    target: Rect,
    lay: &crate::Layout,
    scroll_x: usize,
    scroll_y: usize,
    hover_node: Option<&str>,
    search_matches: &[String],
    t: &theme::Theme,
) {
    if hover_node.is_none() && search_matches.is_empty() {
        return;
    }
    let buf = frame.buffer_mut();
    for (gx, gy) in accent_cells(lay, hover_node, search_matches) {
        let (Some(sx), Some(sy)) = (gx.checked_sub(scroll_x), gy.checked_sub(scroll_y)) else {
            continue; // scrolled off the left/top
        };
        if sx >= target.width as usize || sy >= target.height as usize {
            continue; // scrolled off the right/bottom
        }
        let (x, y) = (target.x + sx as u16, target.y + sy as u16);
        if let Some(cell) = buf.cell_mut((x, y)) {
            let style = cell.style().fg(t.accent).add_modifier(Modifier::BOLD);
            cell.set_style(style);
        }
    }
}

/// Stamp the layer-band labels onto the lineage pane's BOTTOM border: for each
/// band, the visible part of its column's `[x, x+width)` grid range is mapped
/// through the SAME scroll/content mapping the blit used, and the label is
/// written there in the band's layer colour (clipped to the visible segment).
/// Border-row chrome like the scroll markers — pure frame-buffer writes, never
/// CharGrid, so the layout goldens are untouched. Labels are dbt dir names
/// (user data), so ascii_guard treats them like any other data text.
#[allow(clippy::too_many_arguments)]
fn stamp_layer_bands(
    frame: &mut Frame,
    area: Rect,
    target: Rect,
    scroll_x: usize,
    content_w: usize,
    bands: &[LayerBand],
    t: &theme::Theme,
) {
    if area.height < 2 {
        return;
    }
    let bottom = area.y + area.height - 1;
    let buf = frame.buffer_mut();
    for band in bands {
        // Visible slice of the column in grid coords.
        let seg0 = band.x.max(scroll_x);
        let seg1 = (band.x + band.width).min(scroll_x + content_w);
        if seg0 >= seg1 {
            continue; // column fully scrolled out
        }
        let style = Style::default().fg(lens_color(band.tint, t).unwrap_or(t.text_dim));
        let avail = seg1 - seg0;
        for (i, ch) in band.label.chars().take(avail).enumerate() {
            let x = target.x + (seg0 - scroll_x + i) as u16;
            if x >= target.x + target.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, bottom)) {
                cell.set_symbol(&ch.to_string()).set_style(style);
            }
        }
    }
}

/// Stamp scroll-affordance markers (the chrome's left/right/up/down glyphs) onto
/// the lineage pane's border (the bordered `area`, not the interior), one per
/// clipped edge at that edge's midpoint. Drawn after the content so they sit on
/// the frame, coloured as chrome (yellow) so a reader distinguishes them from
/// the connector arrowheads inside the diagram.
///
/// The top edge is special: it carries the (left-aligned) pane title, so a
/// centred up-marker would stab a glyph into the title text — itself reading as
/// breakage. It is therefore nudged to the title-free right end of the top
/// border. The bottom edge mirrors it: the Layer lens's layer-band labels live
/// at column positions along the bottom border, and a centred down-marker would
/// stab whichever label crosses the midpoint. The left/right edges keep their
/// natural midpoints. `draw_lineage_pane` returns early when the interior is
/// 0-wide, so `right - 1 >= area.x + 1` here (never the left border).
fn stamp_scroll_markers(
    frame: &mut Frame,
    area: Rect,
    edges: super::ClipEdges,
    chrome: &super::Chrome,
    t: &theme::Theme,
) {
    if !edges.any() || area.width < 2 || area.height < 2 {
        return;
    }
    let style = Style::default().fg(t.warn).add_modifier(Modifier::BOLD);
    let right = area.x + area.width - 1;
    let bottom = area.y + area.height - 1;
    let mid_y = area.y + area.height / 2;
    let buf = frame.buffer_mut();
    let mut mark = |x: u16, y: u16, sym: &str| {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(sym).set_style(style);
        }
    };
    if edges.left {
        mark(area.x, mid_y, chrome.left);
    }
    if edges.right {
        mark(right, mid_y, chrome.right);
    }
    if edges.top {
        mark(right.saturating_sub(1), area.y, chrome.up);
    }
    if edges.bottom {
        mark(right.saturating_sub(1), bottom, chrome.down);
    }
}

/// A span for a run of cells sharing `(emphasis, attr)`: emphasized runs get the
/// selected style; otherwise colour by materialization class.
fn styled_run(
    run: &str,
    key: (bool, CellAttr),
    emph_style: Style,
    t: &theme::Theme,
) -> Span<'static> {
    let (emph, attr) = key;
    let style = if emph {
        emph_style
    } else {
        attr_style(attr, t)
    };
    Span::styled(run.to_string(), style)
}

/// The render style for a non-selected cell from its attributes, resolving the
/// shared FOREGROUND channel by a fixed precedence and applying the `on_path`
/// background ORTHOGONALLY on top. Deliberately NO per-cell modifiers — an
/// underline across the box's border glyphs reads as doubled lines; the path band
/// is a background COLOUR, never a Modifier, so it cannot double a glyph and stays
/// ASCII-safe (it adds no glyph at all).
///
/// Foreground precedence (emphasis is handled by the CALLER, before this, and
/// fully wins): `dimmed` (focus dim, [`theme::LINEAGE_DIM`] — the cursor is
/// off-root and this node is off-path; a readable muted gray, NOT the darker
/// `TEXT_FAINT`, so off-path context stays legible) > the lens `tint` (the
/// user-selected lens, e.g. coverage
/// Warn) > the materialization class colour. The `on_path` background is
/// independent of all of that, so a dimmed-on-path or tinted-on-path node still
/// shows the path band.
fn attr_style(attr: CellAttr, t: &theme::Theme) -> Style {
    let mut style = Style::default();
    if attr.dimmed {
        // Focus dim wins the fg so the on-path nodes stand out (lens-independent).
        style = style.fg(t.lineage_dim);
    } else if let Some(color) = lens_color(attr.lens, t) {
        // The active lens's tint wins over the materialization fg (opt-in lens).
        style = style.fg(color);
    } else if let Some(color) = class_color(attr.class, t) {
        style = style.fg(color);
    }
    // Path highlight is an ORTHOGONAL raised-surface background band (a colour,
    // not a Modifier), so it composes with whichever foreground won above.
    if attr.on_path {
        style = style.bg(t.surface_hi);
    }
    style
}

/// Colour for each materialization class (`Plain` = no colour, i.e. connectors).
fn class_color(class: MaterializationClass, t: &theme::Theme) -> Option<Color> {
    match class {
        MaterializationClass::Table => Some(t.class_table),
        MaterializationClass::View => Some(t.class_view),
        MaterializationClass::Incremental => Some(t.class_incremental),
        MaterializationClass::Ephemeral => Some(t.class_ephemeral),
        MaterializationClass::Source => Some(t.class_source),
        MaterializationClass::Seed => Some(t.class_seed),
        MaterializationClass::Snapshot => Some(t.class_snapshot),
        MaterializationClass::Exposure => Some(t.class_exposure),
        MaterializationClass::OtherModel => Some(t.class_other),
        // The `+M more` collapse marker renders in the muted "other" gray —
        // it is display chrome standing in for siblings, not a real resource.
        MaterializationClass::Collapsed => Some(t.class_other),
        MaterializationClass::Plain => None,
    }
}

/// Colour for each lineage-lens [`LensTint`] (`None` = no lens fg, so the class
/// colour shows through). Coverage `Warn` and `Violation` reuse the danger red;
/// the heat ramp is low → mid → high; each layer gets a distinct colour. All
/// from the active [`Theme`](theme::Theme), so a retune lands in one place.
fn lens_color(tint: LensTint, t: &theme::Theme) -> Option<Color> {
    match tint {
        LensTint::None => None,
        LensTint::Warn | LensTint::Violation => Some(t.danger),
        LensTint::HeatLow => Some(t.heat_low),
        LensTint::HeatMid => Some(t.heat_mid),
        LensTint::HeatHigh => Some(t.heat_high),
        LensTint::LayerStaging => Some(t.layer_staging),
        LensTint::LayerIntermediate => Some(t.layer_intermediate),
        LensTint::LayerMarts => Some(t.layer_marts),
        LensTint::LayerUtilities => Some(t.layer_utilities),
        LensTint::LayerOther => Some(t.layer_other),
        LensTint::DiffAdd => Some(t.diff_add),
        LensTint::DiffMod => Some(t.diff_mod),
        LensTint::RunWarn => Some(t.warn),
        LensTint::RunSkip => Some(t.text_faint),
        LensTint::ColumnImpact => Some(t.gold),
    }
}

/// Append `piece` to `text` (advancing `cell` by its display width) and,
/// when `kind` is `Some` and the piece is non-empty, record a [`TitleSpan`]
/// over the piece's cell range — the `""` lens-off tag and `None` view
/// suffix leave no phantom hit target.
fn put_title(
    text: &mut String,
    cell: &mut u16,
    spans: &mut Vec<TitleSpan>,
    piece: &str,
    kind: Option<TitleKind>,
) {
    use unicode_width::UnicodeWidthStr;
    let w = UnicodeWidthStr::width(piece) as u16;
    if let Some(kind) = kind.filter(|_| w > 0) {
        spans.push(TitleSpan {
            kind,
            x0: *cell,
            x1: *cell + w,
        });
    }
    text.push_str(piece);
    *cell += w;
}

/// Compose the lineage pane title as a [`TitleLayout`]: the drawn text plus
/// the cell ranges of its clickable parts. THE single composer — `draw`
/// renders `text` verbatim and `hit_target` maps title-row cells through
/// `spans`, so a drawn part can never disagree with its
/// hit range. Byte-compatible with the four shapes the old
/// `compose_lineage_title` produced.
///
/// Four shapes:
/// - **Search**: `" /{query}{caret} {cur}/{total} "` — the
///   lineage-search form: the query + caret + the cycled match position,
///   which OWNS the title while the search is open (the `Lineage: {sel}`
///   body and any breadcrumb/lens suffixes drop). `(0, 0)` while the query
///   matches nothing, `1/total`-based cycling otherwise.
/// - **No breadcrumb**: `" Lineage: {sel} [{v}]{lens} "` / `" Lineage: {sel}{lens} "`
///   — BYTE-IDENTICAL to the pre-breadcrumb titles (the default render is pinned).
/// - **Breadcrumb present**: the `Lineage: {sel}` body is OMITTED (the crumb
///   already ends at the root, so printing the root again would duplicate it), but
///   the `[{v}]{lens}` suffixes are KEPT — they are the only textual cue for the
///   active view + lens. The crumb is then STRICTLY truncated (via
///   `fit_breadcrumb`, dropping oldest entries then a
///   `".."` prefix, finally to empty) so the suffixes ALWAYS survive even when
///   ratatui would otherwise clip the title's right side.
/// - **Overview**: `" {body}{lens} "` — the whole-graph overview title, which
///   takes priority over both the `Lineage: {sel}` body and the breadcrumb (the
///   view suffix is meaningless while it is on, since the direction/depth
///   toggles are gated).
///
/// Clickable parts: each breadcrumb entry — `TitleKind::Crumb(steps)`,
/// replaying `history_back` that many times — the ` [{v}]` view tag
/// ([`TitleKind::View`]), and the ` [lens:…]` tag ([`TitleKind::Lens`]). The
/// ` [col:name]` column-trace suffix stays inert (the `column:` status chip
/// is its clickable surface). The search shape carries no clickable chrome
/// (the `i/N` count is a readout; matches cycle by key).
///
/// `total_w` is the bordered pane width; the title sits between the two
/// corner glyphs, so the usable title width is `total_w - 2`.
#[allow(clippy::too_many_arguments)]
pub fn title_layout(
    sel_name: &str,
    search: Option<(&str, Option<(usize, usize)>)>,
    view_label: Option<&str>,
    breadcrumb: Option<&str>,
    overview: Option<&str>,
    lens: LineageLens,
    col_suffix: Option<&str>,
    caret: &str,
    total_w: u16,
) -> TitleLayout {
    use unicode_width::UnicodeWidthStr;

    // The lineage-search shape owns the title: `/` + query + caret +
    // the `{cur}/{total}` cycle position — the sel body, breadcrumb, and
    // lens suffixes all drop while a search is open. No clickable chrome:
    // the `i/N` count is a readout (matches cycle by key, not click).
    if let Some((query, pos)) = search {
        let count = pos
            .map(|(cur, total)| format!(" {cur}/{total}"))
            .unwrap_or_default();
        return TitleLayout {
            text: format!(" /{query}{caret}{count} "),
            spans: Vec::new(),
        };
    }

    // The `[lens:…]` tag + the inert `[col:name]` suffix share the lens slot
    // the old merged `lens` param carried (pure ASCII, ambiguous-width-safe).
    let lens_tag = lens_title_suffix(lens);
    let col = col_suffix.unwrap_or("");
    let mut text = String::new();
    let mut spans = Vec::new();
    let mut cell = 0u16;

    // Overview: the body replaces both the `Lineage: {name}` body and the
    // breadcrumb; the lens suffix stays clickable (the view suffix is
    // meaningless — the toggles are gated while the overview is on).
    if let Some(body) = overview {
        put_title(&mut text, &mut cell, &mut spans, " ", None);
        put_title(&mut text, &mut cell, &mut spans, body, None);
        put_title(
            &mut text,
            &mut cell,
            &mut spans,
            lens_tag,
            Some(TitleKind::Lens),
        );
        put_title(&mut text, &mut cell, &mut spans, col, None);
        put_title(&mut text, &mut cell, &mut spans, " ", None);
        return TitleLayout { text, spans };
    }

    // The ` [{v}]` view suffix (when a label is present) plus the lens/col
    // suffixes — each recorded at its own cells.
    let view = view_label.map(|v| format!(" [{v}]")).unwrap_or_default();

    let Some(crumb) = breadcrumb else {
        // No breadcrumb → byte-identical to the original default titles.
        put_title(
            &mut text,
            &mut cell,
            &mut spans,
            &format!(" Lineage: {sel_name}"),
            None,
        );
        put_title(
            &mut text,
            &mut cell,
            &mut spans,
            &view,
            Some(TitleKind::View),
        );
        put_title(
            &mut text,
            &mut cell,
            &mut spans,
            lens_tag,
            Some(TitleKind::Lens),
        );
        put_title(&mut text, &mut cell, &mut spans, col, None);
        put_title(&mut text, &mut cell, &mut spans, " ", None);
        return TitleLayout { text, spans };
    };

    // Breadcrumb present: omit the `Lineage: {sel}` body, keep the suffixes.
    // `fit_breadcrumb` strictly truncates the crumb into whatever the
    // suffixes leave of the usable title width, so they ALWAYS survive —
    // including their hit ranges.
    let suffix_w = UnicodeWidthStr::width(format!("{view}{lens_tag}{col}").as_str());
    let usable = (total_w.saturating_sub(2)) as usize;
    let framing = 2; // leading + trailing space
    let budget = usable.saturating_sub(suffix_w + framing);
    let fitted = crate::app::fit_breadcrumb(crumb, budget).unwrap_or_default();

    put_title(&mut text, &mut cell, &mut spans, " ", None);
    if !fitted.is_empty() {
        // Entries join on ` > `; each is a clickable hop back to that root —
        // `steps` = distance from the CURRENT root (the last entry). The
        // `..` elision marker and the current root itself stay inert (a
        // 0-step back is a no-op).
        let entries: Vec<&str> = fitted.split(" > ").collect();
        let n = entries.len();
        for (i, e) in entries.iter().enumerate() {
            if i > 0 {
                put_title(&mut text, &mut cell, &mut spans, " > ", None);
            }
            let steps = n - 1 - i;
            let kind = (steps > 0 && *e != "..").then_some(TitleKind::Crumb(steps));
            put_title(&mut text, &mut cell, &mut spans, e, kind);
        }
    }
    put_title(
        &mut text,
        &mut cell,
        &mut spans,
        &view,
        Some(TitleKind::View),
    );
    put_title(
        &mut text,
        &mut cell,
        &mut spans,
        lens_tag,
        Some(TitleKind::Lens),
    );
    put_title(&mut text, &mut cell, &mut spans, col, None);
    put_title(&mut text, &mut cell, &mut spans, " ", None);
    TitleLayout { text, spans }
}

/// Build the title `Line` from a [`TitleLayout`]: `text` verbatim, split at
/// the clickable ranges so the `hovered` part gains UNDERLINE — the
/// affordance cue for a clickable title part. Every other cell keeps the
/// pane-title style.
fn title_line(layout: &TitleLayout, base: Style, hovered: Option<TitleKind>) -> Line<'static> {
    use unicode_width::UnicodeWidthChar;
    if hovered.is_none() || layout.spans.is_empty() {
        return Line::styled(layout.text.clone(), base);
    }
    let under = base.add_modifier(Modifier::UNDERLINED);
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut run_hot = false;
    let mut cell = 0u16;
    for ch in layout.text.chars() {
        let hot = layout
            .spans
            .iter()
            .any(|s| Some(s.kind) == hovered && cell >= s.x0 && cell < s.x1);
        if hot != run_hot && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_hot { under } else { base },
            ));
        }
        run_hot = hot;
        run.push(ch);
        cell += UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hot { under } else { base }));
    }
    Line::from(spans)
}

/// A short ASCII suffix naming the active lens for the lineage pane title
/// (`""` when `Off`). Pure + ASCII-only so it is ambiguous-width-safe in both
/// glyph modes and a later breadcrumb feature can compose with it deterministically.
fn lens_title_suffix(lens: LineageLens) -> &'static str {
    match lens {
        LineageLens::Off => "",
        LineageLens::Coverage => " [lens:coverage]",
        LineageLens::DegreeHeat => " [lens:heat]",
        LineageLens::Layer => " [lens:layer]",
        LineageLens::LayerViolation => " [lens:violation]",
        LineageLens::Lint => " [lens:lint]",
        LineageLens::Diff => " [lens:diff]",
        LineageLens::RunStatus => " [lens:run]",
        LineageLens::RunTime => " [lens:time]",
        LineageLens::ColumnImpact => " [lens:col]",
        LineageLens::PrimaryKey => " [lens:pk]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default theme, as every non-theme test renders with.
    const T: &theme::Theme = &theme::DEFAULT;

    #[test]
    fn attr_style_lens_tint_wins_over_class_and_path_bg_is_orthogonal() {
        // The lens tint fg wins over the class fg (opt-in lens).
        let tinted = attr_style(
            CellAttr {
                class: MaterializationClass::Table,
                lens: LensTint::Warn,
                ..Default::default()
            },
            T,
        );
        assert_eq!(tinted.fg, Some(theme::DANGER), "Warn tint → danger fg");
        assert_eq!(tinted.bg, None, "no path band when off-path");

        // class fg when the lens is None.
        let plain_class = attr_style(
            CellAttr {
                class: MaterializationClass::Table,
                ..Default::default()
            },
            T,
        );
        assert_eq!(plain_class.fg, Some(theme::CLASS_TABLE), "Table → class fg");

        // on_path adds an orthogonal bg band and composes with the tint fg.
        let both = attr_style(
            CellAttr {
                class: MaterializationClass::Table,
                lens: LensTint::Warn,
                on_path: true,
                ..Default::default()
            },
            T,
        );
        assert_eq!(
            both.fg,
            Some(theme::DANGER),
            "fg unchanged by the path band"
        );
        assert_eq!(
            both.bg,
            Some(theme::SURFACE_HI),
            "on_path → raised-surface bg band"
        );

        // No per-cell modifier on lineage cells (border-glyph doubling guard).
        assert!(
            both.add_modifier.is_empty(),
            "lineage cells carry no modifier"
        );
    }

    #[test]
    fn attr_style_precedence_dim_beats_tint_beats_class() {
        // dimmed wins the fg over BOTH the lens tint and the class colour.
        let dim_over_tint = attr_style(
            CellAttr {
                class: MaterializationClass::Table, // CLASS_TABLE
                lens: LensTint::HeatHigh,           // HEAT_HIGH
                dimmed: true,
                ..Default::default()
            },
            T,
        );
        assert_eq!(
            dim_over_tint.fg,
            Some(theme::LINEAGE_DIM),
            "dimmed fg beats the lens tint AND the class colour"
        );

        // The lens tint beats the class colour (when not dimmed).
        let tint_over_class = attr_style(
            CellAttr {
                class: MaterializationClass::Table, // CLASS_TABLE
                lens: LensTint::HeatMid,            // HEAT_MID
                ..Default::default()
            },
            T,
        );
        assert_eq!(
            tint_over_class.fg,
            Some(theme::HEAT_MID),
            "tint beats class"
        );

        // The heat ramp distinct colours.
        assert_eq!(lens_color(LensTint::HeatLow, T), Some(theme::HEAT_LOW));
        assert_eq!(lens_color(LensTint::HeatMid, T), Some(theme::HEAT_MID));
        assert_eq!(lens_color(LensTint::HeatHigh, T), Some(theme::HEAT_HIGH));
        assert_ne!(theme::HEAT_LOW, theme::HEAT_MID, "ramp steps distinct");
        assert_ne!(theme::HEAT_MID, theme::HEAT_HIGH, "ramp steps distinct");

        // on_path bg is preserved even on a DIMMED cell (orthogonal channel).
        let dim_on_path = attr_style(
            CellAttr {
                class: MaterializationClass::View,
                dimmed: true,
                on_path: true,
                ..Default::default()
            },
            T,
        );
        assert_eq!(dim_on_path.fg, Some(theme::LINEAGE_DIM), "dim fg");
        assert_eq!(
            dim_on_path.bg,
            Some(theme::SURFACE_HI),
            "on_path bg survives on a dimmed cell"
        );
        assert!(dim_on_path.add_modifier.is_empty(), "no modifier");
    }

    #[test]
    fn each_layer_lens_tint_has_a_distinct_colour() {
        use std::collections::HashSet;
        for (name, preset) in theme::presets() {
            let colours: HashSet<_> = [
                LensTint::LayerStaging,
                LensTint::LayerIntermediate,
                LensTint::LayerMarts,
                LensTint::LayerUtilities,
                LensTint::LayerOther,
            ]
            .into_iter()
            .map(|tint| lens_color(tint, preset))
            .collect();
            assert_eq!(
                colours.len(),
                5,
                "preset '{name}': five layers → five distinct colours"
            );
        }
    }

    #[test]
    fn lens_tints_never_collide_with_class_colours() {
        // The pastel-class / vivid-lens contract (see the theme module doc):
        // every lens tint must differ from EVERY materialization-class colour,
        // otherwise an active lens renders some node identically to lens-off
        // and looks broken. Cross-LENS reuse is fine (one lens at a time).
        // Checked for EVERY built-in preset — through the render mapping
        // itself, so it also guards `class_color`/`lens_color` reading the
        // right roles (theme::lint covers the raw palette side).
        for (name, preset) in theme::presets() {
            let classes = [
                MaterializationClass::Table,
                MaterializationClass::View,
                MaterializationClass::Incremental,
                MaterializationClass::Ephemeral,
                MaterializationClass::Source,
                MaterializationClass::Seed,
                MaterializationClass::Snapshot,
                MaterializationClass::Exposure,
                MaterializationClass::OtherModel,
                MaterializationClass::Collapsed,
            ]
            .map(|c| class_color(c, preset).expect("every class has a colour"));
            // Plain is the only colourless class by design (connectors,
            // padding, unstyled nodes) — a lens tint can't collide with None.
            assert!(
                class_color(MaterializationClass::Plain, preset).is_none(),
                "preset '{name}': Plain must stay colourless"
            );
            let tints = [
                LensTint::Warn,
                LensTint::Violation,
                LensTint::HeatLow,
                LensTint::HeatMid,
                LensTint::HeatHigh,
                LensTint::LayerStaging,
                LensTint::LayerIntermediate,
                LensTint::LayerMarts,
                LensTint::LayerUtilities,
                LensTint::LayerOther,
                LensTint::DiffAdd,
                LensTint::DiffMod,
            ];
            for tint in tints {
                let colour = lens_color(tint, preset).expect("every non-None tint has a colour");
                assert!(
                    !classes.contains(&colour),
                    "preset '{name}': lens tint {tint:?} ({colour:?}) collides with a \
                     class colour — the lens would be invisible on that class"
                );
            }
        }
    }

    #[test]
    fn styled_run_emphasis_short_circuits_attr_style() {
        // Emphasis wins the WHOLE precedence: a cell that is emphasized renders the
        // selected style even when its attr also carries an active lens tint AND
        // the dim flag — `styled_run` short-circuits before `attr_style`. Guards the
        // "emphasis still wins" contract against a refactor folding emphasis into
        // attr_style.
        let emph = styled_run(
            "x",
            (
                true,
                CellAttr {
                    class: MaterializationClass::Table,
                    lens: LensTint::HeatHigh,
                    dimmed: true,
                    on_path: true,
                },
            ),
            selected_style(T),
            T,
        );
        assert_eq!(
            emph.style,
            selected_style(T),
            "an emphasized cell ignores its lens tint / dim / class"
        );
        // …and a non-emphasized cell with the same attr does NOT get the selected
        // style (it resolves through attr_style → dim wins the fg here).
        let plain = styled_run(
            "x",
            (
                false,
                CellAttr {
                    class: MaterializationClass::Table,
                    lens: LensTint::HeatHigh,
                    dimmed: true,
                    ..Default::default()
                },
            ),
            selected_style(T),
            T,
        );
        assert_ne!(
            plain.style,
            selected_style(T),
            "non-emph cell is not selected"
        );
        assert_eq!(
            plain.style.fg,
            Some(theme::LINEAGE_DIM),
            "dim fg via attr_style"
        );
    }

    #[test]
    fn lens_title_suffix_per_lens() {
        assert_eq!(lens_title_suffix(LineageLens::Off), "", "Off adds nothing");
        assert_eq!(lens_title_suffix(LineageLens::Coverage), " [lens:coverage]");
        assert_eq!(lens_title_suffix(LineageLens::DegreeHeat), " [lens:heat]");
        assert_eq!(lens_title_suffix(LineageLens::Layer), " [lens:layer]");
        assert_eq!(
            lens_title_suffix(LineageLens::LayerViolation),
            " [lens:violation]"
        );
        assert_eq!(lens_title_suffix(LineageLens::Lint), " [lens:lint]");
        assert_eq!(lens_title_suffix(LineageLens::Diff), " [lens:diff]");
        assert_eq!(lens_title_suffix(LineageLens::RunStatus), " [lens:run]");
        assert_eq!(lens_title_suffix(LineageLens::RunTime), " [lens:time]");
        assert_eq!(lens_title_suffix(LineageLens::ColumnImpact), " [lens:col]");
        assert_eq!(lens_title_suffix(LineageLens::PrimaryKey), " [lens:pk]");
    }

    #[test]
    fn title_layout_overview_wins_over_breadcrumb_and_view() {
        // The overview body replaces both the `Lineage: {sel}` body and the
        // breadcrumb; the lens suffix survives (the view suffix does not, since
        // it is meaningless while the overview is on).
        let title = title_layout(
            "x",
            None,
            Some("↑↓"),
            Some("a > b"),
            Some("Overview: 93 nodes"),
            LineageLens::DegreeHeat,
            None,
            "_",
            80,
        );
        assert_eq!(title.text, " Overview: 93 nodes [lens:heat] ");
        // The `[lens:…]` tag is the ONLY clickable span — covering exactly its
        // cell range inside the title text.
        assert_eq!(title.spans.len(), 1);
        let s = &title.spans[0];
        assert_eq!(s.kind, TitleKind::Lens);
        assert_eq!(
            &title
                .text
                .chars()
                .skip(s.x0 as usize)
                .take((s.x1 - s.x0) as usize)
                .collect::<String>(),
            " [lens:heat]"
        );
    }

    #[test]
    fn title_layout_breadcrumb_entries_click_back_n_steps() {
        // `a > b > root` — `a` is 2 hops back, `b` is 1, `root` (the current
        // root) stays inert. View/lens tags keep their own spans.
        let title = title_layout(
            "root",
            None,
            Some("↑≤2"),
            Some("a > b > root"),
            None,
            LineageLens::Coverage,
            None,
            "_",
            80,
        );
        assert_eq!(title.text, " a > b > root [↑≤2] [lens:coverage] ");
        let kinds: Vec<TitleKind> = title.spans.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TitleKind::Crumb(2),
                TitleKind::Crumb(1),
                TitleKind::View,
                TitleKind::Lens
            ]
        );
        // Each crumb span covers exactly its entry's cells.
        let piece = |s: &TitleSpan| {
            title
                .text
                .chars()
                .skip(s.x0 as usize)
                .take((s.x1 - s.x0) as usize)
                .collect::<String>()
        };
        assert_eq!(piece(&title.spans[0]), "a");
        assert_eq!(piece(&title.spans[1]), "b");
        assert_eq!(piece(&title.spans[2]), " [↑≤2]");
        assert_eq!(piece(&title.spans[3]), " [lens:coverage]");
    }

    #[test]
    fn title_layout_drops_elided_crumb_spans_but_keeps_steps() {
        // Strict-fit elision (`.. > d > root`): the `..` marker is inert, and
        // `d` keeps its real hop distance (1 step back to the direct parent).
        let title = title_layout(
            "root",
            None,
            None,
            Some("long_alpha > long_beta > long_gamma > long_root"),
            None,
            LineageLens::Off,
            None,
            "_",
            24, // narrow: only `.. > long_gamma > long_root` + framing fits
        );
        // Whatever survived, the last entry (current root) and `..` are inert.
        assert!(title.text.starts_with(' '), "leading frame space");
        for s in &title.spans {
            if let TitleKind::Crumb(steps) = s.kind {
                assert!(steps > 0, "the current root is never a crumb target");
                assert!(steps <= 3, "history shows at most 3 back entries");
            } else {
                panic!("unexpected span kind {:?}", s.kind);
            }
        }
    }

    #[test]
    fn title_layout_no_breadcrumb_keeps_default_text_and_tags() {
        let title = title_layout(
            "fct_orders",
            None,
            Some("↑≤2"),
            None,
            None,
            LineageLens::Off,
            None,
            "_",
            80,
        );
        assert_eq!(title.text, " Lineage: fct_orders [↑≤2] ");
        assert_eq!(title.spans.len(), 1);
        assert_eq!(title.spans[0].kind, TitleKind::View);
    }

    #[test]
    fn title_layout_search_shape_owns_the_title_and_stays_inert() {
        // The lineage-search form owns the title: `/` + query + caret +
        // `{cur}/{total}` — sel body, crumb, and suffixes all drop, and no
        // span is clickable (the count is a readout).
        let title = title_layout(
            "x",
            Some(("bud", Some((2, 7)))),
            Some("↑↓"),
            Some("a > b"),
            Some("Overview"),
            LineageLens::Coverage,
            Some(" [col:c]"),
            "_",
            80,
        );
        assert_eq!(title.text, " /bud_ 2/7 ");
        assert!(title.spans.is_empty(), "the search shape has no chrome");
        // No match position wired (or 0/0 while the query matches nothing):
        // the count cell is whatever the loop supplies.
        let title = title_layout(
            "x",
            Some(("bud", Some((0, 0)))),
            None,
            None,
            None,
            LineageLens::Off,
            None,
            "_",
            80,
        );
        assert_eq!(title.text, " /bud_ 0/0 ");
        let title = title_layout(
            "x",
            Some(("bud", None)),
            None,
            None,
            None,
            LineageLens::Off,
            None,
            "_",
            80,
        );
        assert_eq!(title.text, " /bud_ ");
    }
}
