//! The render entry point: [`RenderCtx`], the top-level [`draw`] orchestrator,
//! and the small base panes (list, status). The lineage pane lives in
//! `super::lineage` and overlays in `super::overlay`; `draw` composes them.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::coverage_gap;
use crate::model_list::{DisplayRow, ModelList};

use super::geom::pane_rects;
use super::{
    focus_border, selected_style, theme, title_style, Focus, LineageLens, UiState, SELECTED_MARKER,
    UNSELECTED_MARKER,
};

/// The optional, colored, droppable segments appended to the status bar AFTER its
/// protected core (`[j/k] move` hint + `sel:{name}`). Each field is `Option<&str>`:
/// absent ⇒ that segment is omitted, which is both graceful degradation on a
/// narrow terminal AND the plug-in seam other features fill (coverage / bookmarks
/// / sort). All borrows, so the struct stays a thin Dag-free bundle precomputed in
/// the event loop (like `status`/`lineage_label`).
#[derive(Debug, Clone, Default)]
pub struct StatusSegments<'a> {
    /// Blast radius of the selected node, e.g. `"impact ↓23 ↑5"` (ASCII `v`/`^`).
    /// Always-on (not user-toggled) and the highest-priority droppable segment, so
    /// it survives narrow widths longest — placed right after the protected core.
    pub impact: Option<&'a str>,
    /// Which pane has focus: `"list"` | `"lineage"`.
    pub focus: Option<&'a str>,
    /// The lineage view indicator (direction + depth), only when non-default.
    pub view: Option<&'a str>,
    /// Selected position within the active list, e.g. `"12/45"`.
    pub position: Option<&'a str>,
    /// Test-coverage summary when the lens is on, e.g. `"cov 64% (29/45)"`.
    pub coverage: Option<&'a str>,
    /// Bookmark count when the set is non-empty, e.g. `"bm:3"`.
    pub bookmarks: Option<&'a str>,
    /// Sort mode when it is not the default (Layer), e.g. `"sort:downstream"`.
    pub sort: Option<&'a str>,
    /// The persistent list filter when active, e.g. `"untested"` (bare — the
    /// segment renderer adds the `[..]`). Echoes the list-title tag so it
    /// stays visible while the list pane is hidden.
    pub filter: Option<&'a str>,
}

