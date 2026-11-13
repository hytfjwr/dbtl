use super::*;

// ---- The inspector pane --------------------------------------------------------

/// The inspector's target rule: the list selection while the list is
/// focused; the lineage cursor's node while the lineage or inspector pane is
/// focused; the rooted selection when the cursor is home.
#[test]
fn inspector_target_follows_focus_and_cursor() {
    let mut a = app();
    let sel = a.selected_unique_id().expect("fixture selects");
    assert_eq!(
        a.inspector_target().as_deref(),
        Some(sel.as_str()),
        "list focus inspects the selection"
    );

    // Lineage focus with the cursor home still lands on the root (the
    // selection) — `lineage_cursor_uid` validates home to the root uid.
    a.ui_state.set_focus(Focus::RightPane);
    assert_eq!(
        a.inspector_target().as_deref(),
        Some(sel.as_str()),
        "home cursor inspects the root"
    );

    // Walk the cursor onto a different drawn node: the inspector tracks it.
    let other = {
        let lay = a.styled_lineage_layout().expect("fixture lays out");
        lay.rects
            .keys()
            .find(|u| *u != &sel)
            .cloned()
            .expect("the rooted subgraph has a second node")
    };
    a.tab_mut().lineage_cursor = Some(other.clone());
    assert_eq!(
        a.inspector_target().as_deref(),
        Some(other.as_str()),
        "lineage focus inspects the cursor's node"
    );

    // Inspector focus keeps the same target (the pane shows what you
    // inspected into — it never inspects itself).
    a.ui_state.set_focus(Focus::Inspector);
    assert_eq!(a.inspector_target().as_deref(), Some(other.as_str()));

    // Empty app: nothing to inspect.
    let e = empty_app();
    assert_eq!(e.inspector_target(), None);
}

/// `inspector_view` shares `detail_view_for`'s assembly — the same tests /
/// col_names / impact counts the structure modal would show — plus the
/// pane-only fields (tag, exposures, owner).
#[test]
fn inspector_view_shares_the_detail_view_assembly() {
    let a = app();
    let uid = a.selected_unique_id().expect("fixture selects");
    let view = a.inspector_view().expect("fixture inspects");
    assert_eq!(view.detail.model_id, uid);
    assert_eq!(
        view.detail.tests.len(),
        a.dag.tests(&view.detail.model_id).len(),
        "the test list rides the shared DetailView"
    );
    // The pane's third impact number is the exposure count the modal's
    // two-number blast radius drops — assert the shared breakdown's value.
    let uid = view.detail.model_id.clone();
    let (_, _, exposures) = a.impact_breakdown_for(&uid);
    assert_eq!(view.exposures, exposures);
    // The materialization tag is the classifier's — not a re-derived string.
    let node = a.dag.get(&uid).expect("uid in dag");
    assert_eq!(
        view.tag,
        MaterializationClass::classify(node.resource_type.as_str(), node.materialized.as_deref())
            .tag()
    );
    // The owner row is the meta entry under the configured key (default
    // `owner`), resolved at build time.
    let detail = a.dag.detail(&uid).cloned().unwrap_or_default();
    assert_eq!(
        view.owner,
        detail.meta.get(&a.owner_meta_key).cloned(),
        "owner is the owner_meta_key meta entry"
    );
    // Without side artifacts: no run / no diff / empty catalog types.
    assert_eq!(view.run, None, "no run_results loaded");
    assert_eq!(view.diff, None, "no --diff baseline loaded");
    assert!(view.col_types.is_empty(), "no catalog loaded");
}

