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

use super::geom::pane_layout;
use super::{
    focus_border, selected_style, theme, title_style, Focus, HitTarget, LineageLens, Splitter,
    TabBarLayout, UiState, SELECTED_MARKER, UNSELECTED_MARKER,
};

/// The precomputed tab-strip view the event loop hands [`draw`] — labels,
/// the resolved cell layout, the active index, the project name, and the
/// hovered target. Everything Dag-aware (tab labels, strip geometry inputs)
/// is already resolved, so the painter stays Dag-free like the rest of
/// [`RenderCtx`].
pub struct TabBarView<'a> {
    /// Per-tab label text (`{n} {name}` — the number only on positions 1-9),
    /// the loop's `App::tab_labels` product.
    pub labels: &'a [String],
    /// The resolved cell layout for the bar's rect — the SAME
    /// [`tab_bar_layout`](super::tab_bar_layout) result the hit-test reuses.
    pub layout: &'a TabBarLayout,
    /// The active tab's index into `labels`.
    pub active: usize,
    /// The strip's right-edge project name (`None` = the zone didn't fit).
    pub project: Option<&'a str>,
    /// The strip cell the pointer currently hovers — the `×`-emphasis /
    /// `danger` / `+` accent cues. `None` elsewhere (or un-hovered).
    pub hover: Option<&'a HitTarget>,
}

