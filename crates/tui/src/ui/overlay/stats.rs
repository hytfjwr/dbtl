use super::*;

// ---- stats dashboard modal ------------------------------------------------

/// Cap used for every worklist listing (orphans / violations); the overflow is
/// rolled into an explicit "+K more" line so the modal never floods.
const WORKLIST_CAP: usize = 10;

/// A bg-space mini-bar `Span`: `n` spaces styled with `color` as the background,
/// width-proportional to `value`/`max` over `width` cells (at least 1 cell when
/// `value > 0`). Pure spaces → ASCII in both glyph modes (ascii_guard-safe by
/// construction; no new glyph). `max == 0` yields an empty span.
fn mini_bar(value: usize, max: usize, width: usize, color: Color) -> Span<'static> {
    if max == 0 || value == 0 {
        return Span::raw(String::new());
    }
    let filled = ((value * width) / max).max(1).min(width);
    Span::styled(" ".repeat(filled), Style::default().bg(color))
}

/// The coverage gauge as a two-span run: a graded-colour FILL of spaces over a
/// dark track of spaces (the theme's `surface_hi`), `width` cells total, fill
/// width-proportional to `pct` (integer math). Grade: danger <40, warn <70,
/// ok ≥70. Spaces only — ASCII in both modes.
fn coverage_gauge(pct: usize, width: usize, t: &theme::Theme) -> Vec<Span<'static>> {
    let grade = if pct < 40 {
        t.danger
    } else if pct < 70 {
        t.warn
    } else {
        t.ok
    };
    let filled = (pct * width / 100).min(width);
    let empty = width - filled;
    let mut spans = Vec::new();
    if filled > 0 {
        spans.push(Span::styled(" ".repeat(filled), Style::default().bg(grade)));
    }
    if empty > 0 {
        spans.push(Span::styled(
            " ".repeat(empty),
            Style::default().bg(t.surface_hi),
        ));
    }
    spans
}

