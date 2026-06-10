//! TUI tests against the committed fixture (the synthetic
//! jaffle_finance manifest, schema v12): deterministic TTY-free list-assembly
//! assertions plus `TestBackend` renders of `draw`.

use dbtl::model_list::{build_model_list, ModelList, SortMode};
use dbtl::ui::{draw, handle_key, RenderCtx, UiState};
use dbtl::{load_dag, Dag};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

fn fixture_dag() -> Dag {
    load_dag(FIXTURE).expect("fixture manifest must load")
}

fn fixture_list() -> ModelList {
    build_model_list(&fixture_dag(), SortMode::Layer)
}

// ============================================================================
// list-assembly logic (deterministic, TTY-free core gate)
// ============================================================================

#[test]
fn groups_are_four_in_fixed_logical_order() {
    let list = fixture_list();
    let layers: Vec<&str> = list.groups.iter().map(|g| g.layer.as_str()).collect();
    assert_eq!(
        layers,
        vec!["staging", "intermediate", "marts", "utilities"],
        "must be the 4 dbt layers in logical (not alphabetical) order"
    );
}

#[test]
fn group_counts_are_frozen() {
    let list = fixture_list();
    let count = |layer: &str| {
        list.groups
            .iter()
            .find(|g| g.layer == layer)
            .map(|g| g.models.len())
            .unwrap_or(0)
    };
    assert_eq!(count("staging"), 11, "staging count");
    assert_eq!(count("intermediate"), 8, "intermediate count");
    assert_eq!(count("marts"), 9, "marts count");
    assert_eq!(count("utilities"), 17, "utilities count");
    assert_eq!(list.len(), 45, "total selectable models");
}

#[test]
fn each_group_is_name_ascending() {
    let list = fixture_list();
    for group in &list.groups {
        let names: Vec<&str> = group.models.iter().map(|m| m.name.as_str()).collect();
        for pair in names.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "group {} not name-ascending: {:?} > {:?}",
                group.layer,
                pair[0],
                pair[1]
            );
        }
    }
}

#[test]
fn group_first_and_last_names_are_frozen() {
    let list = fixture_list();
    let ends = |layer: &str| {
        let g = list.groups.iter().find(|g| g.layer == layer).unwrap();
        (
            g.models.first().unwrap().name.clone(),
            g.models.last().unwrap().name.clone(),
        )
    };
    assert_eq!(
        ends("staging"),
        (
            "stg_finance__budgets".into(),
            "stg_payment__warehouses".into()
        )
    );
    assert_eq!(
        ends("intermediate"),
        (
            "int_delivery_classifications__enriched".into(),
            "int_suppliers__enriched".into()
        )
    );
    assert_eq!(
        ends("marts"),
        (
            "dim_delivery_classifications".into(),
            "wt_delivery_shopper_monthly_visits".into()
        )
    );
    assert_eq!(ends("utilities"), ("pos_cat".into(), "pos_txn".into()));
}

#[test]
fn flat_display_order_index_0_and_44_are_frozen() {
    let list = fixture_list();
    assert_eq!(
        list.models[0].unique_id, "model.jaffle_finance.stg_finance__budgets",
        "index 0 = initial selection"
    );
    assert_eq!(
        list.models[44].unique_id, "model.jaffle_finance.pos_txn",
        "index 44 = G jump / bottom clamp target"
    );
}

// ============================================================================
// TestBackend rendering
// ============================================================================

/// Concatenate the buffer into one string per row, then all rows joined by '\n'.
fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

/// Concatenate a single buffer row into a string (no trailing newline). Used by
/// the lineage-title tests, which must scan ONLY the title row (row 0) — the
/// status bar also echoes the selected name, so a whole-buffer scan false-counts.
fn row_text(buffer: &Buffer, y: u16) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for x in 0..area.width {
        if let Some(cell) = buffer.cell((x, y)) {
            out.push_str(cell.symbol());
        }
    }
    out
}

/// Render the initial state into a `width x height` buffer.
fn render(width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let ctx = RenderCtx::new(&list, &state, None);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    terminal.backend().buffer().clone()
}

#[test]
fn all_four_headers_visible_in_tall_buffer() {
    // Tall buffer (height >= 55) so all 49 list rows render without scrolling;
    // at initial selection (index 0, offset 0) the utilities header would
    // otherwise be off-screen on a normal-height terminal.
    let buffer = render(100, 60);
    let text = buffer_to_string(&buffer);
    // Headers render title-cased with a leading rule glyph (display-only; the
    // lowercase grouping key is unchanged — see model_list.rs struct tests).
    for header in ["Staging", "Intermediate", "Marts", "Utilities"] {
        assert!(
            text.contains(header),
            "header {header:?} must be visible in tall buffer"
        );
    }
}

#[test]
fn known_model_name_visible() {
    let buffer = render(100, 60);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("stg_finance__budgets"),
        "the initially selected model name must be visible"
    );
}

#[test]
fn selected_row_is_visually_distinguished() {
    // Normal-size buffer is fine here.
    let buffer = render(100, 30);
    let text = buffer_to_string(&buffer);

    // (1) Marker-based: the selected row carries the "> " marker; no other
    // model row should. We check the marker appears exactly once.
    let marker_lines = text.lines().filter(|l| l.contains("> ")).count();
    assert_eq!(
        marker_lines, 1,
        "exactly one row should carry the selection marker"
    );

    // (2) Style-based: at least one cell carries the REVERSED modifier (the
    // selected row), proving style distinction is also present.
    let area = buffer.area;
    let mut reversed_cells = 0;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                if cell.modifier.contains(Modifier::REVERSED) {
                    reversed_cells += 1;
                }
            }
        }
    }
    assert!(
        reversed_cells > 0,
        "selected row must carry a distinguishing (REVERSED) style"
    );
}

#[test]
fn status_help_line_present() {
    let buffer = render(100, 30);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("[j/k]") || text.contains("sel:"),
        "bottom status/help line with key hints or sel: must be present"
    );
    // The selected node's unique_id is echoed in the status line.
    assert!(
        text.contains("stg_finance__budgets"),
        "status line should echo the selected node"
    );
}

#[test]
fn hidden_list_pane_renders_lineage_full_width() {
    // Cmd-b / Ctrl-b hides the model list: the lineage pane must span the whole
    // screen width and nothing of the list (headers, selection marker) renders.
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let mut state = UiState::new(list.len());
    state.toggle_list_pane(); // hide
    let ctx = RenderCtx::new(&list, &state, None);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);

    // The lineage pane's border now owns both screen edges of the top row.
    assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "╭");
    assert_eq!(buffer.cell((99, 0)).unwrap().symbol(), "╮");
    // No list content: group headers and the selection marker are gone (the
    // status line still echoes the selected unique_id, so match the header's
    // "name (count)" shape, not a bare model name).
    assert!(
        !text.contains("staging ("),
        "list group headers must not render while hidden"
    );
    assert!(
        !text.lines().any(|l| l.contains("> ")),
        "no list selection marker while hidden"
    );
}