/// The optional, colored, droppable chips rendered on the status bar's RIGHT
/// side (screen-spec §8). Each field is `Option<&str>` (the BARE chip text —
/// the chip renderer adds the ` [..]`): absent ⇒ that chip is omitted, which is
/// both graceful degradation on a narrow terminal AND the plug-in seam other
/// features fill (coverage / bookmarks / sort). All borrows, so the struct
/// stays a thin Dag-free bundle precomputed in the event loop (like
/// `lineage_label`). The LEFT side's contextual hints travel separately in
/// [`RenderCtx::hints`] — Issue 07 moved them out of this struct.
#[derive(Debug, Clone, Default)]
pub struct StatusSegments<'a> {
    /// Blast radius of the selected node, e.g. `"impact ↓23 ↑5"` (ASCII `v`/`^`).
    /// Always-on (not user-toggled) and the highest-priority droppable chip, so
    /// it survives narrow widths longest (§8's `impact` rule).
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
    /// Baseline-diff counts when a `--diff` baseline is loaded, e.g.
    /// `"diff +2 ~1 -3"` (or `"diff clean"`). Always-on with a baseline — the
    /// chip is the at-a-glance answer `--diff` exists to give.
    pub diff: Option<&'a str>,
    /// The run_results readout (`run 12/34 @ 2024-01-02` — coverage + freshness
    /// of the loaded artifact). `None` when no run_results is loaded.
    pub run: Option<&'a str>,
    /// The package scope of the loaded graph, `"scope:root"` (the default —
    /// installed-package nodes are absent) or `"scope:+pkgs"` under
    /// `--include-packages`. Emitted on every frame (the event loop always
    /// sets it) because the scope is a statement about what the numbers
    /// describe — but it is still a droppable segment, shed on narrow widths
    /// like any other.
    pub scope: Option<&'a str>,
    /// The persistent list filter when active, e.g. `"untested"` (bare — the
    /// segment renderer adds the `[..]`). Echoes the list-title tag so it
    /// stays visible while the list pane is hidden.
    pub filter: Option<&'a str>,
    /// The active column-focus readout (Issue 09), e.g.
    /// `"column:fct_orders.total_amount ↑3 ↓7"` — the traced column's identity
    /// plus its transitive column-closure sizes. Sits right after `notify`:
    /// while a column is being traced it IS the headline readout.
    pub column: Option<&'a str>,
    /// The unread-warning count (Issue 08), e.g. `"!3"` — the notification
    /// centre's badge. Sits right after `impact` (before `column`): pending
    /// warnings outrank every other droppable readout. `None` when nothing
    /// is unread — the chip is absent, not `!0`.
    pub notify: Option<&'a str>,
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
    /// The contextual status-line hints (screen-spec §8 — the LEFT side),
    /// generated from `BINDINGS` via
    /// [`status_hints`](crate::action::status_hints) and arriving
    /// priority-sorted. The event loop may REPLACE the list with a transient
    /// readout (splitter hover/drag: `action: None`). Empty (the default)
    /// draws no hints.
    pub hints: &'a [crate::action::StatusHint],
    /// The screen cell the pointer currently hovers, resolved through the
    /// SAME `hit_target` a click would use — the §6 hover cues (title-part
    /// underline, status hint/chip emphasis) render from it. `None` (the
    /// default) draws every cue off.
    pub hover: Option<&'a HitTarget>,
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
    /// The whole-graph overview title body (`"Overview: N nodes"`), replacing
    /// the `Lineage: {name}` body (and the breadcrumb/view suffix) while the
    /// overview is on. `None` (the default) leaves every title untouched.
    /// Precomputed at the loop seam (`App::overview_title`) — Dag-free here.
    pub overview_title: Option<&'a str>,
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
    /// The splitter currently hovered or being dragged — its border columns
    /// draw in `theme.accent` (screen-spec §5 "境界列をアクセント色に"). `None`
    /// normally; the loop fills it from the hit-test/`DragState`.
    pub splitter_highlight: Option<Splitter>,
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
    /// The tab strip (Issue 03): `Some` draws the one-row bar at the top and
    /// claims its row in `pane_layout`; `None` (the default — every headless
    /// render) leaves the row unclaimed, so pre-tab frames are byte-identical.
    /// Dag-free: labels + cell geometry arrive precomputed.
    pub tab_bar: Option<TabBarView<'a>>,
    /// The column-focus title suffix (`" [col:name]"`, pure ASCII), appended
    /// to the lineage pane title after the lens tag while a column is being
    /// traced (Issue 09). `None` (the default) leaves every title unchanged.
    /// Precomputed at the loop seam (`App::column_focus_label`) — Dag-free.
    pub col_suffix: Option<&'a str>,
    /// The lineage node the pointer currently hovers (Issue 05's accent cue:
    /// its box frame + direct-edge cells paint `accent`). `None` (the
    /// default) while the pointer is elsewhere or a button is held — a
    /// style-only overlay, so it never reaches the layout cache keys.
    pub hover_node: Option<&'a str>,
    /// Every node matching the open lineage search (Issue 05, §9 — each
    /// match's box frame accents, not just the cycled current). The loop
    /// fills it from `App::lineage_matches` while `Mode::Search(Lineage)` is
    /// up; the default empty slice accents nothing.
    pub search_matches: &'a [String],
    /// The lineage-search title's `{cur}/{total}` position (`1`-based cur
    /// over the match count — `match_idx % len + 1`; `(0, 0)` while the
    /// query matches nothing). `None` in headless renders — the title then
    /// shows the query + caret alone.
    pub search_pos: Option<(usize, usize)>,
    /// The settled-hover tooltip (Issue 05's §9 pointer summary — the loop
    /// arms it after 500 ms of stationary node hover, regardless of the
    /// inspector). A frame-buffer overlay like the toasts: `None` (the
    /// default) draws nothing.
    pub tooltip: Option<&'a super::overlay::TooltipView>,
    /// The visible toast stack (Issue 08): up to 3 [`ToastLine`]s, newest
    /// FIRST (drawn top-most at the upper-right, above every pane and modal).
    /// The event loop owns their per-level lifetimes — info ~2.5s, warning
    /// ~5s — stamping each as it drains [`App::take_notice_leveled`]
    /// (crate::App::take_notice_leveled); an empty slice (the default) draws
    /// nothing, so headless renders are untouched.
    pub toasts: &'a [ToastLine<'a>],
    /// The command palette's candidate rows (commands, `Recent`/`Commands`
    /// section headers, `@`-search model results, availability marks) —
    /// Dag-aware data precomputed at the loop seam via
    /// [`App::palette_rows`](crate::App::palette_rows) so the modal stays
    /// Dag-free. `None` falls back to plain `palette_candidates` rows
    /// (headless renders that never computed them still draw).
    pub palette_rows: Option<&'a [crate::PaletteRow]>,
    /// The one-line reason stamped on the list pane's interior when the
    /// active list is empty (`no models in scope — …` / `no match — …` /
    /// the persistent-filter reason) — Dag-aware, computed at the loop seam
    /// via [`App::list_empty_reason`](crate::App::list_empty_reason). It lands
    /// BELOW any surviving display rows (an all-folded pane still draws its
    /// group headers), else on the first interior row. `None` in headless
    /// renders (or whenever the list has models).
    pub empty_reason: Option<&'a str>,
    /// The folded group keys (`App::folded_groups`) — the group headers'
    /// `▾`/`▸` marker (§10). `None` (the default) treats every group as open,
    /// so headless renders without folds are untouched.
    pub folded_groups: Option<&'a std::collections::BTreeSet<String>>,
    /// The list-pane hit target the pointer hovers (`HitTarget::ListRow` /
    /// `ListHeader` — §6's hover tint: the row paints `theme.surface`). `None`
    /// (the default) draws no tint. The loop fills it from the same
    /// [`hit_target`](super::geom::hit_target) the click path uses — the
    /// tinted row is exactly the row a click would hit.
    pub list_hover: Option<&'a HitTarget>,
    /// The active colour [`Theme`](theme::Theme) every surface paints with.
    /// Defaults to [`theme::DEFAULT`] (so headless renders and the
    /// style-asserting tests keep the legacy palette); the event loop sets the
    /// App's active theme (`--theme` / Ctrl-t). A borrow, like every other
    /// field — themes are data, never a global.
    pub theme: &'a theme::Theme,
    /// The inspector pane's snapshot (Issue 04): the cached
    /// [`InspectorView`](crate::InspectorView) for the lineage cursor's node,
    /// `as_deref()`'d from `App::inspector_view` at the loop seam. `None` in
    /// headless renders or when the Dag is empty — the pane then draws
    /// nothing (its rect may not even be allocated). Dag-free, like every
    /// other field.
    pub inspector: Option<&'a crate::InspectorView>,
}

