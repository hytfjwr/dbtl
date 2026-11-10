use super::*;

/// Compute the number of visible *display rows* in the list pane — through the
/// SAME `pane_layout`/`pane_interior` the draw and the hit-test share, so the
/// tab-bar row (and any future chrome) can't silently diverge from what the
/// list actually gets. `max(1)` guards tiny terminals so we never produce a
/// zero-height window.
pub(super) fn list_pane_height(
    terminal: &DefaultTerminal,
    ui: &dbtl::ui::UiState,
) -> Result<usize> {
    let inner = pane_interior(pane_layout(full_area(terminal)?, &ui.pane_config()).list);
    Ok((inner.height as usize).max(1))
}

/// The lineage pane's interior size `(width, height)` for the current terminal,
/// computed through the SAME pure geometry (`pane_layout` on `pane_config` +
/// `pane_interior`) that `draw` uses — pane widths included — so the
/// anchor/clamp viewport equals the blitted/drawn viewport.
pub(super) fn lineage_viewport(
    terminal: &DefaultTerminal,
    ui: &dbtl::ui::UiState,
) -> Result<(usize, usize)> {
    let interior = pane_interior(pane_layout(full_area(terminal)?, &ui.pane_config()).lineage);
    Ok((interior.width as usize, interior.height as usize))
}

/// The full terminal rect (origin at 0,0). Used for overlay geometry / clamps.
pub(super) fn full_area(terminal: &DefaultTerminal) -> Result<ratatui::layout::Rect> {
    let size = terminal.size().context("failed to read terminal size")?;
    Ok(ratatui::layout::Rect::new(0, 0, size.width, size.height))
}
