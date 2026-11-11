use super::*;
use ratatui::crossterm::event::KeyCode::{Backspace, Char, Enter, Esc};

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
fn cmd(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SUPER)
}
fn search_mode() -> Mode {
    Mode::Search(SearchState {
        target: SearchTarget::List,
        query: String::new(),
        origin_uid: None,
        match_idx: 0,
        hist: None,
    })
}
fn help_mode() -> Mode {
    Mode::Help(HelpView::new(Mode::Selection))
}
fn detail_mode() -> Mode {
    Mode::Detail(DetailView {
        model_id: "m".into(),
        name: "m".into(),
        detail: Box::new(NodeDetail::default()),
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
    })
}
fn sql_mode() -> Mode {
    Mode::Sql(SqlView {
        model_id: "m".into(),
        name: "m".into(),
        sql: String::new(),
        compiled: None,
        compiled_stale: false,
        show_compiled: false,
        path: None,
        scroll: 0,
    })
}
fn palette_mode() -> Mode {
    Mode::Palette(PaletteState::default())
}
fn yank_mode() -> Mode {
    Mode::Yank { back: None, sel: 0 }
}
fn menu_mode() -> Mode {
    Mode::Menu(MenuState {
        target: "m".into(),
        name: "m".into(),
        items: vec![MenuEntry::Item {
            binding: 0,
            label: "item".into(),
        }],
        selected: 0,
        back: None,
        anchor: MenuAnchor::Cell { col: 0, row: 0 },
    })
}
fn diff_mode() -> Mode {
    Mode::Diff(DiffView::default())
}
fn columns_mode() -> Mode {
    Mode::Columns(ColumnView {
        title: "m.c".into(),
        rows: vec![],
        cursor: usize::MAX,
        scroll: 0,
    })
}
fn notifications_mode() -> Mode {
    Mode::Notifications(NotificationsView {
        rows: vec![],
        scroll: 0,
    })
}
fn stats_mode() -> Mode {
    Mode::Stats(StatsView {
        project: String::new(),
        by_resource_type: vec![],
        by_materialization: vec![],
        testable_total: 0,
        testable_tested: 0,
        col_declared: 0,
        col_tested: 0,
        top_hubs: vec![],
        transitive_hubs: vec![],
        orphan_models: vec![],
        layer_violations: vec![],
        lint_findings: vec![],
        lint_errors: 0,
        lint_warnings: 0,
        critical_path: vec![],
        critical_path_runtime: None,
        untested_testable: 0,
        zero_downstream_models: 0,
        no_description_models: 0,
        cll_fallbacks: 0,
        scroll: 0,
    })
}

// ---- Selection dispatch maps the core keys to their actions ----

#[test]
fn selection_legacy_keys_map_to_expected_actions() {
    let sel = Mode::Selection;
    assert_eq!(dispatch(&sel, press(Char('j'))), Some(Action::MoveDown));
    assert_eq!(dispatch(&sel, press(KeyCode::Down)), Some(Action::MoveDown));
    assert_eq!(dispatch(&sel, press(Char('k'))), Some(Action::MoveUp));
    assert_eq!(dispatch(&sel, press(Char('h'))), Some(Action::MoveLeft));
    assert_eq!(dispatch(&sel, press(Char('l'))), Some(Action::MoveRight));
    assert_eq!(dispatch(&sel, press(Char('g'))), Some(Action::JumpTop));
    assert_eq!(dispatch(&sel, press(Char('G'))), Some(Action::JumpBottom));
    assert_eq!(
        dispatch(&sel, press(KeyCode::Tab)),
        Some(Action::ToggleFocus)
    );
    assert_eq!(dispatch(&sel, press(Char('q'))), Some(Action::Quit));
    assert_eq!(dispatch(&sel, ctrl('c')), Some(Action::Quit));
    assert_eq!(dispatch(&sel, press(Char('\\'))), None, "unbound key");
}

#[test]
fn t_cycles_the_lineage_lens_in_selection_but_is_text_in_search() {
    // 't' in Selection cycles the lineage lens; in Search it is query text
    // (the modal text-input footgun: a printable must never command).
    assert_eq!(
        dispatch(&Mode::Selection, press(Char('t'))),
        Some(Action::CycleLens)
    );
    assert_eq!(
        dispatch(&search_mode(), press(Char('t'))),
        Some(Action::SearchType('t')),
        "'t' in search is text, not the lens cycle"
    );
}

#[test]
fn p_runs_dbt_parse_in_selection_but_is_text_in_search() {
    // 'P' (capital — arrives as Char('P'), SHIFT allowed by Trigger::Key)
    // requests a dbt parse; in Search it must stay query text.
    assert_eq!(
        dispatch(&Mode::Selection, press(Char('P'))),
        Some(Action::DbtParse)
    );
    assert_eq!(
        dispatch(&search_mode(), press(Char('P'))),
        Some(Action::SearchType('P')),
        "'P' in search is text, not a dbt parse"
    );
}

