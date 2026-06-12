//! The visual theme: every colour the UI paints, as SEMANTIC roles in one
//! place. Render code never names a raw colour — it names a role (`accent`,
//! `danger`, `class_table`, …) on the [`Theme`] it was handed, so a palette
//! retune is a one-file edit and the integration tests (which assert against
//! the [`DEFAULT`] roles, not literals) survive it.
//!
//! **Themes are data, not globals.** [`Theme`] is a plain `Copy` struct of
//! colours; the ACTIVE theme travels through `RenderCtx` (the App owns the
//! loaded list, the event loop hands the active one to `draw`), so renders stay
//! pure and the headless tests — which default to [`DEFAULT`] — never race on
//! shared state. Besides [`DEFAULT`] there are built-in [`preset`]s, and users
//! can define their own as YAML files (parsed by [`parse_theme`]; the IO and
//! the `--theme` CLI resolution live in `main`).
//!
//! The DEFAULT palette is `Color::Indexed` from the xterm-256 cube: universally
//! supported by modern terminals (truecolor is not), and — unlike the 16 named
//! ANSI colours — NOT remapped by the user's terminal scheme, so the curated
//! palette renders as designed. It assumes a dark background (soft pastel
//! foregrounds over dark surface bands), tuned in the spirit of modern dark
//! themes (Tokyo Night / Catppuccin). The other presets reproduce upstream
//! schemes (ayu, gruvbox) and use truecolor `Color::Rgb` for fidelity — a
//! terminal without truecolor support approximates them; such users should
//! keep the default or write an Indexed custom theme.
//!
//! **Two saturation families, on purpose.** Materialization classes (the
//! lens-off resting colours) are PASTEL; lens tints (`layer_*`, `heat_*`,
//! `danger`-as-tint) are VIVID. An active lens therefore reads as "the
//! saturation turned up", and — the load-bearing part — every lens tint is
//! distinct from EVERY class colour, so a lens can never render a node
//! identically to lens-off. [`lint`] checks exactly that contract (plus the
//! heat-ramp / layer-set distinctness) for ANY theme: the tests assert every
//! preset is lint-clean, and `main` surfaces lint warnings for user themes.
//! Two lenses never co-occur, so tints may reuse values ACROSS lenses.
//!
//! Where two roles deliberately share a value, one ALIASES the other (e.g.
//! [`SECTION`] = [`CLASS_SNAPSHOT`] in the default) so the coupling is visible
//! and a retune propagates — or is consciously split.
//!
//! Colours only — glyphs stay in [`Chrome`](super::Chrome)/`BoxGlyphs` (the
//! glyph-mode seam), and style COMPOSITION (precedence, modifiers) stays in
//! the render fns. ASCII-safety is untouched by definition: a colour never
//! adds a glyph.

use std::collections::BTreeMap;

use ratatui::style::Color;
use serde::Deserialize;

