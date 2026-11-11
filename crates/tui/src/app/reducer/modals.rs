use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- command palette ----
        // Open the fuzzy command finder. Its editing keys reuse the Search* arms,
        // routed by `Mode::Palette` below.
        Action::PaletteOpen => {
            app.mode = Mode::Palette(PaletteState::default());
            Outcome::cont()
        }

        // ---- SQL preview / stats dashboard modals ----
        Action::SqlOpen => {
            // Focus-aware target (`s` previews whatever the lineage cursor is on
            // when the lineage pane is focused, else the list selection — an
            // armed overlay stash wins, so a menu-invoked `s` previews the
            // MENU's node); unlike `Enter`, `s` only PREVIEWS — it never
            // re-roots.
            let Some(uid) = app.pending_or_focus() else {
                return Arm::Early(Outcome::cont());
            };
            // A collapse marker has no SQL — `s` previews nodes, not chrome.
            if app.collapse_group(&uid).is_some() {
                app.set_notice("no SQL for a collapse marker (Enter toggles it)");
                return Arm::Early(Outcome::cont());
            }
            let name = app.node_name_or_uid(&uid);
            // Sources/seeds (and manifests omitting raw_code) get a placeholder —
            // there is no transient-status channel in this app.
            let raw = app.dag.raw_code(&uid).map(str::to_string);
            let sql = match &raw {
                Some(code) => code.clone(),
                None => {
                    let rt = app
                        .dag
                        .get(&uid)
                        .map(|n| n.resource_type.as_str())
                        .unwrap_or("node");
                    format!("(no SQL for this {rt})")
                }
            };
            // The compiled variant rides in the same snapshot (RenderCtx stays
            // Dag-free): `c` inside the modal only flips a flag.
            let (compiled, compiled_stale) = app
                .dag
                .compiled(&uid)
                .map(|c| (Some(c.code.clone()), c.stale))
                .unwrap_or((None, false));
            // Open on the compiled text when it is the only real SQL there is
            // — `raw_code` absent OR blank/whitespace-only (dbt emits `""` for
            // e.g. ephemeral config-only nodes, and an empty modal helps nobody).
            let show_compiled =
                raw.as_deref().is_none_or(|c| c.trim().is_empty()) && compiled.is_some();
            let path = app
                .dag
                .detail(&uid)
                .and_then(|d| d.original_file_path.clone());
            app.mode = Mode::Sql(SqlView {
                model_id: uid,
                name,
                sql,
                compiled,
                compiled_stale,
                show_compiled,
                path,
                scroll: 0,
            });
            Outcome::cont()
        }
        Action::SqlToggleCompiled => {
            match &mut app.mode {
                // Flip the display flag only; both texts were snapshotted at
                // open. The scroll resets — raw and compiled are different
                // texts, so a carried offset would land on an arbitrary line.
                Mode::Sql(sv) if sv.compiled.is_some() => {
                    sv.show_compiled = !sv.show_compiled;
                    sv.scroll = 0;
                }
                Mode::Sql(_) => app.set_notice("no compiled SQL for this node (run dbt compile)"),
                _ => {}
            }
            Outcome::cont()
        }
        Action::StatsOpen => {
            app.mode = Mode::Stats(app.compute_stats_view());
            Outcome::cont()
        }
        // The notification centre (`Ctrl-n`, Issue 08): opens on a snapshot
        // of the log; opening is the read gesture — `!N` zeroes the moment
        // the modal appears.
        Action::NotificationsOpen => {
            app.mark_notifications_read();
            app.mode = Mode::Notifications(app.notifications_snapshot());
            Outcome::cont()
        }
        // The baseline-diff summary: opens only with a --diff baseline loaded
        // (the payload is snapshotted like the other modals); otherwise a toast
        // explains how to get one.
        Action::DiffOpen => {
            match app.compute_diff_view() {
                Some(dv) => app.mode = Mode::Diff(dv),
                None => app.set_notice("No diff baseline (start with --diff <manifest|project>)"),
            }
            Outcome::cont()
        }
        // Export the PR impact report from the open Diff modal: render the
        // snapshotted `DiffView` to Markdown (so the file matches the screen) and
        // write it to `<project>_pr_impact.md`. The borrow of `app.mode` is
        // released before the mutating notice/effect.
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
