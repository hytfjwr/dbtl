use super::*;

// ============================================================================
// search hint/zero-match, palette Recent/@/unavailable/no-match,
// help /-filter, empty-list + empty-lineage reasons
// ============================================================================

#[test]
pub(super) fn search_empty_query_stamps_the_syntax_hint() {
    // With `/` open and the query EMPTY, the first interior row carries the
    // `!`-selector / `?`-full-text / Up-history hint — display-only chrome
    // stamped over the top row, never an injected DisplayRow.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::SearchOpen);
    // The resizable list pane defaults to 34 cols — widen it (via
    // `set_list_width`) so the whole hint line fits inside the pane.
    app.ui_state.set_list_width(60);
    let text = buffer_to_string(&render_app(&app, 200, 30));
    assert!(
        text.contains("!tag:x selector"),
        "the selector hint renders on an empty query:\n{text}"
    );
    assert!(
        text.contains("?text full-text"),
        "the full-text hint renders:\n{text}"
    );
    // The arrow comes from Chrome: ↑ in Unicode mode, ^ in ASCII.
    assert!(text.contains("↑ history"), "the history hint renders");

    // The position: the hint stamps the FIRST interior row (interior row 0,
    // not a row under the headers or the pane's last row).
    let inner = dbtl::ui::pane_interior(
        dbtl::ui::pane_layout(
            ratatui::layout::Rect::new(0, 0, 200, 30),
            &app.ui_state.pane_config(),
        )
        .list,
    );
    let hint_line = text
        .lines()
        .nth(inner.y as usize)
        .expect("a line exists at the first interior row");
    assert!(
        hint_line.contains("!tag:x selector"),
        "the hint sits on interior row 0, got {hint_line:?}"
    );

    // Same hint in ASCII mode — pure-ASCII substitutions only.
    app.glyph_mode = dbtl::GlyphMode::Ascii;
    let ascii = buffer_to_string(&render_app(&app, 200, 30));
    assert!(ascii.contains("^ history"), "ASCII arrow: {ascii:?}");
    assert!(!ascii.contains('↑'), "no ambiguous-width arrow in ASCII");
}

#[test]
pub(super) fn search_hint_vanishes_on_the_first_keystroke() {
    // The hint is a while-the-input-is-empty affordance: once a char lands the
    // stamped row is gone and the normal filtered rows occupy every line.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::SearchOpen);
    apply_action(&mut app, Action::SearchType('p'));
    let text = buffer_to_string(&render_app(&app, 100, 30));
    assert!(
        !text.contains("!tag:x selector"),
        "hint gone once the query is live:\n{text}"
    );
}