/// Last run / lint / diff fields on the built view.
#[test]
fn inspector_view_collects_run_lint_and_diff() {
    // --- run_results: the pane reads the node's own row's status + time ---
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    load_run_results(
        &mut a,
        r#"{"metadata":{"generated_at":"2024-05-01T00:00:00Z"},"results":[
            {"unique_id":"model.p.stg_a","status":"error","execution_time":2.0},
            {"unique_id":"model.p.fct_b","status":"warn","execution_time":5.0},
            {"unique_id":"model.p.mart_c","status":"skipped","execution_time":null}
        ]}"#,
    );
    a.select_by_unique_id("model.p.stg_a");
    let view = a.inspector_view().expect("run app inspects");
    assert_eq!(view.run, Some(("error".to_string(), Some(2.0))));
    a.select_by_unique_id("model.p.mart_c");
    let view = a.inspector_view().expect("inspect mart_c");
    assert_eq!(
        view.run,
        Some(("skipped".to_string(), None)),
        "a null execution_time stays absent"
    );

    // --- lint: only findings whose node_uid IS the inspected node land ---
    let mut a = app();
    let findings = a.lint_findings();
    let uid = findings
        .first()
        .map(|f| f.node_uid.clone())
        .expect("the fixture's default lint config finds violations");
    a.select_by_unique_id(&uid);
    let view = a.inspector_view().expect("inspect a violating node");
    let want: Vec<crate::InspectorLintFinding> = a
        .lint_findings()
        .iter()
        .filter(|f| f.node_uid == uid)
        .map(|f| crate::InspectorLintFinding {
            severity: f.severity,
            rule: f.rule.name().to_string(),
            summary: f.rule.description().to_string(),
            message: f.message.clone(),
        })
        .collect();
    assert_eq!(
        view.detail.lint, want,
        "the node's own findings, in finding order"
    );
    assert!(
        view.detail.lint.iter().all(|f| !f.summary.is_empty()),
        "every snapshot carries the registry summary"
    );
    // A lint-clean node reports an empty list (the pane then draws the
    // "all checks pass" verdict — see the render test).
    let linted: std::collections::HashSet<&str> = a
        .lint_findings()
        .iter()
        .map(|f| f.node_uid.as_str())
        .collect();
    let clean = a
        .dag
        .nodes()
        .values()
        .find(|n| n.resource_type == "model" && !linted.contains(n.unique_id.as_str()))
        .expect("fixture has a lint-clean model")
        .unique_id
        .clone();
    a.select_by_unique_id(&clean);
    assert!(
        a.inspector_view()
            .expect("inspect clean")
            .detail
            .lint
            .is_empty(),
        "{clean} carries no findings"
    );

    // --- diff: `+ added` / `~ modified: <reasons>` per node ---
    let mut a = diff_app();
    a.select_by_unique_id("model.p.c");
    let view = a.inspector_view().expect("inspect added node");
    assert_eq!(view.diff.as_deref(), Some("+ added"));
    a.select_by_unique_id("model.p.b");
    let view = a.inspector_view().expect("inspect modified node");
    let d = view.diff.as_deref().expect("modified reads");
    assert!(d.starts_with("~ modified"), "got {d:?}");
    a.select_by_unique_id("model.p.a");
    assert_eq!(
        a.inspector_view().expect("inspect unchanged").diff,
        None,
        "unchanged nodes carry no diff readout"
    );
}

/// The cached snapshot: same `(uid, generation)` → the same `Rc` (a pointer
/// clone); a generation bump (run results landing — a style input like lint
/// and diff) → a rebuild.
#[test]
fn inspector_view_caches_by_uid_and_generation() {
    let mut a = App::new(run_dag(), PathBuf::from("/p/target/manifest.json"));
    a.select_by_unique_id("model.p.stg_a");
    let v1 = a.inspector_view().expect("inspect");
    let v2 = a.inspector_view().expect("cached inspect");
    assert!(std::rc::Rc::ptr_eq(&v1, &v2), "same key → Rc clone");

    load_run_results(
        &mut a,
        r#"{"metadata":{},"results":[{"unique_id":"model.p.stg_a","status":"success","execution_time":1.5}]}"#,
    );
    let v3 = a.inspector_view().expect("rebuilt inspect");
    assert!(
        !std::rc::Rc::ptr_eq(&v1, &v3),
        "set_run_results bumps the generation → rebuild"
    );
    assert_eq!(v3.run, Some(("success".to_string(), Some(1.5))));

    // A different target is a different key even within one generation.
    a.select_by_unique_id("model.p.fct_b");
    let v4 = a.inspector_view().expect("inspect fct_b");
    assert!(!std::rc::Rc::ptr_eq(&v3, &v4));
}

