use super::*;

// ============================================================================
// render-based SCROLL tests (clip-bug guard)
//
// The clip bug: after `G` (or `j` to the bottom) on a normal-size terminal, the
// selected model row was clipped below the list-pane border and never rendered,
// even though a model-space state test claimed it was "visible". These tests
// reproduce the real event-loop path against a TestBackend and assert the
// selected row is *actually drawn* — the only metric that catches the bug.
// ============================================================================

pub(super) fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The list pane's inner height for a terminal of total `height`, mirroring the
/// binary's layout: minus the 1-row status line, minus the 2 border rows.
pub(super) fn inner_list_height(height: u16) -> usize {
    (height.saturating_sub(1).saturating_sub(2)).max(1) as usize
}

/// Drive the real event-loop steps: apply each key via `handle_key`, then follow
/// scroll exactly as `main::event_loop` does (display-row space), then render at
/// `width x height`. Returns the rendered buffer.
pub(super) fn render_after_keys(
    width: u16,
    height: u16,
    keys: &[KeyCode],
) -> (Buffer, UiState, ModelList) {
    let list = fixture_list();
    let mut state = UiState::new(list.len());
    for &k in keys {
        handle_key(&mut state, press(k));
    }
    // Scroll-follow before drawing — same call the event loop makes.
    let h = inner_list_height(height);
    let sel = state.selected();
    state.ensure_visible(
        h,
        list.reveal_row_of_model(sel),
        list.row_of_model(sel),
        list.row_count(),
    );

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let hints = dbtl::action::status_hints(
            dbtl::action::ModeKind::Selection,
            state.focus(),
            false,
            false,
        );
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.hints = &hints;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    let buffer = terminal.backend().buffer().clone();
    (buffer, state, list)
}

/// The single rendered line that carries the selection marker `> `. The status
/// line never contains `> `, so this uniquely identifies the selected *list*
/// row (avoiding the `contains("pos_pay")` false positive from the status echo).
pub(super) fn marker_line(buffer: &Buffer) -> String {
    let text = buffer_to_string(buffer);
    let lines: Vec<&str> = text.lines().filter(|l| l.contains("> ")).collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one rendered line must carry the selection marker; got {}:\n{text}",
        lines.len()
    );
    lines[0].to_string()
}

#[test]
pub(super) fn g_jump_renders_selected_row_in_normal_terminal() {
    // 100x30: height 30 < ~52, so the full 49-row list cannot fit and the bug
    // would clip the selection. After `G` the selected model is `pos_txn`
    // (index 44). It must be on the rendered marker line — not merely somewhere
    // in the buffer (the status line echoes it regardless).
    let (buffer, state, list) = render_after_keys(100, 30, &[KeyCode::Char('G')]);
    assert_eq!(state.selected(), 44, "G selects the last model");
    assert_eq!(
        list.models[44].name, "pos_txn",
        "frozen: index 44 is pos_txn"
    );

    let line = marker_line(&buffer);
    assert!(
        line.contains("pos_txn"),
        "the selected row (marker line) must render 'pos_txn', got: {line:?}"
    );
    // The window must actually have scrolled (offset advanced past the top).
    assert!(
        state.offset() > 0,
        "selecting the bottom model on a short terminal must scroll the window"
    );
}

#[test]
pub(super) fn j_to_bottom_renders_selected_row_in_normal_terminal() {
    // Same property reached via a long run of `j` instead of `G`.
    let keys = vec![KeyCode::Char('j'); 60]; // more than enough to clamp at 44
    let (buffer, state, list) = render_after_keys(100, 30, &keys);
    assert_eq!(
        state.selected(),
        44,
        "j past the end clamps at the last model"
    );

    let line = marker_line(&buffer);
    assert!(
        line.contains(&list.models[44].name),
        "the selected row must render the bottom model name, got: {line:?}"
    );
}

#[test]
pub(super) fn mid_list_selection_below_fold_renders_selected_row() {
    // Pick a model whose DISPLAY ROW exceeds the inner height so offset must be
    // > 0 (a mid-list model already on-screen at offset 0 would guard nothing —
    // it passes even when the clip is present). Model 30 sits at display row ~34,
    // beyond the inner height (27) of a 100x30 terminal.
    let keys = vec![KeyCode::Char('j'); 30]; // start 0 -> select 30
    let (buffer, state, list) = render_after_keys(100, 30, &keys);
    assert_eq!(state.selected(), 30);

    let target = &list.models[30].name;
    assert!(
        list.row_of_model(30) > inner_list_height(30),
        "precondition: model 30's display row must be below the fold"
    );
    assert!(
        state.offset() > 0,
        "a below-the-fold mid-list selection must scroll"
    );
    let line = marker_line(&buffer);
    // The right-aligned degree badges leave fewer name cells than the name
    // needs, so the row is middle-truncated: both the head and the preserved
    // tail must be present to identify the selected model.
    assert!(
        line.contains("pos_files") && line.contains("assignment"),
        "the selected mid-list row must render '{target}' (middle-truncated), got: {line:?}"
    );
}

