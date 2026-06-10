//! GlyphMode::Ascii guard: in ASCII mode, EVERY cell the UI emits must be plain
//! ASCII — panes, borders, lineage grid, scroll markers, badges, titles,
//! overlays, status line.
//!
//! This is the load-bearing guarantee for terminals that render
//! East-Asian-Ambiguous characters 2 cells wide (common in CJK-configured
//! setups / CJK font fallback), where ANY ambiguous glyph (`─ │ ┌ ▶ ◀ ▲ ▼ ↑ ↓
//! ≤ · ▏ …`) desyncs ratatui's 1-cell buffer model into doubled/ghosted
//! frames. Rendering every surface and scanning the whole buffer is the
//! exhaustive form of the check: no surface can sneak a non-ASCII glyph back
//! in without failing here.

use dbtl::app::{apply_action, App};
use dbtl::layout::{layout_mode, GlyphMode};
use dbtl::ui::{draw, Focus, RenderCtx, StatusSegments};
use dbtl::{load_dag, Action};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");
const FCT: &str = "model.jaffle_finance.fct_subscription_process";

fn ascii_app() -> App {
    let dag = load_dag(FIXTURE).expect("fixture loads");
    let mut app = App::new(dag, std::path::PathBuf::from(FIXTURE));
    app.glyph_mode = GlyphMode::Ascii;
    app.select_by_unique_id(FCT);
    app
}

/// Render the app exactly as the event loop does (layout in the app's glyph
/// mode, styles applied, Dag-derived strings passed) and return the buffer.
fn render(app: &App, width: u16, height: u16) -> Buffer {
    render_with_toast(app, width, height, None)
}

/// [`render`] plus an optional floating toast (the transient copy/bookmark
/// acknowledgement), mirroring the event loop's `ctx.toast` wiring.
fn render_with_toast(app: &App, width: u16, height: u16, toast: Option<&str>) -> Buffer {
    let sg = app.lineage_subgraph();
    let lineage = if sg.nodes.is_empty() {
        None
    } else {
        let mut lay = layout_mode(&sg, app.glyph_mode);
        lay.apply_node_styles(&app.lineage_styles(&lay));
        Some(lay)
    };
    let status = app.selected_status_note();
    let label = app.lineage_view_label();

    // Mirror the event loop's status-segment wiring so the segmented status bar
    // is exercised under the ASCII guard (segments are plain-ASCII bracket chrome;
    // `view` is glyph-mode-correct via lineage_view_label).
    let focus_s = match app.ui_state.focus() {
        Focus::List => "list",
        Focus::RightPane => "lineage",
    };
    let pos_s = format!(
        "{}/{}",
        app.ui_state.selected() + 1,
        app.active_list().len().max(1)
    );
    let cov_s = if app.ui_state.lens() == dbtl::ui::LineageLens::Coverage {
        let (tested, total) = app.coverage_summary();
        let pct = if total == 0 { 0 } else { tested * 100 / total };
        Some(format!("cov {pct}% ({tested}/{total})"))
    } else {
        None
    };
    let bm_s = if app.bookmarks.is_empty() {
        None
    } else {
        Some(format!("bm:{}", app.bookmarks.len()))
    };
    // The impact segment carries the Chrome down/up badges (ASCII `v`/`^`); feed a
    // REAL value so the guard actually scans the rendered badge glyphs for
    // ASCII-safety (a hardcoded `↓`/`↑` would fail this render).
    let impact_s = app.impact_status();
    // The re-root breadcrumb (ASCII " > " / ".." separators); `None` until the
    // app has re-root history, so most renders are unchanged. Exercised in
    // ASCII mode by the dedicated breadcrumb test below.
    let breadcrumb = app.breadcrumb(width as usize);

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_ref());
        ctx.mode = &app.mode;
        ctx.status = status.as_deref();
        ctx.stats = Some(&app.stats);
        ctx.lineage_label = Some(&label);
        ctx.breadcrumb = breadcrumb.as_deref();
        ctx.glyphs = app.glyph_mode;
        ctx.segments = StatusSegments {
            impact: impact_s.as_deref(),
            focus: Some(focus_s),
            view: Some(label.as_str()),
            position: Some(pos_s.as_str()),
            coverage: cov_s.as_deref(),
            bookmarks: bm_s.as_deref(),
            sort: None,
            filter: app.list_filter_label(),
        };
        // Mirror the loop's filter-tag wiring so the `[untested]`/`[marked]`
        // title tag is scanned whenever a guard scenario toggles a filter.
        ctx.filter_label = app.list_filter_label();
        // Mirror the loop's minimap wiring so the ASCII guard scans the inset's
        // `#`/`.`/`+` glyphs whenever the lens is toggled on (all pure ASCII).
        ctx.minimap = app.ui_state.minimap_visible();
        ctx.toast = toast;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    terminal.backend().buffer().clone()
}

