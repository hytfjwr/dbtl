use super::*;

// ============================================================================
// Issue 11: width-tiered columns, truncation, fold markers, sticky, hover
// ============================================================================

/// Render `fixture_list` at `width x height`: `prep_state` mutates `UiState`
/// BEFORE `RenderCtx` borrows it; the `folded`/`hover` ctx inputs are plain
/// borrows (owned by the caller — never temporaries inside a closure, which
/// would not outlive the draw).
pub(super) fn render_list_with(
    width: u16,
    height: u16,
    prep_state: impl Fn(&mut UiState),
    folded: Option<&std::collections::BTreeSet<String>>,
    hover: Option<&dbtl::ui::HitTarget>,
) -> Buffer {
    render_model_list(&fixture_list(), width, height, prep_state, folded, hover)
}

/// [`render_list_with`] on a caller-built list (a folded/filtered view — the
/// fold stage is pipeline-side, so the marker test renders the folded list).
pub(super) fn render_model_list(
    list: &ModelList,
    width: u16,
    height: u16,
    prep_state: impl Fn(&mut UiState),
    folded: Option<&std::collections::BTreeSet<String>>,
    hover: Option<&dbtl::ui::HitTarget>,
) -> Buffer {
    let list = list.clone();
    let mut state = UiState::new(list.len());
    prep_state(&mut state);
    // Scroll-follow exactly like the event loop (display-row space).
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
        let mut ctx = RenderCtx::new(&list, &state, None);
        ctx.folded_groups = folded;
        ctx.list_hover = hover;
        terminal
            .draw(|frame| draw(frame, &ctx))
            .expect("draw must not panic");
    }
    terminal.backend().buffer().clone()
}

#[test]
pub(super) fn list_columns_follow_the_inner_width_tiers() {
    // §10's width table on the pane's INNER width (pane = inner + 2 borders):
    // <30 name only; 30..=43 name + `↑u ↓d`; >=44 adds the materialization tag
    // + `t:N`. The four spec probes: 29, 30, 43, 44.
    for (inner, badges, wide) in [
        (29usize, false, false),
        (30, true, false),
        (43, true, false),
        (44, true, true),
    ] {
        let buffer = render_list_with(
            100,
            30,
            |state| state.set_pane_width(dbtl::ui::Splitter::List, (inner + 2) as u16, 100),
            None,
            None,
        );
        // The first model row (staging's first = stg_finance__budgets, a view).
        let row = row_text(&buffer, 2);
        assert_eq!(
            row.contains('↑'),
            badges,
            "inner {inner}: ↑ badge present == {badges} — row: {row:?}"
        );
        assert_eq!(
            row.contains("View") && row.contains("t:"),
            wide,
            "inner {inner}: tag + t:N present == {wide} — row: {row:?}"
        );
    }
}

#[test]
pub(super) fn model_names_middle_truncate_preserving_the_tail() {
    // A name longer than its field elides from the MIDDLE: the head survives,
    // the `…` marker renders, and the distinguishing tail is preserved.
    let buffer = render_list_with(
        100,
        30,
        |state| state.set_pane_width(dbtl::ui::Splitter::List, 40, 100),
        None,
        None,
    );
    let text = buffer_to_string(&buffer);
    // `stg_masterdata__delivery_groups` (31 cells) does not fit the name
    // field at inner 38: the middle goes, `…` marks it, and both ends stay.
    let truncated = text
        .lines()
        .find(|l| l.contains('…') && l.contains("stg_"))
        .expect("a middle-truncated row renders");
    assert!(
        truncated.contains("stg_"),
        "the head survives: {truncated:?}"
    );
    assert!(
        truncated.contains("groups") || truncated.contains("years"),
        "the distinguishing tail survives: {truncated:?}"
    );
    assert!(
        !text.contains("stg_masterdata__delivery_groups"),
        "the full name no longer fits — it elided"
    );
}

#[test]
pub(super) fn folded_group_renders_folded_marker_and_hides_members() {
    // `ctx.folded_groups` marks the group's header `▸` while every other
    // header stays `▾`; the folded members leave the pane entirely.
    let folded: std::collections::BTreeSet<String> = ["staging".to_string()].into_iter().collect();
    let folded_list = dbtl::model_list::fold_model_list(&fixture_list(), &folded);
    let buffer = render_model_list(&folded_list, 100, 60, |_| {}, Some(&folded), None);
    let text = buffer_to_string(&buffer);
    assert!(
        text.contains("▸ Staging (11)"),
        "the folded header keeps its full count: {text}"
    );
    assert!(text.contains("▾ Intermediate"), "open groups keep ▾");
    assert!(
        !text.contains("stg_finance__budgets"),
        "a folded member renders nowhere"
    );
}

