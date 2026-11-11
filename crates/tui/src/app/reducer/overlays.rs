use super::*;

/// The structure modal's snapshot — every Dag-derived field resolved ONCE at
/// open so the render layer needs no `Dag` (the modal is a pure payload).
/// `col_names` is the cursor-addressable list — declared columns, else the
/// resolver's inferred names so an undocumented model still gets a cursor +
/// counts. `test_index` starts detached (`usize::MAX`): the COLUMNS section
/// owns the h/l cursor until `Tab` moves it to the tests side.
fn detail_view_for(app: &App, uid: String) -> DetailView {
    let name = app.node_name_or_uid(&uid);
    let detail = app.dag.detail(&uid).cloned().unwrap_or_default();
    let tests = app.dag.tests(&uid).to_vec();
    // Blast radius of THIS node (the focus target — may be a non-root
    // source/seed/snapshot the lineage cursor sits on, NOT the root).
    let (downstream_count, upstream_count) = app.impact_counts_for(&uid);
    let catalog = app.catalog_note(&uid, &detail);
    // Column-level lineage snapshot (lazy + memoized in the resolver;
    // resolving THIS node + its direct children is the only cost).
    let col_counts = app.column_counts_for(&uid);
    let col_names: Vec<String> = if detail.columns.is_empty() {
        col_counts.keys().cloned().collect()
    } else {
        detail.columns.iter().map(|c| c.name.clone()).collect()
    };
    let col_index = if col_names.is_empty() { usize::MAX } else { 0 };
    let stale = matches!(
        crate::resolution_sql(&app.dag, &uid),
        Some((_, crate::Confidence::Stale))
    );
    // `column_counts_for` just resolved this uid, so the fallback
    // flag is settled: `[name-match]` marks the name-match path.
    let name_match = app.cll.is_fallback(&uid);
    DetailView {
        model_id: uid,
        name,
        detail,
        tests,
        downstream_count,
        upstream_count,
        catalog,
        col_names,
        col_index,
        test_index: usize::MAX,
        col_counts,
        stale,
        name_match,
        scroll: 0,
    }
}

/// The inspector pane's snapshot for `uid` — built ONCE per
/// `(uid, generation)` into `LineageCaches::inspector`, so the per-frame
/// redraw is an `Rc` clone. The `detail` field is the verbatim output of
/// [`detail_view_for`] (the pane shares the structure
/// modal's construction, it never reassembles domain data itself); the
/// remaining fields are the pane-only extras the modal doesn't carry:
/// materialization tag, the third (exposure) component of the impact
/// breakdown, per-column catalog types, the last-run row, this node's lint
/// findings, and the diff readout.
pub(in crate::app) fn inspector_view_for(app: &App, uid: String) -> crate::InspectorView {
    let detail = detail_view_for(app, uid.clone());
    // `impact_breakdown_for`'s third component — the downstream EXPOSURE
    // count `impact_counts_for` deliberately drops for the modal's
    // two-number blast radius.
    let (_, _, exposures) = app.impact_breakdown_for(&uid);
    // The owner entry under the configured meta key (`owner_meta_key`,
    // default `"owner"`) — resolved here so the render layer never reads
    // the config.
    let owner = detail.detail.meta.get(&app.owner_meta_key).cloned();
    let tag = app
        .dag
        .get(&uid)
        .map(|n| {
            crate::MaterializationClass::classify(
                n.resource_type.as_str(),
                n.materialized.as_deref(),
            )
            .tag()
        })
        .unwrap_or("model");
    // The catalog's real warehouse type per declared column — keyed by the
    // column name as declared (the row lookup needs no normalization; the
    // catalog's own matching is case-insensitive already). Skips the
    // `catalog_note` line shape entirely: the pane renders types INLINE on
    // each column row, not as a separate `  col: type` block.
    let mut col_types = std::collections::BTreeMap::new();
    if let Some(node) = app.catalog().and_then(|c| c.get(&uid)) {
        for col in &detail.detail.columns {
            if let Some(ty) = node.column_type(&col.name) {
                col_types.insert(col.name.clone(), ty.to_string());
            }
        }
    }
    // The `run_results.json` row — `(status word, seconds)`. The status word
    // follows the artifact's own vocabulary so the pane reads like `dbt
    // run`'s summary line.
    let run = app.run_results().and_then(|rr| {
        rr.get(&uid).map(|r| {
            let word = match &r.status {
                crate::RunStatus::Success => "success".to_string(),
                crate::RunStatus::Error => "error".to_string(),
                crate::RunStatus::Fail => "fail".to_string(),
                crate::RunStatus::Warn => "warn".to_string(),
                crate::RunStatus::Skipped => "skipped".to_string(),
                crate::RunStatus::Other(s) => s.clone(),
            };
            (word, r.execution_time)
        })
    });
    // This node's lint findings — `"{rule}: {message}"`, in `lint_findings`'
    // own deterministic order. A finding lands here only when `node_uid` IS
    // this node: `related` uids are context on the OTHER node's violation.
    let lint = app
        .lint_findings()
        .iter()
        .filter(|f| f.node_uid == uid)
        .map(|f| format!("{}: {}", f.rule.name(), f.message))
        .collect();
    // The diff readout for THIS node — `+ added`, or `~ modified:` plus the
    // baseline's recorded reasons (a fixed-order human list). `None` on
    // `Unchanged` (nothing to show) or when no baseline is loaded.
    let diff = app.diff().and_then(|d| match d.status(&uid) {
        crate::DiffStatus::Added => Some("+ added".to_string()),
        crate::DiffStatus::Modified => Some(match d.modified.get(&uid) {
            Some(reasons) if !reasons.is_empty() => {
                format!("~ modified: {}", reasons.join(", "))
            }
            _ => "~ modified".to_string(),
        }),
        crate::DiffStatus::Unchanged => None,
    });
    crate::InspectorView {
        detail,
        tag,
        exposures,
        owner,
        col_types,
        run,
        lint,
        diff,
    }
}