#[test]
pub(super) fn scrolled_render_still_shows_status_and_borders() {
    // After scrolling, the status/help line and the selected node echo must
    // still be present (the chrome doesn't get scrolled away).
    let (buffer, state, list) = render_after_keys(100, 30, &[KeyCode::Char('G')]);
    let text = buffer_to_string(&buffer);
    assert!(text.contains("j/k move"), "the hint row must remain");
    // The selected node's name stays on screen — the list's marker row and
    // the lineage title both carry it after the scroll.
    assert!(
        text.contains(&list.models[state.selected()].name),
        "the selected node name must stay visible"
    );
}

#[test]
pub(super) fn search_filters_and_keeps_selected_row_visible() {
    // The two-coordinate-space guard UNDER SEARCH: after filtering, the selected
    // model (tracked by unique_id) must be re-resolved into the FILTERED index,
    // and ensure_visible(filtered rows) must keep it inside the window — the
    // clip-bug class, via the filter path.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    use ratatui::crossterm::event::KeyCode;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // Open search and type "pos" — narrows to the pos_* utilities models.
    apply_action(&mut app, Action::SearchOpen);
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let filtered_len = app.active_list().len();
    assert!(
        filtered_len > 0 && filtered_len < 45,
        "search narrowed the list: {filtered_len}"
    );
    assert!(
        app.active_list().models.iter().any(|m| m.name == "pos_txn"),
        "a known matching model survives the filter"
    );

    // Move to the last filtered match, follow scroll exactly as the loop does,
    // then render — the selected (marker) row must actually be drawn.
    for _ in 0..filtered_len {
        apply_action(&mut app, Action::SearchMove(dbtl::Direction::Down));
    }
    let sel = app.ui_state.selected();
    let h = inner_list_height(30);
    app.ui_state.ensure_visible(
        h,
        app.active_list().reveal_row_of_model(sel),
        app.active_list().row_of_model(sel),
        app.active_list().row_count(),
    );
    let want = app.active_list().model_at(sel).unwrap().name.clone();

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let line = marker_line(&terminal.backend().buffer().clone());
    assert!(
        line.contains(&want),
        "selected filtered row '{want}' must render, got {line:?}"
    );

    // The list title reflects the live query.
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("Search: pos"), "list title shows the query");

    // Confirm resolves the selection back into the FULL list (filter dropped).
    let _ = KeyCode::Enter; // (dispatch is exercised in action.rs tests)
    apply_action(&mut app, Action::SearchConfirm);
    assert_eq!(app.active_list().len(), 45, "filter dropped on confirm");
    assert_eq!(
        app.active_list()
            .model_at(app.ui_state.selected())
            .unwrap()
            .name,
        want,
        "the chosen match stays selected in the full list"
    );
}

#[test]
pub(super) fn search_title_shows_n_over_m_match_count() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::SearchOpen);
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let n = app.active_list().len();
    let m = app.model_list.len(); // full count
    assert!(n > 0 && n < m, "search narrowed: {n}/{m}");

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.full_model_count = Some(m);
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    // The literal prefix stays (load-bearing) AND the N/M count is present.
    assert!(text.contains("Search: pos"), "query prefix still shown");
    assert!(
        text.contains(&format!("{n}/{m}")),
        "title shows the N/M match count, got: {text:?}"
    );
}

#[test]
pub(super) fn bookmark_badge_renders_on_a_bookmarked_row() {
    use std::collections::BTreeSet;

    let list = fixture_list();
    let mut state = UiState::new(list.len());
    // The badge trails the name + degree badges; give the pane enough columns
    // for the whole row to fit (the default 34 clips this particular name).
    state.set_pane_width(dbtl::ui::Splitter::List, 40, 100);
    // Bookmark the first model in the flat list (row near the top → on screen).
    let uid = list.models[0].unique_id.clone();
    let mut bookmarks = BTreeSet::new();
    bookmarks.insert(uid);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.bookmarks = Some(&bookmarks);
        // Default glyph mode is Unicode ⇒ the badge is the ★ glyph.
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains('★'),
        "the Unicode bookmark badge renders on the bookmarked row: {text:?}"
    );

    // ASCII glyph mode renders the pure-ASCII '*' badge instead (ascii_guard
    // territory: never the ambiguous-width ★).
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.bookmarks = Some(&bookmarks);
        ctx.glyphs = dbtl::GlyphMode::Ascii;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let ascii = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        ascii.contains('*'),
        "ASCII bookmark badge is '*': {ascii:?}"
    );
    assert!(
        !ascii.contains('★'),
        "no ambiguous-width glyph in ASCII mode"
    );
}

#[test]
pub(super) fn search_cancel_restores_origin_selection() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let origin = app.ui_state.selected();
    apply_action(&mut app, Action::SearchOpen);
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    apply_action(&mut app, Action::SearchCancel);
    assert_eq!(app.active_list().len(), 45, "full list restored");
    assert_eq!(
        app.ui_state.selected(),
        origin,
        "selection restored to the search origin"
    );
}

#[test]
pub(super) fn g_jump_does_not_panic_on_tiny_terminal() {
    // Extreme small terminal: even when a header + 1 model can't both fit, the
    // scroll-follow + draw path must not panic.
    for (w, h) in [(8u16, 4u16), (4, 3), (20, 5), (3, 3)] {
        let keys = [KeyCode::Char('G')];
        let _ = render_after_keys(w, h, &keys); // must simply not panic
    }
}