/// One visible toast in the top-right stack (Issue 08): the severity drives
/// the border colour (info → accent, warning → warn) and, on the loop side,
/// the on-screen TTL. `toasts[0]` is the newest — drawn on top.
#[derive(Debug, Clone, Copy)]
pub struct ToastLine<'a> {
    pub level: crate::action::NoticeLevel,
    pub text: &'a str,
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
            hints: &[],
            hover: None,
            stats: None,
            lineage_label: None,
            breadcrumb: None,
            overview_title: None,
            glyphs: crate::GlyphMode::default(),
            bookmarks: None,
            full_model_count: None,
            segments: StatusSegments::default(),
            splitter_highlight: None,
            minimap: false,
            filter_label: None,
            layer_bands: None,
            tab_bar: None,
            col_suffix: None,
            hover_node: None,
            search_matches: &[],
            search_pos: None,
            tooltip: None,
            toasts: &[],
            palette_rows: None,
            empty_reason: None,
            folded_groups: None,
            list_hover: None,
            theme: &theme::DEFAULT,
            inspector: None,
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
    // One shared geometry call: the same PaneConfig the hit-test/viewport math
    // resolve, so drawn rects == hit-tested rects == measured rects. The tab
    // bar's row is claimed iff the ctx carries one — a `None` bar keeps the
    // pre-tab layout every headless render asserts on.
    let mut pane_cfg = ctx.state.pane_config();
    pane_cfg.tab_bar = ctx.tab_bar.is_some();
    let rects = pane_layout(area, &pane_cfg);
    if let Some(tb) = &ctx.tab_bar {
        draw_tab_bar(frame, rects.tab_bar, tb, ctx.glyphs, ctx.theme);
    }

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
            ctx.empty_reason,
            ctx.folded_groups,
            ctx.list_hover,
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
        ctx.search_pos,
        ctx.lineage_label,
        ctx.breadcrumb,
        ctx.overview_title,
        ctx.glyphs,
        ctx.minimap,
        ctx.layer_bands,
        ctx.col_suffix,
        ctx.hover_node,
        ctx.search_matches,
        ctx.hover,
        ctx.theme,
    );
    // The right inspector pane (Issue 04): a published snapshot IS the
    // resolved visibility answer — the loop sets `ctx.inspector` only when
    // the pane is on screen, has a nonzero rect, and has a node to describe.
    if let Some(view) = ctx.inspector {
        super::inspector::draw_inspector_pane(frame, rects.inspector, ctx, view);
    }

    // Restamp the splitter's border columns with the accent while it's
    // hovered/dragged — cheap chrome-level feedback over both panes' drawn
    // borders (screen-spec §5). The status row and modals draw on top
    // regardless (the strip only spans body rows, so nothing overlaps).
    if let Some(which) = ctx.splitter_highlight {
        if let Some(strip) = rects.splitter(which) {
            for col in strip.x..strip.x + strip.width {
                for row in strip.y..strip.y + strip.height {
                    if let Some(cell) = frame.buffer_mut().cell_mut((col, row)) {
                        cell.set_fg(ctx.theme.accent);
                    }
                }
            }
        }
    }
    draw_status(frame, rects.status, ctx);

    // Overlays: a stacked mode (the `y` which-key / the node menu / help over
    // a modal) draws its origin's overlay UNDER itself first, then the whole
    // screen minus the top box is dimmed to `text_faint` (screen-spec §9),
    // then the top overlay paints. `overlay_rect` is the single rect
    // authority — the hit-test routes clicks by the same box. The tab strip
    // already drew above, so a covering overlay dims/covers it too (the
    // hit-test reports it `ModalExterior` under an overlay — same contract).
    let lin_inner = super::geom::pane_interior(rects.lineage);
    if let Some(under) = super::geom::underlay_mode(ctx.mode) {
        draw_overlay(frame, area, lin_inner, under, ctx);
    }
    if let Some(or) = super::geom::overlay_rect(
        ctx.mode,
        area,
        lin_inner,
        ctx.lineage,
        ctx.state.lineage_scroll_x(),
        ctx.state.lineage_scroll_y(),
    ) {
        super::overlay::dim_exterior(frame, or, ctx.theme);
        draw_overlay(frame, area, lin_inner, ctx.mode, ctx);
    }

    // The settled-hover tooltip floats over the panes (§9's pointer summary —
    // the event loop arms it only while a hover is settled, so a covering
    // overlay can never coexist with one). It layers under the toast stack:
    // a toast is a one-shot acknowledgement and always wins.
    if let Some(tip) = ctx.tooltip {
        super::overlay::draw_tooltip(frame, area, tip, ctx.glyphs, ctx.theme);
    }

    // The toast stack floats above EVERYTHING (panes and modals alike), so a
    // yank fired from inside the SQL modal is acknowledged too — drawn last.
    if !ctx.toasts.is_empty() {
        super::overlay::draw_toasts(frame, area, ctx.toasts, ctx.glyphs, ctx.theme);
    }
}

