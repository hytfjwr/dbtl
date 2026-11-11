use super::*;

// ---- command palette round-trips ----

/// Open the palette, type `query`, then return the resulting `App` so a test
/// can confirm the filter/selection state before Enter.
fn open_palette_and_type(query: &str) -> App {
    let mut a = app();
    apply_action(&mut a, Action::PaletteOpen);
    assert!(matches!(a.mode, Mode::Palette(_)), "PaletteOpen → Palette");
    for c in query.chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    a
}

#[test]
fn palette_open_type_enter_runs_the_resolved_action() {
    // Filter to the minimap toggle and run it: the minimap pref flips and the
    // mode returns to Selection (the recursively-applied action's effect lands).
    let mut a = open_palette_and_type("minimap");
    let before = a.ui_state.minimap_visible();
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.mode, Mode::Selection, "palette closes on confirm");
    assert_ne!(
        a.ui_state.minimap_visible(),
        before,
        "the resolved ToggleMinimap action ran"
    );
}

#[test]
fn palette_confirm_propagates_quit_outcome() {
    // Choosing the quit command must quit (the whole Outcome propagates).
    let mut a = open_palette_and_type("quit");
    // "quit" is the only command whose help contains that subsequence.
    assert!(
        palette_candidates("quit")
            .iter()
            .any(|b| b.action == Action::Quit),
        "the quit command is a candidate"
    );
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(out.quit, "confirming the quit command quits");
    assert_eq!(
        a.mode,
        Mode::Selection,
        "mode set to Selection before apply"
    );
}

#[test]
fn palette_confirm_propagates_editor_effect() {
    // Choosing "open SQL in $EDITOR" must yield the editor Effect. A model
    // with a resolvable file path is selected first so the effect is produced.
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut a, Action::PaletteOpen);
    for c in "$EDITOR".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    // The OpenEditor row's help is "open SQL in $EDITOR".
    assert!(
        palette_candidates("$EDITOR")
            .iter()
            .any(|b| b.action == Action::OpenEditor),
        "the editor command is a candidate"
    );
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(
        matches!(out.effects.as_slice(), [Effect::OpenEditor(_)]),
        "confirming the editor command emits its Effect: {:?}",
        out.effects
    );
    assert_eq!(a.mode, Mode::Selection);
}

#[test]
fn palette_selected_clamps_when_the_filter_shrinks() {
    // Move the cursor down on the full list, then narrow the query so fewer
    // candidates remain: `selected` must clamp into range (never out of bounds).
    let mut a = open_palette_and_type("");
    // Step down a few times over the full candidate list.
    for _ in 0..5 {
        apply_action(&mut a, Action::SearchMove(Direction::Down));
    }
    let moved = match &a.mode {
        Mode::Palette(p) => p.selected,
        _ => unreachable!(),
    };
    assert!(moved > 0, "the palette cursor moved down");
    // Now type a narrowing query; `selected` resets to 0 on each keystroke and
    // can never exceed the (smaller) candidate count.
    for c in "lens".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    if let Mode::Palette(p) = &a.mode {
        let count = palette_candidates(&p.query).len();
        assert!(count > 0, "'lens' still has candidates");
        assert!(p.selected < count, "selected stays in range after shrink");
    } else {
        unreachable!();
    }
}

#[test]
fn palette_cancel_returns_to_selection_without_touching_the_filter() {
    // Esc closes the palette and leaves the list filter untouched (the palette
    // shares the SearchCancel action but never drives the list filter).
    let mut a = open_palette_and_type("lens");
    assert!(a.filter.is_none(), "the palette never builds a list filter");
    let out = apply_action(&mut a, Action::SearchCancel);
    assert!(!out.quit && out.effects.is_empty());
    assert_eq!(a.mode, Mode::Selection, "Esc closes the palette");
    assert!(a.filter.is_none(), "filter still untouched after cancel");
}

