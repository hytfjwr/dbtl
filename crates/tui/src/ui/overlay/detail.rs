use super::*;

// ---- structure (detail) modal --------------------------------------------

/// The structure modal's lines BEFORE the columns section: type / location /
/// tags / description, ending with the blank separator. The section split
/// exists so [`detail_columns_line0`] and the renderer agree on where the
/// cursor-addressable rows sit.
fn detail_head_lines(dv: &DetailView, t: &theme::Theme) -> Vec<Line<'static>> {
    let d = &dv.detail;
    let mut out: Vec<Line> = Vec::new();

    let kind = d
        .materialized
        .clone()
        .unwrap_or_else(|| "source".to_string());
    out.push(Line::styled(format!("type:   {kind}"), header_style(t)));
    let location = match (&d.database, &d.schema) {
        (Some(db), Some(sc)) => format!("{db}.{sc}"),
        (None, Some(sc)) => sc.clone(),
        _ => "-".to_string(),
    };
    out.push(Line::from(format!("location: {location}")));
    if let Some(pkg) = &d.package_name {
        out.push(Line::from(format!("package: {pkg}")));
    }
    // Governance (dbt access controls): shown only when declared.
    if let Some(access) = &d.access {
        out.push(Line::from(format!("access:  {access}")));
    }
    if let Some(group) = &d.group {
        out.push(Line::from(format!("group:   {group}")));
    }
    if d.contract_enforced {
        out.push(Line::from("contract: enforced".to_string()));
    }
    match (&d.version, &d.latest_version) {
        (Some(v), Some(latest)) if v != latest => {
            out.push(Line::from(format!("version: {v} (latest {latest})")));
        }
        (Some(v), _) => out.push(Line::from(format!("version: {v}"))),
        _ => {}
    }
    if let Some(p) = &d.original_file_path {
        out.push(Line::from(format!("path:   {p}")));
    }
    out.push(Line::from(format!(
        "impact: {} downstream / {} upstream",
        dv.downstream_count, dv.upstream_count
    )));
    // catalog.json warehouse truth (real types / rows / size / drift) —
    // pre-rendered lines snapshotted when the modal opened.
    out.extend(dv.catalog.iter().map(|l| Line::from(l.clone())));
    if !d.tags.is_empty() {
        out.push(Line::from(format!("tags:   {}", d.tags.join(", "))));
    }
    // The `meta` dict as a key-value block (`BTreeMap` → sorted keys); nested
    // values show their compact-JSON form (`deep: {"x":1}`). Key padding is
    // counted in CHARS (rust's format width is) so a multibyte key still
    // aligns; a `\n`/`\r` in a value would tear the one-row-per-entry layout
    // — flattened to spaces like the docs table's md_cell.
    if !d.meta.is_empty() {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("meta ({}):", d.meta.len()),
            header_style(t),
        ));
        let w = d.meta.keys().map(|k| k.chars().count()).max().unwrap_or(0);
        for (k, v) in &d.meta {
            // CRLF is ONE break (single space), then lone \n / \r.
            let v = v.replace("\r\n", " ").replace(['\n', '\r'], " ");
            out.push(Line::from(format!("  {k:<w$}: {v}")));
        }
    }
    if let Some(desc) = &d.description {
        out.push(Line::from(""));
        out.push(Line::styled("description:", header_style(t)));
        for line in desc.lines() {
            out.push(Line::from(format!("  {line}")));
        }
    }
    out.push(Line::from(""));
    out
}

/// The (name, type, description) rows the columns section renders: the
/// declared columns when the node documents them, else the resolver's
/// inferred `col_names` (so undocumented models show SOMETHING cursorable).
/// The returned bool marks the inferred fallback (the header flags it).
fn detail_column_rows(dv: &DetailView) -> (Vec<(String, String, String)>, bool) {
    let d = &dv.detail;
    if d.columns.is_empty() {
        (
            dv.col_names
                .iter()
                .map(|n| (n.clone(), String::new(), String::new()))
                .collect(),
            true,
        )
    } else {
        (
            d.columns
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        c.data_type.clone().unwrap_or_default(),
                        c.description.clone().unwrap_or_default(),
                    )
                })
                .collect(),
            false,
        )
    }
}