/// Draw one overlay `mode` (the modal / menu / palette the mode paints). A
/// stacked mode's `back` is drawn through the same function — it never nests
/// deeper (backs are modals or base modes by construction). `lin_inner` is the
/// lineage pane's interior, needed to anchor the node menu to the target's
/// drawn box.
fn draw_overlay(
    frame: &mut Frame,
    area: Rect,
    lin_inner: Rect,
    mode: &crate::action::Mode,
    ctx: &RenderCtx,
) {
    use crate::action::Mode;
    match mode {
        Mode::Help(h) => super::overlay::draw_help(frame, area, h, ctx.glyphs, ctx.theme),
        Mode::Detail(dv) => super::overlay::draw_detail(frame, area, dv, ctx.glyphs, ctx.theme),
        Mode::Sql(sv) => super::overlay::draw_sql(frame, area, sv, ctx.glyphs, ctx.theme),
        Mode::Stats(sv) => super::overlay::draw_stats(frame, area, sv, ctx.glyphs, ctx.theme),
        Mode::Diff(dv) => super::overlay::draw_diff(frame, area, dv, ctx.glyphs, ctx.theme),
        Mode::Columns(cv) => super::overlay::draw_columns(frame, area, cv, ctx.glyphs, ctx.theme),
        Mode::Notifications(nv) => {
            super::overlay::draw_notifications(frame, area, nv, ctx.glyphs, ctx.theme)
        }
        Mode::Palette(p) => {
            super::overlay::draw_palette(frame, area, p, ctx.palette_rows, ctx.glyphs, ctx.theme)
        }
        Mode::Yank { sel, .. } => {
            if let Some(r) = super::geom::yank_menu_rect(area) {
                super::overlay::draw_yank_menu(frame, r, *sel, ctx.glyphs, ctx.theme)
            }
        }
        Mode::Menu(ms) => {
            let r = super::geom::menu_rect(
                area,
                lin_inner,
                ctx.lineage,
                ctx.state.lineage_scroll_x(),
                ctx.state.lineage_scroll_y(),
                ms,
            );
            super::overlay::draw_menu(frame, r, ms, ctx.glyphs, ctx.theme)
        }
        // A search's "overlay" is the inline query in the pane titles — the
        // panes themselves already drew it.
        Mode::Selection | Mode::Search(_) => {}
    }
}