#[test]
fn help_overlay_renders_keybindings_from_keymap() {
    // The ? overlay is generated from the keymap (single source of truth), so a
    // representative binding description must render.
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mode = dbtl::Mode::Help { scroll: 0 };
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.mode = &mode;
    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw help overlay must not panic");
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("Keybindings"), "help overlay title present");
    assert!(
        text.contains(concat!("dbtl v", env!("CARGO_PKG_VERSION"))),
        "help title carries the crate version"
    );
    assert!(text.contains("quit"), "the quit binding is documented");
    assert!(
        text.contains("open structure / re-root to cursor"),
        "the Enter/structure binding is documented (derived from the keymap)"
    );
}

#[test]
fn filter_tag_renders_in_list_title_and_status_segment() {
    // Drive the real path: bookmark + filter, then wire RenderCtx like the
    // loop does (bare label; title and segment add their own brackets).
    use dbtl::app::{apply_action, App};
    use dbtl::ui::StatusSegments;
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::BookmarkToggle);
    apply_action(&mut app, Action::ToggleBookmarkFilter);
    assert_eq!(app.active_list().len(), 1);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.filter_label = app.list_filter_label();
        ctx.segments = StatusSegments {
            filter: app.list_filter_label(),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let buffer = terminal.backend().buffer().clone();
    let title = row_text(&buffer, 0);
    assert!(
        title.contains("[marked]"),
        "list title carries the filter tag: {title}"
    );
    let status = row_text(&buffer, buffer.area.height - 1);
    assert!(
        status.contains("[marked]") && !status.contains("[[marked]]"),
        "status segment shows the tag exactly once-bracketed: {status}"
    );
}

#[test]
fn detail_modal_renders_structure_via_real_path() {
    // Drive the real path: build the App, select a known model, open the detail
    // modal, then render. The modal must show the structure (type/columns) drawn
    // from the Dag side maps — without RenderCtx holding a Dag.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::DetailOpen);

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("detail modal render must not panic");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("pos_txn"), "modal titled with the model name");
    assert!(
        text.contains("type:"),
        "structure shows the materialization line"
    );
    assert!(
        text.contains("columns"),
        "structure shows the columns section"
    );
    assert!(text.contains("table"), "pos_txn's materialization is shown");
    // The blast-radius line: pos_txn has 0 downstream / 7 upstream (the SAME
    // closures the fixture freezes in `closure_upstream_only_pos_txn`).
    assert!(
        text.contains("impact: 0 downstream / 7 upstream"),
        "structure shows the blast-radius line"
    );
    assert!(
        text.contains("path:   models/utilities/pos_prep/pos_txn.sql"),
        "structure shows the model's file path"
    );
}

#[test]
fn detail_modal_impact_line_targets_the_cursored_node_not_the_root() {
    // The critical case: when the lineage pane is focused and the cursor sits on a
    // non-root SOURCE, opening the structure modal must count THAT source's blast
    // radius, not the root's. The shoppers source has 2 downstream / 0 upstream.
    use dbtl::app::{apply_action, App};
    use dbtl::{Action, Focus, Mode};

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // Root a model whose upstream INCLUDES the shoppers source.
    app.select_by_unique_id("model.jaffle_finance.int_shoppers__enriched");
    app.ui_state.set_focus(Focus::RightPane);
    // Put the lineage cursor onto the shoppers source (the deterministic path the
    // event loop uses for a click on a non-selectable node — it moves the cursor
    // there rather than re-rooting).
    let src = "source.jaffle_finance.dev_lake_jaffle_payment.shoppers";
    app.click_lineage_node(src);
    assert_eq!(
        app.lineage_cursor_uid().as_deref(),
        Some(src),
        "cursor reached the shoppers source"
    );
    apply_action(&mut app, Action::DetailOpen);
    // A source is non-selectable, so DetailOpen opens its modal (never re-roots).
    assert!(
        matches!(app.mode, Mode::Detail(_)),
        "source opens its modal"
    );

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains("impact: 2 downstream / 0 upstream"),
        "the modal counts the CURSORED source, not the root model"
    );
}

/// Render a `Mode::Stats(sv)` modal over the base panes into a buffer (the same
/// `draw` path the event loop drives, with the modal data carried in the Mode
/// payload — no `Dag` in `RenderCtx`).
fn render_stats_modal(sv: dbtl::StatsView, width: u16, height: u16) -> Buffer {
    use dbtl::Mode;
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mode = Mode::Stats(sv);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.mode = &mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("stats modal render must not panic");
    }
    terminal.backend().buffer().clone()
}

/// A `StatsView` literal with `n_orphans` orphan names and `n_viol` layer
/// violations — built to exercise the render caps without a Dag.
fn stats_view_with(n_orphans: usize, n_viol: usize) -> dbtl::StatsView {
    dbtl::StatsView {
        project: "proj".into(),
        by_resource_type: vec![("model".into(), 8), ("source".into(), 3)],
        by_materialization: vec![("table".into(), 5), ("view".into(), 3)],
        testable_total: 10,
        testable_tested: 9,
        top_hubs: vec![("model.proj.a".into(), "a".into(), 6)],
        transitive_hubs: vec![("a".into(), 12), ("b".into(), 4)],
        orphan_models: (0..n_orphans).map(|i| format!("orphan_{i:02}")).collect(),
        layer_violations: (0..n_viol)
            .map(|i| (format!("mart_{i:02}"), format!("stg_{i:02}")))
            .collect(),
        critical_path: vec!["src_a".into(), "stg_a".into(), "mart_a".into()],
        untested_testable: 1,
        zero_downstream_models: 2,
        no_description_models: 4,
        scroll: 0,
    }
}

#[test]
fn stats_modal_gauge_has_graded_fill_over_dark_track() {
    // High coverage (9/10 = 90%) → an OK-graded fill over a SURFACE_HI dark
    // track, both made of bg-styled spaces (no glyphs). Scan the buffer for a
    // cell with the graded fill bg and a cell with the track bg, proving the
    // two-span gauge rendered (coordinate-free, per the advisor).
    use dbtl::ui::theme;
    let buffer = render_stats_modal(stats_view_with(0, 0), 100, 40);
    // Scan ONLY the coverage row (the line carrying "tested") so the green fill
    // can't be confused with the green transitive-hub mini-bar elsewhere.
    let area = buffer.area;
    let cov_y = (0..area.height)
        .find(|&y| {
            (0..area.width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains("tested")
        })
        .expect("a coverage row containing 'tested' must render");
    let mut saw_fill = false;
    let mut saw_track = false;
    for x in 0..area.width {
        if let Some(c) = buffer.cell((x, cov_y)) {
            if c.bg == theme::OK {
                saw_fill = true;
            }
            if c.bg == theme::SURFACE_HI {
                saw_track = true;
            }
        }
    }
    assert!(
        saw_fill,
        "coverage gauge fill (OK grade @ 90%) on the coverage row"
    );
    assert!(
        saw_track,
        "coverage gauge dark track (SURFACE_HI) on the coverage row"
    );
}

#[test]
fn stats_modal_low_coverage_gauge_is_red() {
    use dbtl::ui::theme;
    let mut sv = stats_view_with(0, 0);
    sv.testable_tested = 2; // 2/10 = 20% → danger grade
    let buffer = render_stats_modal(sv, 100, 40);
    let red = (0..buffer.area.height).any(|y| {
        (0..buffer.area.width).any(|x| buffer.cell((x, y)).map(|c| c.bg) == Some(theme::DANGER))
    });
    assert!(red, "low coverage (<40%) grades the gauge fill DANGER red");
}

#[test]
fn stats_modal_shows_new_sections_and_more_cap() {
    // 12 orphans + 11 violations → both worklists hit the cap-10 + "+K more".
    // Tall buffer so the whole (unscrolled) dashboard fits the modal interior.
    let buffer = render_stats_modal(stats_view_with(12, 11), 100, 80);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("Orphans (12)"),
        "orphans header with full count"
    );
    assert!(
        text.contains("Layer violations (11)"),
        "violations header with full count"
    );
    assert!(
        text.contains("Hubs (transitive"),
        "transitive-hubs section present"
    );
    assert!(
        text.contains("+2 more"),
        "orphans overflow rolls into +K more"
    );
    assert!(
        text.contains("+1 more"),
        "violations overflow rolls into +K more"
    );
    assert!(
        text.contains("mart_00 -> stg_00"),
        "violation rows use the ASCII arrow"
    );
    // Critical path: header carries the depth, hops render as the indented
    // staircase (each name one space further right).
    assert!(
        text.contains("Critical path (depth 3)"),
        "critical-path header with depth"
    );
    assert!(text.contains("  src_a"), "first hop at base indent");
    assert!(text.contains("   stg_a"), "second hop one space deeper");
    assert!(text.contains("    mart_a"), "third hop two spaces deeper");
}

