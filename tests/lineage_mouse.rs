//! the mouse overlays rendered through `draw` on a `TestBackend`: the
//! hovered node's accent frame plus its direct-edge connector cells, every
//! lineage-search match's accent frame with the `i/N` title, and the
//! settled-hover tooltip. All of them are post-blit frame-buffer effects —
//! the assertion split that pins the core invariant: cell SYMBOLS must
//! stay byte-identical whether or not the overlays are fed (the golden
//! fixtures live in `lineage_layout`; this guards the frame-buffer level).
//!
//! Headless geometry at 100x30 with NO tab strip: the lineage interior is
//! 35..99 x 1..28 and the 114x7 fixture grid centres into a 64x7 content rect
//! at (35,11) — so grid (gx,gy) lands on screen (35+gx, 11+gy) at scroll 0.

mod common;
use common::FIXTURE_MANIFEST as FIXTURE;
use dbtl::action::{Mode, SearchState};
use dbtl::layout::Layout;
use dbtl::model_list::{build_model_list, ModelList, SortMode};
use dbtl::ui::{draw, pane_interior, pane_layout, PaneConfig, RenderCtx, TooltipView, UiState};
use dbtl::{load_dag, Dag, GlyphMode, SearchTarget};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

const STG: &str = "model.jaffle_finance.stg_finance__budgets";
const SEED: &str = "seed.jaffle_finance.source_datetime_policy";
const RPT: &str = "model.jaffle_finance.rpt_delivery_base_metrics";

/// The DEFAULT theme's accent (`#87afff`) — the only colour the
/// overlays introduce. Kept as a literal so a theme retune fails loudly here
/// rather than silently passing on a stale constant.
const ACCENT: Color = Color::Indexed(111);

/// Everything the loop hands the overlays, bundled so `render_with`
/// can keep a signature instead of a borrow puzzle (the ctx fields borrow the
/// CALLER's data, so a closure over locals can't feed them).
#[derive(Default)]
struct Scene<'a> {
    mode: Option<&'a Mode>,
    hover: Option<&'a str>,
    matches: &'a [String],
    search_pos: Option<(usize, usize)>,
    tip: Option<&'a TooltipView>,
    glyphs: GlyphMode,
    scroll: (usize, usize),
}

fn fixture(glyphs: GlyphMode) -> (Dag, ModelList, Layout) {
    let dag = load_dag(FIXTURE).expect("fixture manifest must load");
    let list = build_model_list(&dag, SortMode::Layer);
    // The pane shows the subgraph rooted on the fixture's root model — the
    // same 5-node view `App::new` lands on (stg + its seed/source parents,
    // the rpt child, the exposure grandchild). `layout_mode` mirrors the
    // event loop's choice: the grid's glyphs follow the app's glyph mode.
    let lay = dbtl::layout::layout_mode(&dag.subgraph(STG), glyphs);
    (dag, list, lay)
}

/// Render the full UI at `w`x`h` through the same `draw` seam the event loop
/// uses. Scroll is `scene.scroll` (no anchor pass), so the grid->screen
/// mapping in the module doc holds.
fn render_with(w: u16, h: u16, scene: &Scene) -> (Buffer, Rect) {
    let (_dag, list, lay) = fixture(scene.glyphs);
    let mut state = UiState::new(list.len());
    state.pan_lineage(scene.scroll.0, scene.scroll.1);
    let area = Rect::new(0, 0, w, h);
    let interior = pane_interior(pane_layout(area, &PaneConfig::default()).lineage);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, Some(&lay));
        if let Some(mode) = scene.mode {
            ctx.mode = mode;
        }
        ctx.glyphs = scene.glyphs;
        ctx.hover_node = scene.hover;
        ctx.search_matches = scene.matches;
        ctx.search_pos = scene.search_pos;
        ctx.tooltip = scene.tip;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    (terminal.backend().buffer().clone(), interior)
}

/// The symbols of a single buffer row as one string (border + title row).
fn row_text(buffer: &Buffer, y: u16) -> String {
    let area = buffer.area;
    (0..area.width)
        .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(""))
        .collect()
}