/// Paint the one-row tab strip (Issue 03, §2): ` {n} {name} × ` cells over a
/// `surface` band — the active tab `surface_hi`+`accent` bold, the others dim —
/// `‹`/`›` overflow arrows on whichever side hides tabs, ` + ` pinned at the
/// strip's right end, and the project name dim at the bar's right edge. The
/// hover cues come from §6: a hovered tab emphasises its `×`, a hovered `×`
/// turns `danger`, and `+`/arrows take the accent.
///
/// Everything positional arrives resolved in `view.layout` (the same
/// `tab_bar_layout` the hit-test ran — no re-measuring here), and every text
/// arrives pre-truncated in `view.labels`, so this function is pure painting.
fn draw_tab_bar(
    frame: &mut Frame,
    area: Rect,
    view: &TabBarView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    if area.height == 0 {
        return;
    }
    let chrome = super::chrome(glyphs);
    let hover = view.hover;
    // The whole strip is a `surface` band — the panes' `bg` would read as one
    // more content row instead of chrome.
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.surface)),
        area,
    );

    for cell in &view.layout.tabs {
        let active = cell.index == view.active;
        let base = if active {
            Style::default()
                .bg(t.surface_hi)
                .fg(t.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(t.surface).fg(t.text_dim)
        };
        // The `×` restyle ladder: hovered close → danger (§6); hovered tab
        // body → accent emphasis; otherwise the quiet `text_faint`.
        let close_st = if matches!(hover, Some(HitTarget::TabClose(i)) if *i == cell.index) {
            base.patch(Style::default().fg(t.danger))
        } else if matches!(hover, Some(HitTarget::Tab(i)) if *i == cell.index) {
            base.patch(Style::default().fg(t.accent))
        } else {
            base.patch(Style::default().fg(t.text_faint))
        };
        let text = Line::from(vec![
            Span::styled(format!(" {} ", view.labels[cell.index]), base),
            Span::styled(chrome.tab_close, close_st),
            Span::styled(" ", base),
        ]);
        frame.render_widget(Paragraph::new(text), cell.rect);
    }

    let dim = Style::default().bg(t.surface).fg(t.text_dim);
    let hot = Style::default()
        .bg(t.surface)
        .fg(t.accent)
        .add_modifier(Modifier::BOLD);
    if let Some(r) = view.layout.left_arrow {
        let st = if matches!(hover, Some(HitTarget::TabScrollLeft)) {
            hot
        } else {
            dim
        };
        frame.render_widget(Paragraph::new(chrome.tab_prev).style(st), r);
    }
    if let Some(r) = view.layout.right_arrow {
        let st = if matches!(hover, Some(HitTarget::TabScrollRight)) {
            hot
        } else {
            dim
        };
        frame.render_widget(Paragraph::new(chrome.tab_next).style(st), r);
    }
    {
        let st = if matches!(hover, Some(HitTarget::TabNew)) {
            hot
        } else {
            dim
        };
        frame.render_widget(Paragraph::new(" + ").style(st), view.layout.plus);
    }
    if let (Some(name), Some(r)) = (view.project, view.layout.project) {
        frame.render_widget(
            Paragraph::new(format!(" {name}"))
                .style(Style::default().bg(t.surface).fg(t.text_faint))
                .alignment(ratatui::layout::Alignment::Right),
            r,
        );
    }
}