#[test]
fn palette_backspace_resets_selected_to_top() {
    // Backspace resets `selected` to 0 EXACTLY like typing, so the highlighted
    // row is a pure function of the query (no arrival-path dependence). Force a
    // non-zero selection, backspace, and assert it snapped back to the top.
    let mut a = open_palette_and_type("lens");
    if let Mode::Palette(p) = &mut a.mode {
        p.selected = palette_candidates(&p.query).len() - 1;
        assert!(
            p.selected > 0,
            "fixture has >1 'lens' candidate to move off 0"
        );
    }
    apply_action(&mut a, Action::SearchBackspace); // "len"
    if let Mode::Palette(p) = &a.mode {
        assert_eq!(p.selected, 0, "backspace resets the highlight to the top");
    } else {
        unreachable!();
    }
}

// ---- list-search hint/history/zero-match -------------------------

/// Confirm a list search straight on `app` — the SearchConfirm arm records
/// the query and closes (the mode payload is what the arm reads).
fn confirm_list_query(a: &mut App, query: &str) {
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: query.to_string(),
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    a.refilter();
    apply_action(a, Action::SearchConfirm);
}

/// The live list-search state fields the tests inspect (query + browse cursor).
fn search_state(a: &App) -> (String, Option<usize>) {
    match &a.mode {
        Mode::Search(s) => (s.query.clone(), s.hist),
        _ => panic!("expected Mode::Search, got {:?}", a.mode),
    }
}

#[test]
fn search_confirm_records_queries_most_recent_first() {
    let mut a = app();
    confirm_list_query(&mut a, "pos");
    confirm_list_query(&mut a, "fct");
    assert_eq!(a.search_history, vec!["fct".to_string(), "pos".to_string()]);
}

#[test]
fn search_history_dedupes_moves_to_front_and_caps_at_20() {
    let mut a = app();
    for i in 0..25 {
        a.record_search_query(&format!("q{i:02}"));
    }
    assert_eq!(a.search_history.len(), 20, "history caps at 20");
    assert_eq!(a.search_history[0], "q24", "newest first");
    // Re-recording an existing query moves it to the front, no duplicate.
    a.record_search_query("q10");
    assert_eq!(a.search_history[0], "q10");
    assert_eq!(
        a.search_history.iter().filter(|q| *q == "q10").count(),
        1,
        "the duplicate is folded into the front slot"
    );
    // Blank queries never record — there is nothing to recall.
    a.record_search_query("   ");
    assert_eq!(a.search_history.len(), 20);
}

#[test]
fn search_history_up_recalls_and_down_returns_to_live() {
    let mut a = app();
    a.search_history = vec!["fct".to_string(), "pos".to_string()];
    apply_action(&mut a, Action::SearchOpen);
    // Up from the EMPTY live query enters the browse at the newest entry.
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    assert_eq!(search_state(&a), ("fct".to_string(), Some(0)));
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    assert_eq!(search_state(&a), ("pos".to_string(), Some(1)));
    // Up clamps at the oldest entry (no wrap).
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    assert_eq!(search_state(&a), ("pos".to_string(), Some(1)));
    // Down walks newer…
    apply_action(&mut a, Action::SearchMove(Direction::Down));
    assert_eq!(search_state(&a), ("fct".to_string(), Some(0)));
    // …and past the newest it lands back on the live empty query.
    apply_action(&mut a, Action::SearchMove(Direction::Down));
    assert_eq!(search_state(&a), (String::new(), None));
}

#[test]
fn search_history_is_only_recallable_on_an_empty_query() {
    let mut a = app();
    a.search_history = vec!["pos".to_string()];
    // With a NON-empty query the arrows keep their list-move meaning.
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let sel = a.ui_state.selected();
    apply_action(&mut a, Action::SearchOpen);
    apply_action(&mut a, Action::SearchType('x'));
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    let (q, hist) = search_state(&a);
    assert_eq!(q, "x", "query untouched by the history browse");
    assert_eq!(hist, None, "no browse on a live query");
    assert!(
        a.ui_state.selected() < sel,
        "Up moved the filtered-list selection instead"
    );
}