#[test]
fn bookmark_and_sort_keys_dispatch_in_selection() {
    let sel = Mode::Selection;
    assert_eq!(
        dispatch(&sel, press(Char(' '))),
        Some(Action::BookmarkToggle),
        "Space toggles a bookmark in Selection"
    );
    assert_eq!(
        dispatch(&sel, press(Char('\''))),
        Some(Action::BookmarkCycle),
        "' cycles bookmarks"
    );
    assert_eq!(
        dispatch(&sel, press(Char('.'))),
        Some(Action::SortCycle),
        ". cycles the list sort"
    );
    // In Search mode, Space (and ' / .) are query text, never commands.
    let m = search_mode();
    assert_eq!(
        dispatch(&m, press(Char(' '))),
        Some(Action::SearchType(' ')),
        "Space in search is text, not a bookmark toggle"
    );
    assert_eq!(
        dispatch(&m, press(Char('.'))),
        Some(Action::SearchType('.'))
    );
}

#[test]
fn space_label_is_named_not_blank() {
    // The Space trigger must render a legible key in `?` help, not an empty
    // string (it would print as an invisible glyph).
    assert_eq!(Trigger::Key(Char(' ')).label(), "Space");
    assert_eq!(Trigger::Key(Char('\'')).label(), "'");
    assert_eq!(Trigger::Key(Char('.')).label(), ".");
}

#[test]
fn cmd_b_and_ctrl_b_toggle_the_list_pane() {
    let sel = Mode::Selection;
    assert_eq!(dispatch(&sel, cmd('b')), Some(Action::ToggleListPane));
    assert_eq!(dispatch(&sel, ctrl('b')), Some(Action::ToggleListPane));
    // Plain 'b' stays the history-back binding — modifiers matter.
    assert_eq!(dispatch(&sel, press(Char('b'))), Some(Action::HistoryBack));
}

// ---- modal text-input footgun: printable in Search is TEXT, not commands ----

#[test]
fn search_printable_keys_are_text_not_commands() {
    let m = search_mode();
    for c in ['q', 'g', 'j', 'G', 'a', '/'] {
        assert_eq!(
            dispatch(&m, press(Char(c))),
            Some(Action::SearchType(c)),
            "'{c}' in search must be text, not a command"
        );
    }
    assert_eq!(dispatch(&m, press(Esc)), Some(Action::SearchCancel));
    assert_eq!(dispatch(&m, press(Enter)), Some(Action::SearchConfirm));
    assert_eq!(
        dispatch(&m, press(Backspace)),
        Some(Action::SearchBackspace)
    );
    // Ctrl-c still quits even in search (never a printable char).
    assert_eq!(dispatch(&m, ctrl('c')), Some(Action::Quit));
    // A Super-modified char is a command chord, not query text.
    assert_eq!(dispatch(&m, cmd('b')), None);
}

#[test]
fn overlay_q_closes_overlay_not_app() {
    // 'q' must CLOSE the overlay, not quit the application.
    assert_eq!(
        dispatch(&detail_mode(), press(Char('q'))),
        Some(Action::DetailClose)
    );
    assert_eq!(
        dispatch(&help_mode(), press(Char('q'))),
        Some(Action::HelpToggle)
    );
    assert_eq!(
        dispatch(&detail_mode(), press(Esc)),
        Some(Action::DetailClose)
    );
}

#[test]
fn help_editing_printable_keys_are_text_not_commands() {
    // While the `/`-filter is being edited, printable chars are TEXT —
    // they must never reach the Help-mode BINDINGS rows (`q`/`?` close,
    // `j`/`g` scroll, `/` re-arms). The editing pre-emption in dispatch
    // is what stands between the filter and those commands.
    let editing = Mode::Help(HelpView {
        editing: true,
        filter: Some("co".to_string()),
        ..HelpView::new(Mode::Selection)
    });
    for c in ['q', '?', 'j', 'g', '/', 'G', 'k'] {
        assert_eq!(
            dispatch(&editing, press(Char(c))),
            Some(Action::SearchType(c)),
            "'{c}' while editing the help filter must be text, not a command"
        );
    }
    assert_eq!(
        dispatch(&editing, press(Backspace)),
        Some(Action::SearchBackspace)
    );
    assert_eq!(
        dispatch(&editing, press(Enter)),
        Some(Action::SearchConfirm)
    );
    // Esc is not printable: it still reaches the close binding — the
    // reducer reads it as "clear the filter first".
    assert_eq!(dispatch(&editing, press(Esc)), Some(Action::HelpToggle));
    // And NOT editing: the printable chars ARE commands again.
    let idle = help_mode();
    assert_eq!(dispatch(&idle, press(Char('q'))), Some(Action::HelpToggle));
    assert_eq!(
        dispatch(&idle, press(Char('j'))),
        Some(Action::DetailScroll(Direction::Down))
    );
}

// ---- drift guard: help and dispatch can never diverge ----