/// `I` cycles the persisted pref `auto → on → off → auto`; the resolved
/// on-screen flag is untouched (the event loop owns that).
#[test]
fn toggle_inspector_cycles_the_pref() {
    use crate::ui::InspectorPolicy;
    let mut a = app();
    assert_eq!(a.ui_state.inspector_policy(), InspectorPolicy::Auto);
    apply_action(&mut a, Action::ToggleInspector);
    assert_eq!(a.ui_state.inspector_policy(), InspectorPolicy::On);
    apply_action(&mut a, Action::ToggleInspector);
    assert_eq!(a.ui_state.inspector_policy(), InspectorPolicy::Off);
    apply_action(&mut a, Action::ToggleInspector);
    assert_eq!(a.ui_state.inspector_policy(), InspectorPolicy::Auto);
}

/// The `Tab` ring is `list → lineage → inspector → list`, skipping panes
/// that aren't on screen.
#[test]
fn tab_ring_walks_the_three_panes() {
    let mut s = UiState::new(10);
    // Inspector hidden (default): the original two-pane toggle.
    apply_to_state(&mut s);
    fn apply_to_state(s: &mut UiState) {
        reduce_selection(s, Action::ToggleFocus);
    }
    assert_eq!(s.focus(), Focus::RightPane);
    apply_to_state(&mut s);
    assert_eq!(s.focus(), Focus::List);

    // Shown: the full ring.
    s.set_inspector_shown(true);
    assert_eq!(s.focus(), Focus::List);
    apply_to_state(&mut s);
    assert_eq!(s.focus(), Focus::RightPane);
    apply_to_state(&mut s);
    assert_eq!(s.focus(), Focus::Inspector);
    apply_to_state(&mut s);
    assert_eq!(s.focus(), Focus::List);

    // List hidden + inspector shown: lineage ↔ inspector pair.
    s.set_list_visible(false);
    assert_eq!(s.focus(), Focus::RightPane, "hidden list drops focus");
    apply_to_state(&mut s);
    assert_eq!(s.focus(), Focus::Inspector);
    apply_to_state(&mut s);
    assert_eq!(s.focus(), Focus::RightPane, "ring skips the hidden list");
}

/// Hiding the inspector (the loop's resolved answer) drops focus off it —
/// the same pin `toggle_list_pane` keeps on the list.
#[test]
fn hiding_the_inspector_unfocuses_it() {
    let mut s = UiState::new(10);
    s.set_inspector_shown(true);
    s.set_focus(Focus::Inspector);
    s.set_inspector_shown(false);
    assert_eq!(
        s.focus(),
        Focus::RightPane,
        "focus lands on the lineage you were inspecting into"
    );
    // A list-focused app is untouched by the transition.
    let mut s = UiState::new(10);
    s.set_inspector_shown(false);
    assert_eq!(s.focus(), Focus::List);
}

/// A changed inspector target re-homes the pane: scroll and the column
/// cursor reset (carrying a stale offset into different content reads as a
/// bug); re-setting the same target is a no-op.
#[test]
fn inspector_target_change_resets_scroll_and_column() {
    let mut s = UiState::new(10);
    s.set_inspector_target("model.p.a");
    s.set_inspector_scroll(7);
    s.set_inspector_col(2);
    s.set_inspector_target("model.p.b");
    assert_eq!(s.inspector_scroll(), 0);
    assert_eq!(s.inspector_col(), usize::MAX);
    assert_eq!(s.inspector_target(), "model.p.b");
    s.set_inspector_scroll(3);
    s.set_inspector_target("model.p.b");
    assert_eq!(s.inspector_scroll(), 3, "same target → no reset");
}