/// Draw the left model-list pane: a bordered block with interleaved group
/// headers and model rows, scrolled by `state.offset()` (display-row space),
/// the selected model row highlighted by both a marker and reversed style.
///
/// The visible window is exactly `list.rows[offset .. offset + inner_height]` —
/// the *same* row sequence `UiState::ensure_visible` measures against, which is
/// what guarantees the selected model's row is never clipped. The §10 sticky
/// header then overwrites the FIRST window line (never inserts one) — a
/// rendering-only overlay that leaves model-space and display-row-space
/// untouched (`hit_target` resolves the covered row through the same
/// `ModelList::sticky_header`, so "computed row == drawn row" still holds).
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
    empty_reason: Option<&str>,
    folded: Option<&std::collections::BTreeSet<String>>,
    hover: Option<&HitTarget>,
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
        // `exp:` joins only when exposures exist, so a project without them
        // keeps the pre-exposure title byte-identical.
        (None, Some(s)) => {
            let exp = if s.exposures > 0 {
                format!("exp:{} ", s.exposures)
            } else {
                String::new()
            };
            format!(
                " {} - model:{} src:{} seed:{} snap:{} {exp}",
                s.project, s.models, s.sources, s.seeds, s.snapshots
            )
        }
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
    let is_folded = |layer: &str| folded.is_some_and(|f| f.contains(layer));
    let mut lines: Vec<Line> = Vec::with_capacity(inner_height);
    for row in list.rows.iter().skip(state.offset()).take(inner_height) {
        match row {
            DisplayRow::Header { layer, count } => {
                lines.push(list_header_line(layer, *count, is_folded(layer), chrome, t));
            }
            DisplayRow::Model { model_index, name } => {
                lines.push(list_model_line(
                    list,
                    state,
                    search,
                    chrome,
                    bookmarks,
                    *model_index,
                    name,
                    inner.width as usize,
                    t,
                ));
            }
        }
    }
    // §10's sticky header: while the window's first display row is a model
    // mid-group, pin its owning header over that first line (an overwrite —
    // never an inserted row — so display-row-space stays exact). The pin is
    // suppressed while the selected model IS that row (it must keep its
    // marker — `sticky_header` owns the whole rule, shared with hit_target).
    if let Some(DisplayRow::Header { layer, count }) =
        list.sticky_header(state.offset(), state.selected())
    {
        if !lines.is_empty() {
            lines[0] = list_header_line(layer, *count, is_folded(layer), chrome, t);
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);

    // §6's hover cue: the row under the pointer paints `theme.surface` (a
    // one-row bg band over the drawn text — `set_style`-like: it tints every
    // cell of the row, glyphs untouched). The SELECTED row skips it (the
    // reversed marker is already the stronger affordance), and the row is
    // resolved through `list_target_row` — the hit-test's own sticky-pin
    // map — so the tint lands exactly where a click would.
    if let Some(target) = hover {
        let covered = matches!(target, HitTarget::ListRow(i) if *i == state.selected());
        if !covered {
            if let Some(y) = super::geom::list_target_row(
                list,
                state.offset(),
                state.selected(),
                target,
                inner_height,
            ) {
                let row = Rect {
                    y: inner.y + y as u16,
                    height: 1,
                    ..inner
                };
                frame.render_widget(
                    Paragraph::new("").style(Style::default().bg(t.surface)),
                    row,
                );
            }
        }
    }

    // The empty-state / search-hint line: display-only chrome stamped onto the
    // interior AFTER the rows render — never an injected DisplayRow, so
    // model-space and display-row-space stay untouched (the two-coordinate
    // invariant). An EMPTY list shows the reason (`empty_reason`); a live list
    // search with an EMPTY query shows the syntax hint on a non-empty list
    // (it covers the top row for that frame only — it vanishes on the first
    // keystroke, per the spec's "while the input is empty").
    let stamp: Option<String> = if list.is_empty() {
        empty_reason.map(str::to_string)
    } else if search == Some("") {
        Some(format!(
            "!tag:x selector  ?text full-text  {} history",
            chrome.badge_up
        ))
    } else {
        None
    };
    if let Some(text) = stamp {
        if inner.height > 0 {
            // The empty reason goes BELOW any surviving display rows (the
            // all-folded pane still draws its `▸` headers — §10 says folded
            // headers never vanish); a truly empty list's row_count is 0,
            // so the same math lands on row 0. The non-empty search hint
            // always stamps interior row 0 (§11).
            let y = if list.is_empty() {
                list.row_count()
                    .saturating_sub(state.offset())
                    .min(inner_height - 1)
            } else {
                0
            };
            let row = Rect {
                y: inner.y + y as u16,
                height: 1,
                ..inner
            };
            frame.render_widget(ratatui::widgets::Clear, row);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!(" {text}"),
                    Style::default().fg(t.text_faint),
                )),
                row,
            );
        }
    }

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

/// One list group-header line: `▾ Title (N)` while open, `▸ Title (N)` while
/// folded — the §15 fold marker via the shared [`Chrome`], the layer's own
/// accent + bold, the count reporting the FULL membership even folded. Used
/// both for real header rows and the §10 sticky pin (one shape, one style).
fn list_header_line(
    layer: &str,
    count: usize,
    folded: bool,
    chrome: &super::Chrome,
    t: &theme::Theme,
) -> Line<'static> {
    let mark = if folded {
        chrome.group_folded
    } else {
        chrome.group_open
    };
    // The `layer` grouping key is never mutated — `title_case` is display-only.
    let label = format!("{mark} {} ({count})", title_case(layer));
    let accent = Style::default()
        .fg(layer_accent(layer, t))
        .add_modifier(Modifier::BOLD);
    Line::styled(label, accent)
}