/// Every cell symbol in the buffer must be ASCII **or unambiguously wide**.
///
/// The failure class is East-Asian-AMBIGUOUS width: chars `unicode-width`
/// counts as 1 cell but ambiguous-wide terminals draw as 2 (`─ ▶ ↑ ≤ · …`).
/// Chars that are properly Wide (width 2 — e.g. CJK in a model/column
/// description, which is DATA, not chrome) are measured as 2 by both sides and
/// render fine everywhere, so they pass. Anything non-ASCII with width 1 is
/// exactly the desync risk and fails.
fn assert_all_ascii(buffer: &Buffer, surface: &str) {
    use unicode_width::UnicodeWidthStr;
    let area = buffer.area;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                let sym = cell.symbol();
                assert!(
                    sym.is_ascii() || sym.width() == 2,
                    "{surface}: ambiguous-width glyph {sym:?} at ({x},{y}) in ASCII glyph mode"
                );
            }
        }
    }
}

#[test]
fn selection_screen_with_clipped_lineage_is_all_ascii() {
    // fct at 80x24 overflows the lineage pane, so this also exercises the
    // scroll markers, the lineage grid + arrowheads, the list badges, the
    // depth/direction label, and the status note.
    let mut app = ascii_app();
    apply_action(&mut app, Action::DepthDecrease); // label shows "<=3"
    let buffer = render(&app, 80, 24);
    assert_all_ascii(&buffer, "selection 80x24");
    // And at a size where the diagram fits (no markers; centred pad).
    let buffer = render(&app, 200, 60);
    assert_all_ascii(&buffer, "selection 200x60");
}

#[test]
fn persistent_filter_tags_and_coverage_badges_are_all_ascii() {
    // The `[marked]` / `[untested]` title tag + status segment, the `*` ASCII
    // bookmark badge, and the Coverage-lens ` t:N` row badge, all at once.
    let mut app = ascii_app();
    apply_action(&mut app, Action::BookmarkToggle);
    apply_action(&mut app, Action::ToggleBookmarkFilter);
    apply_action(&mut app, Action::CycleLens); // Coverage -> t:N badges
    assert_all_ascii(&render(&app, 80, 24), "bookmarked filter 80x24");
    apply_action(&mut app, Action::ToggleUntestedFilter);
    assert_all_ascii(&render(&app, 80, 24), "untested filter 80x24");
}

#[test]
fn list_and_lineage_search_titles_are_all_ascii() {
    // The live-query caret in both search targets.
    let mut app = ascii_app();
    apply_action(&mut app, Action::SearchOpen); // list focus -> list search
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&app, 100, 30), "list search");
    apply_action(&mut app, Action::SearchCancel);

    app.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut app, Action::SearchOpen); // lineage search
    for c in "dim".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&app, 100, 30), "lineage search");
}

#[test]
fn help_overlay_is_all_ascii() {
    let mut app = ascii_app();
    apply_action(&mut app, Action::HelpToggle);
    assert_all_ascii(&render(&app, 100, 40), "help overlay");
}

#[test]
fn breadcrumb_title_is_all_ascii() {
    // Re-root twice so the lineage pane title carries a multi-entry breadcrumb
    // ("a > b > root"); its separators (" > ", "..") must stay pure ASCII.
    let mut app = ascii_app(); // rooted at fct_subscription_process
    app.jump_to("model.jaffle_finance.wt_delivery_base_metrics");
    app.jump_to("model.jaffle_finance.fct_delivery_monthly_snapshot");
    assert!(
        app.breadcrumb(200).is_some(),
        "two jumps produced a breadcrumb"
    );
    assert_all_ascii(&render(&app, 100, 30), "breadcrumb title");
    // And at a narrow width that forces the ".." left-truncation path.
    assert_all_ascii(&render(&app, 40, 30), "breadcrumb truncated");
}

#[test]
fn detail_modal_is_all_ascii() {
    let mut app = ascii_app();
    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::DetailOpen);
    assert_all_ascii(&render(&app, 100, 40), "detail modal");
}

#[test]
fn sql_modal_is_all_ascii() {
    // pos_txn has clean ASCII SQL — the syntax-highlighting recolours bytes only,
    // so the modal stays pure ASCII (the keyword spans add no glyphs).
    let mut app = ascii_app();
    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::SqlOpen);
    assert_all_ascii(&render(&app, 100, 40), "sql modal");
}