/// The Dag-free bundle of render inputs; future render features add a FIELD, not
/// a new parameter. It deliberately holds NO `Dag`: the headless `TestBackend`
/// renders must be constructible from list + state + lineage alone. Overlay data
/// (e.g. the structure modal) travels inside [`mode`](crate::action::Mode), cloned
/// from the Dag side maps when the overlay opens.
pub struct RenderCtx<'a> {
    pub list: &'a ModelList,
    pub state: &'a UiState,
    pub lineage: Option<&'a crate::Layout>,
    /// The interaction mode (drives which overlay, if any, is drawn on top).
    pub mode: &'a crate::action::Mode,
    /// A precomputed one-line summary for the selected node (materialization +
    /// tests count). Dag-free by design: the App computes it and the loop sets
    /// it here, so headless renders just pass `None`.
    pub status: Option<&'a str>,
    /// Project-level counts for the list title bar (`None` in headless renders).
    pub stats: Option<&'a crate::AppStats>,
    /// A short lineage-view indicator for the lineage pane title (direction +
    /// depth, e.g. `"↑↓"` / `"↑ ≤3"`). `None` in headless renders.
    pub lineage_label: Option<&'a str>,
    /// The lineage re-root breadcrumb trail (`"a > b > root"`, ASCII), prepended
    /// to the lineage pane title. `None` (the default) when the back-history is
    /// empty, so a fresh title is unchanged. Computed at the width-aware loop seam
    /// ([`App::breadcrumb`](crate::App::breadcrumb) is itself size-unaware).
    pub breadcrumb: Option<&'a str>,
    /// Which glyph repertoire the chrome (borders/markers/badges/caret) and the
    /// lineage grid were drawn with. Defaults to Unicode; the event loop sets
    /// the app's detected/forced mode.
    pub glyphs: crate::GlyphMode,
    /// Bookmarked model `unique_id`s, for the list-pane badge. App STATE (not a
    /// `Dag`), borrowed in by the loop; `None` in headless renders (no badges).
    pub bookmarks: Option<&'a std::collections::BTreeSet<String>>,
    /// The FULL model count, for the search title's `N/M` (N = filtered count,
    /// M = full count). `None` in headless renders ⇒ falls back to the filtered
    /// count (so `N/N`, never a panic).
    pub full_model_count: Option<usize>,
    /// The optional, colored, droppable status-bar segments. Defaults to all-`None`
    /// (so headless `RenderCtx::new` renders construct with no segments); the event
    /// loop fills them from App state.
    pub segments: StatusSegments<'a>,
    /// Whether to draw the lineage minimap inset. Defaults to `false`, so every
    /// headless lineage render (whose goldens never opt in) is untouched; the
    /// event loop sets it from `UiState::minimap_visible`.
    pub minimap: bool,
    /// The persistent list-filter tag (`"untested"` / `"marked"`, BARE — the
    /// title adds its own `[..]`), appended to the list title. `None` (the
    /// default) leaves every title byte-identical.
    pub filter_label: Option<&'a str>,
    /// Per-column layer annotations for the lineage pane's bottom border,
    /// present exactly while the Layer lens is active (the loop computes them
    /// via `App::layer_bands`). `None` (the default) draws nothing, so every
    /// non-Layer render is untouched. Dag-free: plain precomputed data.
    pub layer_bands: Option<&'a [super::LayerBand]>,
    /// The transient toast text (copy/bookmark/export/reload feedback), drawn
    /// as a small floating box at the TOP-RIGHT, above every pane and modal.
    /// The event loop owns its ~2.5s lifetime ([`App::take_notice`]
    /// (crate::App::take_notice) + a stamp); `None` (the default) draws
    /// nothing, so headless renders are untouched.
    pub toast: Option<&'a str>,
    /// The active colour [`Theme`](theme::Theme) every surface paints with.
    /// Defaults to [`theme::DEFAULT`] (so headless renders and the
    /// style-asserting tests keep the legacy palette); the event loop sets the
    /// App's active theme (`--theme` / Ctrl-t). A borrow, like every other
    /// field — themes are data, never a global.
    pub theme: &'a theme::Theme,
}

/// Default mode for [`RenderCtx::new`] (so tests don't supply one). A `static`
/// for a `'static` reference; `Mode::Selection` is a unit variant (const-init).
static DEFAULT_MODE: crate::action::Mode = crate::action::Mode::Selection;

impl<'a> RenderCtx<'a> {
    /// The common constructor: base UI inputs with `mode` defaulted to
    /// `Selection`. The event loop sets [`mode`](RenderCtx::mode) afterward.
    pub fn new(
        list: &'a ModelList,
        state: &'a UiState,
        lineage: Option<&'a crate::Layout>,
    ) -> Self {
        RenderCtx {
            list,
            state,
            lineage,
            mode: &DEFAULT_MODE,
            status: None,
            stats: None,
            lineage_label: None,
            breadcrumb: None,
            glyphs: crate::GlyphMode::default(),
            bookmarks: None,
            full_model_count: None,
            segments: StatusSegments::default(),
            minimap: false,
            filter_label: None,
            layer_bands: None,
            toast: None,
            theme: &theme::DEFAULT,
        }
    }
}

