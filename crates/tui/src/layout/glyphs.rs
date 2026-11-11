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
pub(super) struct BoxGlyphs {
    pub(super) tl: char,
    pub(super) tr: char,
    pub(super) bl: char,
    pub(super) br: char,
    pub(super) h: char,
    pub(super) v: char,
    pub(super) arrow: char,
}

impl BoxGlyphs {
    pub(super) fn for_mode(mode: GlyphMode) -> &'static BoxGlyphs {
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
