//! GlyphMode::Ascii guard: in ASCII mode, EVERY cell the UI emits must be plain
//! ASCII — panes, borders, lineage grid, scroll markers, badges, titles,
//! overlays, status line.
//!
//! This is the load-bearing guarantee for terminals that render
//! East-Asian-Ambiguous characters 2 cells wide (common in CJK-configured
//! setups / CJK font fallback), where ANY ambiguous glyph (`─ │ ┌ ▶ ◀ ▲ ▼ ↑ ↓
//! ≤ · ▏ …`) desyncs ratatui's 1-cell buffer model into doubled/ghosted
//! frames. Rendering every surface and scanning the whole buffer is the
//! exhaustive form of the check: no surface can sneak a non-ASCII glyph back
//! in without failing here.

mod common;
use common::FIXTURE_MANIFEST as FIXTURE;
use dbtl::app::{apply_action, App};
use dbtl::layout::GlyphMode;
use dbtl::ui::{draw, Focus, RenderCtx, StatusSegments, ToastLine};
use dbtl::{load_dag, Action, NoticeLevel, NotificationEntry};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

const FCT: &str = "model.jaffle_finance.fct_subscription_process";

fn ascii_app() -> App {
    let dag = load_dag(FIXTURE).expect("fixture loads");
    let mut app = App::new(dag, std::path::PathBuf::from(FIXTURE));
    app.glyph_mode = GlyphMode::Ascii;
    app.select_by_unique_id(FCT);
    app
}

/// Render the app exactly as the event loop does (layout in the app's glyph
/// mode, styles applied, Dag-derived strings passed) and return the buffer.
fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    render_ctx(app, width, height, &[], None, None)
}

/// [`render`] plus an optional toast stack (the transient copy/bookmark
/// acknowledgement, Issue 08), mirroring the event loop's `ctx.toasts` wiring.
fn render_with_toasts(app: &mut App, width: u16, height: u16, toasts: &[ToastLine]) -> Buffer {
    render_ctx(app, width, height, toasts, None, None)
}

/// [`render`] plus the Issue-05 pointer overlays: the hovered node's accent
/// frame (`hover`), the search-match frames + `i/N` title (derived from the
/// app's mode like the loop computes them), and the settled-hover tooltip.
fn render_mouse(
    app: &mut App,
    width: u16,
    height: u16,
    hover: Option<&str>,
    tip: Option<&dbtl::ui::TooltipView>,
) -> Buffer {
    render_ctx(app, width, height, &[], hover, tip)
}