/// Exhaustive categorization. Adding an `Action` variant without an arm here
/// is a COMPILE error, forcing the author to declare whether it is key-bound
/// (must appear in BINDINGS) or dynamic (text/mouse).
fn is_key_bound(a: &Action) -> bool {
    match a {
        Action::MoveDown
        | Action::MoveUp
        | Action::MoveLeft
        | Action::MoveRight
        | Action::JumpTop
        | Action::JumpBottom
        | Action::ToggleFocus
        | Action::ToggleListPane
        | Action::Quit
        | Action::DetailOpen
        | Action::DetailClose
        | Action::DetailScroll(_)
        | Action::SearchOpen
        | Action::SearchBackspace
        | Action::SearchCancel
        | Action::SearchConfirm
        | Action::SearchMove(_)
        | Action::PaletteOpen
        | Action::HelpToggle
        | Action::Recenter
        | Action::OpenEditor
        | Action::YankId
        | Action::YankName
        | Action::YankMermaid
        | Action::YankDot
        | Action::Reload
        | Action::ToggleUpstream
        | Action::ToggleDownstream
        | Action::DepthDecrease
        | Action::DepthIncrease
        | Action::ResetView
        | Action::HistoryBack
        | Action::HistoryForward
        | Action::SqlOpen
        | Action::SqlToggleCompiled
        | Action::StatsOpen
        | Action::CycleLens
        | Action::BookmarkToggle
        | Action::BookmarkCycle
        | Action::SortCycle
        | Action::ToggleMinimap
        | Action::ToggleDensity
        | Action::ToggleOverview
        | Action::PathToggle
        | Action::YankAscii
        | Action::YankSql
        | Action::YankCompiledSql
        | Action::YankColumnLineage
        | Action::YankImpact
        | Action::YankSelector
        | Action::YankPrefix
        | Action::ExportLineage
        | Action::PageDown
        | Action::PageUp
        | Action::DetailScrollPage(_)
        | Action::DetailScrollHome
        | Action::DetailScrollEnd
        | Action::DetailColumnMove(_)
        | Action::GapNext
        | Action::GapPrev
        | Action::BookmarkCycleBack
        | Action::LineageLeftmost
        | Action::LineageRightmost
        | Action::ToggleUntestedFilter
        | Action::ToggleBookmarkFilter
        | Action::ToggleLintFilter
        | Action::ToggleGroupFold
        | Action::DbtParse
        | Action::ThemeCycle
        | Action::DiffOpen
        | Action::ExportPrImpact
        | Action::YankPrSelector
        | Action::YankPrSelectorWide
        | Action::ColumnFocusEnter
        | Action::ColumnFocusExit
        | Action::ColumnsOpen
        | Action::ColumnRowMove(_)
        | Action::ColumnJump
        | Action::PaneNarrow
        | Action::PaneWiden
        | Action::PaneWidthsReset
        | Action::DetailSectionToggle
        | Action::NotificationsOpen
        | Action::MenuOpen
        | Action::MenuMove(_)
        | Action::MenuConfirm
        | Action::TabOpen
        | Action::TabClose
        | Action::TabPrev
        | Action::TabNext
        | Action::TabActivate(_)
        | Action::HomeGo
        | Action::HomeMove(_)
        | Action::HomeOpen
        | Action::ToggleInspector => true,
        // Dynamic: produced without a fixed key binding (text input, a
        // resolved menu item, mouse row picks/invokes). Mouse is handled
        // in the event loop (size-aware), not via the keymap.
        Action::SearchType(_)
        | Action::MenuInvoke(_)
        | Action::MenuSelect(_)
        | Action::YankPick(_)
        | Action::YankInvoke(_)
        | Action::PalettePick(_)
        | Action::ColumnPick(_)
        | Action::InspectorColumnPick(_)
        | Action::HomePick(_)
        | Action::SetLens(_) => false,
    }
}

#[test]
fn every_binding_has_help_and_round_trips() {
    for b in BINDINGS {
        assert!(
            !b.help.is_empty(),
            "binding for {:?} has empty help",
            b.action
        );
        // Each trigger dispatches to this binding's action in its mode.
        let mode = match b.mode {
            ModeKind::Selection => Mode::Selection,
            ModeKind::Search => search_mode(),
            ModeKind::Detail => detail_mode(),
            ModeKind::Sql => sql_mode(),
            ModeKind::Stats => stats_mode(),
            ModeKind::Diff => diff_mode(),
            ModeKind::Columns => columns_mode(),
            ModeKind::Notifications => notifications_mode(),
            ModeKind::Palette => palette_mode(),
            ModeKind::Yank => yank_mode(),
            ModeKind::Menu => menu_mode(),
            ModeKind::Help => help_mode(),
            ModeKind::Home => Mode::Home,
        };
        for t in b.triggers {
            let key = match t {
                Trigger::Key(c) => press(*c),
                Trigger::Ctrl(c) => ctrl(*c),
                Trigger::Super(c) => cmd(*c),
            };
            assert_eq!(
                dispatch(&mode, key),
                Some(b.action),
                "trigger {:?} should dispatch to {:?}",
                t,
                b.action
            );
        }
    }
}