#[test]
fn search_history_browse_without_history_keeps_list_move() {
    let mut a = app();
    a.select_by_unique_id("model.jaffle_finance.pos_txn");
    let sel = a.ui_state.selected();
    apply_action(&mut a, Action::SearchOpen);
    // Empty query but NO saved history → Up/Down still move the list.
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    assert!(a.ui_state.selected() < sel, "Up moved the list as before");
    assert_eq!(search_state(&a).1, None);
}

#[test]
fn search_history_editing_drops_the_browse() {
    let mut a = app();
    a.search_history = vec!["fct".to_string()];
    apply_action(&mut a, Action::SearchOpen);
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    assert_eq!(search_state(&a).1, Some(0), "browsing");
    apply_action(&mut a, Action::SearchType('z'));
    let (q, hist) = search_state(&a);
    assert_eq!(q, "fctz", "typing appends to the recalled query");
    assert_eq!(hist, None, "an edit exits the browse");
}

#[test]
fn list_empty_reason_names_the_miss() {
    let mut a = app();
    // No search, all models in scope → None (list non-empty).
    assert_eq!(a.list_empty_reason(), None);
    // Zero-match name search → the `?` hint; `?`-prefixed → bare "no match".
    a.mode = Mode::Search(crate::action::SearchState {
        target: SearchTarget::List,
        query: "zzzzzz".into(),
        origin_uid: None,
        match_idx: 0,
        hist: None,
    });
    a.refilter();
    assert_eq!(
        a.list_empty_reason().as_deref(),
        Some("no match — try ?text for full-text search"),
        "name/selector miss points at the ? escape"
    );
    if let Mode::Search(s) = &mut a.mode {
        s.query = "?zzzzzz".into();
    }
    a.refilter();
    assert_eq!(
        a.list_empty_reason().as_deref(),
        Some("no match"),
        "a full-text miss needs no hint"
    );
}

#[test]
fn list_empty_reason_names_the_persistent_filter_and_scope() {
    let mut a = app();
    // The bookmarked-only filter with zero bookmarks leaves nothing.
    apply_action(&mut a, Action::ToggleBookmarkFilter);
    assert!(
        a.active_list().is_empty(),
        "no bookmarks → empty filtered list"
    );
    assert_eq!(
        a.list_empty_reason().as_deref(),
        Some("no bookmarked models"),
        "the filter names what it removed"
    );
    // A project with no models at all gets the scope hint.
    use std::collections::HashMap;
    let empty = Dag::build(&crate::RawManifest {
        metadata: Default::default(),
        nodes: HashMap::new(),
        sources: HashMap::new(),
        exposures: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        functions: HashMap::new(),
        parent_map: HashMap::new(),
        child_map: HashMap::new(),
        macros: HashMap::new(),
    });
    let b = App::new(empty, PathBuf::from(FIXTURE));
    assert_eq!(
        b.list_empty_reason().as_deref(),
        Some("no models in scope — try --include-packages")
    );
}

// ---- palette Recent / @-models / availability / zero-match -------

