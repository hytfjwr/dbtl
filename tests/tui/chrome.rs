use super::*;

// ============================================================================
// UI chrome: list-pane scrollbar, segmented status bar, accented layer headers
// ============================================================================

/// The list pane's right-border column for a `width`-wide terminal, resolved
/// through the same pure layout the renderer uses (the saved-width split,
/// not a ratio).
pub(super) fn list_right_border_x(width: u16) -> u16 {
    let list = dbtl::ui::pane_layout(
        ratatui::layout::Rect::new(0, 0, width, 24),
        &dbtl::ui::PaneConfig::default(),
    )
    .list;
    list.x + list.width - 1
}

/// Count thumb glyphs (Unicode `█`) drawn on the list pane's right-border column.
pub(super) fn list_scrollbar_thumb_cells(buffer: &Buffer, width: u16) -> usize {
    let x = list_right_border_x(width);
    let area = buffer.area;
    (0..area.height)
        .filter(|&y| buffer.cell((x, y)).map(|c| c.symbol()) == Some("█"))
        .count()
}

#[test]
pub(super) fn list_scrollbar_absent_when_list_fits() {
    // A tall buffer fits all 49 display rows in the list interior → no thumb.
    let buffer = render(100, 60);
    assert_eq!(
        list_scrollbar_thumb_cells(&buffer, 100),
        0,
        "no scrollbar thumb when the list fits the pane"
    );
}

#[test]
pub(super) fn list_scrollbar_present_when_list_overflows() {
    // A short buffer cannot fit 49 rows → the thumb appears on the right border.
    let buffer = render(100, 20);
    assert!(
        list_scrollbar_thumb_cells(&buffer, 100) >= 1,
        "a scrollbar thumb must render when the list overflows the pane"
    );
}

#[test]
pub(super) fn list_scrollbar_thumb_at_top_when_unscrolled() {
    // Unscrolled (offset 0): the thumb's top cell sits at the very top of the
    // track (interior row 1 of the bordered pane).
    let buffer = render(100, 20);
    let x = list_right_border_x(100);
    // Track top is area.y(0) + 1 border row = row 1.
    assert_eq!(
        buffer.cell((x, 1)).map(|c| c.symbol()),
        Some("█"),
        "offset 0 → thumb flush at the top of the track"
    );
}

#[test]
pub(super) fn list_scrollbar_thumb_flush_bottom_when_scrolled_to_end() {
    // Scroll to the bottom via `G`: the thumb's last cell sits on the track's
    // last interior row (one above the list pane's bottom border).
    let (buffer, state, _) = render_after_keys(100, 20, &[KeyCode::Char('G')]);
    assert!(state.offset() > 0, "G must scroll the short list");
    let x = list_right_border_x(100);
    // The list pane occupies the body (height 20 → status row 19 → body rows
    // 0..=18). Its bottom border is row 18, so the last interior row is 17 — the
    // flush-bottom thumb cell. Below it, the corner glyph (not a thumb) renders.
    assert_eq!(
        buffer.cell((x, 17)).map(|c| c.symbol()),
        Some("█"),
        "scrolled to the end → thumb flush at the bottom of the track (last interior row)"
    );
    assert_ne!(
        buffer.cell((x, 18)).map(|c| c.symbol()),
        Some("█"),
        "the thumb never overwrites the bottom border corner"
    );
}