/// While the inspector is focused the movement/jump/d/u keys scroll ITS
/// content — never the list selection or the lineage cursor.
#[test]
fn inspector_focus_routes_the_scroll_keys() {
    let mut a = app();
    a.ui_state.set_inspector_shown(true);
    a.ui_state.set_focus(Focus::Inspector);
    let sel = a.ui_state.selected();

    apply_action(&mut a, Action::MoveDown);
    assert_eq!(a.ui_state.inspector_scroll(), 1);
    apply_action(&mut a, Action::MoveDown);
    apply_action(&mut a, Action::MoveUp);
    assert_eq!(a.ui_state.inspector_scroll(), 1);
    // h/l carry no inspector meaning (the column cursor is a pick
    // affordance, not an h/l one).
    apply_action(&mut a, Action::MoveLeft);
    apply_action(&mut a, Action::MoveRight);
    assert_eq!(a.ui_state.inspector_scroll(), 1, "h/l don't scroll");

    // d/u — Selection-mode's upstream/downstream toggles — page the pane.
    apply_action(&mut a, Action::ToggleDownstream);
    assert_eq!(a.ui_state.inspector_scroll(), 11);
    apply_action(&mut a, Action::ToggleUpstream);
    assert_eq!(a.ui_state.inspector_scroll(), 1);
    // And the lineage view filters were NOT flipped by d/u.
    assert!(
        a.tab().lineage_view.upstream && a.tab().lineage_view.downstream,
        "d/u under inspector focus scroll, never toggle"
    );

    // g/G home/end the pane's scroll (the loop clamps the end intent).
    apply_action(&mut a, Action::JumpBottom);
    assert_eq!(a.ui_state.inspector_scroll(), usize::MAX);
    apply_action(&mut a, Action::JumpTop);
    assert_eq!(a.ui_state.inspector_scroll(), 0);
    // PageUp/PageDown ride the same fixed step as d/u.
    apply_action(&mut a, Action::PageDown);
    assert_eq!(a.ui_state.inspector_scroll(), 10);

    // The list selection never moved.
    assert_eq!(a.ui_state.selected(), sel);
}

/// `<`/`>` resize the pane that owns focus: the inspector while focused,
/// the list otherwise (the list rule predates the pane).
#[test]
fn pane_width_keys_target_the_focused_pane() {
    let mut a = app();
    a.ui_state.set_inspector_shown(true);
    a.ui_state.set_focus(Focus::Inspector);
    let (list_w, ins_w) = (a.ui_state.list_width(), a.ui_state.inspector_width());
    apply_action(&mut a, Action::PaneWiden);
    assert_eq!(a.ui_state.inspector_width(), ins_w + 4);
    assert_eq!(a.ui_state.list_width(), list_w, "list width untouched");
    apply_action(&mut a, Action::PaneNarrow);
    apply_action(&mut a, Action::PaneNarrow);
    assert_eq!(a.ui_state.inspector_width(), ins_w - 4);
    // `=` still resets BOTH saved widths.
    a.ui_state.set_list_width(60);
    apply_action(&mut a, Action::PaneWidthsReset);
    assert_eq!(a.ui_state.list_width(), DEFAULT_LIST_WIDTH);
    assert_eq!(a.ui_state.inspector_width(), DEFAULT_INSPECTOR_WIDTH);
}

/// `InspectorColumnPick` moves the pane's column cursor, clamped into the
/// built view's `col_names`.
#[test]
fn inspector_column_pick_clamps_to_the_columns() {
    let mut a = app();
    a.ui_state.set_inspector_shown(true);
    a.ui_state.set_focus(Focus::Inspector);
    let cols = a
        .inspector_view()
        .map(|v| v.detail.col_names.len())
        .unwrap_or(0);
    assert!(cols > 1, "fixture's first model declares columns");
    apply_action(&mut a, Action::InspectorColumnPick(1));
    assert_eq!(a.ui_state.inspector_col(), 1);
    apply_action(&mut a, Action::InspectorColumnPick(usize::MAX));
    assert_eq!(
        a.ui_state.inspector_col(),
        cols - 1,
        "a click past the last row clamps into the list"
    );
}