pub(super) fn apply(app: &mut App, action: Action) -> Arm {
    let out: Outcome = match action {
        // ---- overlays ----
        Action::HelpToggle => {
            // Esc/`?`/`q` peel the INNERMOST help state first: a live
            // `/`-filter (editing or committed) clears before the overlay
            // itself closes — and closing restores the mode it was opened
            // over (`back` keeps a modal's snapshot, payload and all).
            app.mode = match &app.mode {
                Mode::Help(h) if h.filter.is_some() => Mode::Help(HelpView {
                    scroll: h.scroll,
                    filter: None,
                    editing: false,
                    from: h.from,
                    back: h.back.clone(),
                }),
                Mode::Help(h) => (*h.back).clone(),
                _ => Mode::Help(HelpView::new(app.mode.clone())),
            };
            Outcome::cont()
        }
        Action::DetailOpen => {
            // Inspector-focused Enter commits the pane's cursored COLUMN row
            // to the column-trace view — the same flow Enter on the
            // structure modal's column cursor takes (Enter on a column row
            // jumps to the column lineage). No row cursored → nothing to commit;
            // the inspector keeps focus.
            if app.ui_state.focus() == Focus::Inspector {
                let col = {
                    let idx = app.ui_state.inspector_col();
                    app.inspector_view().and_then(|v| {
                        v.detail
                            .col_names
                            .get(idx)
                            .map(|c| crate::ColumnRef::new(&v.detail.model_id, c))
                    })
                };
                if let Some(col) = col {
                    app.column_focus_enter(col);
                }
                return Arm::Early(Outcome::cont());
            }
            // Enter acts on the lineage CURSOR when the lineage pane is focused
            // (else the selection): an off-root cursor on a model RE-ROOTS to it
            // (committing the cursor, same as a mouse click); anything else —
            // the root itself, or a non-selectable source/seed/snapshot — opens
            // the structure modal, with detail + tests snapshotted into the mode
            // payload (cloned from the Dag side maps), so the render layer needs
            // no Dag.
            let root = app.selected_unique_id();
            let Some(uid) = app.pending_or_focus() else {
                return Arm::Early(Outcome::cont());
            };
            // A `+M more` / `-M less` marker under the cursor TOGGLES its
            // sibling group instead of opening the structure modal (the marker
            // is chrome, not a Dag node — there is nothing to detail).
            if let Some(group) = app.collapse_group(&uid) {
                app.toggle_collapse(&group.key);
                return Arm::Early(Outcome::cont());
            }
            if app.ui_state.focus() == Focus::RightPane
                && Some(&uid) != root.as_ref()
                && app
                    .dag
                    .get(&uid)
                    .is_some_and(|n| n.resource_type == "model")
            {
                app.jump_to(&uid);
                return Arm::Early(Outcome::cont());
            }
            app.mode = Mode::Detail(detail_view_for(app, uid));
            Outcome::cont()
        }
        Action::DetailClose => {
            // Esc/q is mode-agnostic on purpose — whichever BINDINGS row named
            // it, the close restores what the overlay stacked over: the yank
            // which-key and the node menu carry their origin mode in `back`
            // (so Structure -> y -> Esc lands back on the structure modal,
            // not the idle layer), every other overlay closes to the idle
            // mode (Selection — or Home while the home tab is active).
            let idle = app.idle_mode();
            app.mode = match std::mem::replace(&mut app.mode, Mode::Selection) {
                Mode::Yank { back, .. } => back.map(|b| *b).unwrap_or_else(|| idle.clone()),
                Mode::Menu(ms) => ms.back.map(|b| *b).unwrap_or_else(|| idle.clone()),
                _ => idle,
            };
            Outcome::cont()
        }
        // Open the yank-prefix which-key (`y` + key). The menu itself is
        // rendered from the M::Yank BINDINGS rows; the one-shot close lives in
        // the post-match step at the bottom of this function. Over the Detail
        // modal BOTH the modal's node and its column target are stashed — and
        // the modal itself is stacked into `back` so the which-key's close
        // RESTORES it instead of landing on Selection — or every
        // `y<key>` yank would silently act on the list selection instead of
        // the modal's node. (The `y` row exists in Mode::Detail's bindings.)
        Action::YankPrefix => {
            let back = match std::mem::replace(&mut app.mode, Mode::Selection) {
                Mode::Detail(dv) => {
                    app.pending_node = Some(dv.model_id.clone());
                    app.pending_col = if dv.col_index != usize::MAX {
                        dv.col_names
                            .get(dv.col_index)
                            .map(|c| crate::ColumnRef::new(&dv.model_id, c))
                    } else {
                        None
                    };
                    Some(Box::new(Mode::Detail(dv)))
                }
                m => {
                    app.pending_node = None;
                    app.pending_col = None;
                    match m {
                        // A non-overlay origin needs no restore slot: closing
                        // lands on the idle mode by default anyway.
                        Mode::Selection | Mode::Home | Mode::Search(_) | Mode::Menu(_) => None,
                        m => Some(Box::new(m)),
                    }
                }
            };
            app.mode = Mode::Yank { back, sel: 0 };
            Outcome::cont()
        }
        _ => return Arm::Pass,
    };
    Arm::Done(out)
}
