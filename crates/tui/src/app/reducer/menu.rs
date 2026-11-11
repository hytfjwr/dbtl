use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- node context menu ----
        // `a` (and the event loop's right-click, which goes through
        // `App::open_menu` directly) opens the context menu for the CURRENT
        // node: over a Detail/Sql modal that's the modal's own node (the same
        // target rule `y` uses), in Selection the focus target. The box's
        // origin mode rides in `MenuState.back` so Esc/execute restores it.
        Action::MenuOpen => {
            match app.menu_uid() {
                Some(uid) => app.open_menu(&uid, MenuAnchor::Node),
                None => app.set_notice("nothing to act on"),
            }
            Outcome::cont()
        }
        Action::MenuMove(dir) => {
            // j/k walk the ITEM rows only — separators are unselectable. The
            // cursor wraps at both ends (a menu is a ring, like the yank
            // prefix's implicit "any key closes" ring of options).
            if let Mode::Menu(ms) = &mut app.mode {
                let n = ms.items.len();
                if n > 0 {
                    let mut i = ms.selected;
                    for _ in 0..n {
                        i = match dir {
                            Direction::Up => (i + n - 1) % n,
                            _ => (i + 1) % n,
                        };
                        if matches!(ms.items[i], MenuEntry::Item { .. }) {
                            ms.selected = i;
                            break;
                        }
                    }
                }
            }
            Outcome::cont()
        }
        Action::MenuConfirm => {
            // Enter runs the highlighted row — the same path an item shortcut
            // or a row double-click takes.
            let sel = match &app.mode {
                Mode::Menu(ms) => Some(ms.selected),
                _ => None,
            };
            if let Some(i) = sel {
                return Arm::Early(apply_action(app, Action::MenuInvoke(i)));
            }
            Outcome::cont()
        }
        Action::MenuInvoke(i) => {
            // Resolve the items-row index to the BINDINGS row it points at —
            // the action, key and help text all live there, so the menu
            // literally cannot drift from the keymap. The menu is one-shot
            // like the which-key: restore the stacked origin mode, retarget
            // the selection at the menu's node (its execution contract:
            // items act on the node they were opened for, never on wherever
            // the selection happened to be), then apply the action itself.
            let resolved = match &app.mode {
                Mode::Menu(ms) => match ms.items.get(i) {
                    Some(MenuEntry::Item { binding, .. }) => Some((
                        BINDINGS[*binding].action,
                        ms.back.clone(),
                        ms.target.clone(),
                    )),
                    // A separator row (or a stale index) is inert — the menu
                    // stays open rather than close on a stray click.
                    _ => None,
                },
                _ => None,
            };
            let Some((action, back, target)) = resolved else {
                return Arm::Early(Outcome::cont());
            };
            app.mode = back.map(|b| *b).unwrap_or_else(|| app.idle_mode());
            app.move_selection_to(&target);
            // Arm the overlay target stash for exactly this dispatch: the
            // selection move above can't land on a non-model node (the list
            // holds models only), so every producer that reads the selection
            // — the yanks, bookmark/path, the editor open, `Enter`/`s` —
            // must see the MENU's target instead or the item would act on
            // whatever happened to be selected.
            app.pending_node = Some(target);
            let mut out = apply_action(app, action);
            app.pending_node = None;
            // Same re-anchor contract as the palette confirm below: the loop
            // can't see the menu-resolved action, so the flag travels on the
            // Outcome.
            out.reanchor |= crate::action::reanchors_lineage(action);
            return Arm::Early(out);
        }
        // ---- pointer row actions (mouse) ----
        // A row click in a row-bearing overlay is a SELECTION first (per the
        // modal-mouse rule); a double-click then fires the row's execute
        // action. None of these are key-bound — the event loop produces them.
        Action::MenuSelect(i) => {
            // Hover highlight; separators never take the cursor.
            if let Mode::Menu(ms) = &mut app.mode {
                if matches!(ms.items.get(i), Some(MenuEntry::Item { .. })) {
                    ms.selected = i;
                }
            }
            Outcome::cont()
        }
        Action::YankPick(i) => {
            if let Mode::Yank { sel, .. } = &mut app.mode {
                if i < crate::action::yank_bindings().count() {
                    *sel = i;
                }
            }
            Outcome::cont()
        }
        Action::YankInvoke(i) => {
            // Run the which-key row's own action through the same dispatch the
            // key would take — the row IS its binding.
            if let Some(action) = crate::action::yank_bindings().nth(i).map(|b| b.action) {
                return Arm::Early(apply_action(app, action));
            }
            Outcome::cont()
        }
        Action::PalettePick(i) => {
            // `i` indexes the SELECTABLE rows (`palette_selectable_rows` —
            // headers stripped), the same space `p.selected` and the
            // confirm/move arms use.
            let query = match &app.mode {
                Mode::Palette(p) => p.query.clone(),
                _ => return Arm::Early(Outcome::cont()),
            };
            if i < app.palette_selectable_rows(&query).len() {
                if let Mode::Palette(p) = &mut app.mode {
                    p.selected = i;
                }
            }
            Outcome::cont()
        }
        Action::ColumnPick(i) => {
            // Same landing rule the j/k cursor follows: headers/separators
            // (uid == None) are not selectable rows.
            if let Mode::Columns(cv) = &mut app.mode {
                if cv.rows.get(i).is_some_and(|r| r.uid.is_some()) {
                    cv.cursor = i;
                }
            }
            Outcome::cont()
        }
        // The structure modal's column cursor (h/l): a SELECTION, not a scroll —
        // the loop's per-frame `clamp_detail_scroll` does the ensure-visible.
        // (`Direction::Up`/`Down` here mean previous/next ROW of the column
        // list — the enum is reused for a vertical cursor, not for the
        // lineage's upstream/downstream axes.)
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
