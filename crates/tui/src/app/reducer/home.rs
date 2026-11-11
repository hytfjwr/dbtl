use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- home ----
        // `~` (or the `⌂` cell): activate the fixed home tab. From home it
        // is a no-op (idx 0 is already active).
        Action::HomeGo => {
            app.enter_home();
            Outcome::cont()
        }
        // `j`/`k` under `Mode::Home`: move the item cursor over the flat
        // selectable rows (clamped, no wrap — the list-cursor rule).
        Action::HomeMove(dir) => {
            app.move_home_sel(dir);
            Outcome::cont()
        }
        // Enter / a row click: open the row's model AS A TAB (dedupes to
        // the open one), or run its command — the unavailable ones drop
        // at build, so a drawn command always runs.
        Action::HomeOpen | Action::HomePick(_) => {
            let idx = match action {
                Action::HomePick(i) => i,
                _ => app.home_sel,
            };
            match app.home_data().selectable.get(idx) {
                Some(HomeItem::Model { uid }) => {
                    let uid = uid.clone();
                    app.open_node_as_tab(&uid);
                }
                Some(HomeItem::Command { action: a }) => {
                    return Arm::Early(apply_action(app, *a));
                }
                None => {}
            }
            Outcome::cont()
        }
        // `I`: cycle the inspector visibility pref `auto → on → off` — the
        // persisted value only; the event loop resolves `auto` against the
        // terminal width and publishes `inspector_shown` next frame.
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
