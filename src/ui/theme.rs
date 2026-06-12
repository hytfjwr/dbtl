//! The visual theme: every colour the UI paints, as SEMANTIC constants in one
//! place. Render code never names a raw colour — it names a role (`ACCENT`,
//! `DANGER`, `CLASS_TABLE`, …), so a palette retune is a one-file edit and the
//! integration tests (which assert against these constants, not literals)
//! survive it.
//!
//! All values are `Color::Indexed` from the xterm-256 cube: universally
//! supported by modern terminals (truecolor is not), and — unlike the 16 named
//! ANSI colours — NOT remapped by the user's terminal scheme, so the curated
//! palette renders as designed. The palette assumes a dark background (soft
//! pastel foregrounds over dark surface bands), tuned in the spirit of modern
//! dark themes (Tokyo Night / Catppuccin).
//!
//! **Two saturation families, on purpose.** Materialization classes (the
//! lens-off resting colours) are PASTEL; lens tints (`LAYER_*`, `HEAT_*`,
//! `DANGER`-as-tint) are VIVID. An active lens therefore reads as "the
//! saturation turned up", and — the load-bearing part — every lens tint is
//! distinct from EVERY class colour, so a lens can never render a node
//! identically to lens-off (guarded by `lens_tints_never_collide_with_class_
//! colours` in `ui::lineage`). Two lenses never co-occur, so tints may reuse
//! values ACROSS lenses.
//!
//! Where two roles deliberately share a value, one ALIASES the other (e.g.
//! [`SECTION`] = [`CLASS_SNAPSHOT`]) so the coupling is visible and a retune
//! propagates — or is consciously split.
//!
//! Colours only — glyphs stay in [`Chrome`](super::Chrome)/`BoxGlyphs` (the
//! glyph-mode seam), and style COMPOSITION (precedence, modifiers) stays in
//! the render fns. ASCII-safety is untouched by definition: a colour never
//! adds a glyph.

use ratatui::style::Color;

// ---- core chrome -----------------------------------------------------------

/// The primary accent: focused-pane borders/titles, modal frames, carets,
/// the selection bar (via REVERSED), the sort chip. Soft blue `#87afff`.
pub const ACCENT: Color = Color::Indexed(111);
/// Unfocused pane borders — recede into the background.
pub const BORDER_IDLE: Color = Color::Indexed(238);
/// Bright body text (the selected-name echo in the status bar, modal titles).
pub const TEXT_BRIGHT: Color = Color::Indexed(254);
/// Secondary text: titles of unfocused panes, status-bar core, key labels.
pub const TEXT_DIM: Color = Color::Indexed(245);
/// Tertiary text: dependency badges, minimap.
pub const TEXT_FAINT: Color = Color::Indexed(240);
/// Off-path lineage nodes under the focus dim (cursor off-root). Deliberately
/// BRIGHTER than [`TEXT_FAINT`]: the dim must recede from the path band yet
/// stay readable — the off-path nodes are context, not noise. Distinct from
/// [`TEXT_DIM`]/[`SB_THUMB`]/[`CLASS_OTHER`] so no surface aliases by accident.
pub const LINEAGE_DIM: Color = Color::Indexed(243);
/// The status-bar background band.
pub const SURFACE: Color = Color::Indexed(234);
/// A raised surface: the root↔cursor path band, gauge tracks.
pub const SURFACE_HI: Color = Color::Indexed(237);
/// The right-border scrollbar thumb.
pub const SB_THUMB: Color = Color::Indexed(244);

// ---- semantic states -------------------------------------------------------