#[test]
fn key_bound_actions_appear_in_bindings() {
    // Every action the categorizer calls "key bound" must actually be in the
    // table — so a new bound action without a BINDINGS row fails here.
    let samples = [
        Action::MoveDown,
        Action::MoveUp,
        Action::MoveLeft,
        Action::MoveRight,
        Action::JumpTop,
        Action::JumpBottom,
        Action::ToggleFocus,
        Action::ToggleListPane,
        Action::Quit,
        Action::DetailOpen,
        Action::DetailClose,
        Action::DetailScroll(Direction::Down),
        Action::SearchOpen,
        Action::SearchBackspace,
        Action::SearchCancel,
        Action::SearchConfirm,
        Action::SearchMove(Direction::Down),
        Action::PaletteOpen,
        Action::HelpToggle,
        Action::Recenter,
        Action::OpenEditor,
        Action::YankId,
        Action::YankName,
        Action::YankMermaid,
        Action::YankDot,
        Action::Reload,
        Action::ToggleUpstream,
        Action::ToggleDownstream,
        Action::DepthDecrease,
        Action::DepthIncrease,
        Action::ResetView,
        Action::HistoryBack,
        Action::HistoryForward,
        Action::SqlOpen,
        Action::SqlToggleCompiled,
        Action::StatsOpen,
        Action::CycleLens,
        Action::BookmarkToggle,
        Action::BookmarkCycle,
        Action::SortCycle,
        Action::ToggleMinimap,
        Action::ToggleDensity,
        Action::ToggleOverview,
        Action::PathToggle,
        Action::YankAscii,
        Action::YankSql,
        Action::YankCompiledSql,
        Action::YankColumnLineage,
        Action::YankImpact,
        Action::YankSelector,
        Action::YankPrefix,
        Action::ExportLineage,
        Action::PageDown,
        Action::PageUp,
        Action::DetailScrollPage(Direction::Down),
        Action::DetailScrollHome,
        Action::DetailScrollEnd,
        Action::DetailColumnMove(Direction::Down),
        Action::GapNext,
        Action::GapPrev,
        Action::BookmarkCycleBack,
        Action::LineageLeftmost,
        Action::LineageRightmost,
        Action::ToggleUntestedFilter,
        Action::ToggleBookmarkFilter,
        Action::ToggleLintFilter,
        Action::ToggleGroupFold,
        Action::DbtParse,
        Action::ThemeCycle,
        Action::DiffOpen,
        Action::ExportPrImpact,
        Action::YankPrSelector,
        Action::YankPrSelectorWide,
        Action::ColumnFocusEnter,
        Action::ColumnFocusExit,
        Action::ColumnsOpen,
        Action::ColumnRowMove(Direction::Down),
        Action::ColumnJump,
        Action::NotificationsOpen,
        Action::MenuOpen,
        Action::MenuMove(Direction::Down),
        Action::MenuConfirm,
    ];
    for a in samples {
        assert!(
            is_key_bound(&a),
            "{a:?} categorized as dynamic but listed as bound"
        );
        assert!(
            BINDINGS.iter().any(|b| b.action == a),
            "{a:?} is key-bound but missing from BINDINGS"
        );
    }
    // The dynamic text-input action must NOT be in the table.
    let dynamic = Action::SearchType('x');
    assert!(!is_key_bound(&dynamic));
    assert!(
        BINDINGS.iter().all(|b| b.action != dynamic),
        "SearchType is dynamic, not a fixed binding"
    );
    // The dynamic menu-invoke action must NOT be in the table either —
    // it is produced by the dispatcher's item lookup and mouse clicks.
    let dynamic = Action::MenuInvoke(0);
    assert!(!is_key_bound(&dynamic));
    assert!(
        BINDINGS.iter().all(|b| b.action != dynamic),
        "MenuInvoke is dynamic, not a fixed binding"
    );
}

// ---- command palette ----

#[test]
fn column_focus_keys_dispatch() {
    // The keymap: `K` opens the column modal from Selection or the
    // structure modal, Enter/`c` on a Detail column cursor commits it to
    // the trace, Esc exits the trace in Selection (and closes the modal
    // like every other overlay), and the modal rides ColumnRowMove/
    // ColumnJump + the shared scroll chords.
    assert_eq!(
        dispatch(&Mode::Selection, press(KeyCode::Char('K'))),
        Some(Action::ColumnsOpen)
    );
    assert_eq!(
        dispatch(&Mode::Selection, press(KeyCode::Esc)),
        Some(Action::ColumnFocusExit)
    );
    let dv = detail_mode();
    assert_eq!(
        dispatch(&dv, press(KeyCode::Enter)),
        Some(Action::ColumnFocusEnter)
    );
    assert_eq!(
        dispatch(&dv, press(KeyCode::Char('c'))),
        Some(Action::ColumnFocusEnter)
    );
    assert_eq!(
        dispatch(&dv, press(KeyCode::Char('K'))),
        Some(Action::ColumnsOpen)
    );
    let cv = columns_mode();
    assert_eq!(
        dispatch(&cv, press(KeyCode::Char('j'))),
        Some(Action::ColumnRowMove(Direction::Down))
    );
    assert_eq!(
        dispatch(&cv, press(KeyCode::Char('k'))),
        Some(Action::ColumnRowMove(Direction::Up))
    );
    assert_eq!(
        dispatch(&cv, press(KeyCode::Enter)),
        Some(Action::ColumnJump)
    );
    assert_eq!(
        dispatch(&cv, ctrl('d')),
        Some(Action::DetailScrollPage(Direction::Down))
    );
    assert_eq!(
        dispatch(&cv, press(KeyCode::Esc)),
        Some(Action::DetailClose)
    );
}

#[test]
fn ctrl_p_and_cmd_p_open_the_palette() {
    let sel = Mode::Selection;
    assert_eq!(dispatch(&sel, ctrl('p')), Some(Action::PaletteOpen));
    assert_eq!(dispatch(&sel, cmd('p')), Some(Action::PaletteOpen));
}