/// Every cell's GLYPH must be identical between two renders — the style-only
/// contract: accents and search frames may repaint fg/modifiers, never move a
/// character (the diamond fixture's byte identity is the CharGrid-level form
/// of this same rule).
fn assert_same_glyphs(a: &Buffer, b: &Buffer) {
    let area = a.area;
    assert_eq!(area, b.area);
    for y in 0..area.height {
        for x in 0..area.width {
            let (ca, cb) = (a.cell((x, y)).unwrap(), b.cell((x, y)).unwrap());
            assert_eq!(
                ca.symbol(),
                cb.symbol(),
                "glyph drifted at ({x},{y}): {:?} -> {:?}",
                ca.symbol(),
                cb.symbol()
            );
        }
    }
}

fn cell_fg(b: &Buffer, x: u16, y: u16) -> Color {
    b.cell((x, y)).unwrap().fg
}

#[test]
fn hover_accents_the_node_frame_and_its_direct_edges() {
    let (base, _interior) = render_with(100, 30, &Scene::default());
    let (hovered, _interior) = render_with(
        100,
        30,
        &Scene {
            hover: Some(SEED),
            ..Scene::default()
        },
    );

    // seed's box is grid 0..26 x 0..3 -> screen 35..61 x 11..14: the perimeter
    // (top/bottom rows + left/right columns) accents, the label interior keeps
    // its own class colour (never accented by the frame pass).
    for (x, y) in [
        (35, 11),
        (60, 11),
        (35, 13),
        (60, 13),
        (35, 12),
        (60, 12),
        (47, 11),
        (47, 13),
    ] {
        let c = hovered.cell((x, y)).unwrap();
        assert_eq!(c.fg, ACCENT, "frame cell ({x},{y})");
        assert!(
            c.modifier.contains(Modifier::BOLD),
            "frame cell ({x},{y}) carries BOLD"
        );
    }
    let label = hovered.cell((40, 12)).unwrap();
    assert_ne!(label.fg, ACCENT, "the label interior is not frame chrome");

    // seed's only incident edge (seed -> stg, grid 26..29 at row 1) accents:
    // the connector cells the hover spec lights, all outside every node box.
    for (x, y) in [(61, 12), (62, 12), (63, 12)] {
        assert_eq!(cell_fg(&hovered, x, y), ACCENT, "edge cell ({x},{y})");
    }
    // An unrelated node frame is untouched.
    assert_ne!(
        cell_fg(&hovered, 64, 11),
        ACCENT,
        "stg's frame is not part of seed's hover"
    );
    // And the whole frame is style-only: not one glyph moved.
    assert_same_glyphs(&base, &hovered);
}

#[test]
fn hover_accent_follows_the_scroll_window() {
    // Scroll right by 10: frame cells left of the window drop, the rest map
    // 1:1 — the accent pass runs through the SAME scroll as the blit.
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            hover: Some(SEED),
            scroll: (10, 0),
            ..Scene::default()
        },
    );
    // seed's top frame at grid y0: grid x10..25 -> screen 35..50.
    for x in 35..=50 {
        assert_eq!(cell_fg(&b, x, 11), ACCENT, "scrolled frame cell ({x},11)");
    }
}

#[test]
fn search_matches_accent_every_match_frame() {
    let matches = vec![SEED.to_string(), RPT.to_string()];
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            matches: &matches,
            ..Scene::default()
        },
    );
    // seed's frame (35..61 x 11..14) and rpt's visible left column (grid x56
    // -> screen 91, the rest of its box clipped off the window).
    for (x, y) in [
        (35, 11),
        (60, 11),
        (35, 12),
        (60, 12),
        (35, 13),
        (60, 13),
        (91, 11),
        (91, 12),
        (91, 13),
    ] {
        assert_eq!(cell_fg(&b, x, y), ACCENT, "match frame cell ({x},{y})");
    }
    // A non-match frame is not accented (stg is neither hovered nor matched).
    assert_ne!(cell_fg(&b, 64, 11), ACCENT, "stg is not a match");
}

#[test]
fn lineage_search_title_shows_query_and_position() {
    // `Mode::Search(Lineage)` + the loop-computed position: the title owns
    // the pane's top border as `/{query}{caret} i/N` (unicode caret `▏`).
    let mode = Mode::Search(SearchState {
        target: SearchTarget::Lineage,
        query: "budgets".to_string(),
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            mode: Some(&mode),
            search_pos: Some((1, 2)),
            ..Scene::default()
        },
    );
    let title_row = row_text(&b, 0);
    assert!(
        title_row.contains("/budgets▏ 1/2"),
        "the search title rides the lineage border: {title_row}"
    );
    // No matches: the position renders as 0/0 rather than vanishing.
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            mode: Some(&mode),
            search_pos: Some((0, 0)),
            ..Scene::default()
        },
    );
    assert!(
        row_text(&b, 0).contains("/budgets▏ 0/0"),
        "a zero-match search still shows the position"
    );
}

