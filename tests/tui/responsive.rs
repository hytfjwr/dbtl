use super::*;

// ============================================================================
// Responsive layout: too-small screen, narrow band, the list drawer
// ============================================================================

/// Render `draw` with a `UiState` mutated by `f`, at `width x height`.
pub(super) fn render_with(width: u16, height: u16, f: impl FnOnce(&mut UiState)) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let mut state = UiState::new(list.len());
    f(&mut state);
    let ctx = RenderCtx::new(&list, &state, None);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    terminal.backend().buffer().clone()
}