/// The fixed-width ASCII badge for a test kind — pure ASCII in BOTH glyph
/// modes per the contract (no ambiguous-width glyphs on this surface):
/// `NN` not_null, `U` unique, `R` relationships, `AV` accepted_values,
/// `AR` accepted_range, `*` anything else. `ends_with` catches
/// package-namespaced spellings (`dbt_utils.accepted_range`, a package's
/// `pkg.relationships`) the way `register_test`'s `to:` resolution does —
/// a namespaced relationships test is jumpable, so it must badge `R`, not
/// `*`. `unique_combination_of_columns` has NO badge: it's a model-level
/// test (`column_name` always `None`), so it can never appear on a column
/// row — it shows in the tests list and drives the PrimaryKey lens instead.
fn test_badge(kind: &str) -> &'static str {
    if kind == "not_null" {
        "NN"
    } else if kind == "unique" {
        "U"
    } else if kind.ends_with("relationships") {
        "R"
    } else if kind.ends_with("accepted_values") {
        "AV"
    } else if kind.ends_with("accepted_range") {
        "AR"
    } else {
        "*"
    }
}

/// A column's badge list: the distinct [`test_badge`]s of the tests attached
/// to it (normalized-name match — the same key space `col_counts` uses), in
/// the FIXED display order below so the row is deterministic.
fn column_badges<'a>(dv: &'a DetailView, column: &str) -> Vec<&'a str> {
    const ORDER: &[&str] = &["NN", "U", "R", "AV", "AR", "*"];
    let norm = crate::normalize_column(column);
    let mut seen = std::collections::BTreeSet::new();
    for test in &dv.tests {
        if test
            .column_name
            .as_deref()
            .is_some_and(|c| crate::normalize_column(c) == norm)
        {
            seen.insert(test_badge(&test.kind));
        }
    }
    ORDER.iter().copied().filter(|b| seen.contains(b)).collect()
}

/// The columns section: header + one row per [`detail_column_rows`] entry,
/// each with the h/l cursor marker (`> `, the shared `SELECTED_MARKER`), the
/// per-column test-kind badges, and the one-hop column-level
/// up/down edge counts in the chrome badge glyphs.
fn detail_column_lines(
    dv: &DetailView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) -> Vec<Line<'static>> {
    let chrome = super::chrome(glyphs);
    let (rows, inferred) = detail_column_rows(dv);
    // ASCII-safe header tags (the `ascii_guard` renders scan this surface in
    // GlyphMode::Ascii — no `·`).
    let mut header = format!("columns ({})", rows.len());
    if inferred && !rows.is_empty() {
        header.push_str(" [from SQL]");
    }
    if dv.stale {
        header.push_str(" [stale]");
    }
    if dv.name_match {
        header.push_str(" [name-match]");
    }
    header.push(':');
    let mut out = vec![Line::styled(header, header_style(t))];
    for (i, (name, ty, desc)) in rows.iter().enumerate() {
        // The counts look up the NORMALIZED name — the resolver's key space —
        // while the row prints the declared spelling.
        let counts = dv
            .col_counts
            .get(&crate::normalize_column(name))
            .map(|(u, d)| format!("{}{} {}{}", chrome.badge_up, u, chrome.badge_down, d))
            .unwrap_or_default();
        // The test-kind badge trail for THIS column (`[NN U R]`); untested
        // columns get nothing so the gap stays visible.
        let badges = column_badges(dv, name);
        let badge_str = if badges.is_empty() {
            String::new()
        } else {
            format!(" [{}]", badges.join(" "))
        };
        let selected = i == dv.col_index && dv.test_index == usize::MAX;
        let mark = if selected {
            super::SELECTED_MARKER
        } else {
            "  "
        };
        let text = format!("  {mark}{name:<26} {ty:<14}{badge_str} {desc} {counts}");
        out.push(if selected {
            Line::styled(text, selected_style(t))
        } else {
            Line::from(text)
        });
    }
    out
}

/// One test's display text — `kind(col)` plus the resolved `-> target.col`
/// jump hint for relationships and the `name in:N given:M expect:K` shape
/// for unit tests. Shared by the structure modal's tests section and the
/// inspector pane: one row spelling, never two.
pub(crate) fn test_row_text(test: &crate::TestInfo) -> String {
    let col = test
        .column_name
        .as_deref()
        .map(|c| format!("({c})"))
        .unwrap_or_default();
    // The relationships target: the resolved display name when the node
    // survived pruning, else the raw uid (still informative); `->` is
    // pure ASCII in both glyph modes.
    let target = if test.ref_uid.is_some() {
        let name = test
            .ref_name
            .as_deref()
            .or(test.ref_uid.as_deref())
            .unwrap_or("?");
        let rcol = test
            .ref_column
            .as_deref()
            .map(|c| format!(".{c}"))
            .unwrap_or_default();
        format!(" -> {name}{rcol}")
    } else {
        String::new()
    };
    // The unit-test shape: the test's own name (a unit test's identity —
    // the `kind` alone is just `unit`), the input-table count, and the
    // inline row counts the manifest carries (a `fixture:`/CSV side
    // shows no number).
    let unit = test.unit.as_ref().map_or(String::new(), |u| {
        let mut s = format!(" {} in:{}", test.name, u.inputs);
        if let Some(g) = u.given_rows {
            s.push_str(&format!(" given:{g}"));
        }
        if let Some(e) = u.expect_rows {
            s.push_str(&format!(" expect:{e}"));
        }
        s
    });
    format!("{}{}{}{}", test.kind, col, target, unit)
}