#[test]
fn palette_modal_echoes_query_highlights_selection_and_shows_key_labels() {
    // Drive the real path: open the palette, type a narrowing query, step the
    // cursor, then render. The modal must echo the query, reverse the selected
    // row, and show the dim key labels (right column derived from the keymap).
    use dbtl::app::{apply_action, App};
    use dbtl::{Action, Direction};
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "toggle".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    apply_action(&mut app, Action::SearchMove(Direction::Down));

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("palette modal render must not panic");
    }
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_to_string(&buffer);

    // (1) Title + the live query are echoed.
    assert!(text.contains("commands"), "palette titled 'commands'");
    assert!(
        text.contains("> toggle"),
        "the live query is echoed on the query line"
    );
    // (2) A candidate help text and its key label render (e.g. the minimap toggle
    // is bound to `M`, the downstream toggle to `d`). "toggle" is a subsequence of
    // several Selection-command help strings, so at least one must show.
    assert!(
        text.contains("toggle minimap") || text.contains("toggle downstream"),
        "a matched command's help text renders:\n{text}"
    );
    // (3) The selected row carries the REVERSED style (selection cue).
    let area = buffer.area;
    let mut reversed = 0;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                if cell.modifier.contains(Modifier::REVERSED) {
                    reversed += 1;
                }
            }
        }
    }
    assert!(reversed > 0, "the selected palette row is REVERSED");
}

#[test]
fn palette_highlights_key_label_chars_when_only_the_key_matches() {
    // A row admitted by a KEY-LABEL match (not help) must show its match cue. Query
    // "cmd": the `Cmd-b/Ctrl-b -> show / hide the model list pane` row is admitted
    // by its key label (its help has no "cmd" subsequence). Step the cursor OFF that
    // row (the selected row keeps its reverse, suppressing the highlight cue), then
    // assert ITS key-label chars carry the yellow+bold cue while its help does not.
    use dbtl::action::palette_candidates;
    use dbtl::app::{apply_action, App};
    use dbtl::model_list::match_indices;
    use dbtl::{Action, Direction};

    // Precondition: row 0 of the "cmd" candidates is the key-label-only row.
    let cands = palette_candidates("cmd");
    let kl_row = cands.first().expect("a 'cmd' candidate");
    assert!(
        match_indices(kl_row.help, "cmd").is_empty()
            && !match_indices(&kl_row.key_label(), "cmd").is_empty(),
        "row 0 is admitted via its key label, not help text"
    );
    let help0 = kl_row.help; // the help text that must carry NO highlight

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "cmd".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    // Move selection off row 0 so the key-label row is NOT reversed.
    apply_action(&mut app, Action::SearchMove(Direction::Down));

    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area;

    // Locate the key-label row inside the modal by its (unique) help text, then
    // scan ONLY that row for GOLD+bold (match highlight) cells — a whole-buffer
    // scan would catch the list pane's own accented layer headers outside the
    // modal.
    let mut row_y = None;
    for y in 0..area.height {
        if row_text(&buffer, y).contains(help0) {
            row_y = Some(y);
            break;
        }
    }
    let y = row_y.expect("the key-label row renders inside the modal");
    let mut highlighted = String::new();
    for x in 0..area.width {
        if let Some(cell) = buffer.cell((x, y)) {
            if cell.fg == dbtl::ui::theme::GOLD && cell.modifier.contains(Modifier::BOLD) {
                highlighted.push_str(cell.symbol());
            }
        }
    }
    // The matched key-label chars (C, m, d of "Cmd-b/Ctrl-b") are highlighted…
    let lower = highlighted.to_lowercase();
    assert!(
        lower.contains('c') && lower.contains('m') && lower.contains('d'),
        "the key-label match chars are highlighted GOLD+bold, got {highlighted:?}"
    );
    // …and NO char of the HELP text is highlighted (the help had no match). The help
    // "show / hide the model list pane" has chars (h, w) the key label lacks.
    assert!(
        !highlighted.contains('h') && !highlighted.contains('w'),
        "no help-text char is highlighted, got {highlighted:?}"
    );
}

#[test]
fn draw_does_not_panic_on_tiny_buffer() {
    // Smoke: degenerate small terminals must not panic in draw.
    let backend = TestBackend::new(8, 4);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let ctx = RenderCtx::new(&list, &state, None);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw on tiny buffer must not panic");
}

// ============================================================================
// render-based SCROLL tests (clip-bug guard)
//
// The clip bug: after `G` (or `j` to the bottom) on a normal-size terminal, the
// selected model row was clipped below the list-pane border and never rendered,
// even though a model-space state test claimed it was "visible". These tests
// reproduce the real event-loop path against a TestBackend and assert the
// selected row is *actually drawn* — the only metric that catches the bug.
// ============================================================================

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The list pane's inner height for a terminal of total `height`, mirroring the
/// binary's layout: minus the 1-row status line, minus the 2 border rows.
fn inner_list_height(height: u16) -> usize {
    (height.saturating_sub(1).saturating_sub(2)).max(1) as usize
}

/// Drive the real event-loop steps: apply each key via `handle_key`, then follow
/// scroll exactly as `main::event_loop` does (display-row space), then render at
/// `width x height`. Returns the rendered buffer.
fn render_after_keys(width: u16, height: u16, keys: &[KeyCode]) -> (Buffer, UiState, ModelList) {
    let list = fixture_list();
    let mut state = UiState::new(list.len());
    for &k in keys {
        handle_key(&mut state, press(k));
    }
    // Scroll-follow before drawing — same call the event loop makes.
    let h = inner_list_height(height);
    let sel = state.selected();
    state.ensure_visible(
        h,
        list.reveal_row_of_model(sel),
        list.row_of_model(sel),
        list.row_count(),
    );

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let ctx = RenderCtx::new(&list, &state, None);
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    let buffer = terminal.backend().buffer().clone();
    (buffer, state, list)
}

