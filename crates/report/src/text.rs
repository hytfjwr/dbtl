//! The `text` report — `dbtl lint` and `dbtl check --lint`'s human view.
//!
//! Each counted lint finding renders as an annotated block: a
//! `severity[rule]: message` header, a `-->` position line, the source
//! excerpt with underlines when the source resolves (`.sql` from the dag's
//! `raw_code`, `.yml` from the project root — both already in the
//! report's `sources` map), then `= help:`/`= docs:`/`= note:` footers.
//! `dbtl check` prints those blocks first, then its gate rows — the
//! non-lint rows keep the gate's own message (the same wording the machine
//! formats carry), while lint rows are built here from the gate's
//! structured counts so the node-name lists stay off the lines — and the
//! lint summary closes the report.

use super::*;
use annotate_snippets::{AnnotationKind, Element, Level, Origin, Renderer, Snippet};
use anstyle::{AnsiColor, Effects, Style};
use std::collections::BTreeSet;
use std::ops::Range;

/// The width the excerpt renderer folds long source lines at — fixed so
/// the fold is byte-identical on every terminal.
const SNIPPET_TERM_WIDTH: usize = 200;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let mut sections = Vec::new();
    let findings = sorted_lint_findings(report);
    if !findings.is_empty() {
        sections.push(
            findings
                .iter()
                .map(|f| finding_block(f, ctx))
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    if ctx.command != "lint" {
        sections.push(gates_section(report, ctx));
    }
    if let Some(lint) = &report.lint {
        sections.push(summary_lines(report, lint, ctx.styled));
    }
    let mut out = sections.join("\n\n");
    out.push('\n');
    out
}

/// A `lint:*` gate — the `text`/`concise` formats treat its findings as
/// the report's real payload; everything else stays a gate row.
pub(crate) fn is_lint_gate(gate: &GateResult) -> bool {
    gate.id.starts_with("lint:")
}

/// The counted lint findings across the `lint:*` gates, in display order:
/// positioned findings by (file, line, column, rule, node uid), then every
/// position-less finding by (node uid, rule) — deterministic across runs.
pub(crate) fn sorted_lint_findings(report: &Report) -> Vec<&dbtl_core::Finding> {
    let mut findings: Vec<&dbtl_core::Finding> = report
        .gates
        .iter()
        .filter(|g| is_lint_gate(g))
        .flat_map(|g| g.counted())
        .collect();
    let (mut positioned, mut positionless): (Vec<_>, Vec<_>) = findings
        .drain(..)
        .partition(|f| f.location.as_ref().is_some_and(|l| l.line.is_some()));
    positioned.sort_by_key(|f| {
        let loc = f.location.as_ref().expect("positioned");
        (
            loc.file.as_str(),
            loc.line.unwrap_or(u32::MAX),
            loc.column.unwrap_or(u32::MAX),
            f.rule.name(),
            f.node_uid.as_str(),
        )
    });
    positionless.sort_by_key(|f| (f.node_uid.as_str(), f.rule.name()));
    positioned.extend(positionless);
    positioned
}

/// `dbtl check`'s gate rows: `scope:` first, every non-lint gate as a
/// `PASS`/`WARN`/`FAIL`/`SKIP` row carrying the gate's own message (with
/// its `details` indented underneath when the gate tripped). Lint gates
/// get structured rows — the node-name lists the machine formats carry
/// stay off — and the all-clean lint rules collapse into one summary row
/// instead of a `PASS` line each.
pub(crate) fn gates_section(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let mut lines = vec![format!("scope: {}", report.scope)];
    let mut clean_rules = 0usize;
    for (i, gate) in report.gates.iter().enumerate() {
        if i == report.info_at {
            lines.extend(report.info.iter().cloned());
        }
        if is_lint_gate(gate) {
            match gate.status {
                GateStatus::Pass => {
                    clean_rules += 1;
                }
                GateStatus::Warn | GateStatus::Fail => {
                    lines.push(lint_gate_row(gate, report, ctx.styled));
                }
                GateStatus::Skip => lines.push(gate_row(gate, ctx.styled)),
            }
            continue;
        }
        lines.push(gate_row(gate, ctx.styled));
        if matches!(gate.status, GateStatus::Warn | GateStatus::Fail) {
            lines.extend(gate.details.iter().map(|d| format!("  {d}")));
        }
    }
    if report.info_at >= report.gates.len() {
        lines.extend(report.info.iter().cloned());
    }
    if clean_rules > 0 {
        lines.push(format!(
            "{} lint: {} clean",
            status_word(GateStatus::Pass, ctx.styled),
            counted(clean_rules, "rule", "rules"),
        ));
    }
    lines.join("\n")
}

/// The lint summary: counted findings split by severity with the file
/// tally, baselined and `--diff`-suppressed tallies in parentheses, then
/// the reported/skipped rule line — and the unparsable-SQL line naming the
/// models whose SQL wouldn't parse when [`LintTally::unparsed_models`]
/// lands (ten shown, a `+K more` tail beyond).
pub(crate) fn summary_lines(report: &Report, lint: &LintTally, styled: bool) -> String {
    let (mut errors, mut warnings, mut baselined, mut reported_rules) =
        (0usize, 0usize, 0usize, 0usize);
    let mut files = BTreeSet::new();
    for g in report.gates.iter().filter(|g| is_lint_gate(g)) {
        let mut reported = false;
        for f in &g.findings {
            if f.baselined {
                baselined += 1;
                continue;
            }
            reported = true;
            match f.severity {
                dbtl_core::Severity::Error => errors += 1,
                dbtl_core::Severity::Warn => warnings += 1,
            }
            if let Some(loc) = &f.location {
                files.insert(loc.file.as_str());
            }
        }
        if reported {
            reported_rules += 1;
        }
    }
    let mut first = if errors + warnings == 0 {
        "No findings".to_string()
    } else {
        let mut s = format!(
            "Found {} and {}",
            counted(errors, "error", "errors"),
            counted(warnings, "warning", "warnings")
        );
        if !files.is_empty() {
            s += &format!(" in {}", counted(files.len(), "file", "files"));
        }
        s
    };
    if baselined > 0 {
        first += &format!(" ({baselined} baselined)");
    }
    if lint.pre_existing > 0 {
        first += &format!(" ({} pre-existing, not counted)", lint.pre_existing);
    }
    if styled {
        first = paint(
            &first,
            if errors > 0 {
                error_style()
            } else if warnings > 0 {
                warn_style()
            } else {
                pass_style()
            },
        );
    }
    let mut second = format!(
        "{} reported findings",
        counted(reported_rules, "rule", "rules")
    );
    if !lint.skipped.is_empty() {
        // One reason per missing-input kind — a skipped rule never has a
        // different short reason than its peers.
        let mut reasons: Vec<&'static str> = Vec::new();
        for s in &lint.skipped {
            let r = skip_reason(s);
            if !reasons.contains(&r) {
                reasons.push(r);
            }
        }
        second += &format!("; {} skipped ({})", lint.skipped.len(), reasons.join(", "));
    }
    let mut lines = vec![first, second];
    if !lint.unparsed_models.is_empty() {
        // Name up to 10 skipped models — a wall of names would bury the
        // exit-relevant tally; the `+K more` tail keeps the count honest.
        const SHOWN: usize = 10;
        let shown = lint.unparsed_models.len().min(SHOWN);
        let mut line = format!(
            "{} skipped SQL rules (unparsable SQL): {}",
            counted(lint.unparsed_models.len(), "model", "models"),
            lint.unparsed_models[..shown].join(", "),
        );
        let hidden = lint.unparsed_models.len() - shown;
        if hidden > 0 {
            line += &format!(" +{hidden} more");
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// The summary's short reason for a skipped lint rule — the actionable
/// hint in parentheses on the rule line. The full "requires graph + X
/// input" phrasing stays on the SKIP gate row and in `--explain`.
fn skip_reason(s: &dbtl_core::SkippedRule) -> &'static str {
    match s.missing {
        dbtl_core::Requires::Sql => "need parsed SQL",
        dbtl_core::Requires::Catalog => "need --catalog",
        dbtl_core::Requires::Graph => "need a graph",
    }
}

/// One non-lint gate row — the shape the report has always printed: the
/// status word, the gate title + message (`severity`-suffixed for a
/// tripped gate).
fn gate_row(gate: &GateResult, styled: bool) -> String {
    let word = status_word(gate.status, styled);
    match gate.status {
        GateStatus::Pass | GateStatus::Skip => {
            format!("{word} {}: {}", gate.title, gate.message)
        }
        GateStatus::Warn | GateStatus::Fail => {
            format!("{word} {} ({})", gate.message, gate.severity_tag())
        }
    }
}

/// A tripped lint gate row built from the gate's structured values —
/// `FAIL lint <rule>: <N> [new ]finding(s)[ (<B> baselined)]` — so the
/// node-name list the gate's `message` carries for the machine formats
/// never reaches the line.
fn lint_gate_row(gate: &GateResult, report: &Report, styled: bool) -> String {
    let n = gate.counted().count();
    let baselined = gate.findings.iter().filter(|f| f.baselined).count();
    let mut row = format!(
        "{} {}: {n}{} {}",
        status_word(gate.status, styled),
        gate.title,
        if report.lint.as_ref().is_some_and(|l| l.has_diff) {
            " new"
        } else {
            ""
        },
        if n == 1 { "finding" } else { "findings" },
    );
    if baselined > 0 {
        row += &format!(" ({baselined} baselined)");
    }
    row
}

/// One finding's annotated block — the `annotate-snippets` group carrying
/// the header, position, excerpt+underlines when the source and position
/// both resolve, and the help/docs/note footers.
fn finding_block(f: &dbtl_core::Finding, ctx: &RenderCtx<'_>) -> String {
    let level = match f.severity {
        dbtl_core::Severity::Error => Level::ERROR,
        dbtl_core::Severity::Warn => Level::WARNING,
    };
    let mut group = level
        .primary_title(f.message.clone())
        .id(f.rule.name())
        .element(position_element(f, ctx));
    group = group.element(Level::HELP.message(f.effective_help().to_string()));
    group = group.element(Level::HELP.with_name("docs").message(f.rule.docs_url()));
    if let Some(note) = note_body(f, ctx) {
        group = group.element(Level::NOTE.message(note));
    }
    renderer(ctx).render(&[group])
}

/// The finding's position element: the source excerpt when its file's
/// text AND a pinnable span both resolve, else the bare `-->` origin —
/// file + recorded position, or the node uid for a file-less finding.
/// Nothing is invented: a missing file or an unresolvable span degrades
/// to the origin alone.
fn position_element<'a>(f: &'a dbtl_core::Finding, ctx: &'a RenderCtx<'a>) -> Element<'a> {
    if let Some(s) = snippet(f, ctx) {
        return s.into();
    }
    let mut origin = match &f.location {
        Some(loc) => Origin::path(loc.file.clone()),
        None => Origin::path(f.node_uid.clone()),
    };
    if let Some(loc) = &f.location {
        if let Some(line) = loc.line {
            origin = origin.line(line as usize);
        }
        if let Some(column) = loc.column {
            origin = origin.char_column(column as usize);
        }
    }
    origin.into()
}

/// The excerpt the finding's position underlines — `None` unless the
/// location carries a line+column AND `ctx.sources` has the file's text
/// AND the span pins inside it. The snippet carries only the lines the
/// annotations touch — the renderer folds unannotated lines anyway, so
/// a large `.yml` isn't re-indexed for every finding that lands in it.
fn snippet<'a>(
    f: &'a dbtl_core::Finding,
    ctx: &'a RenderCtx<'a>,
) -> Option<Snippet<'a, annotate_snippets::Annotation<'a>>> {
    let loc = f.location.as_ref()?;
    let _ = (loc.line?, loc.column?);
    let src = ctx.sources?.get(&loc.file)?;
    let primary = primary_span(src, loc)?;
    let mut first = src.line_of_byte(primary.start);
    let mut last = src.line_of_byte(primary.end);
    let mut labels = Vec::new();
    for l in &f.labels {
        if let Some(span) = label_span(src, l) {
            first = first.min(src.line_of_byte(span.start));
            last = last.max(src.line_of_byte(span.end));
            labels.push((span, l));
        }
    }
    let start = src.line_span(first)?.start;
    let end = src.line_span(last)?.end;
    let text = &src.text[start..end];
    let mut primary = AnnotationKind::Primary.span(primary.start - start..primary.end - start);
    if let Some(label) = f.label.clone() {
        primary = primary.label(label);
    }
    let mut s = Snippet::source(text)
        .line_start(first as usize)
        .path(loc.file.clone())
        .annotation(primary);
    for (span, l) in labels {
        s = s.annotation(
            AnnotationKind::Context
                .span(span.start - start..span.end - start)
                .label(l.message.clone()),
        );
    }
    Some(s)
}