/// The shared render body: the base panes plus every overlay surface the
/// event loop can stack — toasts, the hover accent, the tooltip, the tab
/// strip, and the inspector (the two `&mut App` seams mirror the loop's
/// inspector-visibility resolution).
fn render_ctx(
    app: &mut App,
    width: u16,
    height: u16,
    toasts: &[ToastLine],
    hover: Option<&str>,
    tip: Option<&dbtl::ui::TooltipView>,
) -> Buffer {
    // The SAME memoized producer the event loop draws from, so the guard
    // scans exactly what production renders.
    let lineage = app.styled_lineage_layout();
    // Layer bands exactly as the loop wires them (Layer lens only), so the
    // band-stamping path is under the guard too (labels are dir-name data,
    // but the stamping itself must add no chrome glyph).
    let layer_bands = match (&lineage, app.ui_state.lens()) {
        (Some(lay), dbtl::ui::LineageLens::Layer) => Some(app.layer_bands(lay)),
        _ => None,
    };
    // Mirror the loop's narrow-band publish (§12): below 80 columns the list
    // exists only as the drawer overlay, so a guard scenario exercises the
    // drawer whenever the loop would.
    app.ui_state
        .set_list_auto_hidden(width < dbtl::ui::LIST_AUTO_HIDE_W);
    // Mirror the loop's inspector visibility seam (Issue 04): `auto` resolves
    // against the rendered width (§12's >=160 rule), `on`/`off` always/never
    // show — so a guard scenario exercises the pane whenever the loop would.
    let want = app.ui_state.inspector_policy().shown_at(width);
    app.ui_state.set_inspector_shown(want);
    if let Some(uid) = app.inspector_target() {
        app.ui_state.set_inspector_target(&uid);
    }
    // Mirror the loop's phase 2: a squeezed or view-less `want` resolves to
    // `shown = false` — the carved region goes back to the other panes.
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
    // The contextual status hints (§8 — BINDINGS-generated, ASCII-safe in
    // both glyph modes since the key labels are); scanned under the guard.
    let hints = app.status_hints();
    let label = app.lineage_view_label();

    // Mirror the event loop's status-segment wiring so the segmented status bar
    // is exercised under the ASCII guard (segments are plain-ASCII bracket chrome;
    // `view` is glyph-mode-correct via lineage_view_label).
    let focus_s = match app.ui_state.focus() {
        Focus::List => "list",
        Focus::RightPane => "lineage",
        Focus::Inspector => "inspector",
    };
    let pos_s = format!(
        "{}/{}",
        app.ui_state.selected() + 1,
        app.active_list().len().max(1)
    );
    let cov_s = if app.ui_state.lens() == dbtl::ui::LineageLens::Coverage {
        let (tested, total) = app.coverage_summary();
        let pct = (tested * 100).checked_div(total).unwrap_or(0);
        Some(format!("cov {pct}% ({tested}/{total})"))
    } else {
        None
    };
    let bm_s = if app.bookmarks.is_empty() {
        None
    } else {
        Some(format!("bm:{}", app.bookmarks.len()))
    };
    // The impact segment carries the Chrome down/up badges (ASCII `v`/`^`); feed a
    // REAL value so the guard actually scans the rendered badge glyphs for
    // ASCII-safety (a hardcoded `↓`/`↑` would fail this render).
    let impact_s = app.impact_status();
    // The unread-warning badge `!N` (Issue 08, pure ASCII by contract),
    // mirroring the loop's wiring.
    let notify_s = if app.unread_warnings() > 0 {
        Some(format!("!{}", app.unread_warnings()))
    } else {
        None
    };
    // The diff chip (ASCII `+/~/-` counts), present only when a guard scenario
    // loaded a --diff baseline — mirroring the loop's wiring.
    let diff_s = app.diff_status_label();
    // The re-root breadcrumb (ASCII " > " / ".." separators); `None` until the
    // app has re-root history, so most renders are unchanged. Exercised in
    // ASCII mode by the dedicated breadcrumb test below.
    let breadcrumb = app.breadcrumb(width as usize);
    // The whole-graph overview title (`None` outside it), mirroring the
    // loop's wiring so the overview surface is under the guard too.
    let overview_title = app.overview_title();
    // The column-focus surfaces (Issue 09): the `column:m.c ^N vM` status
    // chip (ASCII badges under the guard) and the ` [col:name]` title suffix
    // (pure ASCII by contract).
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
        ctx.hints = &hints;
        ctx.stats = Some(&app.stats);
        ctx.lineage_label = Some(&label);
        ctx.breadcrumb = breadcrumb.as_deref();
        ctx.overview_title = overview_title.as_deref();
        ctx.glyphs = app.glyph_mode;
        ctx.col_suffix = col_suffix.as_deref();
        ctx.segments = StatusSegments {
            impact: impact_s.as_deref(),
            notify: notify_s.as_deref(),
            column: column_s.as_deref(),
            focus: Some(focus_s),
            view: Some(label.as_str()),
            position: Some(pos_s.as_str()),
            coverage: cov_s.as_deref(),
            bookmarks: bm_s.as_deref(),
            sort: None,
            diff: diff_s.as_deref(),
            run: None,
            filter: app.list_filter_label(),
            scope: None,
        };
        // Mirror the loop's filter-tag wiring so the `[untested]`/`[marked]`
        // title tag is scanned whenever a guard scenario toggles a filter.
        ctx.filter_label = app.list_filter_label();
        // Mirror the loop's minimap wiring so the ASCII guard scans the inset's
        // `#`/`.`/`+` glyphs whenever the lens is toggled on (all pure ASCII).
        ctx.minimap = app.ui_state.minimap_visible();
        // Mirror the loop's fold wiring so a scenario that folds groups scans
        // the `v`/`>` header markers (pure ASCII — `▾`/`▸` never leak).
        ctx.folded_groups = Some(&app.folded_groups);
        ctx.layer_bands = layer_bands.as_deref();
        ctx.toasts = toasts;
        // The Issue-05 pointer overlays, computed exactly like the loop does:
        // the search-match frames + `i/N` title ride the app's open search,
        // the hover accent + settled tooltip arrive as parameters.
        let (search_matches, search_pos) = match &app.mode {
            dbtl::Mode::Search(s) if s.target == dbtl::SearchTarget::Lineage => {
                let m = app.lineage_matches(&s.query);
                let pos = if m.is_empty() {
                    (0, 0)
                } else {
                    (s.match_idx % m.len() + 1, m.len())
                };
                (m, Some(pos))
            }
            _ => (Vec::new(), None),
        };
        // Mirror the loop's home wiring (Issue 10): while the fixed home tab
        // is active the whole body band draws the home screen — its `~`/`..`
        // chrome and the health gauge's styled spaces are under the same
        // all-ASCII scan as every other surface.
        let home = if app.home_active() {
            Some(app.home_data())
        } else {
            None
        };
        ctx.hover_node = hover;
        ctx.search_matches = &search_matches;
        ctx.search_pos = search_pos;
        ctx.tooltip = tip;
        // Mirror the loop's tab-strip wiring so the bar's `x`/`<`/`>`/`+`/`..`
        // chrome is under the guard in every scenario (all pure ASCII).
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
        // Mirror the loop's inspector draw seam (Issue 04): the published
        // snapshot rides `RenderCtx` so the pane's marks/badges/scrollbar
        // are under the same all-ASCII scan.
        ctx.inspector = inspector.as_deref();
        ctx.home = home.as_deref();
        ctx.home_sel = app.home_sel;
        ctx.home_scroll = app.home_scroll;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    terminal.backend().buffer().clone()
}