/// The single rendered line that carries the selection marker `> `. The status
/// line never contains `> `, so this uniquely identifies the selected *list*
/// row (avoiding the `contains("pos_pay")` false positive from the status echo).
fn marker_line(buffer: &Buffer) -> String {
    let text = buffer_to_string(buffer);
    let lines: Vec<&str> = text.lines().filter(|l| l.contains("> ")).collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one rendered line must carry the selection marker; got {}:\n{text}",
        lines.len()
    );
    lines[0].to_string()
}

#[test]
fn g_jump_renders_selected_row_in_normal_terminal() {
    // 100x30: height 30 < ~52, so the full 49-row list cannot fit and the bug
    // would clip the selection. After `G` the selected model is `pos_txn`
    // (index 44). It must be on the rendered marker line — not merely somewhere
    // in the buffer (the status line echoes it regardless).
    let (buffer, state, list) = render_after_keys(100, 30, &[KeyCode::Char('G')]);
    assert_eq!(state.selected(), 44, "G selects the last model");
    assert_eq!(
        list.models[44].name, "pos_txn",
        "frozen: index 44 is pos_txn"
    );

    let line = marker_line(&buffer);
    assert!(
        line.contains("pos_txn"),
        "the selected row (marker line) must render 'pos_txn', got: {line:?}"
    );
    // The window must actually have scrolled (offset advanced past the top).
    assert!(
        state.offset() > 0,
        "selecting the bottom model on a short terminal must scroll the window"
    );
}

#[test]
fn j_to_bottom_renders_selected_row_in_normal_terminal() {
    // Same property reached via a long run of `j` instead of `G`.
    let keys = vec![KeyCode::Char('j'); 60]; // more than enough to clamp at 44
    let (buffer, state, list) = render_after_keys(100, 30, &keys);
    assert_eq!(
        state.selected(),
        44,
        "j past the end clamps at the last model"
    );

    let line = marker_line(&buffer);
    assert!(
        line.contains(&list.models[44].name),
        "the selected row must render the bottom model name, got: {line:?}"
    );
}

#[test]
fn mid_list_selection_below_fold_renders_selected_row() {
    // Pick a model whose DISPLAY ROW exceeds the inner height so offset must be
    // > 0 (a mid-list model already on-screen at offset 0 would guard nothing —
    // it passes even when the clip is present). Model 30 sits at display row ~34,
    // beyond the inner height (27) of a 100x30 terminal.
    let keys = vec![KeyCode::Char('j'); 30]; // start 0 -> select 30
    let (buffer, state, list) = render_after_keys(100, 30, &keys);
    assert_eq!(state.selected(), 30);

    let target = &list.models[30].name;
    assert!(
        list.row_of_model(30) > inner_list_height(30),
        "precondition: model 30's display row must be below the fold"
    );
    assert!(
        state.offset() > 0,
        "a below-the-fold mid-list selection must scroll"
    );
    let line = marker_line(&buffer);
    assert!(
        line.contains(target.as_str()),
        "the selected mid-list row must render '{target}', got: {line:?}"
    );
}

#[test]
fn scrolled_render_still_shows_status_and_borders() {
    // After scrolling, the status/help line and the selected node echo must
    // still be present (the chrome doesn't get scrolled away).
    let (buffer, state, list) = render_after_keys(100, 30, &[KeyCode::Char('G')]);
    let text = buffer_to_string(&buffer);
    assert!(text.contains("[j/k]"), "status/help line must remain");
    // The status core echoes the selected node's NAME (the core was trimmed from
    // the full dotted unique_id so the colored segments survive an 80-col line).
    assert!(
        text.contains(&list.models[state.selected()].name),
        "status line must echo the selected node name"
    );
}

#[test]
fn search_filters_and_keeps_selected_row_visible() {
    // The two-coordinate-space guard UNDER SEARCH: after filtering, the selected
    // model (tracked by unique_id) must be re-resolved into the FILTERED index,
    // and ensure_visible(filtered rows) must keep it inside the window — the
    // clip-bug class, via the filter path.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    use ratatui::crossterm::event::KeyCode;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // Open search and type "pos" — narrows to the pos_* utilities models.
    apply_action(&mut app, Action::SearchOpen);
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let filtered_len = app.active_list().len();
    assert!(
        filtered_len > 0 && filtered_len < 45,
        "search narrowed the list: {filtered_len}"
    );
    assert!(
        app.active_list().models.iter().any(|m| m.name == "pos_txn"),
        "a known matching model survives the filter"
    );

    // Move to the last filtered match, follow scroll exactly as the loop does,
    // then render — the selected (marker) row must actually be drawn.
    for _ in 0..filtered_len {
        apply_action(&mut app, Action::SearchMove(dbtl::Direction::Down));
    }
    let sel = app.ui_state.selected();
    let h = inner_list_height(30);
    app.ui_state.ensure_visible(
        h,
        app.active_list().reveal_row_of_model(sel),
        app.active_list().row_of_model(sel),
        app.active_list().row_count(),
    );
    let want = app.active_list().model_at(sel).unwrap().name.clone();

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let line = marker_line(&terminal.backend().buffer().clone());
    assert!(
        line.contains(&want),
        "selected filtered row '{want}' must render, got {line:?}"
    );

    // The list title reflects the live query.
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("Search: pos"), "list title shows the query");

    // Confirm resolves the selection back into the FULL list (filter dropped).
    let _ = KeyCode::Enter; // (dispatch is exercised in action.rs tests)
    apply_action(&mut app, Action::SearchConfirm);
    assert_eq!(app.active_list().len(), 45, "filter dropped on confirm");
    assert_eq!(
        app.active_list()
            .model_at(app.ui_state.selected())
            .unwrap()
            .name,
        want,
        "the chosen match stays selected in the full list"
    );
}

#[test]
fn search_title_shows_n_over_m_match_count() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::SearchOpen);
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let n = app.active_list().len();
    let m = app.model_list.len(); // full count
    assert!(n > 0 && n < m, "search narrowed: {n}/{m}");

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.full_model_count = Some(m);
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    // The literal prefix stays (load-bearing) AND the N/M count is present.
    assert!(text.contains("Search: pos"), "query prefix still shown");
    assert!(
        text.contains(&format!("{n}/{m}")),
        "title shows the N/M match count, got: {text:?}"
    );
}

#[test]
fn bookmark_badge_renders_on_a_bookmarked_row() {
    use std::collections::BTreeSet;

    let list = fixture_list();
    let state = UiState::new(list.len());
    // Bookmark the first model in the flat list (row near the top → on screen).
    let uid = list.models[0].unique_id.clone();
    let mut bookmarks = BTreeSet::new();
    bookmarks.insert(uid);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.bookmarks = Some(&bookmarks);
        // Default glyph mode is Unicode ⇒ the badge is the ★ glyph.
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains('★'),
        "the Unicode bookmark badge renders on the bookmarked row: {text:?}"
    );

    // ASCII glyph mode renders the pure-ASCII '*' badge instead (ascii_guard
    // territory: never the ambiguous-width ★).
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.bookmarks = Some(&bookmarks);
        ctx.glyphs = dbtl::GlyphMode::Ascii;
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let ascii = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        ascii.contains('*'),
        "ASCII bookmark badge is '*': {ascii:?}"
    );
    assert!(
        !ascii.contains('★'),
        "no ambiguous-width glyph in ASCII mode"
    );
}