/// The byte range the finding's position covers inside `src`: the full
/// `(line, column)`–`(end_line, end_column)` span when both ends pin,
/// else the single char under the start — `None` only when the start
/// itself can't be pinned (a position that doesn't exist underlines
/// nothing).
fn primary_span(src: &SourceText, loc: &dbtl_core::Location) -> Option<Range<usize>> {
    let (line, column) = (loc.line?, loc.column?);
    let start = src.position_byte(line, column)?;
    let point = src
        .position_byte(line, column + 1)
        .unwrap_or_else(|| start + src.text[start..].chars().next().map_or(0, |c| c.len_utf8()));
    let end = match (loc.end_line, loc.end_column) {
        (Some(el), Some(ec)) => src.position_byte(el, ec).filter(|e| *e > start),
        _ => None,
    };
    Some(start..end.unwrap_or(point))
}

/// The byte range a secondary label covers: its `(line, column)`–
/// `(end_line, end_column)` span (defaulting the end to one char past the
/// start when only the start is given), else the whole line when the
/// label is line-level.
fn label_span(src: &SourceText, l: &dbtl_core::Label) -> Option<Range<usize>> {
    match l.column {
        Some(c) => {
            let end = match (l.end_line, l.end_column) {
                (Some(el), Some(ec)) => (el, ec),
                _ => (l.line, c + 1),
            };
            span_between(src, (l.line, c), end)
        }
        None => src.line_span(l.line),
    }
}