#[test]
fn tab_keys_dispatch_in_selection_mode() {
    // O/X/Ctrl-w/{/}/1-9 — the tab keymap (uppercase for open/close so
    // the lowercase row stays free; digits are direct tab activation).
    let sel = Mode::Selection;
    assert_eq!(dispatch(&sel, press(Char('O'))), Some(Action::TabOpen));
    assert_eq!(dispatch(&sel, press(Char('X'))), Some(Action::TabClose));
    assert_eq!(dispatch(&sel, ctrl('w')), Some(Action::TabClose));
    assert_eq!(dispatch(&sel, press(Char('{'))), Some(Action::TabPrev));
    assert_eq!(dispatch(&sel, press(Char('}'))), Some(Action::TabNext));
    for n in 1..=9u8 {
        let c = char::from(b'0' + n);
        assert_eq!(
            dispatch(&sel, press(Char(c))),
            Some(Action::TabActivate(n)),
            "'{c}' must activate tab {n}"
        );
    }
    // `0` is not a tab key — it keeps its ResetView binding.
    assert_eq!(dispatch(&sel, press(Char('0'))), Some(Action::ResetView));
    // Tab keys are Selection-mode commands, not text — inside the palette
    // they filter the query instead.
    let m = palette_mode();
    assert_eq!(
        dispatch(&m, press(Char('O'))),
        Some(Action::SearchType('O'))
    );
    assert_eq!(
        dispatch(&m, press(Char('1'))),
        Some(Action::SearchType('1'))
    );
}

#[test]
fn ctrl_t_cycles_the_theme_and_stays_distinct_from_plain_t() {
    // Ctrl-t is the theme cycle; plain `t` stays the lens cycle (the
    // `Trigger::Key`-requires-no-Ctrl rule is what keeps them apart).
    let sel = Mode::Selection;
    assert_eq!(dispatch(&sel, ctrl('t')), Some(Action::ThemeCycle));
    assert_eq!(dispatch(&sel, press(Char('t'))), Some(Action::CycleLens));
    // In text-input modes Ctrl-t is unbound (never typed text).
    assert_eq!(dispatch(&search_mode(), ctrl('t')), None);
    assert_eq!(dispatch(&palette_mode(), ctrl('t')), None);
}

#[test]
fn palette_printable_keys_are_text_not_commands() {
    // The same modal-text-input footgun as Search: a printable in the palette
    // FILTERS, never commands. 'q' must not quit; 't' must not cycle the lens;
    // Space must not toggle a bookmark.
    let m = palette_mode();
    for c in ['q', 't', ' ', 'a', '/'] {
        assert_eq!(
            dispatch(&m, press(Char(c))),
            Some(Action::SearchType(c)),
            "'{c}' in the palette must be text, not a command"
        );
    }
    // Esc cancels (closes the palette); Enter runs; Backspace edits.
    assert_eq!(dispatch(&m, press(Esc)), Some(Action::SearchCancel));
    assert_eq!(dispatch(&m, press(Enter)), Some(Action::SearchConfirm));
    assert_eq!(
        dispatch(&m, press(Backspace)),
        Some(Action::SearchBackspace)
    );
    // Ctrl-c still quits even in the palette (never a printable char).
    assert_eq!(dispatch(&m, ctrl('c')), Some(Action::Quit));
    // A Super-modified char is a command chord, not query text.
    assert_eq!(dispatch(&m, cmd('p')), None);
}

#[test]
fn palette_candidates_empty_query_lists_all_selection_commands_minus_open() {
    let all = palette_candidates("");
    // Every Selection-mode binding except PaletteOpen itself.
    let expected = BINDINGS
        .iter()
        .filter(|b| b.mode == ModeKind::Selection && b.action != Action::PaletteOpen)
        .count();
    assert_eq!(
        all.len(),
        expected,
        "empty query lists every Selection command"
    );
    assert!(
        all.iter().all(|b| b.mode == ModeKind::Selection),
        "only Selection-mode bindings are candidates"
    );
    assert!(
        all.iter().all(|b| b.action != Action::PaletteOpen),
        "the palette opener excludes itself"
    );
}

#[test]
fn palette_candidates_fuzzy_filters_by_help_text() {
    // "lens" is a subsequence of "cycle lineage lens" → the CycleLens row.
    let hits = palette_candidates("lens");
    assert!(
        hits.iter().any(|b| b.action == Action::CycleLens),
        "'lens' finds the cycle-lens command"
    );
    assert!(
        hits.iter()
            .all(|b| crate::model_list::name_matches_query(b.help, "lens")
                || crate::model_list::name_matches_query(&b.key_label(), "lens")),
        "every hit matches the query"
    );
    // A query no command satisfies yields nothing (no panic).
    assert!(palette_candidates("zzzqqq").is_empty());
}

#[test]
fn palette_candidates_preserve_bindings_table_order() {
    // Deterministic: candidates appear in BINDINGS order (no sort).
    let all = palette_candidates("");
    let positions: Vec<usize> = all
        .iter()
        .map(|c| {
            BINDINGS
                .iter()
                .position(|b| std::ptr::eq(b, *c))
                .expect("candidate is a BINDINGS row")
        })
        .collect();
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(positions, sorted, "candidates keep BINDINGS table order");
}

#[test]
fn help_lines_cover_every_binding() {
    let lines = help_lines();
    assert!(!lines.is_empty());
    for b in BINDINGS {
        assert!(
            lines.iter().any(|l| l.mode == b.mode && l.desc == b.help),
            "no help line for {:?}",
            b.action
        );
    }
}

// ---- node context menu ----

fn menu_mode_with(items: Vec<MenuEntry>) -> Mode {
    Mode::Menu(MenuState {
        target: "m".into(),
        name: "m".into(),
        items,
        selected: 0,
        back: None,
        anchor: MenuAnchor::Cell { col: 0, row: 0 },
    })
}