/// The tests section: header + one row per attached test — the plain
/// `kind(col)` for schema tests, a `-> target.col` jump hint for resolved
/// `relationships` targets, and the `in:N given:M expect:K` shape for unit
/// tests. The tests-section h/l cursor (`test_index`, owned via
/// `Tab`) lands only on resolved relationships targets — the rows Enter can
/// re-root to — so it renders the same `> ` marker the columns side does.
fn detail_test_lines(dv: &DetailView, t: &theme::Theme) -> Vec<Line<'static>> {
    let mut out = vec![Line::styled(
        format!("tests ({}):", dv.tests.len()),
        header_style(t),
    )];
    for (i, test) in dv.tests.iter().enumerate() {
        let selected = i == dv.test_index;
        let mark = if selected {
            super::SELECTED_MARKER
        } else {
            "  "
        };
        let text = format!("  {mark}{}", test_row_text(test));
        out.push(if selected {
            Line::styled(text, selected_style(t))
        } else {
            Line::from(text)
        });
    }
    out
}

/// The line index of the FIRST column row inside the modal body (head + the
/// `columns (N):` header) — the base the cursor's ensure-visible math needs.
/// Line-count only, so the theme/glyph args don't matter. `pub(crate)`:
/// consumed by `clamp_detail_scroll` and the renderer only — not part of the
/// crate's API surface.
pub(crate) fn detail_columns_line0(dv: &DetailView) -> usize {
    detail_head_lines(dv, &theme::DEFAULT).len() + 1
}

/// The line index of the FIRST test row inside the modal body (head + the
/// columns header + column rows + the blank separator + the `tests (N):`
/// header) — the same ensure-visible base for the tests-section cursor.
pub(crate) fn detail_tests_line0(dv: &DetailView) -> usize {
    detail_columns_line0(dv) + detail_column_rows(dv).0.len() + 2
}

/// The structure modal content as display lines: type / location / tags /
/// description / columns table / tests list. Built once and shared by the clamp
/// and the renderer so scroll bounds match the content.
fn detail_display_lines(
    dv: &DetailView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) -> Vec<Line<'static>> {
    let mut out = detail_head_lines(dv, t);
    out.extend(detail_column_lines(dv, glyphs, t));

    out.push(Line::from(""));
    out.extend(detail_test_lines(dv, t));

    out
}

/// Clamp the detail modal's scroll offset to its content/viewport (size-aware),
/// then ensure the ACTIVE section cursor's row is inside the window — the
/// minimal-pan follow the lineage cursor already uses. Counts with the
/// DEFAULT theme — the line count is theme- and glyph-independent.
pub fn clamp_detail_scroll(area: Rect, dv: &DetailView) -> usize {
    let total = detail_display_lines(dv, crate::GlyphMode::Ascii, &theme::DEFAULT).len();
    let view_h = modal_rect(area).height.saturating_sub(2) as usize;
    let mut scroll = clamp_modal_scroll(area, total, dv.scroll);
    // Ensure-visible for the h/l cursor: pan just enough to keep its row
    // inside [scroll, scroll + view_h) — never a re-center. The tests-section
    // cursor (Tab-owned) gets the same follow at its own line base.
    if dv.test_index != usize::MAX && dv.test_index < dv.tests.len() {
        let row = detail_tests_line0(dv) + dv.test_index;
        if row < scroll {
            scroll = row;
        } else if view_h > 0 && row >= scroll + view_h {
            scroll = row + 1 - view_h;
        }
    } else if dv.col_index != usize::MAX && dv.col_index < dv.col_names.len() {
        let row = detail_columns_line0(dv) + dv.col_index;
        if row < scroll {
            scroll = row;
        } else if view_h > 0 && row >= scroll + view_h {
            scroll = row + 1 - view_h;
        }
    }
    scroll.min(total.saturating_sub(view_h))
}

