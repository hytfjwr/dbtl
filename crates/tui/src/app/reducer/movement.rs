use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        Action::MoveDown | Action::MoveUp | Action::MoveLeft | Action::MoveRight => {
            if app.ui_state.focus() == Focus::Inspector {
                // Inspector focus: j/k scroll the pane's content; h/l have no
                // meaning there (the column cursor is a mouse affordance —
                // §4's keyboard set is scroll + Enter only).
                let scroll = app.ui_state.inspector_scroll();
                app.ui_state
                    .set_inspector_scroll(if action == Action::MoveDown {
                        scroll.saturating_add(1)
                    } else if action == Action::MoveUp {
                        scroll.saturating_sub(1)
                    } else {
                        scroll
                    });
            } else if app.ui_state.focus() == Focus::RightPane {
                let dir = match action {
                    Action::MoveDown => Direction::Down,
                    Action::MoveUp => Direction::Up,
                    Action::MoveLeft => Direction::Left,
                    _ => Direction::Right,
                };
                // Column focus turns h/l into a COLUMN step (the traced column
                // walks to its next upstream/downstream neighbour); j/k keep
                // their node-cursor meaning inside the filtered graph.
                if app.active_column_focus().is_some()
                    && matches!(dir, Direction::Left | Direction::Right)
                {
                    app.column_focus_step(dir);
                } else {
                    app.move_lineage_cursor(dir);
                }
            } else {
                // §2 T1: a list-cursor move IS the selection — adopt it as the
                // tab's root (with history). Guarded on the index actually
                // changing, so a boundary-clamped keypress (or an h/l no-op on
                // the list) never re-roots a detached tab onto the cursor.
                let before = app.ui_state.selected();
                reduce_selection(&mut app.ui_state, action);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            Outcome::cont()
        }
        // Inspector focus reroutes the shared jump/page bindings to the pane's
        // own scroll slot — they never reach the list path below.
        Action::JumpTop | Action::JumpBottom => {
            if app.ui_state.focus() == Focus::Inspector {
                app.ui_state
                    .set_inspector_scroll(if action == Action::JumpTop {
                        0
                    } else {
                        // `usize::MAX` is the "end" intent the loop's clamp bounds
                        // to content — the modal scrolls use the same idiom.
                        usize::MAX
                    });
            } else {
                // UiState-only arm: forward to the sub-reducer, then adopt on
                // the same changed-index rule as the move keys (§2 T1).
                let before = app.ui_state.selected();
                reduce_selection(&mut app.ui_state, action);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            Outcome::cont()
        }
        Action::PageDown | Action::PageUp => {
            if app.ui_state.focus() == Focus::Inspector {
                let scroll = app.ui_state.inspector_scroll();
                app.ui_state
                    .set_inspector_scroll(if action == Action::PageDown {
                        scroll.saturating_add(crate::ui::INSPECTOR_PAGE_STEP)
                    } else {
                        scroll.saturating_sub(crate::ui::INSPECTOR_PAGE_STEP)
                    });
            } else {
                let before = app.ui_state.selected();
                reduce_selection(&mut app.ui_state, action);
                if app.ui_state.selected() != before {
                    app.adopt_list_selection();
                }
            }
            Outcome::cont()
        }
        // `d`/`u` in Selection mode bind to the lineage view filters — while
        // the inspector is focused they mean what every scrollable pane reads
        // them as: a half-page-style scroll step (the same fixed 10 the modal
        // `DetailScrollPage` uses).
        Action::ToggleDownstream | Action::ToggleUpstream => {
            if app.ui_state.focus() == Focus::Inspector {
                let scroll = app.ui_state.inspector_scroll();
                app.ui_state
                    .set_inspector_scroll(if action == Action::ToggleDownstream {
                        scroll.saturating_add(crate::ui::INSPECTOR_PAGE_STEP)
                    } else {
                        scroll.saturating_sub(crate::ui::INSPECTOR_PAGE_STEP)
                    });
            } else {
                let v = &mut app.tab_mut().lineage_view;
                if action == Action::ToggleUpstream {
                    v.upstream = !v.upstream;
                } else {
                    v.downstream = !v.downstream;
                }
            }
            Outcome::cont()
        }
        // UiState-only arms: forward to the sub-reducer. The focus and
        // visibility toggles never move the cursor, so they never adopt.
        Action::ToggleFocus | Action::ToggleListPane => {
            reduce_selection(&mut app.ui_state, action);
            Outcome::cont()
        }
        // Column-extreme cursor jumps act on the lineage regardless of focus
        // (they have no list meaning, unlike the focus-routed h/l).
        Action::LineageLeftmost | Action::LineageRightmost => {
            app.move_lineage_cursor_extreme(matches!(action, Action::LineageRightmost));
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
