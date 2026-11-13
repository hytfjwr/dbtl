//! The `concise` report — `dbtl lint` and `dbtl check --lint`'s
//! one-finding-per-line view, for editors' quickfix parsers and grep:
//! `<anchor>: <severity>[<rule>] <message>` over the counted lint findings
//! (the same order `text` renders its blocks in), then the same gate rows
//! and lint summary. Positions come from the finding alone — an unknown
//! `line`/`column` drops off the anchor rather than printing a guessed
//! `:1` — and only the `severity[rule]` tag carries color.

use super::*;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let mut sections = Vec::new();
    let lines: Vec<String> = text::sorted_lint_findings(report)
        .iter()
        .map(|f| finding_line(f, ctx))
        .collect();
    if !lines.is_empty() {
        sections.push(lines.join("\n"));
    }
    if ctx.command != "lint" {
        sections.push(text::gates_section(report, ctx));
    }
    if let Some(lint) = &report.lint {
        sections.push(text::summary_lines(report, lint, ctx.styled));
    }
    let mut out = sections.join("\n\n");
    out.push('\n');
    out
}

/// `<anchor>: error|warning[<rule>] <message>` — the anchor is the same
/// `file[:line[:column]]`/uid shape every format uses, the severity tag
/// the only styled part.
fn finding_line(f: &dbtl_core::Finding, ctx: &RenderCtx<'_>) -> String {
    let word = match f.severity {
        dbtl_core::Severity::Error => "error",
        dbtl_core::Severity::Warn => "warning",
    };
    let tag = format!("{word}[{}]", f.rule.name());
    let tag = if ctx.styled {
        text::paint(
            &tag,
            match f.severity {
                dbtl_core::Severity::Error => text::error_style(),
                dbtl_core::Severity::Warn => text::warn_style(),
            },
        )
    } else {
        tag
    };
    // `--path-prefix` is a github/sarif/codeclimate concern — like `text`,
    // `concise` prints the project-relative path verbatim.
    format!("{}: {tag} {}", finding_anchor(f, None), f.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::*;
    use std::collections::BTreeMap;

    /// A synthetic lint report with two positioned findings — one yml,
    /// one sql — whose concise lines and summary render byte-exactly.
    #[test]
    fn concise_lines_are_greppable_anchors() {
        let mut r = Report::new("p", "source (sample)");
        r.lint = Some(LintTally {
            pre_existing: 0,
            has_diff: false,
            skipped: Vec::new(),
            unparsed_models: Vec::new(),
        });
        for (rule, uid, msg, sev, file, line, col) in [
            (
                dbtl_core::LintRule::MissingPrimaryKeyTests,
                "model.p.fct_orders",
                "fct_orders has no primary-key test",
                dbtl_core::Severity::Error,
                "models/marts/schema.yml",
                4u32,
                11u32,
            ),
            (
                dbtl_core::LintRule::SelectStar,
                "model.p.stg_orders",
                "stg_orders uses select *",
                dbtl_core::Severity::Warn,
                "models/staging/stg_orders.sql",
                10u32,
                5u32,
            ),
        ] {
            let mut f = finding(rule, uid, msg, sev, Some(file), false);
            f.location = Some(dbtl_core::Location {
                file: file.to_string(),
                line: Some(line),
                column: Some(col),
                end_line: Some(line),
                end_column: Some(col + 5),
            });
            let mut g = GateResult::new(
                &format!("lint:{}", rule.name()),
                &format!("lint {}", rule.name()),
                sev,
                GateStatus::Fail,
                String::new(),
            );
            g.findings.push(f);
            r.gates.push(g);
        }
        let sources = BTreeMap::new();
        let ctx = RenderCtx {
            command: "lint",
            path_prefix: None,
            dag: None,
            sources: Some(&sources),
            styled: false,
        };
        assert_eq!(
            render(&r, &ctx),
            "models/marts/schema.yml:4:11: error[missing_primary_key_tests] fct_orders has no primary-key test\n\
             models/staging/stg_orders.sql:10:5: warning[select_star] stg_orders uses select *\n\
             \n\
             Found 1 error and 1 warning in 2 files\n\
             2 rules reported findings\n"
        );
    }
}