#[test]
fn search_cancel_restores_origin_selection() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let origin = app.ui_state.selected();
    apply_action(&mut app, Action::SearchOpen);
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    apply_action(&mut app, Action::SearchCancel);
    assert_eq!(app.active_list().len(), 45, "full list restored");
    assert_eq!(
        app.ui_state.selected(),
        origin,
        "selection restored to the search origin"
    );
}

#[test]
fn g_jump_does_not_panic_on_tiny_terminal() {
    // Extreme small terminal: even when a header + 1 model can't both fit, the
    // scroll-follow + draw path must not panic.
    for (w, h) in [(8u16, 4u16), (4, 3), (20, 5), (3, 3)] {
        let keys = [KeyCode::Char('G')];
        let _ = render_after_keys(w, h, &keys); // must simply not panic
    }
}

// ============================================================================
// UI chrome: list-pane scrollbar, segmented status bar, accented layer headers
// ============================================================================

/// The list pane's right-border column for a `width`-wide terminal: the list
/// pane is the 40% left split (`pane_rects`), so its border is at `0.4*width - 1`.
fn list_right_border_x(width: u16) -> u16 {
    ((width as u32 * 40 / 100) as u16) - 1
}

/// Count thumb glyphs (Unicode `█`) drawn on the list pane's right-border column.
fn list_scrollbar_thumb_cells(buffer: &Buffer, width: u16) -> usize {
    let x = list_right_border_x(width);
    let area = buffer.area;
    (0..area.height)
        .filter(|&y| buffer.cell((x, y)).map(|c| c.symbol()) == Some("█"))
        .count()
}

#[test]
fn list_scrollbar_absent_when_list_fits() {
    // A tall buffer fits all 49 display rows in the list interior → no thumb.
    let buffer = render(100, 60);
    assert_eq!(
        list_scrollbar_thumb_cells(&buffer, 100),
        0,
        "no scrollbar thumb when the list fits the pane"
    );
}

#[test]
fn list_scrollbar_present_when_list_overflows() {
    // A short buffer cannot fit 49 rows → the thumb appears on the right border.
    let buffer = render(100, 20);
    assert!(
        list_scrollbar_thumb_cells(&buffer, 100) >= 1,
        "a scrollbar thumb must render when the list overflows the pane"
    );
}

#[test]
fn list_scrollbar_thumb_at_top_when_unscrolled() {
    // Unscrolled (offset 0): the thumb's top cell sits at the very top of the
    // track (interior row 1 of the bordered pane).
    let buffer = render(100, 20);
    let x = list_right_border_x(100);
    // Track top is area.y(0) + 1 border row = row 1.
    assert_eq!(
        buffer.cell((x, 1)).map(|c| c.symbol()),
        Some("█"),
        "offset 0 → thumb flush at the top of the track"
    );
}

#[test]
fn list_scrollbar_thumb_flush_bottom_when_scrolled_to_end() {
    // Scroll to the bottom via `G`: the thumb's last cell sits on the track's
    // last interior row (one above the list pane's bottom border).
    let (buffer, state, _) = render_after_keys(100, 20, &[KeyCode::Char('G')]);
    assert!(state.offset() > 0, "G must scroll the short list");
    let x = list_right_border_x(100);
    // The list pane occupies the body (height 20 → status row 19 → body rows
    // 0..=18). Its bottom border is row 18, so the last interior row is 17 — the
    // flush-bottom thumb cell. Below it, the corner glyph (not a thumb) renders.
    assert_eq!(
        buffer.cell((x, 17)).map(|c| c.symbol()),
        Some("█"),
        "scrolled to the end → thumb flush at the bottom of the track (last interior row)"
    );
    assert_ne!(
        buffer.cell((x, 18)).map(|c| c.symbol()),
        Some("█"),
        "the thumb never overwrites the bottom border corner"
    );
}