#[test]
fn palette_recent_section_leads_after_a_run() {
    let mut a = open_palette_and_type("minimap");
    apply_action(&mut a, Action::SearchConfirm);
    assert_eq!(a.palette_recent, vec!["ToggleMinimap".to_string()]);
    // Reopening at an empty query yields Recent → the command → Commands → all.
    let rows = a.palette_rows("");
    assert_eq!(rows[0], PaletteRow::Header("Recent"));
    assert!(
        matches!(&rows[1], PaletteRow::Command { action, .. } if *action == Action::ToggleMinimap),
        "the run command leads the Recent section"
    );
    assert_eq!(rows[2], PaletteRow::Header("Commands"));
    // Headers are never selectable — `selected` indexes commands/models only.
    let selectable = a.palette_selectable_rows("");
    assert!(
        selectable
            .iter()
            .all(|r| !matches!(r, PaletteRow::Header(_))),
        "headers stripped"
    );
    assert!(
        matches!(&selectable[0], PaletteRow::Command { action, .. } if *action == Action::ToggleMinimap),
        "the recent row is the first selectable"
    );
    // And the same command still appears in the full list below.
    assert!(
        selectable
            .iter()
            .filter(|r| matches!(r, PaletteRow::Command { action, .. } if *action == Action::ToggleMinimap))
            .count()
            > 1,
        "a recent command keeps its normal row too"
    );
}

#[test]
fn palette_recent_dedupes_moves_to_front_and_caps_at_5() {
    let mut a = app();
    for action in [
        Action::ToggleMinimap,
        Action::ToggleDensity,
        Action::ToggleOverview,
        Action::ThemeCycle,
        Action::Reload,
        Action::DbtParse,
        Action::StatsOpen,
    ] {
        a.record_palette_run(action);
    }
    assert_eq!(a.palette_recent.len(), 5, "recents cap at 5");
    assert_eq!(a.palette_recent[0], "StatsOpen", "newest first");
    a.record_palette_run(Action::Reload);
    assert_eq!(a.palette_recent[0], "Reload");
    assert_eq!(
        a.palette_recent.iter().filter(|n| *n == "Reload").count(),
        1,
        "re-runs fold into the front slot"
    );
}

#[test]
fn palette_recent_drops_actions_gone_from_bindings() {
    let mut a = app();
    // A stale name (e.g. an action removed by an update) never reaches a row.
    a.palette_recent = vec!["NoSuchAction".to_string()];
    let rows = a.palette_rows("");
    assert!(
        rows.iter().all(|r| !matches!(r, PaletteRow::Header(_))),
        "no Recent section when every saved name is stale"
    );
}

#[test]
fn palette_at_prefix_searches_models_and_enter_reroots() {
    let mut a = open_palette_and_type("@pos_txn");
    // Every selectable row is a model match; the fixture has exactly one.
    let rows = a.palette_selectable_rows("@pos_txn");
    let names: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            PaletteRow::Model { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["pos_txn"], "@ filters to the model by name");
    apply_action(&mut a, Action::SearchConfirm);
    assert_eq!(a.mode, Mode::Selection, "palette closes on confirm");
    assert_eq!(
        a.selected_unique_id().as_deref(),
        Some("model.jaffle_finance.pos_txn"),
        "Enter re-roots to the model (the 'active tab root')"
    );
}

#[test]
fn palette_unavailable_command_toasts_the_reason_and_stays_open() {
    // No --diff baseline loaded → `D` "diff vs baseline" is greyed.
    let mut a = app();
    assert_eq!(
        a.unavailable_reason(Action::DiffOpen).as_deref(),
        Some("needs --diff"),
        "same predicate the apply arm's no-op condition uses"
    );
    apply_action(&mut a, Action::PaletteOpen);
    for c in "diff".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    let idx = a
        .palette_selectable_rows("diff")
        .iter()
        .position(
            |r| matches!(r, PaletteRow::Command { action, .. } if *action == Action::DiffOpen),
        )
        .expect("the diff command is a candidate");
    if let Mode::Palette(p) = &mut a.mode {
        p.selected = idx;
    }
    apply_action(&mut a, Action::SearchConfirm);
    assert!(
        matches!(a.mode, Mode::Palette(_)),
        "Enter on a greyed row keeps the palette open"
    );
    assert_eq!(
        a.take_notice().as_deref(),
        Some("needs --diff"),
        "the reason toasts"
    );
}