/// Every cell symbol in the buffer must be ASCII **or unambiguously wide**.
///
/// The failure class is East-Asian-AMBIGUOUS width: chars `unicode-width`
/// counts as 1 cell but ambiguous-wide terminals draw as 2 (`─ ▶ ↑ ≤ · …`).
/// Chars that are properly Wide (width 2 — e.g. CJK in a model/column
/// description, which is DATA, not chrome) are measured as 2 by both sides and
/// render fine everywhere, so they pass. Anything non-ASCII with width 1 is
/// exactly the desync risk and fails.
fn assert_all_ascii(buffer: &Buffer, surface: &str) {
    use unicode_width::UnicodeWidthStr;
    let area = buffer.area;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                let sym = cell.symbol();
                assert!(
                    sym.is_ascii() || sym.width() == 2,
                    "{surface}: ambiguous-width glyph {sym:?} at ({x},{y}) in ASCII glyph mode"
                );
            }
        }
    }
}

#[test]
fn selection_screen_with_clipped_lineage_is_all_ascii() {
    // fct at 80x24 overflows the lineage pane, so this also exercises the
    // scroll markers, the lineage grid + arrowheads, the list badges, the
    // depth/direction label, and the status note.
    let mut app = ascii_app();
    apply_action(&mut app, Action::DepthDecrease); // label shows "<=3"
    let buffer = render(&mut app, 80, 24);
    assert_all_ascii(&buffer, "selection 80x24");
    // And at a size where the diagram fits (no markers; centred pad).
    let buffer = render(&mut app, 200, 60);
    assert_all_ascii(&buffer, "selection 200x60");
}

#[test]
fn persistent_filter_tags_and_coverage_badges_are_all_ascii() {
    // The `[marked]` / `[untested]` title tag + status segment, the `*` ASCII
    // bookmark badge, and the Coverage-lens ` t:N` row badge, all at once.
    let mut app = ascii_app();
    apply_action(&mut app, Action::BookmarkToggle);
    apply_action(&mut app, Action::ToggleBookmarkFilter);
    apply_action(&mut app, Action::CycleLens); // Coverage -> t:N badges
    assert_all_ascii(&render(&mut app, 80, 24), "bookmarked filter 80x24");
    apply_action(&mut app, Action::ToggleUntestedFilter);
    assert_all_ascii(&render(&mut app, 80, 24), "untested filter 80x24");
}

#[test]
fn list_and_lineage_search_titles_are_all_ascii() {
    // The live-query caret in both search targets.
    let mut app = ascii_app();
    apply_action(&mut app, Action::SearchOpen); // list focus -> list search
    for c in "pos".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&mut app, 100, 30), "list search");
    apply_action(&mut app, Action::SearchCancel);

    app.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut app, Action::SearchOpen); // lineage search
    for c in "dim".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&mut app, 100, 30), "lineage search");
}

#[test]
fn help_overlay_is_all_ascii() {
    let mut app = ascii_app();
    apply_action(&mut app, Action::HelpToggle);
    assert_all_ascii(&render(&mut app, 100, 40), "help overlay");
}

#[test]
fn breadcrumb_title_is_all_ascii() {
    // Re-root twice so the lineage pane title carries a multi-entry breadcrumb
    // ("a > b > root"); its separators (" > ", "..") must stay pure ASCII.
    let mut app = ascii_app(); // rooted at fct_subscription_process
    app.jump_to("model.jaffle_finance.rpt_delivery_base_metrics");
    app.jump_to("model.jaffle_finance.fct_delivery_monthly_snapshot");
    assert!(
        app.breadcrumb(200).is_some(),
        "two jumps produced a breadcrumb"
    );
    assert_all_ascii(&render(&mut app, 100, 30), "breadcrumb title");
    // And at a narrow width that forces the ".." left-truncation path.
    assert_all_ascii(&render(&mut app, 40, 30), "breadcrumb truncated");
}

#[test]
fn detail_modal_is_all_ascii() {
    let mut app = ascii_app();
    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::DetailOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "detail modal");
}

