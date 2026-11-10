use super::*;

/// The shared yank arm: record the intent toast and request the clipboard
/// effect, or no-op when there is nothing to copy (no selection / empty SQL).
/// `run_effect` overwrites the notice if the clipboard write fails, so the
/// optimistic text never survives a failed copy.
fn yank_with_notice(app: &mut App, text: Option<String>, notice: &str) -> Outcome {
    match text {
        Some(text) => {
            app.set_notice(notice);
            Outcome::effect(Effect::Yank(text))
        }
        None => Outcome::cont(),
    }
}

/// Export filenames embed the node name, which comes from an untrusted
/// `manifest.json`: a crafted name (e.g. `../../x`) would let the export write
/// outside the working directory. Keep ASCII alphanumerics plus `.`/`-`/`_`
/// and map everything else (separators included) to `_`.
fn sanitize_file_stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- effects (performed by the loop, never here) ----
        Action::OpenEditor => match app
            .pending_or_selection()
            .and_then(|uid| app.file_path_for(&uid))
        {
            Some(path) => Outcome::effect(Effect::OpenEditor(path)),
            None => Outcome::cont(),
        },
        Action::YankId => {
            let text = app.yank_target_uid();
            yank_with_notice(app, text, "Copied unique_id")
        }
        Action::YankName => {
            let text = app.yank_target_uid().map(|uid| app.node_name_or_uid(&uid));
            yank_with_notice(app, text, "Copied model name")
        }
        Action::YankMermaid => {
            let text = app.lineage_mermaid();
            yank_with_notice(app, text, "Copied Mermaid lineage")
        }
        Action::YankDot => {
            let text = app.lineage_dot();
            yank_with_notice(app, text, "Copied DOT lineage")
        }
        Action::YankAscii => {
            let text = app.lineage_ascii();
            yank_with_notice(app, text, "Copied ASCII lineage")
        }
        Action::YankSql => {
            let text = app.yank_target_uid().and_then(|uid| app.raw_sql_for(&uid));
            yank_with_notice(app, text, "Copied raw SQL")
        }
        Action::YankCompiledSql => {
            // The yank target's compiled SQL (`yC` stays raw-only). Same
            // blank-is-absent rule as `selected_raw_sql` — an empty expansion
            // must not clobber the clipboard with nothing.
            let text = app
                .yank_target_uid()
                .and_then(|uid| app.dag.compiled(&uid).map(|c| (c.code.clone(), c.stale)))
                .filter(|(code, _)| !code.trim().is_empty());
            match text {
                Some((code, stale)) => {
                    let notice = if stale {
                        "Copied compiled SQL (stale)"
                    } else {
                        "Copied compiled SQL"
                    };
                    yank_with_notice(app, Some(code), notice)
                }
                None => {
                    app.set_notice("no compiled SQL for this node (run dbt compile)");
                    Outcome::cont()
                }
            }
        }
        Action::YankColumnLineage => {
            // `pending_col` is rewritten on every YankPrefix: `Some` = the
            // menu opened over the structure modal's column cursor; `None` =
            // it came from selection, where an ACTIVE trace still names a
            // column (the traced `col` is the fallback target). Only with no
            // column at all do we explain how to pick one.
            match app
                .pending_col
                .take()
                .or_else(|| app.tab().column_focus.as_ref().map(|f| f.col.clone()))
            {
                Some(col) => {
                    let text =
                        app.column_lineage_markdown(&col, crate::ClosureDirection::Downstream);
                    yank_with_notice(app, Some(text), "Copied column lineage")
                }
                None => {
                    app.set_notice("no column selected — Enter, h/l pick a column, then y l");
                    Outcome::cont()
                }
            }
        }
        Action::YankImpact => {
            let text = app
                .yank_target_uid()
                .and_then(|uid| app.impact_report_for(&uid));
            yank_with_notice(app, text, "Copied impact report")
        }
        Action::YankSelector => {
            let text = app
                .yank_target_uid()
                .and_then(|uid| app.dbt_selector_command_for(&uid));
            yank_with_notice(app, text, "Copied dbt selector")
        }
        Action::ExportLineage => {
            // The overview exports the whole graph — name the file after the
            // project, not the (incidental) selection.
            let stem = if app.ui_state.overview() {
                app.dag
                    .project_name()
                    .map(|p| format!("{}_overview", sanitize_file_stem(p)))
            } else {
                app.selected_name()
                    .map(|n| format!("{}_lineage", sanitize_file_stem(&n)))
            };
            match (stem, app.lineage_ascii()) {
                (Some(stem), Some(contents)) => {
                    let path = format!("{stem}.txt");
                    // Optimistic intent; `run_effect` overwrites it if the write fails.
                    app.set_notice(format!("Exported {path}"));
                    Outcome::effect(Effect::WriteFile { path, contents })
                }
                _ => Outcome::cont(),
            }
        }
        Action::Reload => Outcome::effect(Effect::ReloadManifest),
        // The loop performs the whole flow (resolve dbt from PATH, suspend the
        // TUI, run, adopt the manifest) and reports the outcome on the notice
        // channel — the reducer stays free of filesystem/PATH reads.
        Action::DbtParse => Outcome::effect(Effect::DbtParse),
        // Recenter and the lineage-view actions re-anchor the lineage; that is
        // applied size-aware in the loop, so the reducer just records intent.
        // Recenter additionally sends the cursor home, so `z` always means
        // "back to the rooted node".
        Action::Recenter => {
            app.reset_lineage_cursor();
            Outcome::cont()
        }
        Action::DepthDecrease => {
            let v = &mut app.tab_mut().lineage_view;
            v.depth = match v.depth {
                None => Some(3),
                Some(n) => Some(n.saturating_sub(1).max(1)),
            };
            Outcome::cont()
        }
        Action::DepthIncrease => {
            let v = &mut app.tab_mut().lineage_view;
            v.depth = match v.depth {
                None => None,
                Some(n) if n >= 8 => None, // widen past 8 hops → unlimited
                Some(n) => Some(n + 1),
            };
            Outcome::cont()
        }
        Action::ResetView => {
            app.tab_mut().lineage_view = LineageView::default();
            Outcome::cont()
        }
        Action::HistoryBack => {
            app.history_back();
            Outcome::cont()
        }
        Action::HistoryForward => {
            app.history_forward();
            Outcome::cont()
        }

        Action::ExportPrImpact => {
            let payload = if let Mode::Diff(dv) = &app.mode {
                let project = app.dag.project_name().unwrap_or("dbt").to_string();
                Some((project, app.pr_impact_markdown(dv)))
            } else {
                None
            };
            match payload {
                Some((project, contents)) => {
                    let path = format!("{}_pr_impact.md", sanitize_file_stem(&project));
                    // Optimistic intent; `run_effect` overwrites it on failure.
                    app.set_notice(format!("Exported {path}"));
                    Outcome::effect(Effect::WriteFile { path, contents })
                }
                None => Outcome::cont(),
            }
        }
        // Copy the suggested `dbt build --select …` command from the open Diff
        // modal. `y` takes the column-narrowed selector when the analysis
        // produced one (the most precise buildable set), falling back to the
        // wide `ci_command`; `Y` forces the wide command. A blank command
        // (nothing buildable changed) notices instead of silently clobbering
        // the clipboard. The `&app.mode` borrow is released (the command is
        // cloned) before the mutating notice/yank.
        Action::YankPrSelector | Action::YankPrSelectorWide => {
            let wide = matches!(action, Action::YankPrSelectorWide);
            let cmd = match &app.mode {
                Mode::Diff(dv) => Some(if wide {
                    (dv.pr.ci_command.clone(), "Copied dbt build command")
                } else {
                    match &dv.pr.ci_command_columns {
                        // The narrowed selector exists — prefer it.
                        Some(c) => (Some(c.clone()), "Copied column-level dbt build command"),
                        None => (dv.pr.ci_command.clone(), "Copied dbt build command"),
                    }
                }),
                _ => None,
            };
            match cmd {
                Some((Some(c), notice)) => yank_with_notice(app, Some(c), notice),
                Some((None, _)) => {
                    app.set_notice("No changed models to build");
                    Outcome::cont()
                }
                None => Outcome::cont(),
            }
        }
        // Lineage lens cycle: a pure view-pref mutation. Routed DIRECTLY here (not
        // through reduce_selection, which is only the legacy list-movement arms)
        // per the two-level-reducer contract.
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