#[test]
fn status_bar_keeps_protected_core_and_appends_segments() {
    use dbtl::app::{apply_action, App};
    use dbtl::ui::StatusSegments;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    // Cycle to the coverage lens (Off → Coverage) so the cov segment populates.
    apply_action(&mut app, Action::CycleLens);
    let (tested, total) = app.coverage_summary();
    let pct = if total == 0 { 0 } else { tested * 100 / total };
    let cov = format!("cov {pct}% ({tested}/{total})");
    let pos = format!(
        "{}/{}",
        app.ui_state.selected() + 1,
        app.active_list().len()
    );

    // A wide line (160) so every segment fits after the long uid-bearing core.
    let backend = TestBackend::new(160, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.segments = StatusSegments {
            impact: None,
            focus: Some("list"),
            view: None,
            position: Some(pos.as_str()),
            coverage: Some(cov.as_str()),
            bookmarks: None,
            sort: None,
            filter: None,
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());

    // Protected core stays present and is never dropped.
    assert!(text.contains("[j/k]"), "protected key hint must remain");
    assert!(
        text.contains("fct_subscription_process"),
        "protected sel:{{uid}} must remain"
    );
    // Additive colored segments render (they fit a 160-wide status line).
    assert!(text.contains("[list]"), "focus segment renders additively");
    assert!(text.contains(&pos), "position segment renders additively");
    assert!(text.contains(&cov), "coverage segment renders additively");
}

#[test]
fn status_bar_drops_segments_but_never_the_core_on_a_narrow_line() {
    use dbtl::ui::StatusSegments;
    // A narrow width where the protected core nearly fills the line: segments
    // must be dropped (right-first) while the core stays intact.
    let list = fixture_list();
    let state = UiState::new(list.len());
    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.segments = StatusSegments {
            impact: None,
            focus: Some("list"),
            view: None,
            position: Some("1/45"),
            coverage: None,
            bookmarks: None,
            sort: None,
            filter: None,
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    // The protected core is non-droppable.
    assert!(
        text.contains("[j/k]"),
        "the protected core survives a narrow status line"
    );
}

#[test]
fn status_segments_render_at_80_cols() {
    // Regression guard: at the standard 80-col terminal the colored segments must
    // still appear for a realistic dbt uid. Before the core was trimmed (long
    // `[j/k] move [Tab] focus [?] help [q] quit  sel:{dotted_uid}` + the note),
    // the core alone exceeded 80, so EVERY segment was dropped and the whole
    // status-segment feature rendered nothing at the most common width.
    use dbtl::app::{apply_action, App};
    use dbtl::ui::StatusSegments;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // A long-named node + the coverage lens (populates the status `[note]`), the
    // realistic worst case the finding called out.
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    apply_action(&mut app, Action::CycleLens);
    let status_note = app.selected_status_note();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.mode = &app.mode;
        ctx.status = status_note.as_deref();
        ctx.segments = StatusSegments {
            impact: None,
            focus: Some("list"),
            view: None,
            position: Some("1/45"),
            coverage: None,
            bookmarks: None,
            sort: None,
            filter: None,
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());

    // The core stays present, echoing the NAME (not the full dotted uid).
    assert!(text.contains("[j/k]"), "protected core present at 80 cols");
    assert!(
        text.contains("fct_subscription_process"),
        "the selected node name is echoed in the core"
    );
    // The highest-priority segment that fits MUST render at 80 cols — this is
    // the bug the core trim fixed (it was invisible before). Position outranks
    // focus in the deliberate priority order (focus is redundant with the green
    // pane border, so it is among the first to drop on a narrow terminal).
    assert!(
        text.contains("[1/45]"),
        "at least one status segment renders at 80 cols"
    );
}

#[test]
fn status_impact_segment_renders_and_survives_narrowest_droppable_line() {
    // The impact segment is the highest-priority droppable segment (index 0): it
    // renders at a normal width and is the LAST to drop when the line narrows.
    use dbtl::app::App;
    use dbtl::ui::StatusSegments;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let impact = app.impact_status().expect("selected → impact");
    assert_eq!(impact, "impact ↓1 ↑2", "unicode badges in the default mode");

    // At 100 cols every segment fits, so impact renders.
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.segments = StatusSegments {
            impact: Some(impact.as_str()),
            focus: Some("list"),
            view: Some("↑"),
            position: Some("3/45"),
            coverage: None,
            bookmarks: None,
            sort: None,
            filter: None,
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let wide = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(wide.contains(&impact), "impact segment renders at 100 cols");
    assert!(wide.contains("[3/45]"), "position renders too at 100 cols");

    // On a narrow line the lower-priority segments drop while impact survives
    // (it is index 0, so it is the LAST to be dropped). Width chosen so only the
    // core + the impact segment fit.
    use unicode_width::UnicodeWidthStr;
    let sel = "stg_payment__shoppers";
    let core = format!("[j/k] move  [?] help  sel: {sel}");
    let narrow_w = (core.width() + format!(" [{impact}]").width() + 2) as u16;
    let backend = TestBackend::new(narrow_w, 6);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.segments = StatusSegments {
            impact: Some(impact.as_str()),
            focus: Some("list"),
            view: Some("↑"),
            position: Some("3/45"),
            coverage: None,
            bookmarks: None,
            sort: None,
            filter: None,
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let narrow = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(narrow.contains("[j/k]"), "core survives the narrow line");
    assert!(
        narrow.contains(&impact),
        "impact survives as top-priority segment"
    );
    assert!(
        !narrow.contains("[3/45]"),
        "lower-priority position drops on the narrow line"
    );
}

#[test]
fn status_impact_segment_uses_ascii_badges_in_ascii_mode() {
    // In ASCII glyph mode the segment carries the Chrome ASCII badges (v/^),
    // never the unicode arrows — so the segmented status bar stays ascii-safe.
    use dbtl::app::App;
    use dbtl::layout::GlyphMode;
    use dbtl::ui::StatusSegments;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.glyph_mode = GlyphMode::Ascii;
    app.select_by_unique_id("model.jaffle_finance.stg_payment__shoppers");
    let impact = app.impact_status().expect("selected → impact");
    assert_eq!(impact, "impact v1 ^2", "ascii badges, never ↓/↑");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, None);
        ctx.glyphs = GlyphMode::Ascii;
        ctx.segments = StatusSegments {
            impact: Some(impact.as_str()),
            ..StatusSegments::default()
        };
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains("impact v1 ^2"),
        "ascii impact segment renders"
    );
    assert!(
        !text.contains('↓') && !text.contains('↑'),
        "no unicode arrows"
    );
}

#[test]
fn layer_header_renders_accented_and_capitalized() {
    // The staging header renders title-cased with a leading rule glyph, and its
    // per-layer accent color — NOT the shared overlay header_style.
    let buffer = render(100, 60);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("Staging ("),
        "staging header renders title-cased with its count"
    );
    assert!(text.contains('─'), "header carries the Unicode rule glyph");

    // Find a cell on the staging header line and assert its accent color.
    let area = buffer.area;
    let mut found_cyan_header = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            if let Some(c) = buffer.cell((x, y)) {
                row.push_str(c.symbol());
            }
        }
        if row.contains("Staging (") {
            // The 'S' of "Staging" carries the per-layer accent (LAYER_STAGING),
            // proving the accent path is distinct from the overlay header_style.
            for x in 0..area.width {
                if let Some(c) = buffer.cell((x, y)) {
                    if c.symbol() == "S" && c.fg == dbtl::ui::theme::LAYER_STAGING {
                        found_cyan_header = true;
                    }
                }
            }
        }
    }
    assert!(
        found_cyan_header,
        "the staging header carries its LAYER_STAGING accent (not the overlay header style)"
    );
}

// ============================================================================
// Lineage minimap (stretch): default OFF protects every lineage render; ON it
// draws an occupancy inset in the top-right of the lineage interior.
// ============================================================================

/// Render an `App` (with its real lineage layout for the current selection) into
/// a `width x height` buffer, mirroring the event loop's lineage setup. The
/// minimap is read straight from `app.ui_state.minimap_visible()`.
fn render_app(app: &dbtl::app::App, width: u16, height: u16) -> Buffer {
    use dbtl::layout::layout_mode;
    let sg = app.lineage_subgraph();
    let lineage = if sg.nodes.is_empty() {
        None
    } else {
        let mut lay = layout_mode(&sg, app.glyph_mode);
        lay.apply_node_styles(&app.lineage_styles(&lay));
        Some(lay)
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_ref());
        ctx.mode = &app.mode;
        ctx.glyphs = app.glyph_mode;
        ctx.minimap = app.ui_state.minimap_visible();
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    terminal.backend().buffer().clone()
}

#[test]
fn lineage_title_shows_lens_suffix_per_lens_and_nothing_when_off() {
    // End-to-end: the lens suffix the lineage pane title builds from state.lens()
    // actually reaches the rendered buffer, once per lens, and the default Off
    // title carries no suffix. Wide enough (120) that the suffix isn't clipped.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");

    // Default Off: the title shows the model but no `[lens:...]` tag.
    let off = buffer_to_string(&render_app(&app, 120, 24));
    assert!(off.contains("Lineage:"), "lineage title present");
    assert!(!off.contains("[lens:"), "Off lens adds no suffix");

    // One suffix per lens across the cycle.
    for expected in ["coverage", "heat", "layer", "violation"] {
        apply_action(&mut app, Action::CycleLens);
        let text = buffer_to_string(&render_app(&app, 120, 24));
        assert!(
            text.contains(&format!("[lens:{expected}]")),
            "lineage title shows the {expected} lens suffix"
        );
    }
    // Cycling once more returns to Off → suffix gone again.
    apply_action(&mut app, Action::CycleLens);
    let back_off = buffer_to_string(&render_app(&app, 120, 24));
    assert!(
        !back_off.contains("[lens:"),
        "cycle back to Off drops the suffix"
    );
}