#[test]
fn palette_zero_candidates_show_the_no_match_state() {
    let a = open_palette_and_type("zzzzqqq");
    assert!(
        a.palette_selectable_rows("zzzzqqq").is_empty(),
        "no selectable rows — the renderer stamps 'no matching commands'"
    );
    let a = open_palette_and_type("@zzzzqqq");
    assert!(
        a.palette_selectable_rows("@zzzzqqq").is_empty(),
        "no model rows — the renderer stamps 'no matching models'"
    );
}

#[test]
fn unavailable_reason_mirrors_apply_noops() {
    let mut a = app();
    // Selection-present cases: bookmark cycle with no bookmarks; view filters
    // gated in overview; history back with an empty stack.
    assert_eq!(
        a.unavailable_reason(Action::BookmarkCycle).as_deref(),
        Some("no bookmarks in the list")
    );
    assert_eq!(
        a.unavailable_reason(Action::HistoryBack).as_deref(),
        Some("no earlier model")
    );
    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(
        a.unavailable_reason(Action::ToggleUpstream).as_deref(),
        Some("off in overview"),
        "the SAME gate the apply arm uses"
    );
    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(a.unavailable_reason(Action::ToggleUpstream), None);
    // Commands that always run stay unmarked.
    assert_eq!(a.unavailable_reason(Action::Quit), None);
    assert_eq!(a.unavailable_reason(Action::HelpToggle), None);
}

#[test]
fn unavailable_reason_covers_the_empty_graph_under_overview() {
    // The lineage yanks and the `e` export share `yank_subgraph`: an empty
    // Dag yields an EMPTY nodes list even under the overview's whole-graph
    // pull — "no model selected" is the wrong story there, and "available"
    // would have been a silent dead row.
    let mut a = empty_app();
    apply_action(&mut a, Action::ToggleOverview);
    for act in [
        Action::YankMermaid,
        Action::YankDot,
        Action::YankAscii,
        Action::ExportLineage,
    ] {
        assert_eq!(
            a.unavailable_reason(act).as_deref(),
            Some("empty lineage"),
            "{act:?} on an empty graph in overview"
        );
    }
    // Off the overview the same emptiness reads as "nothing selected".
    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(
        a.unavailable_reason(Action::YankMermaid).as_deref(),
        Some("no model selected")
    );
    assert_eq!(
        a.unavailable_reason(Action::ExportLineage).as_deref(),
        Some("no model selected")
    );
    // …while a populated Dag stays available in overview even with no
    // selection (the whole graph IS the yank).
    let mut a = app();
    apply_action(&mut a, Action::ToggleOverview);
    assert_eq!(a.unavailable_reason(Action::YankMermaid), None);
    assert_eq!(a.unavailable_reason(Action::ExportLineage), None);
}

#[test]
fn unavailable_reason_mirrors_focus_routed_movement_noops() {
    // List-side moves die on an empty list — with the list's OWN stamped
    // reason, not a generic one.
    let a = empty_app();
    for act in [Action::MoveDown, Action::JumpTop, Action::PageDown] {
        assert_eq!(
            a.unavailable_reason(act).as_deref(),
            Some("no models in scope — try --include-packages"),
            "{act:?} on an empty list"
        );
    }
    // h/l have no list arm at all — always dead with list focus.
    assert_eq!(
        a.unavailable_reason(Action::MoveLeft).as_deref(),
        Some("lineage pane only")
    );
    // Lineage-focus moves need a cursor; g/G/Pg are list-only.
    let mut a = empty_app();
    apply_action(&mut a, Action::ToggleFocus); // → lineage pane
    for act in [
        Action::MoveDown,
        Action::MoveLeft,
        Action::LineageLeftmost,
        Action::LineageRightmost,
    ] {
        assert_eq!(
            a.unavailable_reason(act).as_deref(),
            Some("no model selected"),
            "{act:?} with lineage focus and no cursor"
        );
    }
    assert_eq!(
        a.unavailable_reason(Action::JumpTop).as_deref(),
        Some("list pane only")
    );
    // And on a populated app the same commands run.
    let mut a = app();
    assert_eq!(a.unavailable_reason(Action::MoveDown), None);
    apply_action(&mut a, Action::ToggleFocus);
    assert_eq!(a.unavailable_reason(Action::MoveDown), None);
    assert_eq!(
        a.unavailable_reason(Action::JumpTop).as_deref(),
        Some("list pane only"),
        "jumps are list-side regardless of focus"
    );
}