/// Declares the [`Theme`] struct, [`ROLE_NAMES`], and the name→field setter
/// from ONE list, so a new role automatically becomes settable from a theme
/// file and visible in the "valid roles" error message — they cannot drift.
macro_rules! theme_roles {
    ($( $(#[$doc:meta])* $name:ident ),+ $(,)?) => {
        /// One complete colour palette: every semantic role the UI paints.
        /// `Copy` — the active theme is handed around by value/borrow, never
        /// via a global.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Theme {
            $( $(#[$doc])* pub $name: Color, )+
        }

        /// Every role name a custom theme file may set, in declaration order
        /// (the YAML `colors:` keys).
        pub const ROLE_NAMES: &[&str] = &[$(stringify!($name)),+];

        impl Theme {
            /// Set a role by its [`ROLE_NAMES`] name; `false` for an unknown name.
            fn set_role(&mut self, name: &str, color: Color) -> bool {
                match name {
                    $(stringify!($name) => self.$name = color,)+
                    _ => return false,
                }
                true
            }
        }
    };
}

theme_roles!(
    // ---- core chrome ----
    /// The primary accent: focused-pane borders/titles, modal frames, carets,
    /// the selection bar (via REVERSED), the sort chip.
    accent,
    /// Unfocused pane borders — recede into the background.
    border_idle,
    /// Bright body text (the selected-name echo in the status bar, modal titles).
    text_bright,
    /// Secondary text: titles of unfocused panes, status-bar core, key labels.
    text_dim,
    /// Tertiary text: dependency badges, minimap.
    text_faint,
    /// Off-path lineage nodes under the focus dim (cursor off-root). Deliberately
    /// BRIGHTER than `text_faint`: the dim must recede from the path band yet
    /// stay readable — the off-path nodes are context, not noise. Distinct from
    /// `text_dim`/`sb_thumb`/`class_other` so no surface aliases by accident.
    lineage_dim,
    /// The status-bar background band.
    surface,
    /// A raised surface: the root↔cursor path band, gauge tracks.
    surface_hi,
    /// The right-border scrollbar thumb.
    sb_thumb,
    // ---- semantic states ----
    /// Good / passing: the coverage-gauge high grade, the focus chip.
    ok,
    /// Caution: coverage chip + mid gauge grade, lineage scroll markers, the
    /// top-hub stats bar.
    warn,
    /// Problem: impact chip, low gauge grade, coverage-gap names, and the
    /// `Warn`/`Violation` lens tints (vivid — no class colour may equal it, so
    /// the tint can never disappear into one).
    danger,
    /// A leaf model with no downstream (a quieter red than `danger`).
    orphan,
    /// Search/palette match-char highlight, the bookmark star + chip.
    gold,
    // ---- materialization classes (lineage box colours; the PASTEL family) ----
    /// Persisted table — the healthy resting state (aliases `ok` in the default).
    class_table,
    class_view,
    class_incremental,
    class_ephemeral,
    class_source,
    class_seed,
    class_snapshot,
    class_other,
    // ---- layers (list headers + the Layer lens; the VIVID family) ----
    layer_staging,
    layer_intermediate,
    layer_marts,
    layer_utilities,
    layer_other,
    // ---- degree-heat ramp (DegreeHeat lens; VIVID — distinct from every class) ----
    heat_low,
    heat_mid,
    heat_high,
    // ---- diff lens (added / modified vs the --diff baseline; VIVID) ----
    diff_add,
    diff_mod,
    // ---- status-bar chips ----
    /// The `[view]` direction/depth chip (a chip role, deliberately NOT
    /// `class_view`: it names the lineage view filter, not a materialization).
    chip_view,
    // ---- overlays ----
    /// Overlay section headers (`columns:` / `tests:` / stats sections).
    section,
    /// SQL keyword highlight in the `s` preview.
    sql_keyword,
    /// SQL string literals in the `s` preview.
    sql_string,
    /// SQL comments (`--`, `/* */`, and Jinja `{# #}`) in the `s` preview.
    sql_comment,
    /// Jinja expressions/statements (`{{ ref(..) }}` / `{% if %}`) in the `s` preview.
    sql_jinja,
    // ---- stats mini-bars (decorative chart fills, one per chart) ----
    bar_resource,
    bar_materialization,
    bar_degree,
    bar_transitive,
);

// ---- the default palette, as the LEGACY constants ---------------------------
//
// The constants predate the Theme struct and remain the canonical names the
// style-asserting tests reference; [`DEFAULT`] is built FROM them, so the two
// can never disagree. New code should read roles off the Theme it was handed —
// the constants describe only the DEFAULT palette.

/// The primary accent. Soft blue `#87afff`.
pub const ACCENT: Color = Color::Indexed(111);
/// Unfocused pane borders.
pub const BORDER_IDLE: Color = Color::Indexed(238);
/// Bright body text.
pub const TEXT_BRIGHT: Color = Color::Indexed(254);
/// Secondary text.
pub const TEXT_DIM: Color = Color::Indexed(245);
/// Tertiary text.
pub const TEXT_FAINT: Color = Color::Indexed(240);
/// Off-path lineage dim (brighter than [`TEXT_FAINT`] — see the role doc).
pub const LINEAGE_DIM: Color = Color::Indexed(243);
/// The status-bar background band.
pub const SURFACE: Color = Color::Indexed(234);
/// A raised surface: the root↔cursor path band, gauge tracks.
pub const SURFACE_HI: Color = Color::Indexed(237);
/// The right-border scrollbar thumb.
pub const SB_THUMB: Color = Color::Indexed(244);

/// Good / passing. `#87d787`.
pub const OK: Color = Color::Indexed(114);
/// Caution. `#d7af5f`.
pub const WARN: Color = Color::Indexed(179);
/// Problem / vivid lens red. `#ff5f5f`.
pub const DANGER: Color = Color::Indexed(203);
/// A leaf model with no downstream (a quieter red than [`DANGER`]).
pub const ORPHAN: Color = Color::Indexed(167);
/// Search/palette match-char highlight, the bookmark star + chip. `#ffd700`.
pub const GOLD: Color = Color::Indexed(220);

/// Persisted table — green. Aliases [`OK`] on purpose: a built table is the
/// healthy resting state.
pub const CLASS_TABLE: Color = OK;
pub const CLASS_VIEW: Color = Color::Indexed(116); // teal `#87d7d7` — virtual
pub const CLASS_INCREMENTAL: Color = Color::Indexed(176); // orchid `#d787d7`
pub const CLASS_EPHEMERAL: Color = Color::Indexed(183); // lavender `#d7afff`
pub const CLASS_SOURCE: Color = Color::Indexed(75); // sky blue `#5fafff` — external
pub const CLASS_SEED: Color = Color::Indexed(180); // pale sand `#d7af87` (≠ WARN)
pub const CLASS_SNAPSHOT: Color = Color::Indexed(215); // peach `#ffaf5f`
pub const CLASS_OTHER: Color = Color::Indexed(246); // gray — unknown

// Five DISTINCT layer colours, each also distinct from every CLASS_* value:
// vivid cyan/mustard/green/violet/silver vs the pastel classes.
pub const LAYER_STAGING: Color = Color::Indexed(38); // vivid cyan `#00afd7`
pub const LAYER_INTERMEDIATE: Color = Color::Indexed(178); // mustard `#d7af00`
pub const LAYER_MARTS: Color = Color::Indexed(35); // vivid green `#00af5f`
pub const LAYER_UTILITIES: Color = Color::Indexed(165); // violet `#d700ff`
pub const LAYER_OTHER: Color = Color::Indexed(250); // silver `#bcbcbc`

/// Low heat — vivid green (shares the value with [`LAYER_MARTS`]; lenses are
/// mutually exclusive, so cross-lens reuse never collides on screen).
pub const HEAT_LOW: Color = Color::Indexed(35);
/// Mid heat — vivid gold (the [`GOLD`] hue; match highlights live in the list
/// pane, the heat tint in the lineage pane).
pub const HEAT_MID: Color = Color::Indexed(220);
/// High heat — the [`DANGER`] red.
pub const HEAT_HIGH: Color = DANGER;

/// Diff lens: node ADDED vs the `--diff` baseline — vivid spring green
/// `#00d787` (the VCS-green convention; distinct from the pastel
/// [`CLASS_TABLE`]/[`OK`] green per the lens contract).
pub const DIFF_ADD: Color = Color::Indexed(42);
/// Diff lens: node MODIFIED vs the baseline — vivid amber `#ffaf00`
/// (distinct from the pastel [`CLASS_SNAPSHOT`] peach and from [`GOLD`]).
pub const DIFF_MOD: Color = Color::Indexed(214);

/// The `[view]` direction/depth chip — orchid hue.
pub const CHIP_VIEW: Color = Color::Indexed(176);

/// Overlay section headers. Aliases the snapshot peach on purpose — a shared
/// warm-accent hue across unrelated surfaces (modal headers vs lineage boxes)
/// that never compete for meaning.
pub const SECTION: Color = CLASS_SNAPSHOT;
/// SQL keyword highlight. Editor-style purple.
pub const SQL_KEYWORD: Color = Color::Indexed(141);
/// SQL string literals. Soft green `#afd787`.
pub const SQL_STRING: Color = Color::Indexed(150);
/// SQL comments. Aliases [`TEXT_FAINT`] on purpose: comments recede like
/// tertiary chrome text.
pub const SQL_COMMENT: Color = TEXT_FAINT;
/// Jinja regions. Aliases [`SECTION`]'s warm peach on purpose: the templating
/// layer is the load-bearing part of a dbt model, so it shares the accent family.
pub const SQL_JINJA: Color = SECTION;

pub const BAR_RESOURCE: Color = Color::Indexed(116); // teal
pub const BAR_MATERIALIZATION: Color = Color::Indexed(176); // orchid
pub const BAR_DEGREE: Color = WARN;
pub const BAR_TRANSITIVE: Color = OK;

/// The default theme — the Indexed-256 palette above, role by role. Headless
/// renders (`RenderCtx::new`) and the style-asserting tests use this one.
pub const DEFAULT: Theme = Theme {
    accent: ACCENT,
    border_idle: BORDER_IDLE,
    text_bright: TEXT_BRIGHT,
    text_dim: TEXT_DIM,
    text_faint: TEXT_FAINT,
    lineage_dim: LINEAGE_DIM,
    surface: SURFACE,
    surface_hi: SURFACE_HI,
    sb_thumb: SB_THUMB,
    ok: OK,
    warn: WARN,
    danger: DANGER,
    orphan: ORPHAN,
    gold: GOLD,
    class_table: CLASS_TABLE,
    class_view: CLASS_VIEW,
    class_incremental: CLASS_INCREMENTAL,
    class_ephemeral: CLASS_EPHEMERAL,
    class_source: CLASS_SOURCE,
    class_seed: CLASS_SEED,
    class_snapshot: CLASS_SNAPSHOT,
    class_other: CLASS_OTHER,
    layer_staging: LAYER_STAGING,
    layer_intermediate: LAYER_INTERMEDIATE,
    layer_marts: LAYER_MARTS,
    layer_utilities: LAYER_UTILITIES,
    layer_other: LAYER_OTHER,
    heat_low: HEAT_LOW,
    heat_mid: HEAT_MID,
    heat_high: HEAT_HIGH,
    diff_add: DIFF_ADD,
    diff_mod: DIFF_MOD,
    chip_view: CHIP_VIEW,
    section: SECTION,
    sql_keyword: SQL_KEYWORD,
    sql_string: SQL_STRING,
    sql_comment: SQL_COMMENT,
    sql_jinja: SQL_JINJA,
    bar_resource: BAR_RESOURCE,
    bar_materialization: BAR_MATERIALIZATION,
    bar_degree: BAR_DEGREE,
    bar_transitive: BAR_TRANSITIVE,
};

impl Default for Theme {
    fn default() -> Self {
        DEFAULT
    }
}

// ---- presets -----------------------------------------------------------------
//
// Each preset is a complete Theme, curated against the same contract as the
// default ([`lint`] must return no warnings — asserted by the tests). The hex
// values come from the upstream schemes; role mapping (which hue plays which
// role) is ours, holding the pastel-class / vivid-lens split per palette.

/// Truecolor from a `0xRRGGBB` literal (preset definitions only).
const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// ayu dark (<https://github.com/ayu-theme/ayu-colors>): for a near-black
/// `#0a0e14` terminal background. Identity hue: the gold accent.
const AYU_DARK: Theme = Theme {
    accent: rgb(0xE6B450),
    border_idle: rgb(0x3D424D),
    text_bright: rgb(0xE6E1CF),
    text_dim: rgb(0xB3B1AD),
    text_faint: rgb(0x626A73),
    lineage_dim: rgb(0x8A9199),
    surface: rgb(0x131721),
    surface_hi: rgb(0x273747),
    sb_thumb: rgb(0x565B66),
    ok: rgb(0xC2D94C),
    warn: rgb(0xFFB454),
    danger: rgb(0xFF3333),
    orphan: rgb(0xD96C75),
    gold: rgb(0xFFEE99),
    class_table: rgb(0xC2D94C),
    class_view: rgb(0x39BAE6),
    class_incremental: rgb(0xD2A6FF),
    class_ephemeral: rgb(0x95E6CB),
    class_source: rgb(0x59C2FF),
    class_seed: rgb(0xE6B673),
    class_snapshot: rgb(0xFF8F40),
    class_other: rgb(0x565B66),
    layer_staging: rgb(0x5CCFE6),
    layer_intermediate: rgb(0xFFCC66),
    layer_marts: rgb(0xBAE67E),
    layer_utilities: rgb(0xDFBFFF),
    layer_other: rgb(0xACB6BF),
    heat_low: rgb(0x91B362),
    heat_mid: rgb(0xE6B450),
    heat_high: rgb(0xFF3333),
    diff_add: rgb(0x7FD962),
    diff_mod: rgb(0x73B8FF),
    chip_view: rgb(0xD2A6FF),
    section: rgb(0xFF8F40),
    sql_keyword: rgb(0xFF8F40),
    sql_string: rgb(0xC2D94C),
    sql_comment: rgb(0x626A73),
    sql_jinja: rgb(0xFFB454),
    bar_resource: rgb(0x39BAE6),
    bar_materialization: rgb(0xD2A6FF),
    bar_degree: rgb(0xFFB454),
    bar_transitive: rgb(0xC2D94C),
};

/// ayu mirage: the softened mid-dark variant, for a `#1f2430` background.
const AYU_MIRAGE: Theme = Theme {
    accent: rgb(0xFFCC66),
    border_idle: rgb(0x3E4859),
    text_bright: rgb(0xD9D7CE),
    text_dim: rgb(0x99A3B5),
    text_faint: rgb(0x5C6773),
    lineage_dim: rgb(0x8590A3),
    surface: rgb(0x171B24),
    surface_hi: rgb(0x34455A),
    sb_thumb: rgb(0x707A8C),
    ok: rgb(0xBAE67E),
    warn: rgb(0xFFA759),
    danger: rgb(0xFF6666),
    orphan: rgb(0xF28779),
    gold: rgb(0xFFD173),
    class_table: rgb(0xBAE67E),
    class_view: rgb(0x5CCFE6),
    class_incremental: rgb(0xD4BFFF),
    class_ephemeral: rgb(0x95E6CB),
    class_source: rgb(0x73D0FF),
    class_seed: rgb(0xFFDFB3),
    class_snapshot: rgb(0xF29E74),
    class_other: rgb(0x707A8C),
    layer_staging: rgb(0x36A3D9),
    layer_intermediate: rgb(0xE6B450),
    layer_marts: rgb(0x87D96C),
    layer_utilities: rgb(0xDFBFFF),
    layer_other: rgb(0xB8CFE6),
    heat_low: rgb(0x87D96C),
    heat_mid: rgb(0xFFCC66),
    heat_high: rgb(0xFF6666),
    diff_add: rgb(0x87D96C),
    diff_mod: rgb(0x80BFFF),
    chip_view: rgb(0xD4BFFF),
    section: rgb(0xF29E74),
    sql_keyword: rgb(0xFFA759),
    sql_string: rgb(0xBAE67E),
    sql_comment: rgb(0x5C6773),
    sql_jinja: rgb(0xFFD173),
    bar_resource: rgb(0x5CCFE6),
    bar_materialization: rgb(0xD4BFFF),
    bar_degree: rgb(0xFFA759),
    bar_transitive: rgb(0xBAE67E),
};

/// ayu light: for a LIGHT (`#fcfcfc`) terminal background — the one preset
/// whose neutral ramp inverts (bright = darkest). Pick it only on a light
/// terminal; the TUI paints no global background of its own.
const AYU_LIGHT: Theme = Theme {
    accent: rgb(0xFF9940),
    border_idle: rgb(0xC9CDD1),
    text_bright: rgb(0x24292F),
    text_dim: rgb(0x5C6166),
    text_faint: rgb(0xA8ADB2),
    lineage_dim: rgb(0x8A9199),
    surface: rgb(0xF0F1F2),
    surface_hi: rgb(0xDCE3EA),
    sb_thumb: rgb(0xB5B9BD),
    ok: rgb(0x86B300),
    warn: rgb(0xF2AE49),
    danger: rgb(0xE65050),
    orphan: rgb(0xF07171),
    gold: rgb(0xB38B00),
    class_table: rgb(0x86B300),
    class_view: rgb(0x55B4D4),
    class_incremental: rgb(0xA37ACC),
    class_ephemeral: rgb(0x4CBF99),
    class_source: rgb(0x399EE6),
    class_seed: rgb(0xA8854B),
    class_snapshot: rgb(0xFA8D3E),
    class_other: rgb(0x787B80),
    layer_staging: rgb(0x008FB8),
    layer_intermediate: rgb(0xCC8800),
    layer_marts: rgb(0x5F8700),
    layer_utilities: rgb(0x7340BF),
    layer_other: rgb(0x8C9196),
    heat_low: rgb(0x5F8700),
    heat_mid: rgb(0xCC8800),
    heat_high: rgb(0xE65050),
    diff_add: rgb(0x6CBF43),
    diff_mod: rgb(0x478ACC),
    chip_view: rgb(0xA37ACC),
    section: rgb(0xFA8D3E),
    sql_keyword: rgb(0xFA8D3E),
    sql_string: rgb(0x86B300),
    sql_comment: rgb(0x787B80),
    sql_jinja: rgb(0xF2AE49),
    bar_resource: rgb(0x55B4D4),
    bar_materialization: rgb(0xA37ACC),
    bar_degree: rgb(0xF2AE49),
    bar_transitive: rgb(0x86B300),
};

/// gruvbox dark (<https://github.com/morhetz/gruvbox>): for a `#282828`
/// background; warm retro hues, bright variants as classes, faded as lenses.
const GRUVBOX_DARK: Theme = Theme {
    accent: rgb(0x83A598),
    border_idle: rgb(0x504945),
    text_bright: rgb(0xEBDBB2),
    text_dim: rgb(0xBDAE93),
    text_faint: rgb(0x7C6F64),
    lineage_dim: rgb(0x928374),
    surface: rgb(0x32302F),
    surface_hi: rgb(0x504945),
    sb_thumb: rgb(0x665C54),
    ok: rgb(0xB8BB26),
    warn: rgb(0xFABD2F),
    danger: rgb(0xFB4934),
    orphan: rgb(0xCC241D),
    gold: rgb(0xD79921),
    class_table: rgb(0xB8BB26),
    class_view: rgb(0x8EC07C),
    class_incremental: rgb(0xD3869B),
    class_ephemeral: rgb(0xB16286),
    class_source: rgb(0x83A598),
    class_seed: rgb(0xD5C4A1),
    class_snapshot: rgb(0xFE8019),
    class_other: rgb(0xA89984),
    layer_staging: rgb(0x458588),
    layer_intermediate: rgb(0xD79921),
    layer_marts: rgb(0x98971A),
    layer_utilities: rgb(0x8F3F71),
    layer_other: rgb(0xBDAE93),
    heat_low: rgb(0x98971A),
    heat_mid: rgb(0xD79921),
    heat_high: rgb(0xFB4934),
    diff_add: rgb(0x98971A),
    diff_mod: rgb(0xD79921),
    chip_view: rgb(0xD3869B),
    section: rgb(0xFE8019),
    sql_keyword: rgb(0xD3869B),
    sql_string: rgb(0xB8BB26),
    sql_comment: rgb(0x7C6F64),
    sql_jinja: rgb(0xFE8019),
    bar_resource: rgb(0x8EC07C),
    bar_materialization: rgb(0xD3869B),
    bar_degree: rgb(0xFABD2F),
    bar_transitive: rgb(0xB8BB26),
};

/// The built-in presets, in cycle/UI order: `(name, theme)`.
const PRESETS: &[(&str, Theme)] = &[
    ("default", DEFAULT),
    ("ayu-dark", AYU_DARK),
    ("ayu-mirage", AYU_MIRAGE),
    ("ayu-light", AYU_LIGHT),
    ("gruvbox-dark", GRUVBOX_DARK),
];

/// The built-in preset table, in cycle/UI order — the API for callers that
/// want all of them (the App's default theme list, the preset-loop tests),
/// so nobody round-trips name → lookup → `expect`.
pub fn presets() -> &'static [(&'static str, Theme)] {
    PRESETS
}

/// Look up a built-in preset by name (`None` for an unknown one).
pub fn preset(name: &str) -> Option<Theme> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// The built-in preset names, in cycle/UI order.
pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(n, _)| *n).collect()
}