/// Resolve every [`NODE_MENU_LAYOUT`] slot to its `(mode, action)`
/// BINDINGS row index — the same lookup `App::menu_entries` performs.
fn slot_binding(mode: ModeKind, action: Action) -> Option<usize> {
    BINDINGS
        .iter()
        .position(|b| b.mode == mode && b.action == action)
}

#[test]
fn menu_layout_slots_resolve_to_real_bindings() {
    // The layout↔keymap drift guard: every item slot names a `(mode,
    // action)` pair that exists in BINDINGS — a renamed action or a
    // re-keyed row fails here, never silently drops a menu item.
    for slot in NODE_MENU_LAYOUT {
        let MenuSlot::Item(mode, action) = slot else {
            continue;
        };
        let idx = slot_binding(*mode, *action)
            .unwrap_or_else(|| panic!("no BINDINGS row for {mode:?}/{action:?}"));
        assert_eq!(
            BINDINGS[idx].action, *action,
            "the resolved row carries the slot's action"
        );
        // An unmapped slot panics in menu_slot_mark — surface it here.
        let _ = menu_slot_mark(*action);
    }
}

#[test]
fn menu_layout_has_no_orphaned_separators() {
    // A menu that starts/ends on a rule or carries two adjacent rules
    // would draw a degenerate box — the layout is screened at open time,
    // so keep the shape clean statically.
    assert!(!matches!(
        NODE_MENU_LAYOUT.first(),
        Some(MenuSlot::Separator)
    ));
    assert!(!matches!(
        NODE_MENU_LAYOUT.last(),
        Some(MenuSlot::Separator)
    ));
    for w in NODE_MENU_LAYOUT.windows(2) {
        assert!(
            !(matches!(w[0], MenuSlot::Separator) && matches!(w[1], MenuSlot::Separator)),
            "adjacent separators"
        );
    }
}

#[test]
fn a_opens_the_menu_in_selection_detail_and_sql() {
    assert_eq!(
        dispatch(&Mode::Selection, press(Char('a'))),
        Some(Action::MenuOpen)
    );
    assert_eq!(
        dispatch(&detail_mode(), press(Char('a'))),
        Some(Action::MenuOpen)
    );
    assert_eq!(
        dispatch(&sql_mode(), press(Char('a'))),
        Some(Action::MenuOpen)
    );
}

#[test]
fn menu_nav_and_close_dispatch() {
    let m = menu_mode_with(vec![]);
    assert_eq!(
        dispatch(&m, press(Char('j'))),
        Some(Action::MenuMove(Direction::Down))
    );
    assert_eq!(
        dispatch(&m, press(KeyCode::Up)),
        Some(Action::MenuMove(Direction::Up))
    );
    assert_eq!(dispatch(&m, press(Enter)), Some(Action::MenuConfirm));
    assert_eq!(dispatch(&m, press(Esc)), Some(Action::DetailClose));
    assert_eq!(dispatch(&m, press(Char('q'))), Some(Action::DetailClose));
}

#[test]
fn an_items_own_key_invokes_it_inside_the_menu() {
    // A menu with two items built from REAL bindings rows: `Space`
    // (BookmarkToggle) and `p` (PathToggle). Pressing either key inside
    // the menu invokes that item directly — the same key the row labels.
    let space = slot_binding(ModeKind::Selection, Action::BookmarkToggle).unwrap();
    let path = slot_binding(ModeKind::Selection, Action::PathToggle).unwrap();
    let m = menu_mode_with(vec![
        MenuEntry::Item {
            binding: space,
            label: "Bookmark".into(),
        },
        MenuEntry::Item {
            binding: path,
            label: "Path anchor here".into(),
        },
    ]);
    assert_eq!(
        dispatch(&m, press(Char(' '))),
        Some(Action::MenuInvoke(0)),
        "Space invokes the bookmark item"
    );
    assert_eq!(
        dispatch(&m, press(Char('p'))),
        Some(Action::MenuInvoke(1)),
        "p invokes the path-anchor item"
    );
    // An unrelated key stays unbound (items don't leak each other's keys).
    assert_eq!(dispatch(&m, press(Char('x'))), None);
    // Separators are skipped by the lookup.
    let m = menu_mode_with(vec![
        MenuEntry::Separator,
        MenuEntry::Item {
            binding: path,
            label: "Path anchor here".into(),
        },
    ]);
    assert_eq!(
        dispatch(&m, press(Char('p'))),
        Some(Action::MenuInvoke(1)),
        "the items index (not the item ordinal) is returned"
    );
}

#[test]
fn esc_action_mirrors_each_modes_esc_binding() {
    assert_eq!(
        esc_action(&detail_mode()),
        Some(Action::DetailClose),
        "the structure modal's Esc is DetailClose"
    );
    assert_eq!(
        esc_action(&help_mode()),
        Some(Action::HelpToggle),
        "help's Esc is HelpToggle, not DetailClose"
    );
    assert_eq!(
        esc_action(&palette_mode()),
        Some(Action::SearchCancel),
        "the palette's Esc is SearchCancel"
    );
    assert_eq!(
        esc_action(&yank_mode()),
        Some(Action::DetailClose),
        "the which-key menu's Esc cancels it"
    );
    assert_eq!(
        esc_action(&menu_mode_with(vec![])),
        Some(Action::DetailClose),
        "the node menu's Esc closes it"
    );
    assert_eq!(
        esc_action(&search_mode()),
        Some(Action::SearchCancel),
        "search's Esc cancels the query"
    );
}