// ---- help `/`-filter + opening-mode section ----------------------

#[test]
fn help_records_the_opening_mode_for_section_order() {
    let mut a = app();
    apply_action(&mut a, Action::HelpToggle);
    match &a.mode {
        Mode::Help(h) => assert_eq!(h.from, ModeKind::Selection, "Selection led"),
        _ => panic!("expected Help mode"),
    }
    // From the structure modal, that mode's section leads instead.
    apply_action(&mut a, Action::HelpToggle); // close
    a.mode = Mode::Detail(DetailView {
        model_id: "m".into(),
        name: "m".into(),
        detail: crate::NodeDetail::default().into(),
        tests: vec![],
        downstream_count: 0,
        upstream_count: 0,
        catalog: vec![],
        col_names: vec![],
        col_index: usize::MAX,
        test_index: usize::MAX,
        col_counts: std::collections::BTreeMap::new(),
        stale: false,
        name_match: false,
        scroll: 0,
    });
    apply_action(&mut a, Action::HelpToggle);
    match &a.mode {
        Mode::Help(h) => assert_eq!(h.from, ModeKind::Detail, "Detail led"),
        _ => panic!("expected Help mode"),
    }
}

#[test]
fn help_slash_filter_edits_commits_and_esc_clears() {
    let mut a = app();
    apply_action(&mut a, Action::HelpToggle);
    // `/` arms the filter; typing goes INTO it via the shared Search* actions.
    apply_action(&mut a, Action::SearchOpen);
    match &a.mode {
        Mode::Help(h) => {
            assert!(h.editing);
            assert_eq!(h.filter.as_deref(), Some(""));
        }
        _ => panic!("expected Help mode"),
    }
    for c in "sql".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    apply_action(&mut a, Action::SearchBackspace); // "sq"
    match &a.mode {
        Mode::Help(h) => assert_eq!(h.filter.as_deref(), Some("sq")),
        _ => panic!(),
    }
    // Enter commits: editing ends but the filter stays applied.
    apply_action(&mut a, Action::SearchConfirm);
    match &a.mode {
        Mode::Help(h) => {
            assert!(!h.editing);
            assert_eq!(h.filter.as_deref(), Some("sq"), "filter stays applied");
        }
        _ => panic!(),
    }
    // Esc clears the filter FIRST; the next Esc closes the overlay.
    apply_action(&mut a, Action::HelpToggle);
    match &a.mode {
        Mode::Help(h) => {
            assert_eq!(h.filter, None);
            assert!(!h.editing);
        }
        _ => panic!("filter cleared but overlay still open"),
    }
    apply_action(&mut a, Action::HelpToggle);
    assert_eq!(a.mode, Mode::Selection, "Esc again closes help");
}

#[test]
fn help_filter_typing_resets_scroll() {
    let mut a = app();
    a.mode = Mode::Help(HelpView {
        scroll: 40,
        filter: None,
        editing: false,
        from: ModeKind::Selection,
        back: Box::new(Mode::Selection),
    });
    apply_action(&mut a, Action::SearchOpen);
    match &a.mode {
        Mode::Help(h) => assert_eq!(h.scroll, 0, "opening the filter recentres"),
        _ => panic!(),
    }
}