/// One list model row — §10's width-tiered columns on the pane's INNER width:
/// `< 30` name only; `30..=43` name + the direct `↑u ↓d` badges (plus the
/// Coverage lens's `t:N` and the `★` bookmark, the pre-tier suffixes); `>= 44`
/// adds the materialization tag + `t:N` to the suffix
/// (`Table t:3 ↑2 ↓4`). The suffix RIGHT-ALIGNS to the pane's inner edge so
/// the badges read as real columns; the name middle-elides into the space
/// left of it ([`crate::model_list::mid_truncate_indexed`] — `…`/`..` from
/// `chrome`, `unicode-width` measured, tail preserved).
///
/// While a non-empty list search is active the name draws char-by-char
/// (matched chars patched gold+bold — the same `match_indices` the filter
/// accepted, mapped through the elision so a truncated name still highlights
/// exactly the right cells). Style-only — the rendered TEXT is unchanged.
#[allow(clippy::too_many_arguments)]
fn list_model_line(
    list: &ModelList,
    state: &UiState,
    search: Option<&str>,
    chrome: &super::Chrome,
    bookmarks: Option<&std::collections::BTreeSet<String>>,
    model_index: usize,
    name: &str,
    inner_w: usize,
    t: &theme::Theme,
) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let selected = model_index == state.selected();
    let marker = if selected {
        SELECTED_MARKER
    } else {
        UNSELECTED_MARKER
    };
    let node = list.model_at(model_index);
    let (up, down) = node.map_or((0, 0), |n| (n.direct_up, n.direct_down));
    // A model with no downstream is a leaf ("orphan") — flag it.
    let orphan = node.is_some_and(|n| n.resource_type == "model" && n.direct_down == 0);
    // Coverage lens (only the Coverage lens, not the others): a testable node
    // with zero tests gets a warning name. The lens wins over the orphan red
    // (the user opted into it), and selection still wins over both.
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
    let bookmarked = bookmarks.is_some_and(|b| node.is_some_and(|n| b.contains(&n.unique_id)));
    let coverage = state.lens() == LineageLens::Coverage;

    // The right-aligned suffix column(s) — everything after the name field.
    // Tier order inside the wide suffix is the spec's: tag, then `t:N`, then
    // the direct-edge badges; `★` always tails.
    let mut suffix = String::new();
    if inner_w >= 44 {
        let tag = node
            .map(|n| {
                crate::MaterializationClass::classify(&n.resource_type, n.materialized.as_deref())
                    .tag()
            })
            .unwrap_or("Node");
        let tests = node.map_or(0, |n| n.test_count);
        suffix = format!(
            " {tag} t:{tests} {}{up} {}{down}",
            chrome.badge_up, chrome.badge_down
        );
    } else if inner_w >= 30 {
        suffix = format!("  {}{up} {}{down}", chrome.badge_up, chrome.badge_down);
        // Under the Coverage lens, surface the metric the red tint is
        // judging: the per-model test count (pure ASCII — guard-safe).
        if coverage {
            if let Some(n) = node {
                suffix.push_str(&format!(" t:{}", n.test_count));
            }
        }
    }
    if inner_w >= 30 && bookmarked {
        suffix.push_str(&format!(" {}", chrome.bookmark));
    }

    // `  ` + marker = a 4-cell lead-in; the name field is what remains before
    // the right-aligned suffix (≥1 cell short circuits to the empty field).
    let name_w = inner_w.saturating_sub(4 + UnicodeWidthStr::width(suffix.as_str()));
    let (text, cmap) = crate::model_list::mid_truncate_indexed(name, name_w, chrome.ellipsis);
    let pad = name_w.saturating_sub(UnicodeWidthStr::width(text.as_str()));

    let name_spans: Vec<Span> = match search {
        Some(q) if !q.trim().is_empty() => {
            let hits = crate::model_list::match_indices(name, q);
            let hi = Style::default().fg(t.gold).add_modifier(Modifier::BOLD);
            text.chars()
                .zip(cmap.iter())
                .map(|(ch, &src)| {
                    let st = if src != usize::MAX && hits.contains(&src) {
                        name_style.patch(hi)
                    } else {
                        name_style
                    };
                    Span::styled(ch.to_string(), st)
                })
                .collect()
        }
        _ => vec![Span::styled(text, name_style)],
    };

    let mut spans = vec![
        Span::raw("  "),                              // indent under header
        Span::styled(marker.to_string(), name_style), // selection marker
    ];
    spans.extend(name_spans);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(t.text_faint)));
    }
    Line::from(spans)
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

