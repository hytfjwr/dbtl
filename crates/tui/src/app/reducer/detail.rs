use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        Action::DetailColumnMove(dir) => {
            if let Mode::Detail(dv) = &mut app.mode {
                if dv.test_index != usize::MAX {
                    // The tests section owns the cursor: walk only the
                    // JUMPABLE rows (relationships `to:` targets) — plain test
                    // rows are skipped the way the columns modal skips
                    // uid-less rows.
                    let back = matches!(dir, Direction::Up | Direction::Left);
                    let next = if back {
                        (0..dv.test_index)
                            .rev()
                            .find(|&i| dv.tests[i].ref_uid.is_some())
                    } else {
                        (dv.test_index + 1..dv.tests.len()).find(|&i| dv.tests[i].ref_uid.is_some())
                    };
                    if let Some(i) = next {
                        dv.test_index = i;
                    }
                } else if dv.col_index != usize::MAX && !dv.col_names.is_empty() {
                    let last = dv.col_names.len() - 1;
                    dv.col_index = match dir {
                        Direction::Up | Direction::Left => dv.col_index.saturating_sub(1),
                        _ => (dv.col_index + 1).min(last),
                    };
                }
            }
            Outcome::cont()
        }
        Action::DetailSectionToggle => {
            // `Tab` inside the structure modal moves the h/l cursor between
            // the COLUMNS list and the TESTS list. The tests side only accepts
            // the cursor when at least one row is jumpable (a relationships
            // test with a resolved `to:` target) — otherwise Tab notices and
            // stays on columns, so Enter can never strand on a dead row.
            if let Mode::Detail(dv) = &mut app.mode {
                if dv.test_index == usize::MAX {
                    match dv.tests.iter().position(|t| t.ref_uid.is_some()) {
                        Some(i) => dv.test_index = i,
                        None => app.set_notice("no relationship test targets on this node"),
                    }
                } else {
                    dv.test_index = usize::MAX;
                }
            }
            Outcome::cont()
        }
        // Overlay scroll: acts inside whichever scrollable overlay is open. The
        // size-aware upper clamp is the loop's.
        Action::DetailScroll(dir) | Action::DetailScrollPage(dir) => {
            // Line vs page step; the loop's per-frame clamp bounds the result.
            let amount = if matches!(action, Action::DetailScroll(_)) {
                1
            } else {
                10
            };
            if let Some(s) = modal_scroll_mut(&mut app.mode) {
                *s = if matches!(dir, crate::action::Direction::Down) {
                    s.saturating_add(amount)
                } else {
                    s.saturating_sub(amount)
                };
            }
            Outcome::cont()
        }
        Action::DetailScrollHome | Action::DetailScrollEnd => {
            // End records MAX as "as far as it goes"; the loop's per-frame
            // clamp (the same one every scroll passes through) bounds it to
            // the content height, so the reducer stays size-unaware.
            if let Some(s) = modal_scroll_mut(&mut app.mode) {
                *s = if matches!(action, Action::DetailScrollHome) {
                    0
                } else {
                    usize::MAX
                };
            }
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