#[test]
fn help_close_restores_the_underlying_modal() {
    // `?` over the Detail modal must reopen that same modal on close — help is
    // a LAYER, not a replacement (the payload has to survive round-trip).
    let mut a = app();
    let detail = Mode::Detail(DetailView {
        model_id: "m".into(),
        name: "m".into(),
        detail: crate::NodeDetail::default().into(),
        tests: vec![],
        downstream_count: 0,
        upstream_count: 0,
        catalog: vec![],
        col_names: vec![],
        col_index: usize::MAX,
        test_index: usize::MAX,
        col_counts: std::collections::BTreeMap::new(),
        stale: false,
        name_match: false,
        scroll: 7,
    });
    a.mode = detail.clone();
    apply_action(&mut a, Action::HelpToggle);
    match &a.mode {
        Mode::Help(h) => {
            assert_eq!(h.from, ModeKind::Detail);
            assert_eq!(*h.back, detail, "the overlay remembers what it covers");
        }
        _ => panic!("expected Help mode"),
    }
    apply_action(&mut a, Action::HelpToggle);
    assert_eq!(
        a.mode, detail,
        "closing help restores the modal, payload and all"
    );
}

#[test]
fn help_esc_while_editing_clears_the_filter_first() {
    // Esc mid-typing is a HelpToggle (printable-only keys route to the filter):
    // it must clear the in-progress filter and KEEP the overlay open.
    let mut a = app();
    apply_action(&mut a, Action::HelpToggle);
    apply_action(&mut a, Action::SearchOpen); // `/` — now editing
    apply_action(&mut a, Action::SearchType('s'));
    apply_action(&mut a, Action::HelpToggle); // Esc while editing
    match &a.mode {
        Mode::Help(h) => {
            assert_eq!(h.filter, None, "in-progress filter dropped");
            assert!(!h.editing);
        }
        _ => panic!("help closed instead of clearing the filter"),
    }
}

#[test]
fn palette_recent_row_enter_runs_the_command() {
    // Run a command once, reopen the EMPTY palette: its Recent row leads the
    // selectable list, and Enter on it runs it again.
    let mut a = open_palette_and_type("minimap");
    apply_action(&mut a, Action::SearchConfirm);
    assert_eq!(a.mode, Mode::Selection);
    let before = a.ui_state.minimap_visible();
    apply_action(&mut a, Action::PaletteOpen);
    // Empty query → Recent section first → the first selectable row is the
    // just-run ToggleMinimap command.
    let rows = a.palette_selectable_rows("");
    assert!(
        matches!(rows.first(), Some(PaletteRow::Command { action, .. }) if *action == Action::ToggleMinimap),
        "the recent command leads: {rows:?}"
    );
    apply_action(&mut a, Action::SearchConfirm);
    assert_ne!(
        a.ui_state.minimap_visible(),
        before,
        "Enter on the Recent row re-ran the command"
    );
}

#[test]
fn palette_unavailable_run_is_not_recorded_in_recents() {
    // Greyed commands toast and stay open — and must never enter Recent.
    let mut a = app();
    apply_action(&mut a, Action::PaletteOpen);
    let target = a
        .palette_selectable_rows("")
        .iter()
        .position(|r| {
            matches!(
                r,
                PaletteRow::Command {
                    unavailable: Some(_),
                    ..
                }
            )
        })
        .expect("a greyed command exists");
    if let Mode::Palette(p) = &mut a.mode {
        p.selected = target;
    }
    apply_action(&mut a, Action::SearchConfirm);
    assert!(matches!(a.mode, Mode::Palette(_)), "palette stays open");
    assert!(
        a.palette_recent.is_empty(),
        "the dead command was not recorded"
    );
}

