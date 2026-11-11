use super::*;

// ============================================================================
// Lineage minimap (stretch): default OFF protects every lineage render; ON it
// draws an occupancy inset in the top-right of the lineage interior.
// ============================================================================

/// Render an `App` (with its real lineage layout for the current selection) into
/// a `width x height` buffer, mirroring the event loop's lineage setup. The
/// minimap is read straight from `app.ui_state.minimap_visible()`.
pub(super) fn render_app(app: &dbtl::app::App, width: u16, height: u16) -> Buffer {
    // The SAME memoized producer the event loop draws from, so the test
    // exercises the production pipeline (cache key -> styles -> layout).
    let lineage = app.styled_lineage_layout();
    // The column-focus surfaces exactly as the event loop wires them (None
    // outside a trace — both simply omit then).
    let column_s = app.column_status();
    let col_suffix = app.column_focus_label();
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        // The palette rows + empty-list reason the EVENT LOOP prepares (the
        // same App-side helpers; RenderCtx itself stays Dag-free).
        let palette_rows = match &app.mode {
            dbtl::Mode::Palette(p) => Some(app.palette_rows(&p.query)),
            _ => None,
        };
        let empty_reason = app.list_empty_reason();
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, lineage.as_deref());
        ctx.mode = &app.mode;
        ctx.palette_rows = palette_rows.as_deref();
        ctx.empty_reason = empty_reason.as_deref();
        ctx.glyphs = app.glyph_mode;
        ctx.minimap = app.ui_state.minimap_visible();
        ctx.col_suffix = col_suffix.as_deref();
        ctx.segments = dbtl::ui::StatusSegments {
            column: column_s.as_deref(),
            ..Default::default()
        };
        // The fold markers exactly as the event loop wires them (`list_hover`
        // stays None — it is pointer-driven, not app state).
        ctx.folded_groups = Some(&app.folded_groups);
        // The tab strip exactly as the event loop wires it: the app's own
        // labels/scroll resolve the strip rect `pane_layout` carved out.
        let tab_labels = app.tab_labels();
        let panes = dbtl::ui::pane_layout(
            ratatui::layout::Rect::new(0, 0, width, height),
            &app.ui_state.pane_config(),
        );
        let tab_layout = app.tab_bar_layout_for(panes.tab_bar, true);
        ctx.tab_bar = Some(dbtl::ui::TabBarView {
            labels: &tab_labels,
            layout: &tab_layout,
            active: app.active_tab(),
            project: Some(app.stats.project.as_str()),
            hover: None,
        });
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    terminal.backend().buffer().clone()
}

#[test]
pub(super) fn tab_bar_renders_cells_plus_and_project_on_row_zero() {
    // the strip: row 0 carries `{n} {name}` cells (numbered to 9,
    // names middle-truncated at 24 with `..`), the ` + ` cell, and the
    // right-pinned project name — pane content starts a row lower.
    use dbtl::app::App;
    use dbtl::GlyphMode;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    app.open_node_as_tab("model.jaffle_finance.rpt_delivery_base_metrics");

    // `⌂` pins at the strip's left edge like `+` pins at the right —
    // model tabs window between them. At 80 cols the window only fits one
    // model tab, so the active one shows and `‹` marks the scrolled-away
    // side; at 160 both fit with no arrows.
    let buf = render_app(&app, 80, 24);
    let row0 = row_text(&buf, 0);
    assert!(
        row0.starts_with(" ⌂") || row0.contains("⌂"),
        "the home cell pins at the strip's left edge: {row0}"
    );
    assert!(
        row0.contains("‹"),
        "hidden model tabs mark the left edge: {row0}"
    );
    assert!(
        row0.contains("2 rpt_deliver..ase_metrics"),
        "the active tab stays in the window: {row0}"
    );
    assert!(row0.contains("+"), "the new-tab cell: {row0}");
    assert!(
        row0.trim_end().ends_with("jaffle_finance"),
        "the project name pins to the strip's right edge: {row0}"
    );

    let buf = render_app(&app, 160, 44);
    let row0 = row_text(&buf, 0);
    assert!(row0.contains("⌂"), "160: the pinned home cell: {row0}");
    assert!(
        row0.contains("1 fct_subscription_process"),
        "160: tab 1 cell on the strip: {row0}"
    );
    assert!(
        row0.contains("2 rpt_deliver..ase_metrics"),
        "160: tab 2's name middle-truncates at 24: {row0}"
    );
    assert!(row0.contains("+"), "160: the new-tab cell: {row0}");
    assert!(
        row0.trim_end().ends_with("jaffle_finance"),
        "160: the project name pins to the strip's right edge: {row0}"
    );
    // The strip claims row 0 — the panes start at row 1.
    assert!(
        row_text(&buf, 1).contains("Lineage:"),
        "the lineage title moved off row 0"
    );
    assert!(!row0.contains("Lineage:"));

    // ASCII mode swaps the home/close/scroll chrome for `~`/`x`/`<`/`>` —
    // the cell layout is geometrically identical (the guard's identity
    // invariant).
    app.glyph_mode = GlyphMode::Ascii;
    let buf = render_app(&app, 80, 24);
    let row0 = row_text(&buf, 0);
    assert!(
        row0.contains("~ < 2 rpt_deliver..ase_metrics x"),
        "ASCII mode draws `~`+`<`+`x`, same cells: {row0}"
    );
}

