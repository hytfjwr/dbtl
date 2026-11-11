//! The `text` format — `dbtl check`'s default stdout shape. The historical
//! `scope:`/`PASS`/`FAIL`/`baselined` lines are kept verbatim (the gate
//! `message` fields are built to reproduce them byte-for-byte); the report
//! adds counted-finding lines (`file:line rule message`) indented under the
//! gate that failed or warned, plus `WARN`/`SKIP` statuses that used to be
//! impossible to express as plain lines.

use super::*;

/// Render the whole [`Report`] as the line-oriented text report.
pub fn render(report: &Report, _ctx: &RenderCtx<'_>) -> String {
    let mut out = format!("scope: {}\n", report.scope);
    for (i, gate) in report.gates.iter().enumerate() {
        if i == report.info_at {
            for line in &report.info {
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&gate_line(gate));
        // Findings and detail lines nest under the gate that produced them —
        // the tripped status is already on the gate line above.
        if matches!(gate.status, GateStatus::Fail | GateStatus::Warn) {
            for f in gate.counted() {
                out.push_str(&format!("  {}\n", finding_line(f)));
            }
            for d in &gate.details {
                out.push_str(&format!("  {d}\n"));
            }
        }
    }
    // An `info_at` past the end appends the context lines after all gates.
    if report.info_at >= report.gates.len() {
        for line in &report.info {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The gate's own status line — `PASS {title}: {message}` for a clean gate,
/// `{WARN|FAIL} {message} ({severity-tag})` for a tripped one (the message
/// carries the gate's name the way the pre-report lines did), `SKIP {title}:
/// {message}` for a gate that was requested but could not evaluate.
fn gate_line(gate: &GateResult) -> String {
    match gate.status {
        GateStatus::Pass => format!("PASS {}: {}\n", gate.title, gate.message),
        GateStatus::Skip => format!("SKIP {}: {}\n", gate.title, gate.message),
        GateStatus::Warn | GateStatus::Fail => format!(
            "{} {} ({})\n",
            gate.status.word(),
            gate.message,
            gate.severity_tag()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::report::samples::*;

    /// The line vocabulary: `PASS`/`SKIP` carry `title: message`, tripped
    /// gates carry the severity tag, and counted findings indent under
    /// their gate while baselined ones only count.
    #[test]
    fn gate_lines_and_nested_findings() {
        let out = render(&report(), &ctx(None));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "scope: root project only");
        assert!(lines.contains(&"PASS coverage: 5/5 (100%)"));
        assert!(lines.contains(&"SKIP breaking column changes: needs --diff"));
        assert!(lines.contains(&"FAIL lint root_models: 2 violation(s): a, b (error, 1 baselined)"));
        // Counted findings nest under the gate; the baselined one never prints.
        assert!(lines.contains(&"  models/a.sql:7 root_models a has no parents"));
        assert!(!out.contains("c has no parents"), "baselined only counts");
        assert!(lines.contains(&"WARN lint undocumented_models: 1 violation(s): d (warn)"));
        // A file-less finding anchors on the node uid.
        assert!(lines.contains(&"  model.proj.d undocumented_models d has no description"));
    }
}