#[test]
fn detail_modal_badges_and_tests_section_are_all_ascii() {
    // Issue 11 surfaces: the `[NN U]`/`[NN R]` column badge trails, the
    // `-> target` relationships row, the tests-section `> ` cursor, and the
    // `Tab tests`/`Enter/c jump` title hints — every glyph pure ASCII in
    // BOTH glyph modes by construction.
    let mut app = ascii_app();
    app.select_by_unique_id("model.jaffle_finance.pos_ord");
    apply_action(&mut app, Action::DetailOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "detail badges + tests rows");
    // Tab into the tests section: the title hint flips to `Tab cols` +
    // `Enter/c jump` and the `> ` marker lands on the relationships row.
    apply_action(&mut app, Action::DetailSectionToggle);
    assert_all_ascii(&render(&mut app, 100, 40), "detail tests-section cursor");
}

#[test]
fn sql_modal_is_all_ascii() {
    // pos_txn has clean ASCII SQL — the syntax-highlighting recolours bytes only,
    // so the modal stays pure ASCII (the keyword spans add no glyphs).
    let mut app = ascii_app();
    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::SqlOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "sql modal");
}

#[test]
fn sql_modal_compiled_variant_is_all_ascii() {
    // The `[raw]`/`[compiled]`/`[compiled, stale]` title tag and the
    // `c: raw/compiled` hint are bracket/letter ASCII chrome; the expansion
    // text is DATA (highlighted like raw). Exercise both title variants from
    // the compiled fixture — mart_one via manifest `compiled_code` (always
    // fresh), stg_one via `target/compiled/` (stale depends on checkout
    // mtimes — either title must stay ASCII).
    const COMPILED: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/compiled_project/target/manifest.json"
    );
    let dag = load_dag(COMPILED).expect("compiled fixture loads");
    let mut app = App::new(dag, std::path::PathBuf::from(COMPILED));
    app.glyph_mode = GlyphMode::Ascii;
    app.select_by_unique_id("model.compiled_demo.mart_one");
    apply_action(&mut app, Action::SqlOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "sql modal [raw]");
    apply_action(&mut app, Action::SqlToggleCompiled);
    assert_all_ascii(&render(&mut app, 100, 40), "sql modal [compiled]");

    app.select_by_unique_id("model.compiled_demo.stg_one");
    apply_action(&mut app, Action::SqlOpen);
    apply_action(&mut app, Action::SqlToggleCompiled);
    assert_all_ascii(
        &render(&mut app, 100, 40),
        "sql modal compiled file fallback",
    );
}

#[test]
fn stats_modal_is_all_ascii() {
    // The coverage bar is plain ASCII (#/-); section headers carry no chrome.
    let mut app = ascii_app();
    apply_action(&mut app, Action::StatsOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "stats modal");
}

#[test]
fn palette_modal_is_all_ascii() {
    // The palette chrome (border / caret) + the dim key labels are pure ASCII in
    // Ascii mode; the highlighted help chars only recolour bytes (add no glyphs).
    let mut app = ascii_app();
    apply_action(&mut app, Action::PaletteOpen);
    for c in "toggle".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&mut app, 100, 40), "command palette");
    // A short height forces the candidate list to overflow → the right-border
    // scrollbar thumb renders (ASCII `#`).
    assert_all_ascii(
        &render(&mut app, 100, 12),
        "command palette overflow scrollbar",
    );
}

#[test]
fn overflowing_overlays_with_scrollbars_are_all_ascii() {
    // Force a short height (100x12) so the help / detail / sql / stats modals all
    // overflow their box → the right-border scrollbar thumb renders in ASCII
    // (`#`), the case the taller fixtures above do not reach.
    let mut app = ascii_app();
    apply_action(&mut app, Action::HelpToggle);
    assert_all_ascii(
        &render(&mut app, 100, 12),
        "help overlay overflow scrollbar",
    );
    apply_action(&mut app, Action::HelpToggle); // close

    app.select_by_unique_id("model.jaffle_finance.pos_txn");
    apply_action(&mut app, Action::DetailOpen);
    assert_all_ascii(
        &render(&mut app, 100, 12),
        "detail modal overflow scrollbar",
    );
    apply_action(&mut app, Action::DetailClose); // all modals close via DetailClose

    apply_action(&mut app, Action::SqlOpen);
    assert_all_ascii(&render(&mut app, 100, 12), "sql modal overflow scrollbar");
    apply_action(&mut app, Action::DetailClose);

    apply_action(&mut app, Action::StatsOpen);
    assert_all_ascii(&render(&mut app, 100, 12), "stats modal overflow scrollbar");
}