/// Convert a [`StatusSegments`] into the priority-ordered chip list
/// [`status_layout`](super::status_layout) and `draw_status` share — the
/// `(chip, bracketed-label)` pairs in §8's keep order (`Impact` survives
/// longest, `View` sheds first). The single producer for chips, so the
/// draw and the hit-test can never disagree on which chips exist.
pub fn status_chips(segments: &StatusSegments) -> Vec<(super::StatusChip, String)> {
    use super::StatusChip as C;
    let mut v = Vec::with_capacity(13);
    let mut push = |chip: C, txt: Option<&str>| {
        if let Some(txt) = txt {
            v.push((chip, format!("[{txt}]")));
        }
    };
    // Declaration order == §8's survival order (see [`StatusChip`]).
    push(C::Impact, segments.impact);
    push(C::Notify, segments.notify);
    push(C::Column, segments.column);
    push(C::Diff, segments.diff);
    push(C::Run, segments.run);
    push(C::Scope, segments.scope);
    push(C::Coverage, segments.coverage);
    push(C::Bookmarks, segments.bookmarks);
    push(C::Filter, segments.filter);
    push(C::Sort, segments.sort);
    push(C::Position, segments.position);
    push(C::Focus, segments.focus);
    push(C::View, segments.view);
    v
}

/// A chip's semantic colour — the same per-segment colour assignments the
/// legacy segment list used, keyed by [`StatusChip`] now.
fn chip_color(chip: super::StatusChip, t: &theme::Theme) -> Color {
    use super::StatusChip as C;
    match chip {
        C::Impact | C::Notify => t.danger,
        C::Column | C::Bookmarks => t.gold,
        C::Diff => t.diff_mod,
        C::Run | C::Coverage => t.warn,
        C::Scope | C::Filter | C::Sort => t.accent,
        C::Position => t.text_faint,
        C::Focus => t.ok,
        C::View => t.chip_view,
    }
}

/// Draw the bottom status line (screen-spec §8): the contextual hints
/// left-anchored (key in `accent`, description in `text_dim`), the chips
/// right-anchored in their semantic colours — all placed by the shared
/// [`status_layout`](super::status_layout) the hit-test also consumed, so
/// every drawn span answers the click its `HitTarget` promises. The hovered
/// span draws BOLD (§6's emphasis cue); the inert `StatusBar` cells keep
/// the plain surface band.
fn draw_status(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    use unicode_width::UnicodeWidthStr;

    let t = ctx.theme;
    let chips = status_chips(&ctx.segments);
    let layout = super::status_layout(area.width, ctx.hints, &chips);

    // The surface band first (the whole row is chrome, not content rows).
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(t.surface)),
        area,
    );
    if area.height == 0 {
        return;
    }
    let row = area.y;
    let buf = frame.buffer_mut();
    let mut stamp = |x: u16, s: &str, st: Style| {
        buf.set_stringn(
            area.x + x,
            row,
            s,
            (area.width.saturating_sub(x)) as usize,
            st,
        );
    };

    // Hints — `layout.hints` is a PREFIX of `ctx.hints` (the drop keeps the
    // head of the priority-sorted input), so the zip pairs each span with
    // its hint.
    for (span, hint) in layout.hints.iter().zip(ctx.hints.iter()) {
        let hot = ctx.hover == Some(&span.target);
        let mut x = span.x0;
        let key_st = Style::default()
            .fg(t.accent)
            .bg(t.surface)
            .add_modifier(if hot {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        stamp(x, &format!(" {}", hint.key), key_st);
        x += 1 + UnicodeWidthStr::width(hint.key.as_str()) as u16;
        if !hint.desc.is_empty() {
            let desc_st = Style::default()
                .fg(t.text_dim)
                .bg(t.surface)
                .add_modifier(if hot {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            stamp(x, &format!(" {}", hint.desc), desc_st);
        }
    }
    // Chips — same prefix-pairing over the right-anchored block.
    for (span, (chip, label)) in layout.chips.iter().zip(chips.iter()) {
        let hot = ctx.hover == Some(&span.target);
        let st = Style::default()
            .fg(chip_color(*chip, t))
            .bg(t.surface)
            .add_modifier(if hot {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        stamp(span.x0, &format!(" {label}"), st);
    }
}