/// Render an `App`'s lineage title row (row 0) for a chosen width, feeding the
/// breadcrumb + view label exactly as the event loop does. Returns the title-row
/// text only (the status bar echoes the name, so a whole-buffer scan over-counts).
fn lineage_title_row(app: &dbtl::app::App, width: u16, height: u16) -> String {
    use dbtl::layout::layout_mode;
    use dbtl::ui::{RenderCtx, StatusSegments};

    let view_label = app.lineage_view_label();
    // The loop hands the FULL trail; the draw seam is the single width authority.
    let breadcrumb = app.breadcrumb(usize::MAX);
    let sg = app.lineage_subgraph();
    let mut lay = layout_mode(&sg, app.glyph_mode);
    lay.apply_node_styles(&app.lineage_styles(&lay));

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, Some(&lay));
        ctx.mode = &app.mode;
        ctx.glyphs = app.glyph_mode;
        ctx.lineage_label = Some(view_label.as_str());
        ctx.breadcrumb = breadcrumb.as_deref();
        ctx.segments = StatusSegments::default();
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    row_text(&terminal.backend().buffer().clone(), 0)
}

#[test]
fn lineage_title_omits_body_when_breadcrumb_present_but_keeps_view_and_lens() {
    // The breadcrumb already ENDS at the current root, so the `Lineage: {root}`
    // body is omitted (no duplicate root) — but the `[{v}]{lens}` suffixes, the
    // only textual cue for the active view + lens, are KEPT. Drive a real re-root +
    // a non-default view + an active lens, then scan the TITLE ROW.
    use dbtl::app::{apply_action, App};
    use dbtl::ui::LineageLens;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    app.jump_to("model.jaffle_finance.wt_delivery_base_metrics");
    apply_action(&mut app, Action::ToggleUpstream); // non-default view label
    apply_action(&mut app, Action::CycleLens); // Off → Coverage
    assert_eq!(app.ui_state.lens(), LineageLens::Coverage);
    apply_action(&mut app, Action::ToggleListPane); // lineage = full width

    let title = lineage_title_row(&app, 160, 24);

    // No `Lineage:` body when a breadcrumb is present.
    assert!(
        !title.contains("Lineage:"),
        "breadcrumb present → the `Lineage:` body is omitted, got {title:?}"
    );
    // The breadcrumb leads with the PREVIOUS root and ends at the CURRENT root.
    let bc = title
        .find("fct_subscription_process")
        .expect("previous root in breadcrumb");
    // The current root (wt_delivery_base_metrics) appears exactly ONCE in the row.
    assert_eq!(
        title.matches("wt_delivery_base_metrics").count(),
        1,
        "the current root appears exactly once in the title row, got {title:?}"
    );
    // The lens suffix survives and follows the breadcrumb.
    let lens = title.find("[lens:coverage]").expect("lens suffix present");
    assert!(bc < lens, "lens suffix follows the breadcrumb");
    // The non-default view suffix survives too (label derived from the app, not
    // hardcoded — it is a glyph-mode-dependent arrow).
    let vlabel = format!("[{}]", app.lineage_view_label());
    assert!(
        title.contains(&vlabel),
        "view label {vlabel:?} kept, got {title:?}"
    );
}

#[test]
fn lineage_title_keeps_violation_lens_suffix_for_long_chain_at_80_cols() {
    // The whole point of FIX 1(b): even a LONG re-root chain at a NARROW 80 cols
    // must not evict the lens suffix — the draw seam reserves the suffix width and
    // truncates the crumb from the left (whole entries, then `..`, then to empty).
    // Uses the WORST case: `[lens:violation]` is the LONGEST suffix, and (unlike
    // Coverage) the title is the ONLY textual cue for the violation/heat/layer
    // lenses (they quote no status-bar segment), so this is the must-survive case.
    use dbtl::app::{apply_action, App};
    use dbtl::ui::LineageLens;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    // A multi-hop chain ending on the long-named snapshot, so the full trail is
    // far wider than 80 cols.
    app.jump_to("model.jaffle_finance.wt_delivery_base_metrics");
    app.jump_to("model.jaffle_finance.fct_delivery_monthly_snapshot");
    // Off -> Coverage -> DegreeHeat -> Layer -> LayerViolation (the longest suffix).
    for _ in 0..4 {
        apply_action(&mut app, Action::CycleLens);
    }
    assert_eq!(app.ui_state.lens(), LineageLens::LayerViolation);
    apply_action(&mut app, Action::ToggleListPane); // lineage = full 80-col width

    let title = lineage_title_row(&app, 80, 24);
    assert!(
        title.contains("[lens:violation]"),
        "the longest lens suffix survives a long chain at 80 cols, got {title:?}"
    );
    // The current root still shows (the strict fit keeps the last entry while it
    // can) and appears exactly ONCE (no `Lineage:` body duplicates it).
    assert_eq!(
        title.matches("fct_delivery_monthly_snapshot").count(),
        1,
        "the current root appears exactly once, got {title:?}"
    );
    // The `..` left-truncation marker is present (the chain overflowed).
    assert!(
        title.contains(".."),
        "a too-long chain is left-truncated with `..`, got {title:?}"
    );
}

#[test]
fn lineage_title_no_breadcrumb_is_byte_identical_to_the_default() {
    // The no-crumb path must be unchanged: WITHOUT a re-root (empty history), the
    // title is exactly the pre-breadcrumb default. Compose with a non-default view
    // + an active lens, and assert the exact title substring.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    apply_action(&mut app, Action::ToggleUpstream); // non-default view label
    apply_action(&mut app, Action::CycleLens); // [lens:coverage]
    apply_action(&mut app, Action::ToggleListPane);
    assert!(
        app.breadcrumb(usize::MAX).is_none(),
        "no re-root → no breadcrumb"
    );

    // The exact pre-breadcrumb default shape: ` Lineage: {name} [{v}]{lens} `, where
    // {v} is the (glyph-mode-dependent) view label and {lens} carries its own
    // leading space. Derived from the app so it tracks the real label glyphs.
    let v = app.lineage_view_label();
    let expected = format!(" Lineage: fct_subscription_process [{v}] [lens:coverage] ");
    let title = lineage_title_row(&app, 160, 24);
    assert!(
        title.contains(&expected),
        "no-crumb title is byte-identical to the default shape:\n  expected substring {expected:?}\n  got {title:?}"
    );
}

#[test]
fn minimap_default_off_keeps_lineage_invariants_intact() {
    // The default-OFF guard: WITHOUT toggling the minimap, fct at 80x24 must
    // still render exactly ONE emphasis region (the selected node's name) in the
    // lineage interior, unclipped — i.e. the lineage_render.rs invariants hold
    // because nothing was drawn over the interior.
    use dbtl::app::App;
    use dbtl::ui::{pane_interior, pane_rects};
    use ratatui::layout::Rect;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    assert!(!app.ui_state.minimap_visible(), "minimap default OFF");

    // Anchor exactly as the loop does so the selected box centers/shows.
    let interior = pane_interior(pane_rects(Rect::new(0, 0, 80, 24), true).lineage);
    {
        use dbtl::layout::layout_mode;
        let sg = app.lineage_subgraph();
        let lay = layout_mode(&sg, app.glyph_mode);
        app.ui_state.anchor_lineage(
            lay.selected_rect,
            &lay.grid,
            interior.width as usize,
            interior.height as usize,
        );
    }
    let buffer = render_app(&app, 80, 24);

    // Count maximal REVERSED runs INSIDE the lineage interior (title/status echo
    // the name but live outside the interior rect).
    let regions = reversed_runs_in(&buffer, interior);
    assert_eq!(
        regions.len(),
        1,
        "minimap OFF: exactly one emphasis region, got {regions:?}"
    );
    assert_eq!(
        regions[0], "fct_subscription_process",
        "minimap OFF: the full selected name renders unclipped"
    );
}

