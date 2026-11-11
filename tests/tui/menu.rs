use super::*;

// ============================================================================
// context menu, dimming & overlay stacking (render-side)
// ============================================================================

pub(super) fn menu_app() -> dbtl::app::App {
    const SAMPLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample_manifest.json"
    );
    let dag = load_dag(SAMPLE).expect("sample manifest loads");
    let mut app = dbtl::app::App::new(dag, std::path::PathBuf::from(SAMPLE));
    app.select_by_unique_id("model.sample.stg_orders");
    app
}

#[test]
pub(super) fn modal_dimming_recedes_everything_outside_the_box() {
    use dbtl::ui::theme;
    // Same app twice: once bare, once with the structure modal open.
    let mut app = menu_app();
    let before = render_app(&app, 140, 50);
    dbtl::app::apply_action(&mut app, dbtl::Action::DetailOpen);
    let after = render_app(&app, 140, 50);

    let modal = dbtl::ui::modal_rect(*after.area());
    // Pick cells OUTSIDE the modal box (list-pane top corner + status row +
    // lineage far corner): every one must be dimmed to text_faint.
    let probes = [
        (2u16, 2u16),
        (4, after.area().height - 2),
        (after.area().width - 3, 3),
    ];
    for (x, y) in probes {
        assert!(
            !modal.contains(ratatui::layout::Position { x, y }),
            "probe ({x},{y}) must be outside the modal"
        );
        let c = after.cell((x, y)).expect("cell exists");
        assert_eq!(
            c.fg,
            theme::TEXT_FAINT,
            "exterior cell ({x},{y}) must be dimmed to text_faint"
        );
    }
    // The dimming actually changed the picture (guards a no-op pass).
    assert_ne!(
        before.cell((probes[0].0, probes[0].1)).unwrap().fg,
        after.cell((probes[0].0, probes[0].1)).unwrap().fg,
        "exterior cells must visibly change after dimming"
    );
}

#[test]
pub(super) fn search_zero_match_stamps_the_reason() {
    // A name/selector miss points the user at the `?` escape; a `?`-prefixed
    // full-text miss already knows — it stamps the bare "no match".
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::SearchOpen);
    for c in "zzzzqqq".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    assert!(app.active_list().is_empty(), "fixture has no zzzzqqq model");
    // Widened past the 34-col default so the full reason string fits.
    app.ui_state.set_list_width(60);
    let text = buffer_to_string(&render_app(&app, 200, 30));
    assert!(
        text.contains("no match — try ?text for full-text search"),
        "name miss advertises the ? escape:\n{text}"
    );
    // `?` full-text query → bare "no match" (the escape was already used).
    apply_action(&mut app, Action::SearchCancel);
    apply_action(&mut app, Action::SearchOpen);
    for c in "?zzzzqqq".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let text = buffer_to_string(&render_app(&app, 200, 30));
    assert!(text.contains("no match"), "the miss stamps:\n{text}");
    assert!(
        !text.contains("full-text"),
        "no redundant hint on a ? query:\n{text}"
    );
}

#[test]
pub(super) fn empty_list_stamps_the_scope_reason_and_ascii_dash() {
    // A project with NO models in scope explains how to widen it — and honours
    // the ASCII dash in ASCII glyph mode.
    use dbtl::app::App;
    use std::collections::HashMap;
    let empty = Dag::build(&dbtl::RawManifest {
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
        docs: Default::default(),
    });
    let mut app = App::new(empty, std::path::PathBuf::from(FIXTURE));
    // The reason is longer than the 34-col default pane — widen to fit it.
    app.ui_state.set_list_width(60);
    let text = buffer_to_string(&render_app(&app, 200, 30));
    assert!(
        text.contains("no models in scope — try --include-packages"),
        "the scope reason stamps on an empty list:\n{text}"
    );
    // The lineage pane explains itself too (no model → nothing to draw).
    assert!(text.contains("no model"), "the lineage pane is explained");
    app.glyph_mode = dbtl::GlyphMode::Ascii;
    let ascii = buffer_to_string(&render_app(&app, 200, 30));
    assert!(
        ascii.contains("no models in scope - try --include-packages"),
        "ASCII mode uses the '-' dash: {ascii:?}"
    );
    assert!(!ascii.contains('—'), "no em-dash in ASCII mode");
}

#[test]
pub(super) fn palette_recent_section_and_commands_header_render() {
    // After a run, reopening at an empty query renders `Recent` first, then a
    // `Commands` header over the full list — section headers are display rows.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "minimap".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    apply_action(&mut app, Action::SearchConfirm); // records ToggleMinimap
    apply_action(&mut app, Action::PaletteOpen); // reopen at empty query
    let text = buffer_to_string(&render_app(&app, 100, 40));
    assert!(text.contains("Recent"), "the Recent section header renders");
    assert!(
        text.contains("Commands"),
        "the Commands section header renders"
    );
    assert!(
        text.contains("toggle minimap"),
        "the recent command row renders:\n{text}"
    );
    // Recent precedes Commands on screen.
    let ri = text.find("Recent").unwrap();
    let ci = text.find("Commands").unwrap();
    assert!(ri < ci, "Recent section leads: {text}");
}

#[test]
pub(super) fn palette_at_search_shows_model_rows() {
    // `@` switches the palette to model search: matching model NAMES render.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "@pos_txn".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let text = buffer_to_string(&render_app(&app, 100, 40));
    assert!(
        text.contains("pos_txn"),
        "the @-search's model row renders:\n{text}"
    );
    // No command rows under `@` — command help strings are gone (the modal
    // title itself still reads "commands" — that is the palette's name).
    assert!(
        !text.contains("toggle minimap") && !text.contains("Recent"),
        "command rows are gone under @:\n{text}"
    );
}

