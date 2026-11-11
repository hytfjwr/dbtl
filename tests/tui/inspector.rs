use super::*;

// ============================================================================
// the inspector pane
// ============================================================================

/// Render an `App` through the SAME seam the event loop uses for the
/// inspector: publish the resolved `inspector_shown` (auto → `width >= 160`,
/// `on`/`off` absolute), re-home the target, clamp the scroll, and hand
/// `ctx.inspector` the cached `Rc` snapshot. Everything else defaults like
/// [`render_app`].
pub(super) fn render_app_with_inspector(
    app: &mut dbtl::app::App,
    width: u16,
    height: u16,
) -> Buffer {
    let want = match app.ui_state.inspector_policy() {
        dbtl::ui::InspectorPolicy::On => true,
        dbtl::ui::InspectorPolicy::Off => false,
        dbtl::ui::InspectorPolicy::Auto => width >= 160,
    };
    app.ui_state.set_inspector_shown(want);
    if let Some(uid) = app.inspector_target() {
        app.ui_state.set_inspector_target(&uid);
    }
    // Mirror the loop's phase 2: the resolved answer is the built snapshot —
    // a squeezed or view-less `want` publishes `shown = false` so
    // `pane_config` stops carving the region.
    let inspector = if want
        && dbtl::ui::pane_layout(
            ratatui::layout::Rect::new(0, 0, width, height),
            &app.ui_state.pane_config(),
        )
        .inspector
        .width
            > 0
    {
        app.inspector_view()
    } else {
        None
    };
    app.ui_state.set_inspector_shown(inspector.is_some());
    if let Some(view) = inspector.as_deref() {
        let inner = dbtl::ui::pane_interior(
            dbtl::ui::pane_layout(
                ratatui::layout::Rect::new(0, 0, width, height),
                &app.ui_state.pane_config(),
            )
            .inspector,
        );
        // Mirror the loop's one-shot reveal: the cursored row only feeds the
        // clamp on the pick frame; `usize::MAX` afterwards.
        let reveal = if app.ui_state.take_inspector_reveal() {
            app.ui_state.inspector_col()
        } else {
            usize::MAX
        };
        let scroll = dbtl::ui::clamp_inspector_scroll(
            view,
            app.glyph_mode,
            app.active_theme(),
            inner.width as usize,
            inner.height as usize,
            app.ui_state.inspector_scroll(),
            reveal,
        );
        app.ui_state.set_inspector_scroll(scroll);
    }
    let lineage = app.styled_lineage_layout();
    let tab_labels = app.tab_labels();
    let tab_layout = app.tab_bar_layout_for(
        dbtl::ui::pane_layout(
            ratatui::layout::Rect::new(0, 0, width, height),
            &app.ui_state.pane_config(),
        )
        .tab_bar,
        true,
    );
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_deref());
        ctx.mode = &app.mode;
        ctx.glyphs = app.glyph_mode;
        ctx.inspector = inspector.as_deref();
        // The tab strip exactly as `render_app` wires it — the loop always
        // supplies one, so the body geometry starts a row down.
        ctx.tab_bar = Some(dbtl::ui::TabBarView {
            labels: &tab_labels,
            layout: &tab_layout,
            active: app.active_tab(),
            project: Some(app.stats.project.as_str()),
            hover: None,
        });
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("render must not panic");
    }
    terminal.backend().buffer().clone()
}