#[test]
pub(super) fn status_bar_renders_hints_left_and_chips_right() {
    use dbtl::app::{apply_action, App};
    use dbtl::ui::StatusSegments;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    // Cycle to the coverage lens (Off → Coverage) so the cov chip populates.
    apply_action(&mut app, Action::CycleLens);
    let (tested, total) = app.coverage_summary();
    let pct = (tested * 100).checked_div(total).unwrap_or(0);
    let cov = format!("cov {pct}% ({tested}/{total})");
    let pos = format!(
        "{}/{}",
        app.ui_state.selected() + 1,
        app.active_list().len()
    );
    let hints = app.status_hints();

    // A wide line (160) so every hint + chip fits.
    let backend = TestBackend::new(160, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.hints = &hints;
        ctx.segments = StatusSegments {
            focus: Some("list"),
            position: Some(pos.as_str()),
            coverage: Some(cov.as_str()),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());

    // The list-focus hint row, generated from BINDINGS.
    assert!(text.contains("j/k move"), "hint row renders");
    assert!(text.contains("/ search"), "the search hint renders");
    // The colored chips render on the right, bracketed by the chip renderer.
    assert!(text.contains("[list]"), "focus chip renders");
    assert!(
        text.contains(&format!("[{pos}]")),
        "position chip renders bracketed"
    );
    assert!(
        text.contains(&format!("[{cov}]")),
        "coverage chip renders bracketed"
    );
}

#[test]
pub(super) fn status_bar_drops_the_hint_tail_then_the_chip_tail_on_a_narrow_line() {
    use dbtl::ui::StatusSegments;
    // The shed order on a 60-col line: the hints' low-priority tail drops
    // first (`? help` then `Tab lineage`), the kept hints + the chips that
    // still fit render.
    let list = fixture_list();
    let state = UiState::new(list.len());
    let hints = list_hints();
    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.hints = &hints;
        ctx.segments = StatusSegments {
            focus: Some("list"),
            position: Some("1/45"),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("j/k move"), "the head hints survive");
    assert!(
        !text.contains("? help"),
        "the hint tail sheds before the chips do"
    );
    assert!(text.contains("[1/45]"), "the chips still fit");
}

#[test]
pub(super) fn status_segments_render_at_80_cols() {
    // Regression guard: at the standard 80-col terminal the hint row AND a
    // chip must still render (the old monolithic core could crowd the whole
    // line out; the prefix-drop keeps the highest-priority pieces).
    use dbtl::app::{apply_action, App};
    use dbtl::ui::StatusSegments;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    apply_action(&mut app, Action::CycleLens);
    let hints = app.status_hints();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.hints = &hints;
        ctx.segments = StatusSegments {
            focus: Some("list"),
            position: Some("1/45"),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());

    assert!(text.contains("j/k move"), "the hint row renders at 80 cols");
    assert!(
        text.contains("fct_subscription_process"),
        "the selected node name stays on screen (lineage title)"
    );
    assert!(text.contains("[1/45]"), "a status chip renders at 80 cols");
}

#[test]
pub(super) fn status_impact_segment_renders_and_survives_narrowest_droppable_line() {
    // The impact chip is the top-survival chip (status_chips index 0): it
    // renders at a normal width and is the LAST chip dropped when the line
    // narrows.
    use dbtl::app::App;
    use dbtl::ui::StatusSegments;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let impact = app.impact_status().expect("selected → impact");
    assert_eq!(impact, "impact ↓1 ↑2", "unicode badges in the default mode");

    // At 100 cols every chip fits, so impact renders.
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.segments = StatusSegments {
            impact: Some(impact.as_str()),
            focus: Some("list"),
            view: Some("↑"),
            position: Some("3/45"),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let wide = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(wide.contains(&impact), "impact chip renders at 100 cols");
    assert!(wide.contains("[3/45]"), "position renders too at 100 cols");

    // On a narrow line the lower-priority chips shed tail-first while impact
    // survives (index 0 = last dropped). Width chosen so only the impact
    // chip fits (no hints wired → the chips are the whole line).
    use unicode_width::UnicodeWidthStr;
    let narrow_w = (format!(" [{impact}]").width() + 1) as u16;
    let backend = TestBackend::new(narrow_w, 6);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.segments = StatusSegments {
            impact: Some(impact.as_str()),
            focus: Some("list"),
            view: Some("↑"),
            position: Some("3/45"),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let narrow = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        narrow.contains(&impact),
        "impact survives as top-priority chip"
    );
    assert!(
        !narrow.contains("[3/45]"),
        "lower-priority position drops on the narrow line"
    );
}

#[test]
pub(super) fn status_impact_segment_uses_ascii_badges_in_ascii_mode() {
    // In ASCII glyph mode the segment carries the Chrome ASCII badges (v/^),
    // never the unicode arrows — so the segmented status bar stays ascii-safe.
    use dbtl::app::App;
    use dbtl::layout::GlyphMode;
    use dbtl::ui::StatusSegments;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.glyph_mode = GlyphMode::Ascii;
    app.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let impact = app.impact_status().expect("selected → impact");
    assert_eq!(impact, "impact v1 ^2", "ascii badges, never ↓/↑");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.glyphs = GlyphMode::Ascii;
        ctx.segments = StatusSegments {
            impact: Some(impact.as_str()),
            notify: None,
            column: None,
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains("impact v1 ^2"),
        "ascii impact segment renders"
    );
    assert!(
        !text.contains('↓') && !text.contains('↑'),
        "no unicode arrows"
    );
}

#[test]
pub(super) fn layer_header_renders_accented_and_capitalized() {
    // The staging header renders title-cased with a leading rule glyph, and its
    // per-layer accent color — NOT the shared overlay header_style.
    let buffer = render(100, 60);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("Staging ("),
        "staging header renders title-cased with its count"
    );
    assert!(text.contains('─'), "header carries the Unicode rule glyph");

    // Find a cell on the staging header line and assert its accent color.
    let area = buffer.area;
    let mut found_cyan_header = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            if let Some(c) = buffer.cell((x, y)) {
                row.push_str(c.symbol());
            }
        }
        if row.contains("Staging (") {
            // The 'S' of "Staging" carries the per-layer accent (LAYER_STAGING),
            // proving the accent path is distinct from the overlay header_style.
            for x in 0..area.width {
                if let Some(c) = buffer.cell((x, y)) {
                    if c.symbol() == "S" && c.fg == dbtl::ui::theme::LAYER_STAGING {
                        found_cyan_header = true;
                    }
                }
            }
        }
    }
    assert!(
        found_cyan_header,
        "the staging header carries its LAYER_STAGING accent (not the overlay header style)"
    );
}