// ---- custom theme files --------------------------------------------------------

/// The on-disk YAML shape of a custom theme: an optional `base` preset to start
/// from (default `"default"`) and a `colors:` map of role name → colour.
/// `deny_unknown_fields` so a typoed top-level key fails loudly instead of
/// silently doing nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, ColorSpec>,
}

/// One colour value in a theme file: an xterm-256 index (`114`) or a string —
/// `"#rrggbb"` truecolor hex, or a decimal index in quotes (`"114"`).
/// Integers deserialize as `i64` (not `u8`) so an out-of-range `300` reaches
/// [`parse_color`]'s range check and gets a NAMED error instead of serde's
/// opaque "did not match any variant".
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ColorSpec {
    Indexed(i64),
    Text(String),
}

fn parse_color(spec: &ColorSpec) -> Result<Color, String> {
    let text = match spec {
        ColorSpec::Indexed(i) => {
            return u8::try_from(*i)
                .map(Color::Indexed)
                .map_err(|_| format!("{i} is not an xterm-256 index (0-255)"));
        }
        ColorSpec::Text(t) => t.trim(),
    };
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("'{text}' is not a #rrggbb colour"));
        }
        let v = u32::from_str_radix(hex, 16).expect("validated hex");
        return Ok(rgb(v));
    }
    text.parse::<u8>()
        .map(Color::Indexed)
        .map_err(|_| format!("'{text}' is neither a #rrggbb colour nor an xterm-256 index (0-255)"))
}

