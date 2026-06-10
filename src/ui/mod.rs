//! UI layer: pure state + terminal-lifecycle-free render functions. `mod.rs`
//! re-exports the public names so the physical split into submodules stays
//! invisible to callers/tests.
//!
//! - [`state`]  — `UiState`, `Focus`, `KeyOutcome`, `reduce_selection`, `handle_key`.
//! - [`geom`]   — pane rects / interiors / centered-overlay rect.
//! - [`render`] — `RenderCtx`, `draw`, and the base panes (list, status).
//! - [`lineage`]— the lineage pane renderer.
//! - [`overlay`]— modal overlays (`?` help, structure modal; search is inline).
//! - [`theme`]  — the colour palette (semantic constants; pub so tests assert
//!   against roles, not literals).

use ratatui::style::{Modifier, Style};

mod geom;
mod lineage;
mod overlay;
mod render;
mod state;
pub mod theme;

pub use geom::{
    clip_edges, hit_test, lineage_content_rect, pane_interior, pane_rects, ClipEdges, PaneRects,
};
pub use overlay::{clamp_detail_scroll, clamp_help_scroll, clamp_sql_scroll, clamp_stats_scroll};
pub use render::{draw, RenderCtx, StatusSegments};
pub use state::{handle_key, reduce_selection, Focus, KeyOutcome, LineageLens, UiState};

// ---- shared render styles (used across the list / lineage / overlay panes) ----

/// Pure-ASCII pane border set (`+ - |`) — the [`GlyphMode::Ascii`] chrome.
const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// UI chrome glyphs per [`GlyphMode`]: the pane border set, the lineage
/// scroll-affordance markers, the list dependency badges, and the search caret.
/// Unicode is the pretty default; ASCII is the fallback for terminals that
/// render East-Asian-Ambiguous characters 2 cells wide (which would desync
/// every Unicode border/marker into doubled/ghosted glyphs — see
/// [`GlyphMode`](crate::GlyphMode)). One struct so a mode switches the WHOLE
/// chrome at once and no surface can mix repertoires.
pub(crate) struct Chrome {
    pub border: ratatui::symbols::border::Set<'static>,
    /// Scroll markers stamped on the lineage pane border (left/right/up/down).
    pub left: &'static str,
    pub right: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    /// List-pane direct-dependency badges (`↑2 ↓3` / `^2 v3`).
    pub badge_up: &'static str,
    pub badge_down: &'static str,
    /// The inline search caret shown after the live query.
    pub caret: &'static str,
    /// The list-pane bookmark badge (`★` Unicode / `*` ASCII). Unicode `★` is
    /// East-Asian-Ambiguous but safe: it only appears in `UNICODE_CHROME`, and
    /// `ascii_guard` renders solely `GlyphMode::Ascii` (where it is `*`).
    pub bookmark: &'static str,
    /// Scrollbar thumb glyph stamped on a pane's right border. Only the thumb is
    /// drawn (the track is left as the plain border so an un-scrolled pane stays
    /// clean). `█` is ambiguous-width but Unicode-only — ASCII mode uses `#`.
    pub sb_thumb: &'static str,
    /// The accented layer-header rule glyph (`─` Unicode / `-` ASCII). The dash
    /// COUNT is mode-identical so the header column width never shifts.
    pub rule: &'static str,
    /// Lineage-minimap inset glyphs (occupied node / empty cell / viewport rect).
    /// Deliberately pure ASCII (`#`/`.`/`+`) in BOTH modes — the minimap is a
    /// purely positional sketch, so it has no need for box-drawing glyphs and is
    /// thus ambiguous-width-free everywhere (no separate Unicode repertoire).
    pub mm_node: &'static str,
    pub mm_empty: &'static str,
    pub mm_view: &'static str,
}

/// The chrome for a [`GlyphMode`](crate::GlyphMode).
pub(crate) fn chrome(mode: crate::GlyphMode) -> &'static Chrome {
    match mode {
        crate::GlyphMode::Unicode => &UNICODE_CHROME,
        crate::GlyphMode::Ascii => &ASCII_CHROME,
    }
}