#[test]
fn yank_bindings_lists_only_the_prefix_rows() {
    let rows: Vec<_> = yank_bindings().collect();
    assert!(
        !rows.is_empty() && rows.iter().all(|b| b.mode == ModeKind::Yank),
        "only ModeKind::Yank rows, in table order"
    );
    // The first row is `yy` (copy unique_id); the cancel row, then `?`
    // (the help layer) close the table.
    assert_eq!(rows[0].action, Action::YankId);
    assert_eq!(rows[rows.len() - 2].action, Action::DetailClose);
    assert_eq!(rows.last().unwrap().action, Action::HelpToggle);
}

// ---- The contextual status hints (BINDINGS-generated) ----

/// The generated hints as `(key, desc)` pairs — the shape `draw_status`
/// renders left-anchored.
fn hint_pairs(mode: ModeKind, focus: Focus, traced: bool, overview: bool) -> Vec<(String, String)> {
    status_hints(mode, focus, traced, overview)
        .iter()
        .map(|h| (h.key.clone(), h.desc.clone()))
        .collect()
}

#[test]
fn status_hints_emit_the_screen_spec_rows() {
    // The per-context hint tables — generated from BINDINGS, so a key
    // the status line teaches always dispatches.
    assert_eq!(
        hint_pairs(ModeKind::Selection, Focus::List, false, false),
        vec![
            ("j/k".into(), "move".into()),
            ("Enter".into(), "open".into()),
            ("/".into(), "search".into()),
            ("O".into(), "new tab".into()),
            ("Tab".into(), "lineage".into()),
            ("?".into(), "help".into()),
        ],
        "the list-focus row"
    );
    assert_eq!(
        hint_pairs(ModeKind::Selection, Focus::RightPane, false, false),
        vec![
            ("hjkl".into(), "move".into()),
            ("Enter".into(), "re-root".into()),
            ("O".into(), "new tab".into()),
            ("a".into(), "actions".into()),
            ("t".into(), "lens".into()),
            ("?".into(), "help".into()),
        ],
        "the lineage-focus row"
    );
    assert_eq!(
        hint_pairs(ModeKind::Selection, Focus::List, true, false),
        vec![
            ("h/l".into(), "step column".into()),
            ("K".into(), "column modal".into()),
            ("Esc".into(), "exit trace".into()),
        ],
        "a live column trace supersedes the pane row (no Any hints)"
    );
    assert_eq!(
        hint_pairs(ModeKind::Selection, Focus::List, false, true),
        vec![
            ("j/k".into(), "move".into()),
            ("Enter".into(), "open".into()),
            ("/".into(), "search".into()),
            ("1-9".into(), "tabs".into()),
            ("q".into(), "quit".into()),
        ],
        "the overview row (no Any hints either)"
    );
    assert_eq!(
        hint_pairs(ModeKind::Selection, Focus::Inspector, false, false),
        vec![
            ("j/k".into(), "scroll".into()),
            ("Enter".into(), "trace column".into()),
            ("Tab".into(), "list".into()),
            ("?".into(), "help".into()),
        ],
        "the inspector-focus row"
    );
    // A modal mode emits its Any-context rows — Detail is the
    // representative scrollable modal.
    assert_eq!(
        hint_pairs(ModeKind::Detail, Focus::List, false, false),
        vec![
            ("j/k".into(), "scroll".into()),
            ("d/u".into(), "page".into()),
            ("G/g".into(), "jump".into()),
            ("h/l".into(), "column".into()),
            ("Tab".into(), "sections".into()),
            ("Enter".into(), "trace".into()),
            ("K".into(), "column modal".into()),
            ("y".into(), "yank".into()),
            ("a".into(), "actions".into()),
            ("Esc".into(), "close".into()),
            ("?".into(), "help".into()),
        ],
        "the Detail-modal Any row"
    );
}

#[test]
fn status_hints_sort_by_prio_and_carry_bound_actions() {
    // Every generated hint's action is bound in the mode it was emitted
    // for — the drift guard: a status-hint click can never name an action
    // the keymap doesn't offer there. A merged family (`j/k move`) names
    // no single command and is click-inert (`action: None`).
    for (mode, focus, traced, overview) in [
        (ModeKind::Selection, Focus::List, false, false),
        (ModeKind::Selection, Focus::RightPane, false, false),
        (ModeKind::Selection, Focus::Inspector, false, false),
        (ModeKind::Selection, Focus::List, true, false),
        (ModeKind::Selection, Focus::List, false, true),
        (ModeKind::Detail, Focus::List, false, false),
        (ModeKind::Search, Focus::List, false, false),
        (ModeKind::Palette, Focus::List, false, false),
        (ModeKind::Yank, Focus::List, false, false),
        (ModeKind::Menu, Focus::List, false, false),
        (ModeKind::Help, Focus::List, false, false),
        (ModeKind::Sql, Focus::List, false, false),
        (ModeKind::Stats, Focus::List, false, false),
        (ModeKind::Diff, Focus::List, false, false),
        (ModeKind::Columns, Focus::List, false, false),
        (ModeKind::Notifications, Focus::List, false, false),
    ] {
        let hints = status_hints(mode, focus, traced, overview);
        assert!(
            hints.windows(2).all(|w| w[0].prio <= w[1].prio),
            "hints arrive priority-sorted in {mode:?}"
        );
        for h in &hints {
            if let Some(a) = h.action {
                assert!(
                    BINDINGS.iter().any(|b| b.mode == mode && b.action == a),
                    "hint action {a:?} must be bound in {mode:?}"
                );
            }
        }
    }
}