/// Render the whole UI into `frame` from a [`RenderCtx`].
///
/// Side-effect-free: only reads its inputs, so `TestBackend` renders are
/// reproducible. The lineage pane blits the layout's CharGrid at the state's
/// scroll offset, so the computed window equals the drawn window. Overlays (driven
/// by `ctx.mode`) layer on top — adding one is a new `Mode` variant + a new arm here.
pub fn draw(frame: &mut Frame, ctx: &RenderCtx) {
    let area = frame.area();
    let rects = pane_rects(area, ctx.state.list_visible());

    // A list-target search shows its query live in the list title (non-covering,
    // so the filtered results stay visible while typing).
    let list_query = match ctx.mode {
        crate::action::Mode::Search(s) if s.target == crate::action::SearchTarget::List => {
            Some(s.query.as_str())
        }
        _ => None,
    };
    // A lineage-target search shows its query in the lineage pane title.
    let lineage_query = match ctx.mode {
        crate::action::Mode::Search(s) if s.target == crate::action::SearchTarget::Lineage => {
            Some(s.query.as_str())
        }
        _ => None,
    };
    if ctx.state.list_visible() {
        draw_list(
            frame,
            rects.list,
            ctx.list,
            ctx.state,
            list_query,
            ctx.stats,
            ctx.glyphs,
            ctx.bookmarks,
            ctx.full_model_count,
            ctx.filter_label,
            ctx.theme,
        );
    }
    super::lineage::draw_lineage_pane(
        frame,
        rects.lineage,
        ctx.list,
        ctx.state,
        ctx.lineage,
        lineage_query,
        ctx.lineage_label,
        ctx.breadcrumb,
        ctx.glyphs,
        ctx.minimap,
        ctx.layer_bands,
        ctx.theme,
    );
    draw_status(
        frame,
        rects.status,
        ctx.list,
        ctx.state,
        ctx.status,
        &ctx.segments,
        ctx.theme,
    );

    match ctx.mode {
        crate::action::Mode::Help { scroll } => {
            super::overlay::draw_help(frame, area, *scroll, ctx.glyphs, ctx.theme)
        }
        crate::action::Mode::Detail(dv) => {
            super::overlay::draw_detail(frame, area, dv, ctx.glyphs, ctx.theme)
        }
        crate::action::Mode::Sql(sv) => {
            super::overlay::draw_sql(frame, area, sv, ctx.glyphs, ctx.theme)
        }
        crate::action::Mode::Stats(sv) => {
            super::overlay::draw_stats(frame, area, sv, ctx.glyphs, ctx.theme)
        }
        crate::action::Mode::Palette(p) => {
            super::overlay::draw_palette(frame, area, p, ctx.glyphs, ctx.theme)
        }
        crate::action::Mode::Selection | crate::action::Mode::Search(_) => {}
    }

    // The transient toast floats above EVERYTHING (panes and modals alike), so
    // a yank fired from inside the SQL modal is acknowledged too — drawn last.
    if let Some(text) = ctx.toast {
        super::overlay::draw_toast(frame, area, text, ctx.glyphs, ctx.theme);
    }
}