#[test]
fn stats_modal_is_all_ascii() {
    // The coverage bar is plain ASCII (#/-); section headers carry no chrome.
    let mut app = ascii_app();
    apply_action(&mut app, Action::StatsOpen);
    assert_all_ascii(&render(&app, 100, 40), "stats modal");
}

#[test]
fn palette_modal_is_all_ascii() {
    // The palette chrome (border / caret) + the dim key labels are pure ASCII in
    // Ascii mode; the highlighted help chars only recolour bytes (add no glyphs).
    let mut app = ascii_app();
    apply_action(&mut app, Action::PaletteOpen);
    for c in "toggle".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&app, 100, 40), "command palette");
    // A short height forces the candidate list to overflow → the right-border
    // scrollbar thumb renders (ASCII `#`).
    assert_all_ascii(&render(&app, 100, 12), "command palette overflow scrollbar");
}

#[test]
fn overflowing_overlays_with_scrollbars_are_all_ascii() {
    // Force a short height (100x12) so the help / detail / sql / stats modals all
    // overflow their box → the right-border scrollbar thumb renders in ASCII
    // (`#`), the case the taller fixtures above do not reach.
    let mut app = ascii_app();
    apply_action(&mut app, Action::HelpToggle);
    assert_all_ascii(&render(&app, 100, 12), "help overlay overflow scrollbar");
    apply_action(&mut app, Action::HelpToggle); // close

    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::DetailOpen);
    assert_all_ascii(&render(&app, 100, 12), "detail modal overflow scrollbar");
    apply_action(&mut app, Action::DetailClose); // all modals close via DetailClose

    apply_action(&mut app, Action::SqlOpen);
    assert_all_ascii(&render(&app, 100, 12), "sql modal overflow scrollbar");
    apply_action(&mut app, Action::DetailClose);

    apply_action(&mut app, Action::StatsOpen);
    assert_all_ascii(&render(&app, 100, 12), "stats modal overflow scrollbar");
}

#[test]
fn every_lens_with_off_root_cursor_is_all_ascii() {
    // Every lineage lens AND an off-root cursor active at once: each lens tints the
    // boxes (COLOUR-only fg), the off-root cursor dims off-path nodes (DarkGray fg)
    // + bands the path (Indexed(236) bg), and the lens adds an ASCII title suffix
    // (` [lens:...]`). All colour/ASCII — no ambiguous-width glyph — so the ASCII
    // surface must stay pure ASCII across the WHOLE cycle. The key regression guard
    // that the lens + dim overlays add no ambiguous-width glyph.
    let mut app = ascii_app();
    app.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut app, Action::MoveLeft); // cursor off-root → non-empty path
    assert_ne!(
        app.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "precondition: the cursor walked off the root (path is non-empty)"
    );
    // Walk the full lens cycle (Off → Coverage → Heat → Layer → Violation → Off).
    for step in 0..=5 {
        // At 80x24 the diagram overflows (lens tint + dim + scroll markers + list
        // tint exercised); 200x60 fits (centred pad, no markers).
        assert_all_ascii(
            &render(&app, 80, 24),
            &format!("lens step {step} + off-root cursor 80x24"),
        );
        assert_all_ascii(
            &render(&app, 200, 60),
            &format!("lens step {step} + off-root cursor 200x60"),
        );
        apply_action(&mut app, Action::CycleLens);
    }
}

#[test]
fn minimap_inset_is_all_ascii() {
    // Minimap ON: the inset stamps `#`/`.`/`+` glyphs over the top-right of the
    // lineage interior. These are pure ASCII in BOTH glyph modes (the minimap has
    // no Unicode repertoire), so the ASCII surface stays pure ASCII. fct at 80x24
    // overflows the pane, so the minimap guard fires and the inset renders.
    let mut app = ascii_app();
    apply_action(&mut app, Action::ToggleMinimap);
    assert!(app.ui_state.minimap_visible(), "minimap toggled on");
    assert_all_ascii(&render(&app, 80, 24), "minimap inset 80x24");
}

#[test]
fn toast_overlay_is_all_ascii() {
    // The toast floats over the top-right with the glyph-mode border set, and
    // its overflow truncation appends a plain ".." (never an ellipsis glyph).
    let app = ascii_app();
    let buffer = render_with_toast(&app, 80, 24, Some("Copied unique_id"));
    assert_all_ascii(&buffer, "toast 80x24");
    // A toast wider than the screen budget exercises the truncation path.
    let long =
        "Removed bookmark: some_extremely_long_model_name_that_cannot_possibly_fit_in_the_box";
    assert_all_ascii(
        &render_with_toast(&app, 40, 24, Some(long)),
        "truncated toast 40x24",
    );
}
