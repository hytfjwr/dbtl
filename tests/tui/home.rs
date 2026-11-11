use super::*;

// ============================================================================
// Issue 10: the home screen
// ============================================================================

/// The home frame exactly as the event loop wires it: the app's own
/// `home_data` snapshot in `ctx.home`, the `⌂`-led tab strip, and the mode
/// (a home-target search swaps the body to its results).
pub(super) fn render_home(app: &dbtl::App, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let home = app.home_data();
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.glyphs = app.glyph_mode;
        ctx.home = Some(&home);
        ctx.home_sel = app.home_sel;
        ctx.home_scroll = app.home_scroll;
        let tab_labels = app.tab_labels();
        let panes = dbtl::ui::pane_layout(
            ratatui::layout::Rect::new(0, 0, width, height),
            &app.ui_state.pane_config(),
        );
        let tab_layout = app.tab_bar_layout_for(panes.tab_bar, true);
        ctx.tab_bar = Some(dbtl::ui::TabBarView {
            labels: &tab_labels,
            layout: &tab_layout,
            active: app.active_tab(),
            project: Some(app.stats.project.as_str()),
            hover: None,
        });
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("home render must not panic");
    }
    terminal.backend().buffer().clone()
}

#[test]
pub(super) fn too_small_screen_shows_only_the_message() {
    // §12: below 50x12 the whole frame is the one message — no pane, status,
    // tab strip, or overlay cell may leak through.
    let backend = TestBackend::new(48, 11);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            dbtl::ui::draw_too_small(
                frame,
                dbtl::layout::GlyphMode::Unicode,
                &dbtl::ui::theme::DEFAULT,
            )
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("terminal too small: 48\u{d7}11 (need 50\u{d7}12)"),
        "the current size and the floor both report"
    );
    // `Clear` wiped the whole buffer: every non-message cell is blank.
    let msg_row = 11 / 2;
    for y in 0..11u16 {
        for x in 0..48u16 {
            let cell = buffer.cell((x, y)).unwrap();
            if y != msg_row {
                assert_eq!(cell.symbol(), " ", "({x},{y}) must be blank");
            }
        }
    }
    // Nothing pane-like survived the clear pass.
    assert!(!text.contains('╭') && !text.contains('│') && !text.contains("sel:"));
}

#[test]
pub(super) fn narrow_band_hides_the_inline_list() {
    // §12: below 80 columns the list never sits inline — `pane_layout` zeroes
    // its rect regardless of the pref, so the lineage owns the whole body.
    let buffer = render(70, 24);
    let text = buffer_to_string(&buffer);
    assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "╭");
    assert_eq!(
        buffer.cell((69, 0)).unwrap().symbol(),
        "╮",
        "the lineage border reaches the right edge"
    );
    assert!(
        !text.contains("Staging"),
        "no group header draws while the inline list is gone"
    );
    assert!(
        !text.lines().any(|l| l.contains("> ")),
        "no selection marker draws inline"
    );
    // Exactly 80 restores the inline pane — the band edge is `<`, not `<=`.
    let buffer = render(80, 24);
    assert_eq!(
        buffer.cell((0, 0)).unwrap().symbol(),
        "╭",
        "the list pane's own corner is back"
    );
    assert_eq!(buffer.cell((33, 0)).unwrap().symbol(), "╮");
    assert!(buffer_to_string(&buffer).contains("Staging"));
}

#[test]
pub(super) fn narrow_band_drawer_renders_the_list_over_the_lineage() {
    // §12: `Ctrl-b` in the band floats the SAME list pane — `min(34, 60%)`
    // wide, the body's full height — over the lineage's left edge.
    let buffer = render_with(70, 24, |s| {
        s.set_list_auto_hidden(true); // the loop's <80 verdict
        s.set_list_drawer(true);
    });
    let text = buffer_to_string(&buffer);
    // The drawer's border claims x=0..33; the lineage's top border continues
    // to the right edge behind it.
    assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((33, 0)).unwrap().symbol(), "╮");
    assert_eq!(
        buffer.cell((69, 0)).unwrap().symbol(),
        "╮",
        "the lineage's own right edge still shows"
    );
    // The drawer's content is the list itself — group header and a model row.
    assert!(text.contains("Staging"));
    assert!(text.contains("stg_finance__budgets"));
    // And it draws with the list's focused border — the drawer IS the list
    // surface in the band (set_list_drawer parked Focus::List there).
    assert_eq!(
        buffer.cell((0, 1)).unwrap().fg,
        dbtl::ui::theme::DEFAULT.accent,
        "the focused-pane accent border is on the drawer"
    );
}