#[test]
pub(super) fn lineage_title_shows_lens_suffix_per_lens_and_nothing_when_off() {
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

    // One suffix per lens across the cycle. The fixture has lint findings so
    // the Lint lens lands between LayerViolation and PrimaryKey; PrimaryKey
    // needs only `Dag::tests` so it's never skipped (no baseline, run_results,
    // or column focus → Diff, the run lenses, and ColumnImpact are skipped).
    for expected in ["coverage", "heat", "layer", "violation", "lint", "pk"] {
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
pub(super) fn lineage_title_row(app: &dbtl::app::App, width: u16, height: u16) -> String {
    use dbtl::ui::{RenderCtx, StatusSegments};

    let view_label = app.lineage_view_label();
    // The loop hands the FULL trail; the draw seam is the single width authority.
    let breadcrumb = app.breadcrumb(usize::MAX);
    // `None` outside the whole-graph overview, mirroring the loop's wiring.
    let overview_title = app.overview_title();
    // The production producer (memoized), exactly as the event loop draws.
    let lay = app.styled_lineage_layout().expect("non-empty lineage");

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    {
        let mut ctx = RenderCtx::new(app.active_list(), &app.ui_state, Some(&lay));
        ctx.mode = &app.mode;
        ctx.glyphs = app.glyph_mode;
        ctx.lineage_label = Some(view_label.as_str());
        ctx.breadcrumb = breadcrumb.as_deref();
        ctx.overview_title = overview_title.as_deref();
        ctx.segments = StatusSegments::default();
        terminal.draw(|frame| draw(frame, &ctx)).expect("render");
    }
    row_text(&terminal.backend().buffer().clone(), 0)
}

#[test]
pub(super) fn lineage_title_omits_body_when_breadcrumb_present_but_keeps_view_and_lens() {
    // The breadcrumb already ENDS at the current root, so the `Lineage: {root}`
    // body is omitted (no duplicate root) — but the `[{v}]{lens}` suffixes, the
    // only textual cue for the active view + lens, are KEPT. Drive a real re-root +
    // a non-default view + an active lens, then scan the TITLE ROW.
    use dbtl::app::{apply_action, App};
    use dbtl::ui::LineageLens;
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    app.jump_to("model.jaffle_finance.rpt_delivery_base_metrics");
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
    // The current root (rpt_delivery_base_metrics) appears exactly ONCE in the row.
    assert_eq!(
        title.matches("rpt_delivery_base_metrics").count(),
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
pub(super) fn overview_title_replaces_lineage_title() {
    // Toggling the whole-graph overview replaces the `Lineage: {name}` title
    // body with `Overview: N nodes` — the lineage pane title row must show it,
    // and must not still say `Lineage:`.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    apply_action(&mut app, Action::ToggleOverview);
    assert!(app.ui_state.overview(), "precondition: overview is on");

    let expected = app.overview_title().expect("overview title is set");
    let title = lineage_title_row(&app, 100, 30);
    assert!(
        title.contains(&expected),
        "title row shows the overview body, got {title:?}"
    );
    assert!(
        !title.contains("Lineage:"),
        "the `Lineage:` body is replaced while the overview is on, got {title:?}"
    );
}

#[test]
pub(super) fn lineage_title_keeps_violation_lens_suffix_for_long_chain_at_80_cols() {
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
    app.jump_to("model.jaffle_finance.rpt_delivery_base_metrics");
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
pub(super) fn lineage_title_no_breadcrumb_is_byte_identical_to_the_default() {
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
pub(super) fn minimap_default_off_keeps_lineage_invariants_intact() {
    // The default-OFF guard: WITHOUT toggling the minimap, fct at 80x24 must
    // still render exactly ONE emphasis region (the selected node's name) in the
    // lineage interior, unclipped — i.e. the lineage_render.rs invariants hold
    // because nothing was drawn over the interior.
    use dbtl::app::App;
    use dbtl::ui::{pane_interior, pane_layout, PaneConfig};
    use ratatui::layout::Rect;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    assert!(!app.ui_state.minimap_visible(), "minimap default OFF");

    // Anchor exactly as the loop does so the selected box centers/shows.
    let interior =
        pane_interior(pane_layout(Rect::new(0, 0, 80, 24), &PaneConfig::default()).lineage);
    {
        let lay = app.styled_lineage_layout().expect("non-empty lineage");
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
pub(super) fn minimap_on_draws_node_and_viewport_glyphs_in_the_inset() {
    // Toggle the minimap ON via the real apply_action path, render the fct
    // fan-out at 80x24 (the diagram overflows → the minimap guard fires), and
    // assert the occupancy `#` and viewport `+` glyphs appear in the top-right
    // inset of the lineage interior (and ONLY scan that inset, since the minimap
    // legitimately overwrites a corner of the diagram when ON).
    use dbtl::app::{apply_action, App};
    use dbtl::ui::{pane_interior, pane_layout, PaneConfig};
    use dbtl::Action;
    use ratatui::layout::Rect;

    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    app.select_by_unique_id("model.jaffle_finance.fct_subscription_process");
    apply_action(&mut app, Action::ToggleMinimap);
    assert!(app.ui_state.minimap_visible(), "M toggled the minimap on");

    let interior =
        pane_interior(pane_layout(Rect::new(0, 0, 80, 24), &PaneConfig::default()).lineage);
    {
        let lay = app.styled_lineage_layout().expect("non-empty lineage");
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
    let x0 = interior.x + interior.width - 16;
    let x1 = interior.x + interior.width; // exclusive
    let y0 = interior.y;
    let y1 = interior.y + 6; // exclusive
    let mut has_node = false;
    let mut has_view = false;
    for y in y0..y1 {
        for x in x0..x1 {
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
pub(super) fn reversed_runs_in(buffer: &Buffer, interior: ratatui::layout::Rect) -> Vec<String> {
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
