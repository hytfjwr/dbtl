use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- tabs ----
        // `O`: the focus target's model opens in a NEW tab right of the
        // active one — the lineage cursor's node when the lineage pane is
        // focused, else the list cursor's (a non-model / filtered-out node
        // toasts its kind instead). Already-open roots just activate.
        Action::TabOpen => {
            if let Some(uid) = app.focus_target() {
                app.open_node_as_tab(&uid);
            }
            Outcome::cont()
        }
        // `X`/`Ctrl-w`: close the ACTIVE tab (the right neighbour, else the
        // left, takes over; the last tab refuses with a toast inside).
        Action::TabClose => {
            app.close_tab(app.active_tab());
            Outcome::cont()
        }
        // `{`/`}`: cycle one tab, wrapping at both ends.
        Action::TabPrev | Action::TabNext => {
            app.cycle_tab(matches!(action, Action::TabNext));
            Outcome::cont()
        }
        // `1`..=`9`: activate the tab at that position — the bar numbers
        // MODEL tabs 1-9 and home sits at index 0, so the key IS the strip
        // index (out-of-range is a no-op; `~` reaches home).
        Action::TabActivate(n) => {
            app.activate_tab(usize::from(n));
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
