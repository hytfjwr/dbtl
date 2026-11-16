use super::*;

/// Draw the left model-list pane: a bordered block with interleaved group
/// headers and model rows, scrolled by `state.offset()` (display-row space),
/// the selected model row highlighted by both a marker and reversed style.
///
/// The visible window is exactly `list.rows[offset .. offset + inner_height]` —
/// the *same* row sequence `UiState::ensure_visible` measures against, which is
/// what guarantees the selected model's row is never clipped. The sticky
/// header then overwrites the FIRST window line (never inserts one) — a
/// rendering-only overlay that leaves model-space and display-row-space
/// untouched (`hit_target` resolves the covered row through the same
/// `ModelList::sticky_header`, so "computed row == drawn row" still holds).
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_list(
    frame: &mut Frame,
    area: Rect,
    list: &ModelList,
    state: &UiState,
    search: Option<&str>,
    stats: Option<&crate::AppStats>,
    glyphs: crate::GlyphMode,
    bookmarks: Option<&std::collections::BTreeSet<String>>,
    full_count: Option<usize>,
    filter_label: Option<&str>,
    empty_reason: Option<&str>,
    folded: Option<&std::collections::BTreeSet<String>>,
    hover: Option<&HitTarget>,
    t: &theme::Theme,
) {
    let chrome = super::chrome(glyphs);
    let mut title = match (search, stats) {
        // While searching, the title is the live query plus an `N/M` match count
        // (N = filtered/visible models, M = full count). The `Search: {query}`
        // prefix is load-bearing (a TestBackend assertion); only the count format
        // changed from ` (N) ` to ` N/M `.
        (Some(query), _) => {
            let total = full_count.unwrap_or_else(|| list.len());
            format!(" Search: {query}{} {}/{} ", chrome.caret, list.len(), total)
        }
        // Otherwise show the project + resource counts (the title-bar stats).
        // `exp:` joins only when exposures exist, so a project without them
        // keeps the pre-exposure title byte-identical.
        (None, Some(s)) => {
            let exp = if s.exposures > 0 {
                format!("exp:{} ", s.exposures)
            } else {
                String::new()
            };
            format!(
                " {} - model:{} src:{} seed:{} snap:{} {exp}",
                s.project, s.models, s.sources, s.seeds, s.snapshots
            )
        }
        (None, None) => format!(" Models ({}) ", list.len()),
    };
    // The persistent-filter tag rides at the END of whichever title is up
    // (search included — the filter still narrows underneath the query).
    if let Some(tag) = filter_label {
        title.push_str(&format!("[{tag}] "));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(chrome.border)
        .border_style(focus_border(state, Focus::List, t))
        .title(Line::styled(title, title_style(state, Focus::List, t)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let inner_height = inner.height as usize;
    let is_folded = |layer: &str| folded.is_some_and(|f| f.contains(layer));
    let mut lines: Vec<Line> = Vec::with_capacity(inner_height);
    for row in list.rows.iter().skip(state.offset()).take(inner_height) {
        match row {
            DisplayRow::Header { layer, count } => {
                lines.push(list_header_line(layer, *count, is_folded(layer), chrome, t));
            }
            DisplayRow::Model { model_index, name } => {
                lines.push(list_model_line(
                    list,
                    state,
                    search,
                    chrome,
                    bookmarks,
                    *model_index,
                    name,
                    inner.width as usize,
                    t,
                ));
            }
        }
    }
    // The sticky header: while the window's first display row is a model
    // mid-group, pin its owning header over that first line (an overwrite —
    // never an inserted row — so display-row-space stays exact). The pin is
    // suppressed while the selected model IS that row (it must keep its
    // marker — `sticky_header` owns the whole rule, shared with hit_target).
    if let Some(DisplayRow::Header { layer, count }) =
        list.sticky_header(state.offset(), state.selected())
    {
        if !lines.is_empty() {
            lines[0] = list_header_line(layer, *count, is_folded(layer), chrome, t);
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);

    // The pointer hover cue: the row under the pointer paints `theme.surface` (a
    // one-row bg band over the drawn text — `set_style`-like: it tints every
    // cell of the row, glyphs untouched). The SELECTED row skips it (the
    // reversed marker is already the stronger affordance), and the row is
    // resolved through `list_target_row` — the hit-test's own sticky-pin
    // map — so the tint lands exactly where a click would.
    if let Some(target) = hover {
        let covered = matches!(target, HitTarget::ListRow(i) if *i == state.selected());
        if !covered {
            if let Some(y) = super::geom::list_target_row(
                list,
                state.offset(),
                state.selected(),
                target,
                inner_height,
            ) {
                let row = Rect {
                    y: inner.y + y as u16,
                    height: 1,
                    ..inner
                };
                frame.render_widget(
                    Paragraph::new("").style(Style::default().bg(t.surface)),
                    row,
                );
            }
        }
    }

    // The empty-state / search-hint line: display-only chrome stamped onto the
    // interior AFTER the rows render — never an injected DisplayRow, so
    // model-space and display-row-space stay untouched (the two-coordinate
    // invariant). An EMPTY list shows the reason (`empty_reason`); a live list
    // search with an EMPTY query shows the syntax hint on a non-empty list
    // (it covers the top row for that frame only — it vanishes on the first
    // keystroke — it only shows while the input is empty).
    let stamp: Option<String> = if list.is_empty() {
        empty_reason.map(str::to_string)
    } else if search == Some("") {
        Some(format!(
            "!tag:x selector  ?text full-text  {} history",
            chrome.badge_up
        ))
    } else {
        None
    };
    if let Some(text) = stamp {
        if inner.height > 0 {
            // The empty reason goes BELOW any surviving display rows (the
            // all-folded pane still draws its `▸` headers — folded
            // headers never vanish); a truly empty list's row_count is 0,
            // so the same math lands on row 0. The non-empty search hint
            // always stamps interior row 0.
            let y = if list.is_empty() {
                list.row_count()
                    .saturating_sub(state.offset())
                    .min(inner_height - 1)
            } else {
                0
            };
            let row = Rect {
                y: inner.y + y as u16,
                height: 1,
                ..inner
            };
            frame.render_widget(ratatui::widgets::Clear, row);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!(" {text}"),
                    Style::default().fg(t.text_faint),
                )),
                row,
            );
        }
    }

    // Right-border scrollbar thumb. DISPLAY-ROW space (row_count / offset /
    // inner.height) — using model count would reintroduce the two-coordinate
    // clip bug. Drawn only when the rows overflow the interior.
    super::stamp_scrollbar(
        frame,
        area,
        list.row_count(),
        state.offset(),
        inner.height as usize,
        glyphs,
        t,
    );
}

/// One list group-header line: `▾ Title (N)` while open, `▸ Title (N)` while
/// folded — the fold marker via the shared [`Chrome`], the layer's own
/// accent + bold, the count reporting the FULL membership even folded. Used
/// both for real header rows and the sticky pin (one shape, one style).
fn list_header_line(
    layer: &str,
    count: usize,
    folded: bool,
    chrome: &super::Chrome,
    t: &theme::Theme,
) -> Line<'static> {
    let mark = if folded {
        chrome.group_folded
    } else {
        chrome.group_open
    };
    // The `layer` grouping key is never mutated — `title_case` is display-only.
    let label = format!("{mark} {} ({count})", title_case(layer));
    let accent = Style::default()
        .fg(layer_accent(layer, t))
        .add_modifier(Modifier::BOLD);
    Line::styled(label, accent)
}

