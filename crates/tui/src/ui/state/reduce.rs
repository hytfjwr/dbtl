use super::*;

/// How many rows/cells one mouse-wheel notch moves.
pub(super) const WHEEL_STEP: usize = 3;

/// Apply the **UiState-only** subset of an [`Action`] (the legacy keys).
///
/// The pure, size-unaware sub-reducer at the bottom of the two-level reducer:
/// `app::apply_action` forwards these arms here (for the LIST-focused side of
/// the movement keys — the lineage-focused side moves the App-level lineage
/// cursor, which needs the `Dag` and so never reaches this layer), and the
/// frozen [`handle_key`] facade drives it. The `move_*`/`jump_*` methods no-op
/// unless the list pane is focused. Domain actions (search/detail/help/yank/…)
/// are no-ops here; they are handled at the `App` level where the `Dag` is in
/// reach.
///
/// [`Action`]: crate::action::Action
pub fn reduce_selection(state: &mut UiState, action: crate::action::Action) {
    use crate::action::Action;
    match action {
        Action::MoveDown => state.move_down(),
        Action::MoveUp => state.move_up(),
        Action::PageDown => state.page_move(true),
        Action::PageUp => state.page_move(false),
        Action::JumpTop => state.jump_top(),
        Action::JumpBottom => state.jump_bottom(),
        Action::ToggleFocus => state.toggle_focus(),
        Action::ToggleListPane => state.toggle_list_pane(),
        _ => {}
    }
}

/// Apply a key event to the UI state and report whether to keep running.
///
/// **Frozen facade.** Kept for the unit/integration tests that drive [`UiState`]
/// directly. Dispatches the key in `Mode::Selection` through the keymap (the
/// single source of truth) and applies the UiState subset via [`reduce_selection`].
/// The real event loop calls `app::apply_action` instead.
pub fn handle_key(state: &mut UiState, key: KeyEvent) -> KeyOutcome {
    match crate::action::dispatch(&crate::action::Mode::Selection, key) {
        Some(crate::action::Action::Quit) => KeyOutcome::Quit,
        Some(action) => {
            reduce_selection(state, action);
            KeyOutcome::Continue
        }
        None => KeyOutcome::Continue,
    }
}