#[test]
pub(super) fn sticky_header_pins_mid_group_scroll() {
    // Selecting model 30 scrolls the window so its first display row is a
    // non-selected staging model — the group's header pins over that row.
    let buffer = render_list_with(100, 30, |state| state.set_selected(30), None, None);
    let pinned = row_text(&buffer, 1); // first interior row (under the border)
    assert!(
        pinned.contains("▾ Staging (11)"),
        "the sticky header paints over the covered model row: {pinned:?}"
    );
}

#[test]
pub(super) fn sticky_hit_test_reports_the_pinned_header() {
    // The covered model row hit-tests as the PINNED header (a click folds the
    // group, it can't select the covered model) — the draw/hit-test contract.
    use dbtl::ui::{hit_target, pane_layout, HitGeometry, HitTarget, PaneConfig};
    let list = fixture_list();
    let mut state = UiState::new(list.len());
    state.set_selected(30);
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let panes = pane_layout(area, &PaneConfig::default());
    let inner = dbtl::ui::pane_interior(panes.list);
    let h = inner.height as usize;
    state.ensure_visible(
        h,
        list.reveal_row_of_model(30),
        list.row_of_model(30),
        list.row_count(),
    );
    let g = HitGeometry {
        panes,
        tab_bar: None,
        home: None,
        list: &list,
        list_offset: state.offset(),
        list_selected: state.selected(),
        layout: None,
        scroll_x: 0,
        scroll_y: 0,
        minimap: false,
        overlay: None,
        status: None,
        title: None,
    };
    // The sticky pin is active: the window's first row is a mid-group model.
    assert!(
        list.sticky_header(state.offset(), state.selected())
            .is_some(),
        "precondition: a mid-group scroll activates the pin"
    );
    let target = hit_target(&g, inner.x + 3, inner.y);
    assert!(
        matches!(target, Some(HitTarget::ListHeader(ref l)) if l == "staging"),
        "the pinned row hit-tests as the staging header: {target:?}"
    );
    // One row below resolves the real display row again.
    let target = hit_target(&g, inner.x + 3, inner.y + 1);
    assert!(
        matches!(
            target,
            Some(HitTarget::ListRow(_)) | Some(HitTarget::ListHeader(_))
        ),
        "the next row resolves normally: {target:?}"
    );
}

#[test]
pub(super) fn hovered_row_tints_the_surface_colour() {
    // §6's hover cue: a `ListRow` hover paints `theme.surface` across the row.
    use dbtl::ui::HitTarget;
    let target = HitTarget::ListRow(1);
    let buffer = render_list_with(100, 30, |_| {}, None, Some(&target));
    // Model index 1 is the second model row (display row 2 → interior y 3).
    let mut tinted = false;
    for x in 0..buffer.area.width {
        if let Some(cell) = buffer.cell((x, 3)) {
            if cell.bg == dbtl::ui::theme::DEFAULT.surface {
                tinted = true;
                break;
            }
        }
    }
    assert!(tinted, "the hovered row carries the surface bg");
}

#[test]
pub(super) fn hovered_folded_header_row_tints() {
    // The header's hover tint lands at the header's own row (the click that
    // would fold is what the pointer sees).
    use dbtl::ui::HitTarget;
    let target = HitTarget::ListHeader("staging".to_string());
    let buffer = render_list_with(100, 30, |_| {}, None, Some(&target));
    let mut tinted = false;
    for x in 0..buffer.area.width {
        if let Some(cell) = buffer.cell((x, 1)) {
            if cell.bg == dbtl::ui::theme::DEFAULT.surface {
                tinted = true;
                break;
            }
        }
    }
    assert!(tinted, "the hovered header row carries the surface bg");
}

#[test]
pub(super) fn wide_char_names_truncate_cell_exactly() {
    // CJK model names measure 2 cells: the middle-elision must never split a
    // wide char or overflow the row's cell budget.
    use dbtl::model_list::mid_truncate_indexed;
    use unicode_width::UnicodeWidthStr;
    let (text, _) = mid_truncate_indexed("テーブル名", 5, "…");
    assert!(
        UnicodeWidthStr::width(text.as_str()) <= 5,
        "the truncation honours its cell budget: {text:?}"
    );
    // No lone halves — every char survives whole.
    assert!("テーブル名".chars().any(|c| text.contains(c)));
}