/// Draw the structure (detail) modal for the selected node. `pub(crate)` for
/// `render::draw`. Data comes from the [`DetailView`] payload (cloned at open),
/// so the render layer never needs a `Dag`.
pub(crate) fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    dv: &DetailView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    // The gesture hints follow the ACTIVE cursor section: the columns side
    // traces a column on Enter, the tests side jumps to the relationships
    // `to:` target. `Tab` is only advertised when the tests section HAS a
    // jumpable row — otherwise the key notices and stays on columns.
    let (sect_hint, tab_hint) = if dv.test_index != usize::MAX {
        (", h/l test, Enter/c jump", ", Tab cols")
    } else {
        let col_hint = if dv.col_index == usize::MAX {
            ""
        } else {
            ", h/l column, Enter/c trace"
        };
        let tab_hint = if dv.tests.iter().any(|t| t.ref_uid.is_some()) {
            ", Tab tests"
        } else {
            ""
        };
        (col_hint, tab_hint)
    };
    draw_scrollable_modal(
        frame,
        area,
        format!(
            " {}  (Esc/q to close, j/k scroll{sect_hint}{tab_hint}) ",
            dv.name
        ),
        detail_display_lines(dv, glyphs, t),
        dv.scroll,
        glyphs,
        t,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_meta_block_is_sorted_and_key_aligned() {
        use std::collections::BTreeMap;
        let dv = DetailView {
            model_id: "model.p.m".into(),
            name: "m".into(),
            detail: crate::NodeDetail {
                meta: BTreeMap::from_iter([
                    ("zone".to_string(), "z1".to_string()),
                    ("owner".to_string(), "alice".to_string()),
                    ("deep".to_string(), r#"{"x":1}"#.to_string()),
                ]),
                ..Default::default()
            },
            tests: vec![],
            downstream_count: 0,
            upstream_count: 0,
            catalog: vec![],
            col_names: vec![],
            col_index: usize::MAX,
            test_index: usize::MAX,
            col_counts: Default::default(),
            stale: false,
            name_match: false,
            scroll: 0,
        };
        let lines = detail_display_lines(&dv, crate::GlyphMode::Unicode, &theme::DEFAULT);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let meta_at = text
            .iter()
            .position(|l| l == "meta (3):")
            .expect("the meta header");
        // Rows follow the header, BTreeMap-sorted by key (deep < owner < zone),
        // key column padded to the widest key.
        assert_eq!(
            &text[meta_at + 1..meta_at + 4],
            &[
                "  deep : {\"x\":1}".to_string(),
                "  owner: alice".to_string(),
                "  zone : z1".to_string(),
            ]
        );
        // An empty meta dict draws no block at all.
        let mut empty = dv.clone();
        empty.detail.meta = BTreeMap::new();
        let lines = detail_display_lines(&empty, crate::GlyphMode::Unicode, &theme::DEFAULT);
        assert!(lines
            .iter()
            .all(|l| !l.spans.iter().any(|s| s.content.starts_with("meta"))));
    }

    #[test]
    fn detail_meta_block_flattens_newlines_and_pads_multibyte_keys() {
        use std::collections::BTreeMap;
        // A \n inside a meta VALUE would tear the one-row-per-entry layout —
        // flattened to a space. And the key column pads by CHAR count (the
        // format width), not bytes: `équipe` is 6 chars but 7 bytes, so a
        // byte-counted pad would misalign the `:` column.
        let dv = DetailView {
            model_id: "model.p.m".into(),
            name: "m".into(),
            detail: crate::NodeDetail {
                meta: BTreeMap::from_iter([
                    ("k".to_string(), "line1\nline2\r\nline3".to_string()),
                    ("équipe".to_string(), "x".to_string()),
                ]),
                ..Default::default()
            },
            tests: vec![],
            downstream_count: 0,
            upstream_count: 0,
            catalog: vec![],
            col_names: vec![],
            col_index: usize::MAX,
            test_index: usize::MAX,
            col_counts: Default::default(),
            stale: false,
            name_match: false,
            scroll: 0,
        };
        let lines = detail_display_lines(&dv, crate::GlyphMode::Unicode, &theme::DEFAULT);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let meta_at = text
            .iter()
            .position(|l| l == "meta (2):")
            .expect("the meta header");
        assert_eq!(
            &text[meta_at + 1..meta_at + 3],
            &[
                "  k     : line1 line2 line3".to_string(),
                "  équipe: x".to_string(),
            ],
            "char-width padding + newline flattening"
        );
    }
}