/// One list model row — the width-tiered columns on the pane's INNER width:
/// `< 30` name only; `30..=43` name + the direct `↑u ↓d` badges (plus the
/// Coverage lens's `t:N` and the `★` bookmark, the pre-tier suffixes); `>= 44`
/// adds the materialization tag + `t:N` to the suffix
/// (`Table t:3 ↑2 ↓4`). The suffix RIGHT-ALIGNS to the pane's inner edge so
/// the badges read as real columns; the name middle-elides into the space
/// left of it ([`crate::model_list::mid_truncate_indexed`] — `…`/`..` from
/// `chrome`, `unicode-width` measured, tail preserved).
///
/// While a non-empty list search is active the name draws char-by-char
/// (matched chars patched gold+bold — the same `match_indices` the filter
/// accepted, mapped through the elision so a truncated name still highlights
/// exactly the right cells). Style-only — the rendered TEXT is unchanged.
#[allow(clippy::too_many_arguments)]
fn list_model_line(
    list: &ModelList,
    state: &UiState,
    search: Option<&str>,
    chrome: &super::Chrome,
    bookmarks: Option<&std::collections::BTreeSet<String>>,
    model_index: usize,
    name: &str,
    inner_w: usize,
    t: &theme::Theme,
) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let selected = model_index == state.selected();
    let marker = if selected {
        SELECTED_MARKER
    } else {
        UNSELECTED_MARKER
    };
    let node = list.model_at(model_index);
    let (up, down) = node.map_or((0, 0), |n| (n.direct_up, n.direct_down));
    // A model with no downstream is a leaf ("orphan") — flag it.
    let orphan = node.is_some_and(|n| n.resource_type == "model" && n.direct_down == 0);
    // Coverage lens (only the Coverage lens, not the others): a testable node
    // with zero tests gets a warning name. The lens wins over the orphan red
    // (the user opted into it), and selection still wins over both.
    let gap = state.lens() == LineageLens::Coverage && node.is_some_and(coverage_gap);
    let name_style = if selected {
        selected_style(t)
    } else if gap {
        Style::default().fg(t.danger)
    } else if orphan {
        Style::default().fg(t.orphan)
    } else {
        Style::default()
    };
    let bookmarked = bookmarks.is_some_and(|b| node.is_some_and(|n| b.contains(&n.unique_id)));
    let coverage = state.lens() == LineageLens::Coverage;

    // The right-aligned suffix column(s) — everything after the name field.
    // Tier order inside the wide suffix is fixed: tag, then `t:N`, then
    // the direct-edge badges; `★` always tails.
    let mut suffix = String::new();
    if inner_w >= 44 {
        let tag = node
            .map(|n| {
                crate::MaterializationClass::classify(&n.resource_type, n.materialized.as_deref())
                    .tag()
            })
            .unwrap_or("Node");
        let tests = node.map_or(0, |n| n.test_count);
        suffix = format!(
            " {tag} t:{tests} {}{up} {}{down}",
            chrome.badge_up, chrome.badge_down
        );
    } else if inner_w >= 30 {
        suffix = format!("  {}{up} {}{down}", chrome.badge_up, chrome.badge_down);
        // Under the Coverage lens, surface the metric the red tint is
        // judging: the per-model test count (pure ASCII — guard-safe).
        if coverage {
            if let Some(n) = node {
                suffix.push_str(&format!(" t:{}", n.test_count));
            }
        }
    }
    if inner_w >= 30 && bookmarked {
        suffix.push_str(&format!(" {}", chrome.bookmark));
    }

    // `  ` + marker = a 4-cell lead-in; the name field is what remains before
    // the right-aligned suffix (≥1 cell short circuits to the empty field).
    let name_w = inner_w.saturating_sub(4 + UnicodeWidthStr::width(suffix.as_str()));
    let (text, cmap) = crate::model_list::mid_truncate_indexed(name, name_w, chrome.ellipsis);
    let pad = name_w.saturating_sub(UnicodeWidthStr::width(text.as_str()));

    let name_spans: Vec<Span> = match search {
        Some(q) if !q.trim().is_empty() => {
            let hits = crate::model_list::match_indices(name, q);
            let hi = Style::default().fg(t.gold).add_modifier(Modifier::BOLD);
            text.chars()
                .zip(cmap.iter())
                .map(|(ch, &src)| {
                    let st = if src != usize::MAX && hits.contains(&src) {
                        name_style.patch(hi)
                    } else {
                        name_style
                    };
                    Span::styled(ch.to_string(), st)
                })
                .collect()
        }
        _ => vec![Span::styled(text, name_style)],
    };

    let mut spans = vec![
        Span::raw("  "),                              // indent under header
        Span::styled(marker.to_string(), name_style), // selection marker
    ];
    spans.extend(name_spans);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(t.text_faint)));
    }
    Line::from(spans)
}

/// The accent color for a layer-header row (list pane only; a SEPARATE path from
/// the shared `header_style()`). Unknown layers fall back to the gray
/// `layer_other`.
fn layer_accent(layer: &str, t: &theme::Theme) -> Color {
    match layer {
        "staging" => t.layer_staging,
        "intermediate" => t.layer_intermediate,
        "marts" => t.layer_marts,
        "utilities" => t.layer_utilities,
        _ => t.layer_other,
    }
}

/// Display-only capitalization of a layer name (uppercase the first char). NEVER
/// mutate the grouping key — this is for the rendered label only.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
