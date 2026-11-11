use super::*;

// ---- baseline-diff summary modal -------------------------------------------

/// The diff modal's content as display lines: one section per change kind
/// (added / modified / removed nodes, then edge changes), each header counting
/// its list. UNCAPPED on purpose — unlike the stats worklists (top-N teasers
/// over data visible elsewhere), this modal is the only COMPLETE listing
/// surface (removed nodes exist nowhere else in the app), and it scrolls.
/// Line marks are plain ASCII (`+` / `~` / `-` / `->`), coloured by the diff
/// roles so the modal agrees with the Diff lens tints. The SINGLE source of
/// the modal's line list, so [`clamp_diff_scroll`] stays correct automatically.
/// `width` is the modal's inner content width: the joined lists (`cols: …`,
/// `affected marts: …`, `CI columns: …`) wrap to it instead of silently
/// clipping off the right edge on narrow terminals.
fn diff_display_lines(dv: &DiffView, width: usize, t: &theme::Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    out.push(Line::from(format!("baseline: {}", dv.baseline)));
    // Same `- scope:` line the export and `dbtl impact` print: the numbers
    // below describe the ROOT project under either scope, and the modal must
    // say so or a screenshot reads differently from the pasted report.
    out.push(Line::from(format!("scope: {}", dv.scope.describe())));

    // `column_changes` is deliberately NOT summed: it refines `modified`
    // (every row belongs to a modified node), so it can never be non-empty
    // while `total` is zero — the check below still guards on it so a
    // future independent-sourced column list can't slip a stale
    // "no changes" past this branch.
    let total = dv.added.len()
        + dv.modified.len()
        + dv.removed.len()
        + dv.edges_added.len()
        + dv.edges_removed.len();
    if total == 0 && dv.column_changes.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from("no changes vs the baseline"));
        return out;
    }

    // One section: a counting header + every row, pre-marked and tinted.
    // Shared so the five sections can't drift in shape.
    let section = |title: &str, rows: Vec<String>, color: Color, out: &mut Vec<Line>| {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("{title} ({}):", rows.len()),
            header_style(t),
        ));
        for row in rows {
            out.push(Line::styled(row, Style::default().fg(color)));
        }
    };

    let node_row = |mark: char, name: &str, kind: &str| {
        if kind.is_empty() {
            format!("  {mark} {name}")
        } else {
            format!("  {mark} {name} ({kind})")
        }
    };
    section(
        "Added",
        dv.added.iter().map(|(n, k)| node_row('+', n, k)).collect(),
        t.diff_add,
        &mut out,
    );
    section(
        "Modified",
        dv.modified
            .iter()
            .map(|(n, reasons)| format!("  ~ {n}: {reasons}"))
            .collect(),
        t.diff_mod,
        &mut out,
    );
    section(
        "Removed",
        dv.removed
            .iter()
            .map(|(n, k)| node_row('-', n, k))
            .collect(),
        t.danger,
        &mut out,
    );
    section(
        "Edges added",
        dv.edges_added
            .iter()
            .map(|(p, c)| format!("  + {p} -> {c}"))
            .collect(),
        t.diff_add,
        &mut out,
    );
    section(
        "Edges removed",
        dv.edges_removed
            .iter()
            .map(|(p, c)| format!("  - {p} -> {c}"))
            .collect(),
        t.danger,
        &mut out,
    );

    // ---- Column changes: the CLL refinement of the diff -------------------
    // One tinted head row per classified change plus dim detail rows for the
    // downstream columns (the "via" list) and exposures it reaches — the
    // same data the `e` export / `dbtl impact` "Column changes" section emits.
    if !dv.column_changes.is_empty() {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("Column changes ({}):", dv.column_changes.len()),
            header_style(t),
        ));
        for ch in &dv.column_changes {
            let (mark, color) = match ch.class {
                crate::ColumnChangeClass::Breaking => ('x', t.danger),
                crate::ColumnChangeClass::Partial => ('~', t.warn),
                crate::ColumnChangeClass::NonBreaking => ('+', t.diff_add),
            };
            let name = match &ch.renamed_from {
                Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                None => format!("{}.{}", ch.node_name, ch.column),
            };
            let what = match ch.kind {
                crate::ColumnChangeKind::Removed => "removed".to_string(),
                crate::ColumnChangeKind::Renamed => "renamed".to_string(),
                crate::ColumnChangeKind::TypeChanged => format!(
                    "type {} -> {}",
                    ch.type_from.as_deref().unwrap_or("?"),
                    ch.type_to.as_deref().unwrap_or("?")
                ),
                crate::ColumnChangeKind::ExprChanged => "expression changed".to_string(),
                crate::ColumnChangeKind::Added => "added".to_string(),
            };
            out.push(Line::styled(
                format!("  {mark} {name}: {what} [{}]", ch.class.label()),
                Style::default().fg(color),
            ));
            if !ch.affected_columns.is_empty() {
                for line in wrap_prefixed("      cols: ", &ch.affected_columns.join(", "), width) {
                    out.push(Line::styled(line, Style::default().fg(t.text_dim)));
                }
            }
            if !ch.affected_exposures.is_empty() {
                for line in wrap_prefixed(
                    "      exposures: ",
                    &ch.affected_exposures.join(", "),
                    width,
                ) {
                    out.push(Line::styled(line, Style::default().fg(t.text_dim)));
                }
            }
        }
    }

    // ---- PR impact pack: blast radius / exposures / CI command / risks ----
    // Appended after the diff sections so the modal IS the reviewer's report.
    // Reached only when there ARE changes (the no-change case returned above).
    let pr = &dv.pr;
    out.push(Line::from(""));
    out.push(Line::styled("PR impact pack", header_style(t)));
    out.push(Line::from(format!(
        "  affected models: {}",
        pr.affected_models
    )));
    if pr.column_affected_models > 0 {
        out.push(Line::from(format!(
            "  column-affected models: {}",
            pr.column_affected_models
        )));
    }
    if !pr.affected_marts.is_empty() {
        for line in wrap_prefixed("  affected marts: ", &pr.affected_marts.join(", "), width) {
            out.push(Line::from(line));
        }
    }
    // The "who to page" line — the meta[owner_key] values of the affected
    // downstream models (same set the export's Affected owners section lists).
    if !pr.affected_owners.is_empty() {
        for line in wrap_prefixed("  affected owners: ", &pr.affected_owners.join(", "), width) {
            out.push(Line::from(line));
        }
    }
    match &pr.ci_command {
        Some(cmd) => {
            for line in wrap_prefixed("  CI: ", cmd, width) {
                out.push(Line::styled(line, Style::default().fg(t.accent)));
            }
        }
        None => out.push(Line::from("  CI: no buildable changes")),
    }
    // The column-narrowed sibling of `CI:` — the changed nodes plus exactly
    // the models their changed columns reach (`+` kept only where the change
    // escaped the column surface).
    if let Some(cmd) = &pr.ci_command_columns {
        for line in wrap_prefixed("  CI columns: ", cmd, width) {
            out.push(Line::styled(line, Style::default().fg(t.accent)));
        }
    }

    if !pr.affected_exposures.is_empty() {
        section(
            "Affected exposures",
            pr.affected_exposures
                .iter()
                .map(|e| format!("  {e}"))
                .collect(),
            t.accent,
            &mut out,
        );
    }

    if !pr.untested_changes.is_empty()
        || !pr.changed_hubs.is_empty()
        || !pr.new_layer_violations.is_empty()
    {
        out.push(Line::from(""));
        out.push(Line::styled("Risk flags", header_style(t)));
        for name in &pr.untested_changes {
            out.push(Line::styled(
                format!("  untested change: {name}"),
                Style::default().fg(t.warn),
            ));
        }
        for (name, count) in &pr.changed_hubs {
            out.push(Line::styled(
                format!("  hub change: {name} ({count} downstream)"),
                Style::default().fg(t.warn),
            ));
        }
        for (p, c) in &pr.new_layer_violations {
            out.push(Line::styled(
                format!("  new layer violation: {p} -> {c}"),
                Style::default().fg(t.danger),
            ));
        }
    }
    out
}

/// Clamp the diff modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_stats_scroll`]. Counts with the DEFAULT theme and the
/// SAME content width the draw path wraps to — the line count is
/// theme-independent but width-dependent (joined lists wrap).
pub fn clamp_diff_scroll(area: Rect, dv: &DiffView) -> usize {
    clamp_modal_scroll(
        area,
        diff_display_lines(dv, modal_content_width(area), &theme::DEFAULT).len(),
        dv.scroll,
    )
}

/// Draw the baseline-diff summary modal. `pub(crate)` for `render::draw`. The
/// diff rides inside the [`DiffView`] payload (snapshotted at open), so the
/// render layer never needs a `Dag`.
pub(crate) fn draw_diff(
    frame: &mut Frame,
    area: Rect,
    dv: &DiffView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        " Diff vs baseline - PR impact  (e: export, y/Y: copy CI cmd, Esc/q: close) ",
        diff_display_lines(dv, modal_content_width(area), t),
        dv.scroll,
        glyphs,
        t,
    );
}
