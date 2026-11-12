use super::*;

#[test]
fn yank_and_bookmark_record_oneshot_notices() {
    let mut a = app();
    assert_eq!(a.take_notice(), None, "fresh app has no pending notice");
    let out = apply_action(&mut a, Action::YankId);
    assert!(
        matches!(out.effects.as_slice(), [Effect::Yank(_)]),
        "yank still requests the clipboard effect"
    );
    assert_eq!(a.take_notice().as_deref(), Some("Copied unique_id"));
    assert_eq!(a.take_notice(), None, "take_notice drains (one-shot)");

    apply_action(&mut a, Action::BookmarkToggle);
    let note = a.take_notice().expect("bookmark toggle notices");
    assert!(note.starts_with("Bookmarked: "), "got {note:?}");
    apply_action(&mut a, Action::BookmarkToggle);
    let note = a.take_notice().expect("un-bookmark notices");
    assert!(note.starts_with("Removed bookmark: "), "got {note:?}");
}

#[test]
fn yank_prefix_opens_menu_and_second_key_executes_then_closes() {
    let mut a = app();
    apply_action(&mut a, Action::YankPrefix);
    assert!(matches!(a.mode, Mode::Yank { .. }), "y opens the yank menu");
    // The second key dispatches the yank AND closes the menu (one-shot).
    let out = apply_action(&mut a, Action::YankMermaid);
    assert!(matches!(out.effects.as_slice(), [Effect::Yank(_)]));
    assert_eq!(a.mode, Mode::Selection, "menu closes after the yank");
    // Esc/q cancel closes without any yank effect.
    apply_action(&mut a, Action::YankPrefix);
    let out = apply_action(&mut a, Action::DetailClose);
    assert!(out.effects.is_empty());
    assert_eq!(a.mode, Mode::Selection);
}

#[test]
fn yank_prefix_keymap_routes_second_keys() {
    use crate::dispatch;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let press = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
    // `y` in Selection opens the prefix; the same letters then dispatch the
    // yank actions from Mode::Yank.
    let yank = Mode::Yank { back: None, sel: 0 };
    assert_eq!(
        dispatch(&Mode::Selection, press(KeyCode::Char('y'))),
        Some(Action::YankPrefix)
    );
    assert_eq!(
        dispatch(&yank, press(KeyCode::Char('m'))),
        Some(Action::YankMermaid)
    );
    assert_eq!(
        dispatch(&yank, press(KeyCode::Char('y'))),
        Some(Action::YankId)
    );
    assert_eq!(
        dispatch(&yank, press(KeyCode::Esc)),
        Some(Action::DetailClose)
    );
    // Unbound second keys don't dispatch (menu stays open until a valid key).
    assert_eq!(dispatch(&yank, press(KeyCode::Char('z'))), None);
}

#[test]
fn theme_cycle_wraps_the_loaded_list_and_notices_the_landing_name() {
    let mut a = app();
    assert_eq!(a.theme_name(), "default", "fresh app starts on the default");
    assert_eq!(a.active_theme(), &crate::ui::theme::DEFAULT);

    // A full lap over the built-in presets lands back on the default, and
    // every step records a `theme: {name}` toast.
    let count = crate::ui::theme::preset_names().len();
    for _ in 0..count {
        let out = apply_action(&mut a, Action::ThemeCycle);
        assert!(out.effects.is_empty(), "a theme cycle is pure state");
        let note = a.take_notice().expect("cycle notices the landing theme");
        assert_eq!(note, format!("theme: {}", a.theme_name()));
    }
    assert_eq!(a.theme_name(), "default", "a full lap wraps around");

    // set_themes (the --theme seam): the list is replaced, the index selects,
    // an out-of-range index clamps, and an empty list is ignored.
    let ayu = crate::ui::theme::preset("ayu-dark").expect("preset");
    a.set_themes(
        vec![
            ("default".into(), crate::ui::theme::DEFAULT),
            ("mine".into(), ayu),
        ],
        1,
    );
    assert_eq!(a.theme_name(), "mine");
    assert_eq!(a.active_theme(), &ayu);
    a.set_themes(Vec::new(), 99);
    assert_eq!(a.theme_name(), "mine", "empty list ignored, index clamped");
}

#[test]
fn export_notice_names_the_written_path() {
    let mut a = app();
    let out = apply_action(&mut a, Action::ExportLineage);
    let Some(Effect::WriteFile { path, .. }) = out.effects.first() else {
        panic!("export must request the write effect");
    };
    assert_eq!(a.take_notice(), Some(format!("Exported {path}")));
}

#[test]
fn apply_action_forwards_uistate_arms_like_handle_key() {
    // For the legacy keys UNDER LIST FOCUS, apply_action must mutate
    // ui_state identically to driving reduce_selection directly (the
    // wrapping relationship is exact). Under lineage focus the movement
    // keys intentionally diverge: they drive the App-level lineage cursor,
    // which the bare UiState cannot represent (see the cursor tests below).
    let mut a = app();
    let mut bare = UiState::new(a.model_list.len());
    for action in [
        Action::MoveDown,
        Action::MoveDown,
        Action::JumpBottom,
        Action::MoveUp,
    ] {
        let out = apply_action(&mut a, action);
        assert!(!out.quit && out.effects.is_empty());
        reduce_selection(&mut bare, action);
        assert_eq!(
            a.ui_state.selected(),
            bare.selected(),
            "{action:?} divergent"
        );
    }
}

#[test]
fn quit_action_sets_quit() {
    let mut a = app();
    assert!(apply_action(&mut a, Action::Quit).quit);
}

#[test]
fn unimplemented_domain_actions_are_safe_noops() {
    let mut a = app();
    let before = a.ui_state.selected();
    for action in [
        Action::HelpToggle,
        Action::DetailOpen,
        Action::SearchOpen,
        Action::DetailScroll(Direction::Down),
    ] {
        let out = apply_action(&mut a, action);
        assert!(!out.quit && out.effects.is_empty());
    }
    assert_eq!(
        a.ui_state.selected(),
        before,
        "no-op domain actions don't move selection"
    );
}