#[test]
fn minimap_on_draws_node_and_viewport_glyphs_in_the_inset() {
    // Toggle the minimap ON via the real apply_action path, render the fct
    // fan-out at 80x24 (the diagram overflows → the minimap guard fires), and
    // assert the occupancy `#` and viewport `+` glyphs appear in the top-right
    // inset of the lineage interior (and ONLY scan that inset, since the minimap
    // legitimately overwrites a corner of the diagram when ON).
    use dbtl::app::{apply_action, App};
    use dbtl::ui::{pane_interior, pane_rects};
    use dbtl::Action;
    use ratatui::layout::Rect;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    apply_action(&mut app, Action::ToggleMinimap);
    assert!(app.ui_state.minimap_visible(), "M toggled the minimap on");

    let interior = pane_interior(pane_rects(Rect::new(0, 0, 80, 24), true).lineage);
    {
        use dbtl::layout::layout_mode;
        let sg = app.lineage_subgraph();
        let lay = layout_mode(&sg, app.glyph_mode);
        // Precondition: the fct grid genuinely overflows this interior (so the
        // minimap guard `edges.any()` fires) AND the interior is big enough.
        assert!(
            lay.grid.width() > interior.width as usize
                || lay.grid.height() > interior.height as usize,
            "precondition: fct grid overflows the 80x24 lineage interior"
        );
        assert!(
            interior.width >= 18 && interior.height >= 8,
            "precondition: interior big enough for the 16x6 inset (+2 margin)"
        );
        app.ui_state.anchor_lineage(
            lay.selected_rect,
            &lay.grid,
            interior.width as usize,
            interior.height as usize,
        );
    }
    let buffer = render_app(&app, 80, 24);

    // The inset rect: top-right of the interior, MM_W=16 x MM_H=6.
    let ix0 = interior.x + interior.width - 16;
    let ix1 = interior.x + interior.width; // exclusive
    let iy0 = interior.y;
    let iy1 = interior.y + 6; // exclusive
    let mut has_node = false;
    let mut has_view = false;
    for y in iy0..iy1 {
        for x in ix0..ix1 {
            if let Some(c) = buffer.cell((x, y)) {
                match c.symbol() {
                    "#" => has_node = true,
                    "+" => has_view = true,
                    _ => {}
                }
            }
        }
    }
    assert!(
        has_node,
        "minimap ON: a '#' node-occupancy glyph appears in the inset"
    );
    assert!(
        has_view,
        "minimap ON: a '+' viewport glyph appears in the inset"
    );
}

/// Maximal contiguous horizontal runs of REVERSED-styled cells WITHIN `interior`,
/// as spelled strings (the buffer-side analog of `CharGrid::emphasis_regions`,
/// restricted to the lineage interior so title/status echoes can't leak in).
fn reversed_runs_in(buffer: &Buffer, interior: ratatui::layout::Rect) -> Vec<String> {
    let mut regions = Vec::new();
    for y in interior.y..interior.y + interior.height {
        let mut x = interior.x;
        while x < interior.x + interior.width {
            let reversed = buffer
                .cell((x, y))
                .map(|c| c.modifier.contains(Modifier::REVERSED))
                .unwrap_or(false);
            if reversed {
                let mut s = String::new();
                while x < interior.x + interior.width
                    && buffer
                        .cell((x, y))
                        .map(|c| c.modifier.contains(Modifier::REVERSED))
                        .unwrap_or(false)
                {
                    s.push_str(buffer.cell((x, y)).unwrap().symbol());
                    x += 1;
                }
                regions.push(s);
            } else {
                x += 1;
            }
        }
    }
    regions
}
// ============================================================================
// transient toast (copy / bookmark / export feedback)
// ============================================================================

/// The toast floats at the TOP-RIGHT: a 3-row bordered box inset 2 cols from
/// the right edge and 1 row from the top, its text on the middle row. Absent
/// `ctx.toast` (the default) the buffer is byte-identical to a plain render —
/// every other test in this file implicitly asserts that.
#[test]
fn toast_renders_top_right_with_its_text() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.toast = Some("Copied unique_id");
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();

    // Geometry: text 16 wide -> box 20 wide; x = 100 - 20 - 2 = 78, y = 1..4.
    assert_eq!(
        buffer.cell((78, 1)).unwrap().symbol(),
        "╭",
        "toast top-left"
    );
    assert_eq!(
        buffer.cell((97, 1)).unwrap().symbol(),
        "╮",
        "toast top-right"
    );
    assert_eq!(
        buffer.cell((78, 3)).unwrap().symbol(),
        "╰",
        "toast bottom-left"
    );
    let row = row_text(&buffer, 2);
    let pos = row
        .find("Copied unique_id")
        .expect("toast text on middle row");
    assert!(
        pos >= 70,
        "toast text sits in the top-RIGHT corner, got col {pos}"
    );

    // Without a toast the text never appears anywhere.
    let plain = render(100, 30);
    assert!(
        !buffer_to_string(&plain).contains("Copied unique_id"),
        "no toast field -> no toast text"
    );
}

#[test]
fn toast_truncates_to_the_screen_with_ascii_marker() {
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    let long = "Removed bookmark: some_extremely_long_model_name";
    ctx.toast = Some(long);
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    let row = row_text(&buffer, 2);
    // Width budget at 40 cols: 40 - 4 (frame) - 2 (inset) = 34 -> 32 chars + "..".
    assert!(
        row.contains("Removed bookmark: some_extremely.."),
        "truncated to the display budget with a '..' marker, got {row:?}"
    );
    assert!(
        !buffer_to_string(&buffer).contains(long),
        "the overlong text never renders in full"
    );
}

/// The toast layers ABOVE modal overlays (drawn last), so a yank fired from
/// inside the SQL/help modal is still acknowledged.
#[test]
fn toast_floats_above_modal_overlays() {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    let mode = dbtl::Mode::Help { scroll: 0 };
    ctx.mode = &mode;
    ctx.toast = Some("Copied raw SQL");
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    assert!(
        row_text(&buffer, 2).contains("Copied raw SQL"),
        "toast text wins over the modal's cells on its row"
    );
}

/// On a terminal too short to float the box above the status bar (the 3-row
/// toast sits at y=1, so height 4 would land its bottom border ON the status
/// row), the toast is skipped entirely — the protected status core survives.
#[test]
fn toast_skips_tiny_terminals_and_never_covers_the_status_bar() {
    let backend = TestBackend::new(60, 4);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let list = fixture_list();
    let state = UiState::new(list.len());
    let mut ctx = RenderCtx::new(&list, &state, None);
    ctx.toast = Some("Copied unique_id");
    terminal
        .draw(|frame| draw(frame, &ctx))
        .expect("draw must not panic");
    let buffer = terminal.backend().buffer().clone();
    assert!(
        !buffer_to_string(&buffer).contains("Copied"),
        "no toast on a 4-row terminal"
    );
    assert!(
        row_text(&buffer, 3).contains("[j/k]"),
        "the status bar's protected core is untouched"
    );
}