#[test]
fn palette_confirm_flags_reanchor_for_reshaping_actions() {
    // The loop can't see the resolved action behind SearchConfirm, so the
    // outcome carries the anchor intent. Navigate to the Recenter row
    // explicitly so the pick doesn't depend on fuzzy-match order.
    let mut a = app();
    apply_action(&mut a, Action::PaletteOpen);
    let at = a
        .palette_selectable_rows("")
        .iter()
        .position(|r| {
            matches!(
                r,
                PaletteRow::Command {
                    action: Action::Recenter,
                    ..
                }
            )
        })
        .expect("Recenter is a palette row");
    if let Mode::Palette(p) = &mut a.mode {
        p.selected = at;
    }
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(out.reanchor, "palette-run Recenter flags the reanchor");
    // A non-reshaping command does not.
    let mut a = open_palette_and_type("minimap");
    let out = apply_action(&mut a, Action::SearchConfirm);
    assert!(!out.reanchor, "ToggleMinimap leaves the anchor alone");
}

#[test]
fn search_bare_selector_prefix_keeps_the_full_list() {
    // Typing `!` alone is the selector PREFIX, not a zero-term query — the
    // list must not collapse into a false "no match" flash mid-keystroke.
    let mut a = app();
    let total = a.active_list().models.len();
    apply_action(&mut a, Action::SearchOpen);
    apply_action(&mut a, Action::SearchType('!'));
    assert_eq!(
        a.active_list().models.len(),
        total,
        "bare `!` keeps the unfiltered list"
    );
    // …but a real (empty-matching) selector still filters.
    apply_action(&mut a, Action::SearchType('z'));
    apply_action(&mut a, Action::SearchType('z'));
    assert!(
        a.active_list().models.len() < total,
        "`!zz` runs the selector path"
    );
}

#[test]
fn search_selector_name_globs_filter_the_list() {
    // `!stg_*` narrows the list to glob-matched names — the same set
    // `select` returns (the `!` prefix IS the selector path, one parse).
    let mut a = app();
    apply_action(&mut a, Action::SearchOpen);
    for c in "!stg_*".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    let expected = crate::select::select(&a.dag, a.diff(), "stg_*");
    let got: std::collections::BTreeSet<String> = a
        .active_list()
        .models
        .iter()
        .map(|n| n.unique_id.clone())
        .collect();
    assert!(!got.is_empty(), "the fixture has stg_* models");
    assert_eq!(got, expected);
    assert!(got.iter().all(|u| u.contains(".stg_")));
    // A non-glob name keeps exact-match semantics.
    apply_action(&mut a, Action::SearchCancel);
    apply_action(&mut a, Action::SearchOpen);
    for c in "!pos_sku".chars() {
        apply_action(&mut a, Action::SearchType(c));
    }
    let names: Vec<_> = a
        .active_list()
        .models
        .iter()
        .map(|n| n.unique_id.as_str())
        .collect();
    assert_eq!(names, vec!["model.jaffle_finance.pos_sku"]);
}

#[test]
fn search_history_recall_reruns_selector_and_fulltext_queries() {
    // Recalled queries re-run their OWN syntax — `!` selector and `?`
    // full-text included (the recall restores the query string wholesale).
    let mut a = app();
    // Record one of each kind through the live confirm path.
    for q in ["!tag:nightly", "?select"] {
        apply_action(&mut a, Action::SearchOpen);
        for c in q.chars() {
            apply_action(&mut a, Action::SearchType(c));
        }
        apply_action(&mut a, Action::SearchConfirm);
    }
    assert_eq!(
        a.search_history,
        vec!["?select".to_string(), "!tag:nightly".to_string()]
    );
    // Reopen and recall the newest: the query AND its filtered list return.
    apply_action(&mut a, Action::SearchOpen);
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    let (q, hist) = search_state(&a);
    assert_eq!(q, "?select", "newest recall is the `?` query");
    assert_eq!(hist, Some(0));
    assert!(
        a.filter.is_some(),
        "the full-text filter was applied on recall"
    );
    // One more Up reaches the `!` selector.
    apply_action(&mut a, Action::SearchMove(Direction::Up));
    let (q, _) = search_state(&a);
    assert_eq!(q, "!tag:nightly");
}