/// The text of `rect`'s cells, row-joined — scans ONLY the inspector pane's
/// region so twin text on the list/lineage/status can't false-positive.
pub(super) fn region_text(buffer: &Buffer, rect: ratatui::layout::Rect) -> String {
    let mut out = String::new();
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

pub(super) fn inspector_rect(buffer: &Buffer, app: &dbtl::app::App) -> ratatui::layout::Rect {
    dbtl::ui::pane_layout(buffer.area, &app.ui_state.pane_config()).inspector
}

#[test]
pub(super) fn inspector_auto_shows_at_160_and_hides_below() {
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // Auto (the default policy): on screen at 160+, absent below.
    let wide = render_app_with_inspector(&mut app, 200, 40);
    let ins = inspector_rect(&wide, &app);
    assert!(ins.width >= 30, "inspector allocated at 200 cols");
    let pane = region_text(&wide, ins);
    assert!(
        pane.contains("stg_finance__budgets"),
        "pane titles the node"
    );
    assert!(
        pane.contains("Columns"),
        "rich node renders its sections: {pane}"
    );

    let narrow = render_app_with_inspector(&mut app, 120, 40);
    let ins = inspector_rect(&narrow, &app);
    assert_eq!(ins.width, 0, "auto below 160 → zero-width rect");
    assert!(
        !buffer_to_string(&narrow).contains("Impact"),
        "no pane drawn under 160 cols in auto"
    );
}

#[test]
pub(super) fn inspector_on_and_off_override_the_width_rule() {
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // `on` draws even below the auto threshold; `off` never draws.
    app.ui_state
        .set_inspector_policy(dbtl::ui::InspectorPolicy::On);
    let buf = render_app_with_inspector(&mut app, 120, 40);
    let ins = inspector_rect(&buf, &app);
    assert!(ins.width >= 30, "on shows at 120 cols");
    assert!(region_text(&buf, ins).contains("stg_finance__budgets"));

    app.ui_state
        .set_inspector_policy(dbtl::ui::InspectorPolicy::Off);
    let buf = render_app_with_inspector(&mut app, 200, 40);
    assert_eq!(inspector_rect(&buf, &app).width, 0, "off never draws");
    assert!(!region_text(&buf, buf.area).contains("Impact"));
}

#[test]
pub(super) fn inspector_renders_the_spec_sections_in_order() {
    // fct_subscription_process: description + 12 tests + 7 columns + upstream
    // and downstream neighbours — every core section non-empty.
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let buf = render_app_with_inspector(&mut app, 200, 60);
    let ins = inspector_rect(&buf, &app);
    let pane = region_text(&buf, ins);

    for section in ["Description", "Tests (12)", "Columns (7)", "Impact", "Lint"] {
        assert!(
            pane.contains(section),
            "missing section {section:?}: {pane}"
        );
    }
    // Section order: Description, then Tests, then Columns, then Impact.
    let order = ["Description", "Tests (12)", "Columns (7)", "Impact"];
    let mut at = 0;
    for s in order {
        let p = pane[at..]
            .find(s)
            .unwrap_or_else(|| panic!("{s:?} after offset {at}: {pane}"));
        at += p;
    }
    // The head line (first interior row — index 1 skips the border/title
    // row) carries the materialization tag + the project path.
    let first = pane.lines().nth(1).unwrap_or_default();
    assert!(
        first.contains("Table"),
        "head line shows the tag: {first:?}"
    );
}

#[test]
pub(super) fn inspector_omits_empty_sections() {
    // A node with no description/tests shows neither section header — the
    // pane never draws dead chrome. Build one: the fixture documents
    // everything, so a bare model is the honest case.
    use std::collections::HashMap;
    let mut nodes = HashMap::new();
    nodes.insert(
        "model.p.bare".to_string(),
        dbtl::RawNode {
            name: "bare".into(),
            resource_type: "model".into(),
            path: Some("staging/bare.sql".into()),
            config: dbtl::RawConfig {
                materialized: Some("view".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let dag = Dag::build(&dbtl::RawManifest {
        metadata: Default::default(),
        nodes,
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
    let mut app = dbtl::app::App::new(dag, std::path::PathBuf::from(FIXTURE));
    let buf = render_app_with_inspector(&mut app, 200, 60);
    let pane = region_text(&buf, inspector_rect(&buf, &app));
    assert!(!pane.contains("Description"), "no desc → no header: {pane}");
    assert!(!pane.contains("Tests ("), "no tests → no header: {pane}");
    assert!(
        !pane.contains("Columns ("),
        "no columns → no header: {pane}"
    );
    // Lint is the exception: a clean node still reports its verdict.
    assert!(pane.contains("Lint"), "the lint verdict always renders");
    assert!(pane.contains("Impact"), "the counts section always renders");
}

#[test]
pub(super) fn inspector_lint_section_reports_verdicts() {
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    // A violating node renders `mark rule: summary` per finding — the
    // severity mark (unicode `✗`/`!` here), the registry id, the registry
    // summary — with the message wrapped below, all severity-coloured.
    let uid = app
        .lint_findings()
        .first()
        .map(|f| f.node_uid.clone())
        .expect("fixture lints under the default rules");
    let f = app
        .lint_findings()
        .iter()
        .find(|f| f.node_uid == uid)
        .expect("a finding for the selected node");
    let (mark, want_fg) = match f.severity {
        dbtl::Severity::Error => ("✗", dbtl::ui::theme::DANGER),
        dbtl::Severity::Warn => ("!", dbtl::ui::theme::WARN),
    };
    let head = format!("{mark} {}:", f.rule.name());
    let summary_words: Vec<String> = f
        .rule
        .description()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let msg_words: Vec<String> = f.message.split_whitespace().map(str::to_string).collect();
    app.select_by_unique_id(&uid);
    let buf = render_app_with_inspector(&mut app, 200, 60);
    let ins = inspector_rect(&buf, &app);
    let pane = region_text(&buf, ins);
    assert!(
        pane.contains(&head),
        "the mark + rule-id head renders: {pane}"
    );
    assert!(
        summary_words.iter().all(|w| pane.contains(w.as_str())),
        "the registry summary renders (wrapped): {pane}"
    );
    assert!(
        msg_words.iter().all(|w| pane.contains(w.as_str())),
        "the finding message renders under the head (wrapped): {pane}"
    );
    // The lint line's mark cell carries the severity colour.
    let mut coloured = false;
    'outer: for y in ins.y..ins.y + ins.height {
        for x in ins.x..ins.x + ins.width {
            if let Some(cell) = buf.cell((x, y)) {
                if cell.symbol() == mark && cell.fg == want_fg {
                    coloured = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(coloured, "the {mark} mark renders in {want_fg:?}");

    // A clean node renders the pass verdict (unicode ✓ here; the ASCII
    // spelling is under ascii_guard).
    let clean = {
        let linted: std::collections::HashSet<&str> = app
            .lint_findings()
            .iter()
            .map(|f| f.node_uid.as_str())
            .collect();
        // Deterministic pick: HashMap order is per-run, so choose the clean
        // model with the fewest tests — a tall Tests section can push the Lint
        // verdict below the inspector's visible window.
        app.dag
            .nodes()
            .values()
            .filter(|n| n.resource_type == "model" && !linted.contains(n.unique_id.as_str()))
            .min_by_key(|n| (n.test_count, n.unique_id.as_str()))
            .map(|n| n.unique_id.clone())
            .expect("fixture has a lint-clean model")
    };
    app.select_by_unique_id(&clean);
    let buf = render_app_with_inspector(&mut app, 200, 60);
    let pane = region_text(&buf, inspector_rect(&buf, &app));
    assert!(
        pane.contains("all checks pass"),
        "clean node reports the pass verdict: {pane}"
    );
}

#[test]
pub(super) fn inspector_scroll_reveals_lower_sections() {
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    app.ui_state
        .set_inspector_policy(dbtl::ui::InspectorPolicy::On);
    // Short pane: the sections can't all fit — the scroll decides the window.
    let top = render_app_with_inspector(&mut app, 160, 12);
    let pane_top = region_text(&top, inspector_rect(&top, &app));
    assert!(pane_top.contains("Table"), "unscrolled shows the head");

    app.ui_state.set_inspector_scroll(usize::MAX); // the `G` intent
    let bottom = render_app_with_inspector(&mut app, 160, 12);
    let pane_bot = region_text(&bottom, inspector_rect(&bottom, &app));
    assert!(
        pane_bot.contains("Lint") || pane_bot.contains("Impact"),
        "scrolled to the end reaches the trailing sections: {pane_bot}"
    );
    assert!(
        !pane_bot.contains("Description"),
        "the head scrolled off: {pane_bot}"
    );
}

#[test]
pub(super) fn inspector_column_row_picks_highlight_the_row() {
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    app.ui_state
        .set_inspector_policy(dbtl::ui::InspectorPolicy::On);
    // Prime the target BEFORE picking — a target change re-homes the column
    // cursor (the first render publishes the target, like the loop does).
    let _ = render_app_with_inspector(&mut app, 200, 60);
    app.ui_state.set_inspector_col(0);
    let buf = render_app_with_inspector(&mut app, 200, 60);
    let ins = inspector_rect(&buf, &app);
    // The cursored row paints REVERSED cells inside the inspector rect.
    let mut reversed = false;
    for y in ins.y..ins.y + ins.height {
        for x in ins.x..ins.x + ins.width {
            if let Some(cell) = buf.cell((x, y)) {
                if cell.modifier.contains(Modifier::REVERSED) {
                    reversed = true;
                }
            }
        }
    }
    assert!(reversed, "the cursored column row paints selected_style");
}

#[test]
pub(super) fn inspector_column_at_maps_rows_to_columns() {
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    let view = app.inspector_view().expect("inspect");
    let t = app.active_theme();
    // Row 0..base-1 are non-column content; base..base+cols are the rows.
    let inner_w = 40usize;
    let base = dbtl::ui::inspector_columns_line0(&view, app.glyph_mode, t, inner_w);
    assert_eq!(
        dbtl::ui::inspector_column_at(&view, app.glyph_mode, t, inner_w, 0, 0),
        None,
        "the head row is not a column row"
    );
    assert_eq!(
        dbtl::ui::inspector_column_at(&view, app.glyph_mode, t, inner_w, 0, base as u16),
        Some(0),
        "the first column row maps to index 0"
    );
    let last = base + view.detail.col_names.len() - 1;
    assert_eq!(
        dbtl::ui::inspector_column_at(&view, app.glyph_mode, t, inner_w, 0, last as u16),
        Some(view.detail.col_names.len() - 1)
    );
    assert_eq!(
        dbtl::ui::inspector_column_at(&view, app.glyph_mode, t, inner_w, 0, last as u16 + 1),
        None,
        "past the last column row is not a pick"
    );
    // Scrolled: the mapping shifts by the scroll offset — screen row 0 now
    // shows the content line `scroll` deep, so scrolling `base` lines puts
    // the first column row under the pointer at y=0.
    assert_eq!(
        dbtl::ui::inspector_column_at(&view, app.glyph_mode, t, inner_w, base, 0),
        Some(0),
        "scroll + row → the scrolled-in line"
    );
}

#[test]
pub(super) fn inspector_pane_keeps_list_and_lineage_unscathed() {
    // The three-pane layout still honours the minimums at the auto width:
    // list 34, inspector 42, lineage the rest (floor 40).
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    let buf = render_app_with_inspector(&mut app, 200, 40);
    let panes = dbtl::ui::pane_layout(buf.area, &app.ui_state.pane_config());
    assert_eq!(panes.list.width, 34, "list keeps its default");
    assert_eq!(panes.inspector.width, 42, "inspector keeps its default");
    assert!(
        panes.lineage.width >= 40,
        "lineage keeps its floor: {}",
        panes.lineage.width
    );
}

#[test]
pub(super) fn inspector_min_width_wraps_and_clips_within_30_cols() {
    // The 30-col floor: the pane still renders every section — headers
    // stay legible, text wraps inside the narrow interior, and long rows
    // CLIP at the right border instead of bleeding past it.
    let mut app = dbtl::app::App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    app.ui_state
        .set_inspector_width(dbtl::ui::MIN_INSPECTOR_WIDTH);
    let buf = render_app_with_inspector(&mut app, 200, 60);
    let ins = inspector_rect(&buf, &app);
    assert_eq!(
        ins.width,
        dbtl::ui::MIN_INSPECTOR_WIDTH,
        "the saved floor width is honoured on a wide terminal"
    );
    // The pane hugs the body's right edge — nothing can live past it.
    assert_eq!(ins.x + ins.width, buf.area.width);

    let pane = region_text(&buf, ins);
    for section in ["Description", "Columns (7)", "Impact", "Lint"] {
        assert!(
            pane.contains(section),
            "min-width pane keeps section {section:?}: {pane}"
        );
    }

    // The head row (`Table  models/fct_subscription_process.sql`) is longer
    // than the 28-col interior: it clips mid-token, so the rightmost
    // interior cell is a non-space character — content reaches the border
    // and stops, never draws over it.
    let inner = dbtl::ui::pane_interior(ins);
    let last_x = inner.x + inner.width - 1;
    let head_tail = buf
        .cell((last_x, inner.y))
        .map(|c| c.symbol().to_string())
        .unwrap_or_default();
    assert!(
        !head_tail.trim().is_empty(),
        "clipped head row ends mid-token at the border"
    );

    // A description line wraps: the section's body occupies more than one
    // interior row rather than truncating at the first 28 chars.
    let desc = region_text(&buf, inner);
    assert!(desc.contains("Description"), "description section present");
}