#[test]
pub(super) fn home_screen_renders_title_health_sections_cursor_and_home_cell() {
    use dbtl::app::App;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.enter_home();
    // 60 rows: the fixture's ~29 attention rows leave room for every
    // section (a 30-row body legitimately clips them under "+N more").
    let buffer = render_home(&app, 100, 60);
    let text = buffer_to_string(&buffer);

    assert!(text.contains("dbtl home"), "the title row renders");
    assert!(
        text.contains("[Health]") && text.contains("coverage"),
        "the health info section renders"
    );
    assert!(text.contains("[Actions]"), "the actions section renders");
    assert!(text.contains("[Recent]"), "the recent section renders");
    assert!(
        text.contains("search models"),
        "the search bar's placeholder renders"
    );
    assert!(
        text.contains("stats dashboard"),
        "a BINDINGS action row renders its help text"
    );
    assert!(text.contains(" > "), "the cursor marks the selected row");
    // The footer counts clipped rows when the terminal is short — on this
    // height the "+N more" line is absent instead of lying.
    let row0 = row_text(&buffer, 0);
    assert!(
        row0.contains('⌂'),
        "the home cell draws the chrome glyph: {row0}"
    );
    // The home cell is the only unclosable one — its own 4 columns carry no
    // `×` (the model tab beside it does).
    let home_cell: String = row0.chars().take(4).collect();
    assert!(
        !home_cell.contains('×'),
        "the home cell has no close glyph: {home_cell:?}"
    );
    assert!(row0.contains('×'), "the model tab keeps its close glyph");
}

#[test]
pub(super) fn home_screen_short_terminal_clips_with_a_more_footer() {
    use dbtl::app::App;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.enter_home();
    // An 8-row body can't hold the sections — the footer names the clip.
    let buffer = render_home(&app, 80, 9);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("more"),
        "a clipped home shows the +N more footer:\n{text}"
    );
}

#[test]
pub(super) fn home_screen_search_swaps_sections_for_results() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.enter_home();
    apply_action(&mut app, Action::SearchOpen);
    for c in "budgets".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let buffer = render_home(&app, 100, 30);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("budgets"),
        "the live query renders in the bar"
    );
    assert!(
        text.contains("stg_finance__budgets"),
        "the matched model row renders"
    );
    assert!(
        !text.contains("[Actions]"),
        "sections are replaced while searching"
    );
}

#[test]
pub(super) fn leaving_the_band_restores_the_inline_list() {
    // A stale drawer flag is inert above 80 — `pane_layout` ignores it, the
    // pref's inline pane simply returns where it was.
    let buffer = render_with(100, 30, |s| {
        s.set_list_drawer(true); // leftover from a narrower frame
        s.set_list_auto_hidden(false);
    });
    let text = buffer_to_string(&buffer);
    assert_eq!(
        buffer.cell((33, 0)).unwrap().symbol(),
        "╮",
        "the inline list's right edge is back at x=33"
    );
    assert!(
        text.contains("Staging"),
        "the list rows render inline again"
    );
}

#[test]
pub(super) fn inspector_auto_boundary_publishes_into_pane_config() {
    use dbtl::ui::{pane_layout, InspectorPolicy};
    // The event loop's publish seam: `policy.shown_at(width)` →
    // `set_inspector_shown` → `pane_config` → `pane_layout`. At 159 the
    // region is never carved…
    let mut state = UiState::new(45);
    state.set_inspector_shown(InspectorPolicy::Auto.shown_at(159));
    assert!(!state.inspector_shown());
    let p = pane_layout(
        ratatui::layout::Rect::new(0, 0, 159, 30),
        &state.pane_config(),
    );
    assert_eq!(p.inspector.width, 0, "auto hides below 160");
    // …and at 160 it is.
    state.set_inspector_shown(InspectorPolicy::Auto.shown_at(160));
    assert!(state.inspector_shown());
    let p = pane_layout(
        ratatui::layout::Rect::new(0, 0, 160, 30),
        &state.pane_config(),
    );
    assert_eq!(p.inspector.width, 42, "auto shows at 160");
}

#[test]
pub(super) fn band_transitions_preserve_selection_and_pref() {
    // §12's preservation rule, at the state level: band publishes and drawer
    // open/close cycles are overlay operations — the selection, the saved
    // list pref, and the focus semantics must all survive a round trip.
    let mut state = UiState::new(45);
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    let selected = state.selected();
    state.set_list_auto_hidden(true);
    state.set_list_drawer(true);
    state.set_list_drawer(false);
    state.set_list_auto_hidden(false);
    assert_eq!(state.selected(), selected, "the selection never moved");
    assert!(state.list_visible(), "the inline pref survived untouched");
    // The focus pin is one-directional (same rule the inspector hide keeps):
    // it parks `Focus::List` on the lineage while no list surface exists, and
    // leaving the band does NOT hand it back — the pinned value IS the state
    // that survives.
    assert_eq!(state.focus(), dbtl::ui::Focus::RightPane);
}

#[test]
pub(super) fn home_screen_tiny_sizes_never_panic() {
    use dbtl::app::App;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.enter_home();
    for (w, h) in [(1, 1), (10, 3), (30, 8), (200, 50)] {
        render_home(&app, w, h);
    }
    // Ascii glyph mode renders the `~` home cell and `..` ellipsis.
    app.glyph_mode = dbtl::GlyphMode::Ascii;
    let buffer = render_home(&app, 60, 20);
    let row0 = row_text(&buffer, 0);
    assert!(row0.contains('~'), "ascii mode draws `~` for home: {row0}");
}