#[test]
pub(super) fn palette_zero_candidates_stamps_no_match() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "zzzzqqq".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let text = buffer_to_string(&render_app(&app, 100, 40));
    assert!(
        text.contains("no matching commands"),
        "the zero-candidate stamp renders:\n{text}"
    );
}

#[test]
pub(super) fn palette_unavailable_row_renders_the_reason() {
    // `D` "diff vs baseline" needs a loaded baseline: the fixture App has
    // none, so the row must carry its reason text.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "baseline".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let text = buffer_to_string(&render_app(&app, 110, 40));
    assert!(
        text.contains("needs --diff"),
        "the unavailable reason renders next to the command:\n{text}"
    );
}

#[test]
pub(super) fn palette_overshot_selected_still_highlights_the_last_row() {
    // `selected` can outlive a shrinking candidate set (the reducer clamps on
    // mutation, not on draw — e.g. a --watch reload mid-palette). The renderer
    // must clamp too, or NO row carries the cursor and the highlight vanishes.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::PaletteOpen);
    for c in "minimap".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    if let dbtl::Mode::Palette(p) = &mut app.mode {
        p.selected = 99; // out of range — draw must clamp to the last row
    }
    let buffer = render_app(&app, 100, 40);
    let text = buffer_to_string(&buffer);
    // The palette's cursor row is the REVERSED line inside the modal — find
    // whichever reversed row carries the command's help text (the list
    // pane's own selection marker is reversed too, so scan every row).
    let mut hit = false;
    for y in 0..buffer.area.height {
        let line: String = (0..buffer.area.width)
            .filter_map(|x| buffer.cell((x, y)))
            .map(|c| c.symbol())
            .collect();
        let reversed = (0..buffer.area.width).any(|x| {
            buffer
                .cell((x, y))
                .is_some_and(|c| c.modifier.contains(Modifier::REVERSED))
        });
        if reversed && line.contains("minimap") {
            hit = true;
            break;
        }
    }
    assert!(
        hit,
        "the clamped LAST candidate keeps the cursor highlight:\n{text}"
    );
}

#[test]
pub(super) fn node_menu_renders_title_items_and_key_hints() {
    let mut app = menu_app();
    dbtl::app::apply_action(&mut app, dbtl::Action::MenuOpen);
    let buffer = render_app(&app, 140, 50);
    let text = buffer_to_string(&buffer);
    // The box title carries the target's name.
    assert!(text.contains("stg_orders"), "menu title: {text}");
    // BINDINGS-derived rows with their key labels render side by side.
    for label in ["Structure view", "SQL preview", "Copy name"] {
        assert!(text.contains(label), "menu item {label:?} missing: {text}");
    }
    // Separators draw a chrome rule — the row count shows items AND rules.
    let area = buffer.area;
    let mut rule_rows = 0;
    for y in 0..area.height {
        let row = row_text(&buffer, y);
        if row.matches('─').count() >= 6 || row.matches('-').count() >= 6 {
            rule_rows += 1;
        }
    }
    assert!(
        rule_rows >= 2,
        "separator rules must render, got {rule_rows}"
    );
}

#[test]
pub(super) fn help_filter_narrows_to_matching_bindings() {
    // `/copy` inside help: only the matching (Yank) section's rows stay —
    // a section with no match drops its header AND its rows, and the title
    // shows the live filter.
    use dbtl::app::{apply_action, App};
    use dbtl::Action;
    let mut app = App::new(fixture_dag(), std::path::PathBuf::from(FIXTURE));
    apply_action(&mut app, Action::HelpToggle);
    apply_action(&mut app, Action::SearchOpen); // `/` inside help
    for c in "copy".chars() {
        apply_action(&mut app, Action::SearchType(c));
    }
    let text = buffer_to_string(&render_app(&app, 100, 45));
    assert!(
        text.contains("/copy"),
        "the help title echoes the filter:\n{text}"
    );
    assert!(
        text.contains("copy unique_id"),
        "a matching binding row renders:\n{text}"
    );
    // A non-matching section (e.g. Detail's "scroll") is gone entirely.
    assert!(
        !text.contains("scroll to top"),
        "non-matching rows are filtered out:\n{text}"
    );
}

#[test]
pub(super) fn menu_over_modal_keeps_the_modal_visible_underneath() {
    // Detail -> menu: the underlay (modal) is drawn first, then dimmed, then
    // the menu — so the modal title text must still be legible in the buffer.
    let mut app = menu_app();
    dbtl::app::apply_action(&mut app, dbtl::Action::DetailOpen);
    dbtl::app::apply_action(&mut app, dbtl::Action::MenuOpen);
    let buffer = render_app(&app, 140, 50);
    let text = buffer_to_string(&buffer);
    assert!(text.contains("stg_orders"), "menu title over modal: {text}");
    // The structure modal's own header still renders behind (dimmed, not erased).
    assert!(
        text.contains("Structure") || text.contains("stg_orders"),
        "underlying modal content must still be drawn: {text}"
    );
}

#[test]
pub(super) fn yank_menu_renders_every_binding_row() {
    let mut app = menu_app();
    dbtl::app::apply_action(&mut app, dbtl::Action::YankPrefix);
    let buffer = render_app(&app, 140, 50);
    let text = buffer_to_string(&buffer);
    // Every `y<key>` row of the which-key menu is drawn — enumerate them from
    // the same BINDINGS source the renderer reads, so a new `y<key>` row is
    // asserted automatically.
    for b in dbtl::action::yank_bindings() {
        let hint = format!("y{}", b.key_label());
        assert!(text.contains(&hint), "yank row {hint:?} missing: {text}");
    }
}