#[test]
fn every_lens_with_off_root_cursor_is_all_ascii() {
    // Every lineage lens AND an off-root cursor active at once: each lens tints the
    // boxes (COLOUR-only fg), the off-root cursor dims off-path nodes (DarkGray fg)
    // + bands the path (Indexed(236) bg), and the lens adds an ASCII title suffix
    // (` [lens:...]`). All colour/ASCII — no ambiguous-width glyph — so the ASCII
    // surface must stay pure ASCII across the WHOLE cycle. The key regression guard
    // that the lens + dim overlays add no ambiguous-width glyph.
    let mut app = ascii_app();
    app.ui_state.set_focus(Focus::RightPane);
    apply_action(&mut app, Action::MoveLeft); // cursor off-root → non-empty path
    assert_ne!(
        app.lineage_cursor_uid().as_deref(),
        Some(FCT),
        "precondition: the cursor walked off the root (path is non-empty)"
    );
    // Walk the live lens cycle (Off → Coverage → Heat → Layer → Violation →
    // Lint → PrimaryKey → Off; Diff/Run* are skipped — no baseline or
    // run_results here; PrimaryKey is data-free and never skipped).
    for step in 0..=6 {
        // At 80x24 the diagram overflows (lens tint + dim + scroll markers + list
        // tint exercised); 200x60 fits (centred pad, no markers).
        assert_all_ascii(
            &render(&mut app, 80, 24),
            &format!("lens step {step} + off-root cursor 80x24"),
        );
        assert_all_ascii(
            &render(&mut app, 200, 60),
            &format!("lens step {step} + off-root cursor 200x60"),
        );
        apply_action(&mut app, Action::CycleLens);
    }
}

#[test]
fn minimap_inset_is_all_ascii() {
    // Minimap ON: the inset stamps `#`/`.`/`+` glyphs over the top-right of the
    // lineage interior. These are pure ASCII in BOTH glyph modes (the minimap has
    // no Unicode repertoire), so the ASCII surface stays pure ASCII. fct at 80x24
    // overflows the pane, so the minimap guard fires and the inset renders.
    let mut app = ascii_app();
    apply_action(&mut app, Action::ToggleMinimap);
    assert!(app.ui_state.minimap_visible(), "minimap toggled on");
    assert_all_ascii(&render(&mut app, 80, 24), "minimap inset 80x24");
}

#[test]
fn toast_stack_is_all_ascii() {
    // The stacked toasts float over the top-right with the glyph-mode border
    // set (info + warning rows stacked, Issue 08), and overflow truncation
    // appends a plain ".." (never an ellipsis glyph).
    let mut app = ascii_app();
    let stack = [
        ToastLine {
            level: NoticeLevel::Info,
            text: "Copied unique_id",
        },
        ToastLine {
            level: NoticeLevel::Warning,
            text: "Reload failed (kept previous data)",
        },
        ToastLine {
            level: NoticeLevel::Info,
            text: "theme: default",
        },
    ];
    assert_all_ascii(
        &render_with_toasts(&mut app, 80, 24, &stack),
        "toast stack 80x24",
    );
    // A toast wider than the screen budget exercises the truncation path.
    let long =
        "Removed bookmark: some_extremely_long_model_name_that_cannot_possibly_fit_in_the_box";
    let stack = [ToastLine {
        level: NoticeLevel::Warning,
        text: long,
    }];
    assert_all_ascii(
        &render_with_toasts(&mut app, 40, 24, &stack),
        "truncated toast 40x24",
    );
}

#[test]
fn notification_center_and_chip_are_all_ascii() {
    // Issue 08: the `Ctrl-n` centre modal (rows `HH:MM:SS  level  text` — the
    // `warn`/`info` labels and stamp are plain ASCII), the `!N` unread chip,
    // and the empty state, all under the ASCII guard.
    let mut app = ascii_app();
    app.record_notification(NotificationEntry {
        stamp: "10:20:30".into(),
        level: NoticeLevel::Warning,
        text: "resource type 'check' is not supported".into(),
    });
    app.record_notification(NotificationEntry {
        stamp: "10:20:31".into(),
        level: NoticeLevel::Info,
        text: "Reloaded".into(),
    });
    assert_eq!(app.unread_warnings(), 1, "one unread warning → !1 chip");
    assert_all_ascii(&render(&mut app, 100, 30), "!1 status chip 100x30");

    apply_action(&mut app, Action::NotificationsOpen); // clears !N, opens modal
    assert_all_ascii(&render(&mut app, 100, 40), "notification center 100x40");
    assert_all_ascii(&render(&mut app, 60, 20), "notification center 60x20");

    // The empty state — a fresh app with nothing logged.
    let mut empty = ascii_app();
    apply_action(&mut empty, Action::NotificationsOpen);
    assert_all_ascii(&render(&mut empty, 100, 40), "notification center empty");
}