/// Good / passing: the coverage-gauge high grade, the focus chip. `#87d787`.
pub const OK: Color = Color::Indexed(114);
/// Caution: coverage chip + mid gauge grade, lineage scroll markers, the
/// top-hub stats bar. `#d7af5f`.
pub const WARN: Color = Color::Indexed(179);
/// Problem: impact chip, low gauge grade, coverage-gap names, and the
/// `Warn`/`Violation`/`HeatHigh` lens tints (vivid — no class colour is red,
/// so the tint can never disappear into one). `#ff5f5f`.
pub const DANGER: Color = Color::Indexed(203);
/// A leaf model with no downstream (a quieter red than [`DANGER`]).
pub const ORPHAN: Color = Color::Indexed(167);
/// Search/palette match-char highlight, the bookmark star + chip. `#ffd700`.
pub const GOLD: Color = Color::Indexed(220);

// ---- materialization classes (lineage box colours; the PASTEL family) -------

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

// ---- layers (list headers + the Layer lens; the VIVID family) ---------------
//
// Five DISTINCT colours, each also distinct from every CLASS_* value (see the
// module doc): vivid cyan/mustard/green/violet/silver vs the pastel classes.

pub const LAYER_STAGING: Color = Color::Indexed(38); // vivid cyan `#00afd7`
pub const LAYER_INTERMEDIATE: Color = Color::Indexed(178); // mustard `#d7af00`
pub const LAYER_MARTS: Color = Color::Indexed(35); // vivid green `#00af5f`
pub const LAYER_UTILITIES: Color = Color::Indexed(165); // violet `#d700ff`
pub const LAYER_OTHER: Color = Color::Indexed(250); // silver `#bcbcbc`

// ---- degree-heat ramp (DegreeHeat lens; VIVID — distinct from every class) --

/// Low heat — vivid green (shares the value with [`LAYER_MARTS`]; lenses are
/// mutually exclusive, so cross-lens reuse never collides on screen).
pub const HEAT_LOW: Color = Color::Indexed(35);
/// Mid heat — vivid gold (the [`GOLD`] hue; match highlights live in the list
/// pane, the heat tint in the lineage pane).
pub const HEAT_MID: Color = Color::Indexed(220);
/// High heat — the [`DANGER`] red.
pub const HEAT_HIGH: Color = DANGER;

// ---- status-bar chips --------------------------------------------------------

/// The `[view]` direction/depth chip — orchid hue (a chip role, deliberately
/// NOT [`CLASS_VIEW`]: the chip names the lineage view filter, not a
/// materialization).
pub const CHIP_VIEW: Color = Color::Indexed(176);

// ---- overlays ---------------------------------------------------------------

/// Overlay section headers (`columns:` / `tests:` / stats sections). Aliases
/// the snapshot peach on purpose — a shared warm-accent hue across unrelated
/// surfaces (modal headers vs lineage boxes) that never compete for meaning.
pub const SECTION: Color = CLASS_SNAPSHOT;
/// SQL keyword highlight in the `s` preview. Editor-style purple.
pub const SQL_KEYWORD: Color = Color::Indexed(141);
/// SQL string literals in the `s` preview. Soft green `#afd787` — the classic
/// editor convention; distinct from every chrome/status role.
pub const SQL_STRING: Color = Color::Indexed(150);
/// SQL comments (`--`, `/* */`, and Jinja `{# #}`) in the `s` preview. Aliases
/// [`TEXT_FAINT`] on purpose: comments recede like tertiary chrome text.
pub const SQL_COMMENT: Color = TEXT_FAINT;
/// Jinja expressions/statements (`{{ ref(..) }}` / `{% if %}`) in the `s`
/// preview. Aliases [`SECTION`]'s warm peach on purpose: the templating layer
/// is the load-bearing part of a dbt model, so it shares the accent family.
pub const SQL_JINJA: Color = SECTION;

// ---- stats mini-bars (decorative chart fills, one per chart) ----------------

pub const BAR_RESOURCE: Color = Color::Indexed(116); // teal
pub const BAR_MATERIALIZATION: Color = Color::Indexed(176); // orchid
pub const BAR_DEGREE: Color = WARN;
pub const BAR_TRANSITIVE: Color = OK;