static UNICODE_CHROME: Chrome = Chrome {
    // Rounded corners (`╭╮╰╯`): the same East-Asian-Ambiguous box-drawing block
    // as PLAIN (no new width hazard — ASCII mode already covers those
    // terminals), but the soft-corner look of modern TUIs.
    border: ratatui::symbols::border::ROUNDED,
    left: "◀",
    right: "▶",
    up: "▲",
    down: "▼",
    badge_up: "↑",
    badge_down: "↓",
    caret: "▏",
    bookmark: "★",
    sb_thumb: "█",
    rule: "─",
    mm_node: "#",
    mm_empty: ".",
    mm_view: "+",
};

static ASCII_CHROME: Chrome = Chrome {
    border: ASCII_BORDER,
    left: "<",
    right: ">",
    up: "^",
    down: "v",
    badge_up: "^",
    badge_down: "v",
    caret: "_",
    bookmark: "*",
    sb_thumb: "#",
    rule: "-",
    mm_node: "#",
    mm_empty: ".",
    mm_view: "+",
};

/// Highlight style for the selected row / emphasized lineage label: an
/// accent-coloured bar. The fg is the accent and REVERSED swaps it into the
/// background, so the bar reads as "painted accent" while the REVERSED modifier
/// stays the machine-readable selection cue the tests scan for.
pub(crate) fn selected_style() -> Style {
    Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD)
}

/// Style for group-header rows / overlay section headers.
pub(crate) fn header_style() -> Style {
    Style::default()
        .fg(theme::SECTION)
        .add_modifier(Modifier::BOLD)
}

/// Border style emphasising the focused pane (read via the public accessor, so
/// sibling render modules need no access to `UiState`'s private fields).
pub(crate) fn focus_border(state: &UiState, pane: Focus) -> Style {
    if state.focus() == pane {
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::BORDER_IDLE)
    }
}

/// Pane-title style paired with [`focus_border`]: the focused pane's title is
/// the accent (bold), an unfocused one recedes to dim text — the title and the
/// border always agree on which pane is active.
pub(crate) fn title_style(state: &UiState, pane: Focus) -> Style {
    if state.focus() == pane {
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::TEXT_DIM)
    }
}

/// Marker prefixes for selected / unselected model rows (a visual cue
/// independent of style, so marker-based assertions also work on `TestBackend`).
pub(crate) const SELECTED_MARKER: &str = "> ";
pub(crate) const UNSELECTED_MARKER: &str = "  ";

/// Stamp a vertical scrollbar thumb onto a bordered pane's RIGHT border, mirroring
/// `lineage::stamp_scroll_markers`: drawn on `frame.buffer_mut()` AFTER the block +
/// content render, so it reflows nothing and never touches the interior. Only the
/// thumb cells are drawn (the track is left as the plain border) so an un-scrolled
/// pane keeps a clean border; when the content fits, nothing is drawn at all.
///
/// `total`/`offset`/`viewport` are in the pane's own scroll units — for the list
/// pane that is DISPLAY-ROW space (`row_count`/`offset`), never model count.
pub(crate) fn stamp_scrollbar(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    total: usize,
    offset: usize,
    viewport: usize,
    glyphs: crate::GlyphMode,
) {
    // The right border column, and the interior y-range between the top/bottom
    // border corners. Guard tiny rects so the math never underflows.
    let track_h = area.height.saturating_sub(2);
    if area.width < 2 || track_h < 1 {
        return;
    }
    let track_y0 = area.y + 1;
    let Some((thumb_top, thumb_len)) =
        geom::scrollbar_thumb(total, offset, viewport, track_y0, track_h)
    else {
        return; // content fits → leave the plain border
    };
    let x = area.x + area.width - 1;
    let sym = chrome(glyphs).sb_thumb;
    let style = Style::default().fg(theme::SB_THUMB);
    let buf = frame.buffer_mut();
    for y in thumb_top..thumb_top + thumb_len {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(sym).set_style(style);
        }
    }
}