#[test]
fn diff_lens_chip_and_modal_are_all_ascii() {
    // Load a --diff baseline (the fixture against itself = a clean diff, plus
    // a re-load against an empty-ish second fixture would be identical), then
    // exercise the three diff surfaces: the Diff lens tint pass, the status
    // chip, and the `D` summary modal. All marks are plain ASCII (+ ~ - ->).
    let mut app = ascii_app();
    let base = load_dag(FIXTURE).expect("fixture loads");
    app.set_diff_base(base, "baseline".to_string());
    assert!(app.diff().is_some(), "baseline loaded");

    // Diff lens active (clean diff: tints nothing, but the lens title suffix
    // and the chip render).
    app.ui_state.set_lens(dbtl::ui::LineageLens::Diff);
    assert_all_ascii(&render(&mut app, 80, 24), "diff lens + chip 80x24");
    assert_all_ascii(&render(&mut app, 200, 60), "diff lens + chip 200x60");

    // The summary modal over the panes.
    apply_action(&mut app, Action::DiffOpen);
    assert!(
        matches!(app.mode, dbtl::Mode::Diff(_)),
        "D opens the diff modal with a baseline"
    );
    assert_all_ascii(&render(&mut app, 80, 24), "diff modal 80x24");
    assert_all_ascii(&render(&mut app, 120, 40), "diff modal 120x40");

    // A DIFFERING baseline (the sample manifest is another project, so every
    // current node reads as added) makes the diff non-empty — now the PR impact
    // pack (CI command, affected marts/exposures, risk flags) actually renders.
    // Scan those new lines for ambiguous-width glyphs too.
    const SAMPLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let mut app = ascii_app();
    let other = load_dag(SAMPLE).expect("sample manifest loads");
    app.set_diff_base(other, "sample".to_string());
    apply_action(&mut app, Action::DiffOpen);
    assert_all_ascii(&render(&mut app, 80, 24), "PR impact pack 80x24");
    assert_all_ascii(&render(&mut app, 120, 60), "PR impact pack 120x60");
}

#[test]
fn overview_pane_is_all_ascii() {
    // The whole-graph overview draws every node in the fixture at once (with
    // the minimap auto-enabled) — the biggest diagram the guard ever renders,
    // and its title body (`Overview: N nodes`) is a new title shape.
    let mut app = ascii_app();
    apply_action(&mut app, Action::ToggleOverview);
    assert!(app.ui_state.overview(), "precondition: overview is on");
    assert_all_ascii(&render(&mut app, 80, 24), "overview 80x24");
    assert_all_ascii(&render(&mut app, 200, 60), "overview 200x60");
}

#[test]
fn column_focus_and_columns_modal_are_all_ascii() {
    // Issue 09's surfaces under the guard: the traced column's `>` row and the
    // kind marks (`-`/`%`/`~`/`*` — `·` must NOT leak: it is ambiguous-width,
    // so `col_mark` resolves it to `-` here), the `^N vM` status badges, the
    // ` [col:name]` title suffix, and the column modal's bracket chrome.
    const SAMPLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let dag = load_dag(SAMPLE).expect("sample fixture loads");
    let mut app = App::new(dag, std::path::PathBuf::from(SAMPLE));
    app.glyph_mode = GlyphMode::Ascii;
    app.select_by_unique_id("model.sample.cll_demo");
    apply_action(&mut app, Action::DetailOpen);
    apply_action(&mut app, Action::ColumnFocusEnter);
    assert!(app.column_status().is_some(), "the focus engaged");
    let buffer = render(&mut app, 100, 30);
    assert_all_ascii(&buffer, "column focus 100x30");
    // The column modal over the trace.
    apply_action(&mut app, Action::ColumnsOpen);
    assert_all_ascii(&render(&mut app, 100, 30), "columns modal 100x30");
}

