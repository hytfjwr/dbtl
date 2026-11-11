//! The `markdown` format — the PR-comment / step-summary shape: a hidden
//! `<!-- dbtl:report -->` marker (so the Action can find and update its own
//! comment), a gate summary table, per-gate finding sections capped at 50
//! items each, then the shared PR-impact pack and docs-drift listings.

use super::*;

/// Findings a per-gate section lists before collapsing to `+K more`.
const SECTION_CAP: usize = 50;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let mut out = String::from("<!-- dbtl:report -->\n");
    out.push_str(&format!("# dbtl {} report\n\n", ctx.command));
    out.push_str(&format!("**scope:** {}\n\n", report.scope));
    for line in &report.info {
        out.push_str(line);
        out.push('\n');
    }
    if !report.info.is_empty() {
        out.push('\n');
    }

    // The summary table — one row per gate, in evaluation order.
    out.push_str("| Gate | Severity | Status | Message |\n");
    out.push_str("| --- | --- | --- | --- |\n");
    for gate in &report.gates {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            gate.title,
            gate.severity.name(),
            gate.status.json(),
            escape_cell(&gate.message),
        ));
    }
    let s = &report.summary;
    out.push_str(&format!(
        "\n**summary:** {} error(s), {} warning(s), {} baselined, {} skipped\n",
        s.errors, s.warnings, s.baselined, s.skipped
    ));

    // A section per gate that surfaced anything — counted findings listed,
    // baselined findings reduced to a count.
    for gate in &report.gates {
        if gate.findings.is_empty() && gate.details.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {}\n\n", gate.title));
        let counted: Vec<&dbtl::Finding> = gate.counted().collect();
        for f in counted.iter().take(SECTION_CAP) {
            out.push_str(&format!("- {}\n", finding_line(f)));
        }
        if counted.len() > SECTION_CAP {
            out.push_str(&format!("- _+{} more_\n", counted.len() - SECTION_CAP));
        }
        for d in &gate.details {
            out.push_str(&format!("- {d}\n"));
        }
        let baselined = gate.findings.iter().filter(|f| f.baselined).count();
        if baselined > 0 {
            out.push_str(&format!("- _{baselined} baselined_\n"));
        }
    }

    // The PR impact pack embeds verbatim — `pr_impact_markdown` is the single
    // source for this document and its `# PR Impact Report` heading doubles
    // as the section heading here.
    if let Some(impact) = &report.impact {
        out.push_str("\n---\n\n");
        out.push_str(&dbtl::pr_impact_markdown(&report.project, impact));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    if let Some(drift) = &report.docs_drift {
        out.push_str("\n## Docs drift\n\n");
        let lines = drift.lines();
        for line in lines.iter().take(SECTION_CAP) {
            out.push_str(&format!("- {line}\n"));
        }
        if lines.len() > SECTION_CAP {
            out.push_str(&format!("- _+{} more_\n", lines.len() - SECTION_CAP));
        }
    }
    out
}

/// A `|` inside a message would split the table row — escape it. Messages
/// are single-line by construction, so no other markdown needs neutralizing.
fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::report::samples::*;

    /// The document contract: marker first, summary table, per-gate sections
    /// capped at 50 listed findings with `+K more`, baselined reduced to a
    /// count.
    #[test]
    fn marker_table_sections_and_cap() {
        let mut r = report();
        // Push the failing gate past the cap: 51 counted findings, keeping
        // the kitchen-sink baselined one so both contracts show at once.
        let mut many: Vec<dbtl::Finding> = (0..51)
            .map(|i| {
                finding(
                    dbtl::LintRule::RootModels,
                    &format!("model.proj.n{i}"),
                    &format!("n{i} has no parents"),
                    dbtl::Severity::Error,
                    Some("models/n.sql"),
                    false,
                )
            })
            .collect();
        many.push(finding(
            dbtl::LintRule::RootModels,
            "model.proj.c",
            "c has no parents",
            dbtl::Severity::Error,
            Some("models/c.sql"),
            true,
        ));
        r.gates[1].findings = many;
        r.summary = Report::summarize(&r.gates);
        let out = render(&r, &ctx(None));
        assert!(
            out.starts_with("<!-- dbtl:report -->\n"),
            "the marker leads"
        );
        assert!(out.contains("| Gate | Severity | Status | Message |"));
        assert!(out.contains("## lint root_models"));
        assert!(out.contains("+1 more"), "the 51st finding collapses: {out}");
        assert!(out.contains("1 baselined"), "baselined counts only");
        assert!(!out.contains("c has no parents"), "baselined never lists");
        // No --diff / --docs-dir → no Impact / Docs drift sections.
        assert!(!out.contains("PR Impact") && !out.contains("Docs drift"));
    }

    /// `report.impact` embeds the shared PR-impact document verbatim — what
    /// `dbtl impact` and the step summary print is a substring of this render.
    #[test]
    fn impact_embeds_the_shared_document() {
        let mut r = report();
        r.impact = Some(dbtl::DiffReport {
            baseline: "base".into(),
            scope: dbtl::PackageScope::Root,
            ..Default::default()
        });
        let expected = dbtl::pr_impact_markdown(&r.project, r.impact.as_ref().unwrap());
        let out = render(&r, &ctx(None));
        assert!(
            out.contains(expected.trim_end()),
            "the impact pack embeds verbatim"
        );
    }

    /// `report.docs_drift` renders a `## Docs drift` section listing the
    /// `missing:`/`changed:`/`stale:` divergence lines.
    #[test]
    fn docs_drift_lists_the_divergence() {
        let mut r = report();
        r.docs_drift = Some(DocsDrift {
            missing: vec!["models/gone.md".into()],
            changed: vec!["README.md".into()],
            stale: vec!["models/old.md".into()],
        });
        let out = render(&r, &ctx(None));
        assert!(out.contains("## Docs drift"), "the section heads: {out}");
        assert!(out.contains("- changed: README.md"));
        assert!(out.contains("- missing: models/gone.md"));
        assert!(out.contains("- stale: models/old.md"));
    }
}