/// A pick arms the reveal for exactly ONE clamped frame — then j/k/d/u/
/// wheel scroll past the picked row freely. Re-pinning every frame would
/// cage the scroll keys inside `inner_h` of the last pick forever.
#[test]
fn inspector_pick_reveal_is_one_shot() {
    let mut a = app();
    a.ui_state.set_inspector_shown(true);
    a.ui_state.set_focus(Focus::Inspector);
    let view = a.inspector_view().expect("fixture inspects");
    let cols = view.detail.col_names.len();
    assert!(cols > 1, "fixture's first model declares columns");

    // No pick yet: nothing armed.
    assert!(!a.ui_state.take_inspector_reveal());
    apply_action(&mut a, Action::InspectorColumnPick(cols - 1));
    assert_eq!(a.ui_state.inspector_col(), cols - 1);
    // The pick arms the reveal; the drain is one-shot.
    assert!(a.ui_state.take_inspector_reveal());
    assert!(
        !a.ui_state.take_inspector_reveal(),
        "a second frame scrolls free — the clamp can't drag back"
    );

    // The loop-shaped frame: with the intent drained, the clamp scrolls
    // the picked row into view…
    let (w, h) = (30usize, 5usize);
    let pinned = crate::ui::clamp_inspector_scroll(
        view.as_ref(),
        a.glyph_mode,
        a.active_theme(),
        w,
        h,
        0,
        cols - 1,
    );
    assert!(pinned > 0, "the reveal scrolls the picked row into view");
    // …and the same clamp WITHOUT the intent leaves a scrolled-away
    // viewport exactly where the user's keys put it (0 here).
    let home = crate::ui::clamp_inspector_scroll(
        view.as_ref(),
        a.glyph_mode,
        a.active_theme(),
        w,
        h,
        0,
        usize::MAX,
    );
    assert_eq!(home, 0, "no re-pin once the reveal is spent");

    // A retarget clears a still-pending reveal.
    a.ui_state.set_inspector_col(0);
    a.ui_state.set_inspector_target("model.p.other");
    assert!(!a.ui_state.take_inspector_reveal());
}

/// Enter on a cursored inspector column row enters the SAME column-trace
/// flow the structure modal's Enter runs (focus lands on the lineage — the
/// trace IS the lineage view now).
#[test]
fn inspector_enter_traces_the_cursored_column() {
    let mut a = app();
    a.ui_state.set_inspector_shown(true);
    a.ui_state.set_focus(Focus::Inspector);
    let (uid, column) = {
        let v = a.inspector_view().expect("fixture inspects");
        (v.detail.model_id.clone(), v.detail.col_names[0].clone())
    };
    a.ui_state.set_inspector_col(0);
    apply_action(&mut a, Action::DetailOpen);
    let focus = a.active_column_focus().expect("Enter traced the column");
    assert_eq!(focus.col.node, uid);
    assert_eq!(focus.col.column, crate::normalize_column(&column));
    assert_eq!(
        a.ui_state.focus(),
        Focus::RightPane,
        "the trace owns the lineage pane"
    );

    // Enter with NO column cursored stays put (nothing to commit).
    let mut a = app();
    a.ui_state.set_inspector_shown(true);
    a.ui_state.set_focus(Focus::Inspector);
    apply_action(&mut a, Action::DetailOpen);
    assert!(a.active_column_focus().is_none());
    assert_eq!(a.ui_state.focus(), Focus::Inspector);
    assert!(matches!(a.mode, Mode::Selection), "no modal opened");
}

/// The inspector pref round-trips through the persisted state (and ignores
/// unknown spellings, like every prefs field).
#[test]
fn inspector_policy_persists() {
    use crate::persist::PersistedState;
    use crate::ui::InspectorPolicy;
    let mut a = app();
    a.ui_state.set_inspector_policy(InspectorPolicy::On);
    let snap = a.prefs_snapshot();
    assert_eq!(snap.inspector.as_deref(), Some("on"));

    let mut b = app();
    b.apply_prefs(&snap);
    assert_eq!(b.ui_state.inspector_policy(), InspectorPolicy::On);

    b.apply_prefs(&PersistedState {
        inspector: Some("bogus".to_string()),
        ..PersistedState::default()
    });
    assert_eq!(
        b.ui_state.inspector_policy(),
        InspectorPolicy::On,
        "unknown spelling skipped, current pref kept"
    );
    b.apply_prefs(&PersistedState {
        inspector: Some("off".to_string()),
        ..PersistedState::default()
    });
    assert_eq!(b.ui_state.inspector_policy(), InspectorPolicy::Off);
}