#[test]
fn issue09_surfaces_are_all_ascii() {
    // Every NEW Issue-09 surface, in ASCII glyph mode: the empty-query search
    // hint's `^ history` arrow, the zero-match line's `-` dash, the empty
    // list's scope reason, the empty lineage's reason, the palette's Recent/
    // Commands headers + unavailable reason + `@` model rows, and the help
    // `/`-filter title — no surface may leak an ambiguous-width glyph.
    let mut app = ascii_app();

    // Empty-query list search: `!tag:x selector  ?text full-text  ^ history`.
    apply_action(&mut app, Action::SearchOpen);
    assert_all_ascii(&render(&mut app, 200, 30), "search hint 200x30");

    // Zero-match: `no match - try ?text for full-text search` (ASCII dash).
    for c in "zzzzqqq".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&mut app, 200, 30), "search zero-match 200x30");
    apply_action(&mut app, Action::SearchCancel);

    // Command palette: a Recent row + Commands header + an UNAVAILABLE row's
    // reason (no baseline -> `D` greyed) + the `@` model rows, all scanned.
    apply_action(&mut app, Action::PaletteOpen);
    for c in "minimap".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    apply_action(&mut app, Action::SearchConfirm); // records the Recent
    apply_action(&mut app, Action::PaletteOpen);
    assert_all_ascii(&render(&mut app, 110, 40), "palette Recent+Commands");
    for c in "baseline".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&mut app, 110, 40), "palette unavailable reason");
    app.mode = dbtl::Mode::Palette(dbtl::action::PaletteState {
        query: "@pos".to_string(),
        selected: 0,
        new_tab: false,
    });
    assert_all_ascii(&render(&mut app, 110, 40), "palette @-model rows");
    app.mode = dbtl::Mode::Selection;

    // Help `/`-filter live: the `/copy` title + narrowed sections.
    apply_action(&mut app, Action::HelpToggle);
    apply_action(&mut app, Action::SearchOpen);
    for c in "copy".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(&render(&mut app, 110, 45), "help /-filter");
    apply_action(&mut app, Action::HelpToggle); // clear filter
    apply_action(&mut app, Action::HelpToggle); // close help

    // Empty project: `no models in scope - try --include-packages` + the
    // lineage pane's own one-line reason, in ASCII (dash must be `-`).
    use std::collections::HashMap;
    let empty = dbtl::Dag::build(&dbtl::RawManifest {
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
    let mut app = App::new(empty, std::path::PathBuf::from(FIXTURE));
    app.glyph_mode = GlyphMode::Ascii;
    assert_all_ascii(&render(&mut app, 200, 30), "empty project 200x30");
}

#[test]
fn node_menu_overlay_is_all_ascii() {
    // The `a` context menu: title, item labels, key column, separator rules
    // and the box border must all stay pure ASCII in Ascii mode.
    let mut app = ascii_app();
    apply_action(&mut app, Action::MenuOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "node context menu 100x40");
    assert_all_ascii(&render(&mut app, 80, 24), "node context menu 80x24");
}

#[test]
fn yank_menu_overlay_is_all_ascii() {
    // The `y` which-key menu (y<key> rows + box) in both sizes.
    let mut app = ascii_app();
    apply_action(&mut app, Action::YankPrefix);
    assert_all_ascii(&render(&mut app, 100, 40), "yank menu 100x40");
    assert_all_ascii(&render(&mut app, 80, 24), "yank menu 80x24");
}

#[test]
fn menu_stacked_over_modal_is_all_ascii() {
    // Nested overlay: menu over the structure modal — underlay + dimming +
    // top box must introduce no ambiguous-width glyph.
    let mut app = ascii_app();
    apply_action(&mut app, Action::DetailOpen);
    apply_action(&mut app, Action::MenuOpen);
    assert_all_ascii(&render(&mut app, 100, 40), "menu over modal 100x40");
    assert_all_ascii(&render(&mut app, 80, 24), "menu over modal 80x24");
}

#[test]
fn inspector_pane_is_all_ascii() {
    // Issue 04's pane under the guard: the `v`/`x`/`!` lint and test marks,
    // the `^N vM` column badges, and the scrollbar thumb must all resolve to
    // ASCII here — including a forced `On` narrow-screen render (the pane
    // squeezes but stays ASCII) and a focused-column pick.
    let mut app = ascii_app();
    app.ui_state
        .set_inspector_policy(dbtl::ui::InspectorPolicy::On);
    // Publish `inspector_shown` before toggling — the traversal skips the
    // inspector while `shown` is still false (the loop publishes it each
    // frame; the test renders once to do the same).
    let _ = render(&mut app, 200, 60);
    apply_action(&mut app, Action::ToggleFocus);
    apply_action(&mut app, Action::ToggleFocus);
    assert_eq!(
        app.ui_state.focus(),
        Focus::Inspector,
        "precondition: inspector focus"
    );
    assert_all_ascii(&render(&mut app, 200, 60), "inspector 200x60");
    assert_all_ascii(&render(&mut app, 120, 40), "inspector forced-on 120x40");
    app.ui_state.set_inspector_col(0);
    assert_all_ascii(&render(&mut app, 200, 60), "inspector column pick");
}

#[test]
fn issue05_pointer_surfaces_are_all_ascii() {
    // Issue 05's surfaces under the guard: the settled-hover tooltip (border
    // set + `^`/`v` stats badges — `↑`/`↓` must NOT leak), the hovered node's
    // accent frame, and the lineage-search title's `i/N` + `_` caret (the
    // `▏` unicode caret is ambiguous-width and must not leak either).
    let mut app = ascii_app();
    app.ui_state.set_focus(Focus::RightPane);

    // The tooltip (at the pointer cell beside a real node).
    let tip = app
        .tooltip_view("model.jaffle_finance.stg_finance__budgets", (40, 12))
        .expect("a real node produces a tooltip");
    assert_all_ascii(
        &render_mouse(
            &mut app,
            100,
            30,
            Some("model.jaffle_finance.stg_finance__budgets"),
            Some(&tip),
        ),
        "hover accent + tooltip 100x30",
    );
    // Near the right/bottom edge: the flip-clamp path must stay ASCII too.
    let tip_edge = app
        .tooltip_view("model.jaffle_finance.stg_finance__budgets", (90, 25))
        .expect("edge tooltip");
    assert_all_ascii(
        &render_mouse(&mut app, 100, 30, None, Some(&tip_edge)),
        "flipped tooltip 100x30",
    );

    // A live lineage search: every match's frame accents and the title rides
    // `/{query}{caret} i/N` — the `_` ASCII caret + ASCII digits/slashes only.
    apply_action(&mut app, Action::SearchOpen);
    for c in "budgets".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert_all_ascii(
        &render_mouse(&mut app, 100, 30, None, None),
        "lineage search title + match accents 100x30",
    );
}

#[test]
fn folded_groups_and_row_truncation_are_all_ascii() {
    // Issue 11's list surfaces under the guard: the `v` open / `>` folded
    // group markers, the `..` middle-elision, and the all-folded empty reason
    // — `▾`/`▸`/`…` must never reach an ASCII frame.
    let mut app = ascii_app();
    let fold = |app: &mut App, layer: &str| {
        app.click_target(
            &dbtl::ui::HitTarget::ListHeader(layer.to_string()),
            ratatui::crossterm::event::MouseButton::Left,
            (5, 5),
        );
    };
    fold(&mut app, "staging");
    let buffer = render(&mut app, 100, 40);
    assert_all_ascii(&buffer, "folded staging 100x40");
    let text = {
        let area = buffer.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(c) = buffer.cell((x, y)) {
                    s.push_str(c.symbol());
                }
            }
            s.push('\n');
        }
        s
    };
    assert!(
        text.contains("> Staging (11)"),
        "the folded header is the ASCII `>` marker: {text}"
    );
    assert!(text.contains("v Intermediate"), "open groups keep `v`");
    // A narrow list PANE (not terminal — that hides the list outright)
    // forces the `..` middle-elision on the long names.
    app.ui_state
        .set_pane_width(dbtl::ui::Splitter::List, 36, 100);
    let buffer = render(&mut app, 100, 40);
    assert_all_ascii(&buffer, "folded + narrow list pane");
    let text = {
        let area = buffer.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(c) = buffer.cell((x, y)) {
                    s.push_str(c.symbol());
                }
            }
            s.push('\n');
        }
        s
    };
    assert!(
        text.contains(".."),
        "a truncated name renders the `..` marker: {text}"
    );
    assert!(
        !text.contains('…'),
        "the Unicode ellipsis never reaches ASCII mode"
    );
    // Every group folded: the empty-list reason stays ASCII too.
    for layer in ["intermediate", "marts", "utilities"] {
        fold(&mut app, layer);
    }
    let buffer = render(&mut app, 80, 24);
    assert_all_ascii(&buffer, "all folded 80x24");
}

