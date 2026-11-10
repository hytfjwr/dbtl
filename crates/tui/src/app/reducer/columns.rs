use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- column-level lineage (Issue 09) ----
        Action::ColumnFocusEnter => {
            // Enter is contextual over the structure modal: while the tests
            // section owns the cursor it jumps to the relationships `to:`
            // target (the same re-root contract as the column modal's Enter —
            // a non-model target notices instead); over the columns section it
            // commits the cursored column to the trace view; a column-less
            // modal keeps Enter's classic close behaviour.
            if let Mode::Detail(dv) = &app.mode {
                if dv.test_index != usize::MAX {
                    let target = dv.tests.get(dv.test_index).and_then(|t| t.ref_uid.clone());
                    match target {
                        Some(uid)
                            if app
                                .dag
                                .get(&uid)
                                .is_some_and(|n| n.resource_type == "model") =>
                        {
                            app.jump_to(&uid);
                            app.mode = app.idle_mode();
                        }
                        Some(_) => app.set_notice("cannot re-root to a non-model node"),
                        None => {}
                    }
                    return Arm::Early(Outcome::cont());
                }
            }
            let col = match &app.mode {
                Mode::Detail(dv) if dv.col_index != usize::MAX => dv
                    .col_names
                    .get(dv.col_index)
                    .map(|c| crate::ColumnRef::new(&dv.model_id, c)),
                _ => None,
            };
            app.mode = app.idle_mode();
            if let Some(col) = col {
                app.column_focus_enter(col);
            }
            Outcome::cont()
        }
        Action::ColumnFocusExit => {
            // Esc exits the trace view — the lineage returns to the normal
            // rooted subgraph of whatever is selected.
            app.tab_mut().column_focus = None;
            Outcome::cont()
        }
        Action::ColumnsOpen => {
            // The modal's column: the structure modal's cursored column when
            // opened over it, else the traced column's (Selection `K`).
            let col = match &app.mode {
                Mode::Detail(dv) if dv.col_index != usize::MAX => dv
                    .col_names
                    .get(dv.col_index)
                    .map(|c| crate::ColumnRef::new(&dv.model_id, c)),
                _ => app.tab().column_focus.as_ref().map(|f| f.col.clone()),
            };
            match col {
                Some(col) => app.mode = Mode::Columns(app.columns_view_for(&col)),
                None => app.set_notice("no column selected — Enter a model, h/l pick a column"),
            }
            Outcome::cont()
        }
        Action::ColumnRowMove(dir) => {
            // The column modal's row cursor (j/k): a SELECTION — the loop's
            // `clamp_columns_scroll` does the ensure-visible follow.
            if let Mode::Columns(cv) = &mut app.mode {
                cv.move_cursor(dir);
            }
            Outcome::cont()
        }
        Action::ColumnJump => {
            // Re-root to the cursored row's model (records history like a
            // lineage click; landing on a DIFFERENT model exits the trace via
            // reroot_select — the traced column's own row keeps it). A
            // non-model row (a source/seed column) notices instead.
            let uid = match &app.mode {
                Mode::Columns(cv) => cv.rows.get(cv.cursor).and_then(|r| r.uid.clone()),
                _ => None,
            };
            match uid {
                Some(uid)
                    if app
                        .dag
                        .get(&uid)
                        .is_some_and(|n| n.resource_type == "model") =>
                {
                    app.jump_to(&uid);
                    app.mode = app.idle_mode();
                }
                Some(_) => app.set_notice("cannot re-root to a non-model node"),
                None => {}
            }
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