/// Parse a custom theme from its YAML text: start from the `base` preset
/// (default: `default`) and overlay each `colors:` entry. Unknown base names,
/// unknown roles, and unparseable colours are errors (with the valid
/// alternatives listed); IO and name resolution stay in `main`.
pub fn parse_theme(text: &str) -> Result<Theme, String> {
    let file: ThemeFile =
        serde_norway::from_str(text).map_err(|e| format!("invalid theme YAML: {e}"))?;
    let mut theme = match file.base.as_deref() {
        None => DEFAULT,
        Some(name) => preset(name).ok_or_else(|| {
            format!(
                "unknown base theme '{name}' (presets: {})",
                preset_names().join(", ")
            )
        })?,
    };
    for (role, spec) in &file.colors {
        let color = parse_color(spec).map_err(|e| format!("colors.{role}: {e}"))?;
        if !theme.set_role(role, color) {
            return Err(format!(
                "unknown colour role '{role}' (valid roles: {})",
                ROLE_NAMES.join(", ")
            ));
        }
    }
    Ok(theme)
}

/// Check a theme against the palette contract and return human-readable
/// warnings (empty = clean): every lens tint (`danger` as the coverage /
/// violation tint, the heat ramp, the layer set) must differ from every class
/// colour (else that lens renders some node identically to lens-off), the heat
/// ramp steps must be distinct, and the five layer colours must be distinct.
/// The presets are asserted lint-clean by the tests; `main` surfaces these as
/// warnings for user themes (a custom theme may knowingly break them).
pub fn lint(theme: &Theme) -> Vec<String> {
    // Each role group is listed ONCE; the tint set is the concatenation of
    // danger + heat + layers (a tint is a tint regardless of which lens owns it).
    let classes = [
        ("class_table", theme.class_table),
        ("class_view", theme.class_view),
        ("class_incremental", theme.class_incremental),
        ("class_ephemeral", theme.class_ephemeral),
        ("class_source", theme.class_source),
        ("class_seed", theme.class_seed),
        ("class_snapshot", theme.class_snapshot),
        ("class_other", theme.class_other),
    ];
    let heat = [
        ("heat_low", theme.heat_low),
        ("heat_mid", theme.heat_mid),
        ("heat_high", theme.heat_high),
    ];
    let layers = [
        ("layer_staging", theme.layer_staging),
        ("layer_intermediate", theme.layer_intermediate),
        ("layer_marts", theme.layer_marts),
        ("layer_utilities", theme.layer_utilities),
        ("layer_other", theme.layer_other),
    ];
    let diff = [("diff_add", theme.diff_add), ("diff_mod", theme.diff_mod)];
    let danger = ("danger (coverage/violation lens tint)", theme.danger);

    let mut warnings = Vec::new();
    for (tint_name, tint) in std::iter::once(danger)
        .chain(heat)
        .chain(layers)
        .chain(diff)
    {
        for (class_name, class) in classes {
            if tint == class {
                warnings.push(format!(
                    "lens tint {tint_name} equals {class_name}: that lens will be \
                     invisible on those nodes"
                ));
            }
        }
    }
    // Within-group distinctness: a shared value collapses two grades/layers.
    let mut distinct = |items: &[(&str, Color)], consequence: &str| {
        for i in 0..items.len() {
            for j in i + 1..items.len() {
                if items[i].1 == items[j].1 {
                    warnings.push(format!(
                        "{} and {} share a colour: {consequence}",
                        items[i].0, items[j].0
                    ));
                }
            }
        }
    };
    distinct(&heat, "the DegreeHeat lens loses a grade");
    distinct(&layers, "the Layer lens cannot tell them apart");
    distinct(&diff, "the Diff lens cannot tell added from modified");
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_matches_the_legacy_constants() {
        assert_eq!(DEFAULT.accent, ACCENT);
        assert_eq!(DEFAULT.class_snapshot, CLASS_SNAPSHOT);
        assert_eq!(DEFAULT.section, SECTION, "alias preserved");
        assert_eq!(Theme::default(), DEFAULT);
    }

    #[test]
    fn every_preset_is_lint_clean() {
        for (name, theme) in presets() {
            let warnings = lint(theme);
            assert!(
                warnings.is_empty(),
                "preset '{name}' violates the palette contract:\n  {}",
                warnings.join("\n  ")
            );
        }
    }

    #[test]
    fn preset_lookup_is_by_exact_name() {
        assert!(preset("ayu-dark").is_some());
        assert!(preset("ayu").is_none());
        assert!(preset("AYU-DARK").is_none());
        assert_eq!(preset("default"), Some(DEFAULT));
        assert_eq!(preset_names()[0], "default", "default cycles first");
    }

    #[test]
    fn parse_theme_overlays_roles_on_the_base() {
        let theme = parse_theme("base: ayu-dark\ncolors:\n  accent: \"#112233\"\n  ok: 114\n")
            .expect("valid theme");
        assert_eq!(theme.accent, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(theme.ok, Color::Indexed(114));
        // Untouched roles come from the base preset, not the default.
        assert_eq!(theme.danger, AYU_DARK.danger);
    }

    #[test]
    fn parse_theme_defaults_base_to_default() {
        let theme = parse_theme("colors:\n  gold: \"220\"\n").expect("valid");
        assert_eq!(theme.gold, Color::Indexed(220));
        assert_eq!(theme.accent, DEFAULT.accent);
        // An empty file is exactly the default palette.
        assert_eq!(parse_theme("{}").expect("empty is valid"), DEFAULT);
    }

    #[test]
    fn parse_theme_rejects_unknown_roles_bases_and_colours() {
        let err = parse_theme("colors:\n  acent: \"#112233\"\n").unwrap_err();
        assert!(err.contains("unknown colour role 'acent'"), "{err}");
        assert!(err.contains("accent"), "error lists the valid roles: {err}");

        let err = parse_theme("base: nope\n").unwrap_err();
        assert!(err.contains("unknown base theme 'nope'"), "{err}");
        assert!(err.contains("ayu-dark"), "error lists the presets: {err}");

        let err = parse_theme("colors:\n  accent: \"#12345\"\n").unwrap_err();
        assert!(err.contains("colors.accent"), "{err}");

        let err = parse_theme("colors:\n  accent: \"mauve\"\n").unwrap_err();
        assert!(err.contains("neither"), "{err}");

        // An out-of-range index gets a NAMED range error, not serde's opaque
        // untagged-enum mismatch.
        let err = parse_theme("colors:\n  accent: 300\n").unwrap_err();
        assert!(err.contains("not an xterm-256 index"), "{err}");
        let err = parse_theme("colors:\n  accent: -1\n").unwrap_err();
        assert!(err.contains("not an xterm-256 index"), "{err}");

        // A typoed TOP-LEVEL key fails loudly (deny_unknown_fields).
        assert!(parse_theme("colours:\n  accent: 1\n").is_err());
    }

    #[test]
    fn role_names_cover_every_field_and_set_role_writes_it() {
        // set_role accepts exactly ROLE_NAMES (macro-generated from one list,
        // so this is a tautology guard against a hand-edited divergence) and
        // the sentinel lands on the named field.
        let sentinel = Color::Rgb(1, 2, 3);
        for role in ROLE_NAMES {
            let mut theme = DEFAULT;
            assert!(theme.set_role(role, sentinel), "role '{role}' settable");
            assert_ne!(theme, DEFAULT, "setting '{role}' changes the theme");
        }
        let mut theme = DEFAULT;
        assert!(!theme.set_role("not_a_role", sentinel));
    }
}