#[test]
fn too_small_screen_is_all_ascii() {
    // §12's gate screen in ASCII mode: the `×` in `WxH` / `50×12` degrades to
    // `x` — the whole frame is the one message, nothing else draws.
    let app = ascii_app();
    let backend = TestBackend::new(48, 11);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            dbtl::ui::draw_too_small(frame, app.glyph_mode, app.active_theme());
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    assert_all_ascii(&buffer, "too-small 48x11");
    let text = {
        let area = buffer.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
        }
        s
    };
    assert!(
        text.contains("terminal too small: 48x11 (need 50x12)"),
        "ASCII mode reports the size with plain x: {text:?}"
    );
}

#[test]
fn narrow_band_drawer_is_all_ascii() {
    // §12's <80 band: the inline list is gone and the open drawer floats the
    // same bordered list over the lineage — pure ASCII chrome in ASCII mode.
    let mut app = ascii_app();
    app.ui_state.set_list_drawer(true);
    let buffer = render(&mut app, 70, 24);
    assert_all_ascii(&buffer, "narrow-band drawer 70x24");
    // The drawer actually drew: its top border sits on the body's first row
    // (y=1, below the tab strip) with ASCII corners at x=0 and x=33.
    assert_eq!(buffer.cell((0, 1)).unwrap().symbol(), "+");
    assert_eq!(buffer.cell((33, 1)).unwrap().symbol(), "+");
}

// ---- Issue 10: the home screen ------------------------------------------------

#[test]
fn home_screen_and_its_search_are_all_ascii() {
    // The fixed home tab in ASCII mode: the `~` cell (not `⌂`), `..`
    // truncation (not `…`), the gauge's styled spaces, and every section
    // header/row detail must stay inside the repertoire — at BOTH spec
    // sizes.
    let mut app = ascii_app();
    app.enter_home();
    for (w, h) in [(160, 44), (100, 30)] {
        let buffer = render(&mut app, w, h);
        assert_all_ascii(&buffer, "home");
    }
    // The `/` search surface: bar, results, and the empty-result note.
    apply_action(&mut app, Action::SearchOpen);
    for c in "orders".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let buffer = render(&mut app, 100, 30);
    assert_all_ascii(&buffer, "home search (matched)");
    let mut app = ascii_app();
    app.enter_home();
    apply_action(&mut app, Action::SearchOpen);
    for c in "zzz_no_match".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let buffer = render(&mut app, 100, 30);
    assert_all_ascii(&buffer, "home search (no matches)");
}