/// Draw the left model-list pane: a bordered block with interleaved group headers
/// and model rows, scrolled by `state.offset()` (display-row space), the selected
/// model row highlighted by both a marker and reversed style.
///
/// The visible window is exactly `list.rows[offset .. offset + inner_height]` —
/// the *same* row sequence `UiState::ensure_visible` measures against, which is
/// what guarantees the selected model's row is never clipped. No sticky-header
/// injection (it would consume a line the scroll math doesn't know about).
#[allow(clippy::too_many_arguments)]
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    list: &ModelList,
    state: &UiState,
    search: Option<&str>,
    stats: Option<&crate::AppStats>,
    glyphs: crate::GlyphMode,
    bookmarks: Option<&std::collections::BTreeSet<String>>,
    full_count: Option<usize>,
    filter_label: Option<&str>,
    t: &theme::Theme,
) {
    let chrome = super::chrome(glyphs);
    let mut title = match (search, stats) {
        // While searching, the title is the live query plus an `N/M` match count
        // (N = filtered/visible models, M = full count). The `Search: {query}`
        // prefix is load-bearing (a TestBackend assertion); only the count format
        // changed from ` (N) ` to ` N/M `.
        (Some(query), _) => {
            let total = full_count.unwrap_or_else(|| list.len());
            format!(" Search: {query}{} {}/{} ", chrome.caret, list.len(), total)
        }
        // Otherwise show the project + resource counts (the title-bar stats).
        (None, Some(s)) => format!(
            " {} - model:{} src:{} seed:{} snap:{} ",
            s.project, s.models, s.sources, s.seeds, s.snapshots
        ),
        (None, None) => format!(" Models ({}) ", list.len()),
    };
    // The persistent-filter tag rides at the END of whichever title is up
    // (search included — the filter still narrows underneath the query).
    if let Some(tag) = filter_label {
        title.push_str(&format!("[{tag}] "));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome.border)
        .border_style(focus_border(state, Focus::List, t))
        .title(Line::styled(title, title_style(state, Focus::List, t)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let inner_height = inner.height as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(inner_height);
    for row in list.rows.iter().skip(state.offset()).take(inner_height) {
        match row {
            DisplayRow::Header { layer, count } => {
                // Accented header: a per-layer color + rule glyphs + a display-only
                // capitalized label. The shared `header_style()` (used by overlay
                // section headers) is deliberately NOT recolored here — this is a
                // separate accent path applied only in the list pane. The lowercase
                // `layer` grouping key is never mutated (title_case is display-only).
                let rule = chrome.rule.repeat(2);
                let label = format!("{rule} {} ({count})", title_case(layer));
                let accent = Style::default()
                    .fg(layer_accent(layer, t))
                    .add_modifier(Modifier::BOLD);
                lines.push(Line::styled(label, accent));
            }
            DisplayRow::Model { model_index, name } => {
                let selected = *model_index == state.selected();
                let marker = if selected {
                    SELECTED_MARKER
                } else {
                    UNSELECTED_MARKER
                };
                let node = list.model_at(*model_index);
                let (up, down) = node.map_or((0, 0), |n| (n.direct_up, n.direct_down));
                // A model with no downstream is a leaf ("orphan") — flag it.
                let orphan = node.is_some_and(|n| n.resource_type == "model" && n.direct_down == 0);
                // Coverage lens (only the Coverage lens, not the others): a
                // testable node with zero tests gets a warning name. The lens wins
                // over the orphan red (the user opted into it), and selection still
                // wins over both.
                let gap = state.lens() == LineageLens::Coverage && node.is_some_and(coverage_gap);
                let name_style = if selected {
                    selected_style(t)
                } else if gap {
                    Style::default().fg(t.danger)
                } else if orphan {
                    Style::default().fg(t.orphan)
                } else {
                    Style::default()
                };
                // The model name: a single span normally, but char-by-char
                // (matched chars patched yellow+bold) while a non-empty list
                // search is active, so the highlight marks exactly the chars the
                // filter accepted. Style-only — the rendered TEXT is unchanged.
                let name_spans: Vec<Span> = match search {
                    Some(q) if !q.trim().is_empty() => {
                        let hits = crate::model_list::match_indices(name, q);
                        let hi = Style::default().fg(t.gold).add_modifier(Modifier::BOLD);
                        name.chars()
                            .enumerate()
                            .map(|(i, ch)| {
                                let st = if hits.contains(&i) {
                                    name_style.patch(hi)
                                } else {
                                    name_style
                                };
                                Span::styled(ch.to_string(), st)
                            })
                            .collect()
                    }
                    _ => vec![Span::styled(name.to_string(), name_style)],
                };
                let bookmarked =
                    bookmarks.is_some_and(|b| node.is_some_and(|n| b.contains(&n.unique_id)));
                let mut spans = vec![
                    Span::raw("  "),                              // indent under header
                    Span::styled(marker.to_string(), name_style), // selection marker
                ];
                spans.extend(name_spans);
                // Direct parents/children badges (`↑2 ↓3` / `^2 v3`).
                spans.push(Span::styled(
                    format!("  {}{up} {}{down}", chrome.badge_up, chrome.badge_down),
                    Style::default().fg(t.text_faint),
                ));
                // Under the Coverage lens, surface the metric the red tint is
                // judging: the per-model test count (pure ASCII — guard-safe).
                if state.lens() == LineageLens::Coverage {
                    if let Some(n) = node {
                        spans.push(Span::styled(
                            format!(" t:{}", n.test_count),
                            Style::default().fg(t.text_faint),
                        ));
                    }
                }
                if bookmarked {
                    spans.push(Span::styled(
                        format!(" {}", chrome.bookmark),
                        Style::default().fg(t.gold),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);

    // Right-border scrollbar thumb. DISPLAY-ROW space (row_count / offset /
    // inner.height) — using model count would reintroduce the two-coordinate
    // clip bug. Drawn only when the rows overflow the interior.
    super::stamp_scrollbar(
        frame,
        area,
        list.row_count(),
        state.offset(),
        inner.height as usize,
        glyphs,
        t,
    );
}

/// The accent color for a layer-header row (list pane only; a SEPARATE path from
/// the shared `header_style()`). Unknown layers fall back to the gray
/// `layer_other`.
fn layer_accent(layer: &str, t: &theme::Theme) -> Color {
    match layer {
        "staging" => t.layer_staging,
        "intermediate" => t.layer_intermediate,
        "marts" => t.layer_marts,
        "utilities" => t.layer_utilities,
        _ => t.layer_other,
    }
}

/// Display-only capitalization of a layer name (uppercase the first char). NEVER
/// mutate the grouping key — this is for the rendered label only.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Draw the bottom status/help line. The PROTECTED CORE — a compact `[j/k] move`
/// key hint (full keybindings live in the `?` help overlay), the `sel:{name}` echo
/// (the node NAME, not the full dotted `unique_id` — keeping the core short enough
/// that the colored segments survive an 80-col terminal), and an optional
/// precomputed `[note]` (materialization + tests) — is derived in-place from
/// `list`+`state`, rendered FIRST as a single contiguous span, and is never dropped
/// (so `status_help_line_present` and `scrolled_render_still_shows_status_and_borders`
/// always match `[j/k]` and the name). The colored [`StatusSegments`] are then
/// appended left-to-right ONLY while they fit `area.width` (measured with
/// `unicode_width`), so they degrade gracefully on a narrow terminal and never push
/// the core off-screen.
fn draw_status(
    frame: &mut Frame,
    area: Rect,
    list: &ModelList,
    state: &UiState,
    status: Option<&str>,
    segments: &StatusSegments,
    t: &theme::Theme,
) {
    use unicode_width::UnicodeWidthStr;

    let sel = list
        .model_at(state.selected())
        .map(|n| n.name.as_str())
        .unwrap_or("-");
    let note = status.map(|s| format!("  [{s}]")).unwrap_or_default();
    // `[?] help` stays in the protected core: it is the key that reveals every
    // other binding, so first-run discoverability must survive narrow widths.
    // `sel:` echoes the NAME (dbt model names are project-unique), not the
    // dotted unique_id — that is what keeps the core short enough for the
    // colored segments to still fit on an 80-col terminal.
    //
    // The core is split into styled spans (hints dim, the name bright) but its
    // concatenated TEXT is byte-identical to the single-span original, so the
    // contains-based assertions keep matching.
    let hints = "[j/k] move  [?] help  sel: ";
    let mut used = hints.width() + sel.width() + note.width();
    let dim = Style::default().fg(t.text_dim);
    let mut spans: Vec<Span> = vec![
        Span::styled(hints, dim),
        Span::styled(
            sel.to_string(),
            Style::default()
                .fg(t.text_bright)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(note, dim),
    ];

    // (text, fg) in append/drop priority order (right-first drop, since a segment
    // that doesn't fit stops the loop — so index 0 survives the narrowest line).
    // Each rendered as a bracketed ` [txt]`. Priority:
    //   1. impact — always-on blast radius, the headline readout, right after the
    //      protected core so it survives narrow widths longest.
    //   2. the user-ACTIVATED states (coverage lens %, bookmarks, sort —
    //      information shown nowhere else).
    //   3. position.
    //   4. focus / view LAST (focus is already the accent pane border, and the
    //      view label is echoed in the lineage pane title — redundant cues that
    //      should drop first on a narrow terminal).
    let candidates: [(Option<&str>, Color); 8] = [
        (segments.impact, t.danger),
        (segments.coverage, t.warn),
        (segments.bookmarks, t.gold),
        (segments.filter, t.accent),
        (segments.sort, t.accent),
        (segments.position, t.text_faint),
        (segments.focus, t.ok),
        (segments.view, t.chip_view),
    ];
    let cap = area.width as usize;
    for (txt, color) in candidates.into_iter() {
        let Some(txt) = txt else { continue };
        let seg = format!(" [{txt}]");
        let w = seg.width();
        if used + w > cap {
            break; // first segment that doesn't fit: stop (drops the rest).
        }
        used += w;
        spans.push(Span::styled(seg, Style::default().fg(color)));
    }

    // The paragraph style paints the SURFACE band across the whole status row
    // (spans inherit the bg), turning the line into a footer bar.
    let paragraph =
        Paragraph::new(Line::from(spans)).style(Style::default().fg(t.text_dim).bg(t.surface));
    frame.render_widget(paragraph, area);
}