/// The stats-dashboard content as display lines, grouped into sections via
/// [`header_style`]. Visual bars are bg-styled SPACES (a gauge for coverage,
/// proportional mini-bars after counts) — never block / shade glyphs, which are
/// East-Asian-Ambiguous and would fail ascii_guard in BOTH modes. Percentages
/// are integer math (`StatsView` is `Eq`, no f64). This is the SINGLE source of
/// the modal's line list, so [`clamp_stats_scroll`] stays correct automatically.
fn stats_display_lines(sv: &StatsView, t: &theme::Theme) -> Vec<Line<'static>> {
    const BAR_W: usize = 16;
    let mut out: Vec<Line> = Vec::new();
    out.push(Line::from(format!("project: {}", sv.project)));

    out.push(Line::from(""));
    out.push(Line::styled("Resource counts:", header_style(t)));
    let rt_max = sv
        .by_resource_type
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (rt, count) in &sv.by_resource_type {
        out.push(Line::from(vec![
            Span::raw(format!("  {rt:<12} {count:<4} ")),
            mini_bar(*count, rt_max, BAR_W, t.bar_resource),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled("Materialization (models):", header_style(t)));
    let mat_max = sv
        .by_materialization
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (mat, count) in &sv.by_materialization {
        out.push(Line::from(vec![
            Span::raw(format!("  {mat:<12} {count:<4} ")),
            mini_bar(*count, mat_max, BAR_W, t.bar_materialization),
        ]));
    }

    out.push(Line::from(""));
    // Same coverage_gap base as the `t` lens and the status `cov` segment.
    out.push(Line::styled(
        "Test coverage (models / seeds / snapshots):",
        header_style(t),
    ));
    let pct = (sv.testable_tested * 100)
        .checked_div(sv.testable_total)
        .unwrap_or(0);
    let mut cov_spans = vec![Span::raw("  ")];
    cov_spans.extend(coverage_gauge(pct, 20, t));
    cov_spans.push(Span::raw(format!(
        " {}/{} tested ({pct}%)",
        sv.testable_tested, sv.testable_total
    )));
    out.push(Line::from(cov_spans));

    // Column-level coverage: the SAME `column_coverage_counts`
    // numbers `dbtl docs`' README quotes — declared columns carrying ≥1 test.
    out.push(Line::styled(
        "Column coverage (declared columns):",
        header_style(t),
    ));
    let cpct = (sv.col_tested * 100)
        .checked_div(sv.col_declared)
        .unwrap_or(0);
    let mut col_spans = vec![Span::raw("  ")];
    col_spans.extend(coverage_gauge(cpct, 20, t));
    col_spans.push(Span::raw(format!(
        " {}/{} columns tested ({cpct}%)",
        sv.col_tested, sv.col_declared
    )));
    out.push(Line::from(col_spans));

    out.push(Line::from(""));
    out.push(Line::styled("Top hubs (degree):", header_style(t)));
    let deg_max = sv.top_hubs.iter().map(|(_, _, d)| *d).max().unwrap_or(0);
    for (_, name, degree) in &sv.top_hubs {
        out.push(Line::from(vec![
            Span::raw(format!("  {name:<28} {degree:<4} ")),
            mini_bar(*degree, deg_max, BAR_W, t.bar_degree),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        "Hubs (transitive downstream):",
        header_style(t),
    ));
    let tr_max = sv
        .transitive_hubs
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(0);
    for (name, count) in &sv.transitive_hubs {
        out.push(Line::from(vec![
            Span::raw(format!("  {name:<28} {count:<4} ")),
            mini_bar(*count, tr_max, BAR_W, t.bar_transitive),
        ]));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Critical path (depth {}):", sv.critical_path.len()),
        header_style(t),
    ));
    for (i, name) in sv.critical_path.iter().take(WORKLIST_CAP).enumerate() {
        // Indent each hop one step further: the staircase reads as the chain
        // without any new glyph (pure spaces — ascii_guard-safe).
        out.push(Line::from(format!("  {}{name}", " ".repeat(i))));
    }
    if sv.critical_path.len() > WORKLIST_CAP {
        out.push(Line::from(format!(
            "  +{} more",
            sv.critical_path.len() - WORKLIST_CAP
        )));
    }

    // The runtime-weighted sibling: which chain costs wall-clock time, not
    // just depth. Only present when run_results.json loaded.
    if let Some((path, total)) = &sv.critical_path_runtime {
        out.push(Line::from(""));
        out.push(Line::styled(
            format!("Critical path by runtime ({total}):"),
            header_style(t),
        ));
        for (i, name) in path.iter().take(WORKLIST_CAP).enumerate() {
            out.push(Line::from(format!("  {}{name}", " ".repeat(i))));
        }
        if path.len() > WORKLIST_CAP {
            out.push(Line::from(format!("  +{} more", path.len() - WORKLIST_CAP)));
        }
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Orphans ({}):", sv.orphan_models.len()),
        header_style(t),
    ));
    for name in sv.orphan_models.iter().take(WORKLIST_CAP) {
        out.push(Line::from(format!("  {name}")));
    }
    if sv.orphan_models.len() > WORKLIST_CAP {
        out.push(Line::from(format!(
            "  +{} more",
            sv.orphan_models.len() - WORKLIST_CAP
        )));
    }

    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Layer violations ({}):", sv.layer_violations.len()),
        header_style(t),
    ));
    for (parent, child) in sv.layer_violations.iter().take(WORKLIST_CAP) {
        out.push(Line::from(format!("  {parent} -> {child}")));
    }
    if sv.layer_violations.len() > WORKLIST_CAP {
        out.push(Line::from(format!(
            "  +{} more",
            sv.layer_violations.len() - WORKLIST_CAP
        )));
    }

    // The static-lint worklist: per-rule node names from the same `run_rules`
    // findings `dbtl check --lint` gates on — so the dashboard can never
    // disagree with the CI gate. Each rule's names cap like the other lists.
    let lint_total: usize = sv.lint_findings.iter().map(|(_, names)| names.len()).sum();
    out.push(Line::from(""));
    out.push(Line::styled(
        format!("Lint findings ({lint_total}):"),
        header_style(t),
    ));
    if lint_total == 0 {
        out.push(Line::from("  none"));
    }
    for (rule, names) in &sv.lint_findings {
        out.push(Line::from(format!("  {rule} ({}):", names.len())));
        for name in names.iter().take(WORKLIST_CAP) {
            out.push(Line::from(format!("    {name}")));
        }
        if names.len() > WORKLIST_CAP {
            out.push(Line::from(format!(
                "    +{} more",
                names.len() - WORKLIST_CAP
            )));
        }
    }

    out.push(Line::from(""));
    out.push(Line::styled("Warnings:", header_style(t)));
    out.push(Line::from(format!(
        "  untested (model/seed/snapshot): {}",
        sv.untested_testable
    )));
    out.push(Line::from(format!(
        "  models with no downstream:      {}",
        sv.zero_downstream_models
    )));
    out.push(Line::from(format!(
        "  models with no description:     {}",
        sv.no_description_models
    )));
    // Same number `dbtl columns` reports on stderr; the detail modal flags the
    // individual models with `[name-match]`.
    out.push(Line::from(format!(
        "  models resolved by name-match:  {}",
        sv.cll_fallbacks
    )));

    out
}

/// Clamp the stats modal's scroll offset to its content/viewport (size-aware),
/// mirroring [`clamp_detail_scroll`]. Counts with the DEFAULT theme — the line
/// count is theme-independent.
pub fn clamp_stats_scroll(area: Rect, sv: &StatsView) -> usize {
    clamp_modal_scroll(
        area,
        stats_display_lines(sv, &theme::DEFAULT).len(),
        sv.scroll,
    )
}

/// Draw the stats-dashboard modal. `pub(crate)` for `render::draw`. The stats
/// ride inside the [`StatsView`] payload (computed at open), so the render layer
/// never needs a `Dag`.
pub(crate) fn draw_stats(
    frame: &mut Frame,
    area: Rect,
    sv: &StatsView,
    glyphs: crate::GlyphMode,
    t: &theme::Theme,
) {
    draw_scrollable_modal(
        frame,
        area,
        format!(" Stats - {}  (Esc/q to close, j/k to scroll) ", sv.project),
        stats_display_lines(sv, t),
        sv.scroll,
        glyphs,
        t,
    );
}