#[test]
fn tooltip_draws_name_tag_stats_beside_the_pointer() {
    // Anchored at (40,12): the box hangs below-right — x41..76, y13..18 —
    // with the name on its title border, the tag + description + stats inside.
    let tip = TooltipView {
        name: "stg_finance__budgets".to_string(),
        tag: "View model".to_string(),
        desc: vec!["Rollup of the budgets staging table".to_string()],
        tests: 3,
        up: 2,
        down: 1,
        at: (40, 12),
    };
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            tip: Some(&tip),
            ..Scene::default()
        },
    );
    assert!(
        row_text(&b, 13).contains("stg_finance__budgets"),
        "the tooltip title names the node"
    );
    assert!(
        row_text(&b, 14).contains("View model"),
        "the kind tag rides inside"
    );
    assert!(
        row_text(&b, 15).contains("Rollup of the budgets staging table"),
        "the description line rides inside"
    );
    assert!(
        row_text(&b, 16).contains("tests:3"),
        "the stats row rides inside"
    );
    // The unicode-mode border glyph: a box, not bare text.
    assert_eq!(b.cell((41, 13)).unwrap().symbol(), "╭");
}

#[test]
fn tooltip_flips_left_when_the_right_side_overflows() {
    // Pointer near the right edge: the box can not hang below-right, so
    // `tooltip_rect` lands it to the LEFT of the pointer instead of clipping.
    let tip = TooltipView {
        name: "m".to_string(),
        tag: "Source".to_string(),
        desc: vec![],
        tests: 0,
        up: 0,
        down: 0,
        at: (95, 10),
    };
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            tip: Some(&tip),
            ..Scene::default()
        },
    );
    // The box drew (its tag rides the first interior row) and its top border
    // sits wholly left of the pointer column — the flip rule: had it anchored
    // right at x96 its right corner would clip at the screen edge (x>=95).
    assert!(
        row_text(&b, 12).contains("Source"),
        "the tooltip drew its tag"
    );
    for x in 95..b.area.width {
        let sym = b.cell((x, 11)).unwrap().symbol();
        assert!(
            sym != "╭" && sym != "╮",
            "a box corner at x{x} means the tooltip overflowed the pointer"
        );
    }
}

#[test]
fn overlays_never_move_a_glyph() {
    // The invariant the whole issue stands on: feeding the accent overlays
    // changes only styles, never a symbol — the buffer-level form of the
    // golden fixtures' byte identity.
    let (a, _interior) = render_with(100, 30, &Scene::default());
    let matches = vec![SEED.to_string(), STG.to_string()];
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            hover: Some(RPT),
            matches: &matches,
            ..Scene::default()
        },
    );
    assert_same_glyphs(&a, &b);
}

#[test]
fn ascii_mode_search_title_and_tooltip_are_ascii() {
    // The integration-level mirror of the ascii_guard check: in ASCII glyph
    // mode the caret becomes `_` and the tooltip box is `+`/`-`/`|` — the
    // ambiguous-width glyphs (`▏`, `─`, `│`, `↑`, `↓`) never leak.
    let mode = Mode::Search(SearchState {
        target: SearchTarget::Lineage,
        query: "budgets".to_string(),
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    let tip = TooltipView {
        name: "seed".to_string(),
        tag: "Seed".to_string(),
        desc: vec![],
        tests: 1,
        up: 0,
        down: 2,
        at: (40, 12),
    };
    let (b, _interior) = render_with(
        100,
        30,
        &Scene {
            glyphs: GlyphMode::Ascii,
            mode: Some(&mode),
            search_pos: Some((1, 2)),
            tip: Some(&tip),
            ..Scene::default()
        },
    );
    assert!(
        row_text(&b, 0).contains("/budgets_ 1/2"),
        "ASCII caret + i/N"
    );
    let area = b.area;
    for y in 0..area.height {
        for x in 0..area.width {
            let sym = b.cell((x, y)).unwrap().symbol();
            assert!(
                sym.is_ascii() || unicode_width::UnicodeWidthStr::width(sym) == 2,
                "ambiguous-width glyph {sym:?} at ({x},{y})"
            );
        }
    }
}