#[test]
fn merged_hint_families_are_click_inert() {
    // A group whose members disagree on the action (a direction pair, a
    // tab-number range) dispatches nothing on click — firing an
    // arbitrary member would be a quiet surprise.
    let list = status_hints(ModeKind::Selection, Focus::List, false, false);
    let by_desc =
        |hints: &[StatusHint], d: &str| hints.iter().find(|h| h.desc == d).unwrap().action;
    assert_eq!(
        by_desc(&list, "move"),
        None,
        "j/k is a family, not a command"
    );
    assert_eq!(
        by_desc(&list, "open"),
        Some(Action::DetailOpen),
        "single-command hints keep their dispatch"
    );
    let overview = status_hints(ModeKind::Selection, Focus::List, false, true);
    assert_eq!(
        by_desc(&overview, "tabs"),
        None,
        "1-9 must not pick a tab number"
    );
    let detail = status_hints(ModeKind::Detail, Focus::List, false, false);
    assert_eq!(by_desc(&detail, "scroll"), None, "j/k scroll is a family");
    assert_eq!(
        by_desc(&detail, "close"),
        Some(Action::DetailClose),
        "Esc/q share one action — the hint stays live"
    );
}

#[test]
fn compact_keys_collapses_ranges_runs_and_pairs() {
    let k = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(compact_keys(&k(&["j", "k"])), "j/k");
    assert_eq!(compact_keys(&k(&["h", "j", "k", "l"])), "hjkl");
    assert_eq!(
        compact_keys(&k(&["1", "2", "3", "4", "5", "6", "7", "8", "9"])),
        "1-9"
    );
    // Two members keep the `/` convention even when they form a run.
    assert_eq!(compact_keys(&k(&["h", "l"])), "h/l");
    // Multi-char labels never merge into a run.
    assert_eq!(compact_keys(&k(&["Down", "Up"])), "Down/Up");
    assert_eq!(compact_keys(&k(&["Enter"])), "Enter");
}

/// The README's Keybindings table must never teach a key the
/// keymap lacks. Every `code` span in a table row resolves: a plain token
/// is a [`Trigger::label`] somewhere in [`BINDINGS`] (any mode); a `y<c>`
/// chord is the `y` prefix plus a [`ModeKind::Yank`] trigger `<c>`. Tokens
/// that can't be keys (non-ASCII glyphs like `⌂`/`▾`, words like `key`,
/// phrases like `+N more` or `[raw]`, `$EDITOR`) aren't held to it.
#[test]
fn readme_keybindings_resolve_against_bindings() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
        .expect("workspace README.md is readable");
    // The Keybindings section runs to the next `## ` heading; its rows
    // are the `|…|` lines (header and `---` separator carry no keys).
    let section = readme
        .split("## Keybindings")
        .nth(1)
        .expect("README has a Keybindings section")
        .split("\n## ")
        .next()
        .unwrap();
    // What may spell a key: a single ASCII char, a named key, a modifier
    // chord, or a `y<c>` yank chord. Everything else is prose.
    const NAMED: &[&str] = &[
        "Enter",
        "Esc",
        "Tab",
        "BackTab",
        "S-Tab",
        "Space",
        "Bksp",
        "Backspace",
        "Up",
        "Down",
        "Left",
        "Right",
        "PgUp",
        "PgDn",
        "Home",
        "End",
        "Ins",
        "Del",
    ];
    let key_like = |tok: &str| {
        let mut cs = tok.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => c.is_ascii() && !c.is_whitespace(),
            _ => {
                NAMED.contains(&tok)
                    || ["Ctrl-", "Cmd-", "Alt-", "Super-", "Shift-"]
                        .iter()
                        .any(|p| tok.starts_with(p))
                    || (tok.starts_with('y') && tok.chars().count() == 2)
            }
        }
    };
    let labels: std::collections::HashSet<String> = BINDINGS
        .iter()
        .flat_map(|b| b.triggers.iter().map(|t| t.label()))
        .collect();
    let yank_labels: std::collections::HashSet<String> = BINDINGS
        .iter()
        .filter(|b| b.mode == ModeKind::Yank)
        .flat_map(|b| b.triggers.iter().map(|t| t.label()))
        .collect();
    // The `y` prefix itself must stay bound for a chord to mean anything.
    assert!(
        BINDINGS.iter().any(|b| {
            b.mode == ModeKind::Selection && b.triggers.iter().any(|t| t.label() == "y")
        }),
        "the `y` yank prefix is unbound"
    );
    let mut checked = 0usize;
    for line in section.lines().filter(|l| l.trim_start().starts_with('|')) {
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('`') else {
                break;
            };
            let tok = &rest[..close];
            rest = &rest[close + 1..];
            if !key_like(tok) {
                continue;
            }
            checked += 1;
            if labels.contains(tok) {
                continue;
            }
            if let Some(ch) = tok.strip_prefix('y') {
                assert!(
                    tok.chars().count() == 2 && yank_labels.contains(ch),
                    "README yank chord `{tok}` is not a Yank-mode binding"
                );
                continue;
            }
            panic!("README key `{tok}` is not bound in BINDINGS");
        }
    }
    assert!(
        checked > 40,
        "only {checked} key tokens found — the table scan likely broke"
    );
}