/// The byte range `(line, column)`–`(end_line, end_column)` covers inside
/// `src` — all positions 1-based `char`s, the end exclusive — `None`
/// when either end can't be pinned exactly or the range would be empty.
fn span_between(
    src: &SourceText,
    (sl, sc): (u32, u32),
    (el, ec): (u32, u32),
) -> Option<Range<usize>> {
    let start = src.position_byte(sl, sc)?;
    let end = src.position_byte(el, ec)?;
    (end > start).then_some(start..end)
}

/// The `= note:` body — the related node names the message doesn't
/// already mention, resolved through the dag, up to five then
/// ` and N more`. `None` when there's nothing to add.
fn note_body(f: &dbtl_core::Finding, ctx: &RenderCtx<'_>) -> Option<String> {
    if f.related.is_empty() {
        return None;
    }
    let mut names = Vec::new();
    for uid in &f.related {
        let name = ctx
            .dag
            .and_then(|d| d.get(uid))
            .map_or(uid.as_str(), |n| n.name.as_str());
        if !f.message.contains(name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        return None;
    }
    let shown = names.iter().take(5).copied().collect::<Vec<_>>().join(", ");
    Some(if names.len() > 5 {
        format!("{shown} and {} more", names.len() - 5)
    } else {
        shown
    })
}

/// `1 <one>` / `N <many>` — the report's pluralization in one place.
pub(crate) fn counted(n: usize, one: &'static str, many: &'static str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The gate status word — `PASS`/`WARN`/`FAIL`/`SKIP` — styled for the
/// colored output, plain otherwise.
pub(crate) fn status_word(status: GateStatus, styled: bool) -> String {
    let word = status.word();
    if !styled {
        return word.to_string();
    }
    let style = match status {
        GateStatus::Pass => pass_style(),
        GateStatus::Warn => warn_style(),
        GateStatus::Fail => AnsiColor::Red.on_default(),
        GateStatus::Skip => Style::new().effects(Effects::DIMMED),
    };
    paint(word, style)
}

/// `text` with `style`'s ANSI codes — used where the hand-built lines
/// carry their own color (the annotate-snippets renderer styles its own
/// elements).
pub(crate) fn paint(text: &str, style: Style) -> String {
    format!("{style}{text}{style:#}")
}

pub(crate) fn error_style() -> Style {
    AnsiColor::BrightRed.on_default().effects(Effects::BOLD)
}

pub(crate) fn warn_style() -> Style {
    AnsiColor::Yellow.on_default().effects(Effects::BOLD)
}

fn pass_style() -> Style {
    AnsiColor::Green.on_default()
}

/// The excerpt renderer: ANSI-styled for the colored output, plain
/// everywhere else (report files, `--color never`, machine formats never
/// reach it). The fixed `term_width` keeps long-line folds deterministic.
fn renderer(ctx: &RenderCtx<'_>) -> Renderer {
    if ctx.styled {
        Renderer::styled().term_width(SNIPPET_TERM_WIDTH)
    } else {
        Renderer::plain().term_width(SNIPPET_TERM_WIDTH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::*;

    /// `position_byte` maps a 1-based (line, char-column) to a byte offset
    /// across `\n` and `\r\n` endings, and refuses out-of-range positions.
    #[test]
    fn position_byte_pins_char_columns() {
        let text = SourceText::new("a\r\nb\u{00e9}c\nd\n".to_string());
        assert_eq!(text.position_byte(1, 1), Some(0));
        assert_eq!(text.position_byte(2, 1), Some(3), "past the \\r\\n");
        assert_eq!(
            text.position_byte(2, 3),
            Some(6),
            "é is one char, two bytes"
        );
        assert_eq!(text.position_byte(3, 1), Some(8));
        assert_eq!(text.position_byte(4, 1), None, "no fourth line's start");
        assert_eq!(text.position_byte(1, 5), None, "past the line end");
        assert_eq!(text.position_byte(0, 1), None);
    }

    /// A synthetic yml+sql finding set renders the byte-exact excerpt
    /// block — the position, underline, and footers all land where the
    /// position says.
    #[test]
    fn finding_blocks_render_the_annotated_shape() {
        let mut r = Report::new("p", "source (sample)");
        r.lint = Some(LintTally {
            pre_existing: 0,
            has_diff: false,
            skipped: Vec::new(),
            unparsed_models: Vec::new(),
        });
        let yml = "- name: fct_orders\n  columns:\n    - name: order_id\n";
        let sql = "select a,\n       b.*\nfrom t\n";
        let mut f = finding(
            dbtl_core::LintRule::MissingPrimaryKeyTests,
            "model.p.fct_orders",
            "fct_orders has no primary-key test",
            dbtl_core::Severity::Error,
            Some("models/marts/schema.yml"),
            false,
        );
        f.location = Some(dbtl_core::Location {
            file: "models/marts/schema.yml".to_string(),
            line: Some(1),
            column: Some(9),
            end_line: Some(1),
            end_column: Some(19),
        });
        f.label = Some("declared here".to_string());
        r.gates.push(GateResult::new(
            "lint:missing_primary_key_tests",
            "lint missing_primary_key_tests",
            dbtl_core::Severity::Error,
            GateStatus::Fail,
            "lint missing_primary_key_tests: 1 violation(s): fct_orders".to_string(),
        ));
        r.gates[0].findings.push(f);
        // A .sql finding with a pinned span — positions on .sql files
        // precede the work that assigns them, so the test builds its own.
        let mut f = finding(
            dbtl_core::LintRule::SelectStar,
            "model.p.stg_orders",
            "stg_orders's final SELECT uses select * / t.*",
            dbtl_core::Severity::Warn,
            Some("models/staging/stg_orders.sql"),
            false,
        );
        f.location = Some(dbtl_core::Location {
            file: "models/staging/stg_orders.sql".to_string(),
            line: Some(2),
            column: Some(10),
            end_line: Some(2),
            end_column: Some(11),
        });
        f.label = Some("star here".to_string());
        r.gates.push(GateResult::new(
            "lint:select_star",
            "lint select_star",
            dbtl_core::Severity::Warn,
            GateStatus::Warn,
            "lint select_star: 1 violation(s): stg_orders".to_string(),
        ));
        r.gates[1].findings.push(f);

        let mut sources = std::collections::BTreeMap::new();
        sources.insert(
            "models/marts/schema.yml".to_string(),
            SourceText::new(yml.to_string()),
        );
        sources.insert(
            "models/staging/stg_orders.sql".to_string(),
            SourceText::new(sql.to_string()),
        );
        let ctx = RenderCtx {
            command: "lint",
            path_prefix: None,
            dag: None,
            sources: Some(&sources),
            styled: false,
        };
        let out = render(&r, &ctx);
        assert_eq!(
            out,
            "error[missing_primary_key_tests]: fct_orders has no primary-key test\n\
             \x20--> models/marts/schema.yml:1:9\n\
             \x20 |\n\
             1 | - name: fct_orders\n\
             \x20 |         ^^^^^^^^^^ declared here\n\
             \x20 |\n\
             \x20 = help: add unique and not_null tests to one key column, or a unique_combination_of_columns test\n\
             \x20 = docs: https://hytfjwr.github.io/dbtl/reference/lint-rules/#missing_primary_key_tests\n\
             \n\
             warning[select_star]: stg_orders's final SELECT uses select * / t.*\n\
             \x20--> models/staging/stg_orders.sql:2:10\n\
             \x20 |\n\
             2 |        b.*\n\
             \x20 |          ^ star here\n\
             \x20 |\n\
             \x20 = help: list the columns the model produces\n\
             \x20 = docs: https://hytfjwr.github.io/dbtl/reference/lint-rules/#select_star\n\
             \n\
             Found 1 error and 1 warning in 2 files\n\
             2 rules reported findings\n",
            "the annotated excerpts + summary render byte-exactly"
        );
    }

    /// The unparsed-SQL summary line names the skipped models — up to ten
    /// in the tally's `unique_id` order, then a `+K more` tail — and stays
    /// absent when every model's SQL parsed.
    #[test]
    fn summary_names_up_to_ten_unparsable_models() {
        let report = Report::new("p", "source (x)");
        let lint = |names: Vec<&str>| LintTally {
            unparsed_models: names.iter().map(|s| s.to_string()).collect(),
            ..LintTally::default()
        };
        // ≤ 10 — every name on the line.
        assert_eq!(
            summary_lines(&report, &lint(vec!["a", "b", "c"]), false),
            "No findings\n\
             0 rules reported findings\n\
             3 models skipped SQL rules (unparsable SQL): a, b, c"
        );
        // > 10 — the first ten then the hidden count.
        let names: Vec<String> = (0..13).map(|i| format!("m{i:02}")).collect();
        let l = LintTally {
            unparsed_models: names,
            ..LintTally::default()
        };
        assert_eq!(
            summary_lines(&report, &l, false),
            "No findings\n\
             0 rules reported findings\n\
             13 models skipped SQL rules (unparsable SQL): \
             m00, m01, m02, m03, m04, m05, m06, m07, m08, m09 +3 more"
        );
        // None — no line at all.
        assert_eq!(
            summary_lines(&report, &LintTally::default(), false),
            "No findings\n0 rules reported findings"
        );
    }
}
