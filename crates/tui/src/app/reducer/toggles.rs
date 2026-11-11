use super::*;

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        Action::CycleLens => {
            app.ui_state.cycle_lens();
            // Data-less lenses look identical to Off — skip them so the cycle
            // never lands on a dead slot: Lint with no findings, Diff without a
            // --diff baseline, RunStatus/RunTime without a loaded
            // run_results.json.
            loop {
                let dead = match app.ui_state.lens() {
                    LineageLens::Lint => app.lint_findings().is_empty(),
                    LineageLens::Diff => app.diff().is_none(),
                    LineageLens::RunStatus | LineageLens::RunTime => app.run_results().is_none(),
                    // The ColumnImpact lens only tints inside a column trace —
                    // without one it looks identical to Off, so skip the slot.
                    // The EFFECTIVE focus is the test: an overview-suppressed
                    // focus (state alive, display off) is a dead slot too.
                    LineageLens::ColumnImpact => app.active_column_focus().is_none(),
                    _ => false,
                };
                if !dead {
                    break;
                }
                app.ui_state.cycle_lens();
            }
            Outcome::cont()
        }
        // Pointer-only lens set: the status chips name the lens they want
        // (the `run …` chip jumps straight to RunStatus rather than pedalling
        // the `t` cycle through dead slots). Never key-bound — the chip only
        // exists while its lens has data, so no dead-slot skip is needed.
        Action::SetLens(lens) => {
            app.ui_state.set_lens(lens);
            Outcome::cont()
        }

        Action::ToggleMinimap => {
            app.ui_state.toggle_minimap();
            Outcome::cont()
        }
        // Toggle the lineage density (a pure UiState view-pref, like the
        // minimap). The loop force-anchors next frame — the grid reshapes.
        Action::ToggleDensity => {
            app.ui_state.toggle_density();
            Outcome::cont()
        }
        // Toggle the whole-graph overview (a pure UiState view-pref, like the
        // density). Entering also switches the minimap on (the diagram is
        // guaranteed to overflow) and toasts an orientation hint; the loop
        // force-anchors next frame — the grid reshapes completely.
        Action::ToggleOverview => {
            // The home body has no lineage to overview — toggling here
            // would leak the flag into the next model tab opened.
            if app.home_active() {
                app.set_notice("overview needs a model tab".to_string());
                return Arm::Early(Outcome::cont());
            }
            app.ui_state.toggle_overview();
            if app.ui_state.overview() {
                if !app.ui_state.minimap_visible() {
                    app.ui_state.toggle_minimap();
                }
                // ASCII-only: the toast draws in BOTH glyph modes, and an em
                // dash is ambiguous-width (the exact desync ASCII mode exists
                // to avoid).
                app.set_notice(format!("overview: {} nodes (w to exit)", app.dag.len()));
            }
            Outcome::cont()
        }
        // `<`/`>`: nudge the saved pane width by §5's 4-column step, a pure
        // UiState view-pref like the minimap — direct here, never through
        // `reduce_selection`. The pane follows §5's focus rule: the inspector
        // while focused, the list otherwise. The floor is in the write; the
        // terminal-relative cap is the event loop's post-clamp (size-aware)
        // plus `pane_layout`'s display clamp — the reducer stays size-blind.
        Action::PaneNarrow | Action::PaneWiden => {
            let step = if matches!(action, Action::PaneWiden) {
                4
            } else {
                -4
            };
            if app.ui_state.focus() == Focus::Inspector {
                app.ui_state.nudge_inspector_width(step);
            } else {
                app.ui_state.nudge_list_width(step);
            }
            Outcome::cont()
        }
        // `=`: every resizable pane's saved width back to its §1 default
        // (§5 "全部を既定値に" — the inspector's saved width included, though
        // its pane only materializes in Issue 04).
        Action::PaneWidthsReset => {
            app.ui_state.reset_pane_width(crate::ui::Splitter::List);
            app.ui_state
                .reset_pane_width(crate::ui::Splitter::Inspector);
            Outcome::cont()
        }

        Action::ToggleInspector => {
            app.ui_state.cycle_inspector();
            Outcome::cont()
        }
        // A click on an inspector column row moves the pane's column cursor
        // to it — the same select-before-Enter contract the columns modal's
        // `ColumnPick` keeps (double-click wiring lands with Issue 05).
        Action::InspectorColumnPick(i) => {
            let cols = app
                .inspector_view()
                .map(|v| v.detail.col_names.len())
                .unwrap_or(0);
            app.ui_state.set_inspector_col(if cols == 0 {
                usize::MAX
            } else {
                i.min(cols - 1)
            });
            Outcome::cont()
        }
        // `p` two-node path: first press anchors the path start at the
        // selection (the highlight then runs anchor↔cursor), second press
        // clears it back to root↔cursor. Pure state + a toast; the
        // path-set BFS reads `path_anchor` next frame — no cache reset needed
        // (styles are recomputed per draw anyway).
        Action::PathToggle => {
            if let Some(uid) = app.tab_mut().path_anchor.take() {
                let name = app.node_name_or_uid(&uid);
                app.set_notice(format!("path anchor cleared ({name})"));
            } else if let Some(uid) = app.pending_or_selection() {
                let name = app.node_name_or_uid(&uid);
                app.tab_mut().path_anchor = Some(uid);
                app.set_notice(format!(
                    "path anchor: {name} — move cursor to target (p clears)"
                ));
            }
            Outcome::cont()
        }
        // Cycle the colour theme (App-level data: the loaded list lives on the
        // App, not UiState). Pure state + a toast; the loop hands the new
        // active theme to `RenderCtx` next frame — no cache is touched (the
        // cached layout's attrs are semantic, colours resolve at draw).
        Action::ThemeCycle => {
            app.cycle_theme();
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
