//! The `junit` format — the xUnit XML every CI's test-result viewer parses.
//! One `<testcase>` per gate: `fail` becomes `<failure>` carrying the finding
//! list, `warn` stays a pass but records findings in `<system-out>`, `skip`
//! becomes `<skipped/>`. No timestamps or durations — the report is
//! deterministic.

use super::*;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    let failures = report
        .gates
        .iter()
        .filter(|g| g.status == GateStatus::Fail)
        .count();
    let skipped = report
        .gates
        .iter()
        .filter(|g| g.status == GateStatus::Skip)
        .count();

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");
    out.push_str(&format!(
        "  <testsuite name=\"dbtl {}\" tests=\"{}\" failures=\"{}\" skipped=\"{}\">\n",
        escape_attr(ctx.command),
        report.gates.len(),
        failures,
        skipped,
    ));
    for gate in &report.gates {
        let head = format!(
            "    <testcase classname=\"{}\" name=\"{}\"",
            escape_attr(ctx.command),
            escape_attr(&gate.id),
        );
        match gate.status {
            GateStatus::Skip => {
                out.push_str(&format!(
                    "{}>\n      <skipped message=\"{}\"/>\n    </testcase>\n",
                    head,
                    escape_attr(&gate.message),
                ));
            }
            GateStatus::Fail => {
                out.push_str(&format!(
                    "{}>\n      <failure>{}</failure>\n    </testcase>\n",
                    head,
                    escape_text(&body(gate)),
                ));
            }
            GateStatus::Warn => {
                let body = body(gate);
                if body.is_empty() {
                    out.push_str(&format!("{head}/>\n"));
                } else {
                    out.push_str(&format!(
                        "{}>\n      <system-out>{}</system-out>\n    </testcase>\n",
                        head,
                        escape_text(&body),
                    ));
                }
            }
            GateStatus::Pass => out.push_str(&format!("{head}/>\n")),
        }
    }
    out.push_str("  </testsuite>\n</testsuites>\n");
    out
}

/// The finding/detail listing a failing or warning testcase carries:
/// the gate message first, then one `file:line rule message` line each.
fn body(gate: &GateResult) -> String {
    let mut body = gate.message.clone();
    for f in gate.counted() {
        body.push('\n');
        body.push_str(&finding_line(f));
    }
    for d in &gate.details {
        body.push('\n');
        body.push_str(d);
    }
    body
}

/// XML-escape text/attr content: the five predefined entities plus control
/// characters XML 1.0 forbids outright (kept `\t`/`\n`/`\r`).
fn escape(s: &str, quote: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quote => out.push_str("&quot;"),
            '\'' if quote => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    escape(s, true)
}

fn escape_text(s: &str) -> String {
    escape(s, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::*;

    /// One testcase per gate; fail → `<failure>` carrying the finding list,
    /// warn → `<system-out>`, skip → `<skipped/>`; suite counters agree.
    #[test]
    fn one_testcase_per_gate_with_status_bodies() {
        let out = render(&report(), &ctx(None));
        assert_eq!(out.matches("<testcase").count(), 4, "one per gate");
        assert!(out.contains("classname=\"check\""));
        assert!(out.contains("name=\"lint:root_models\""));
        assert!(out.contains("<failure>"));
        assert!(out.contains("models/a.sql:7:3 root_models a has no parents"));
        assert!(!out.contains("c has no parents"), "baselined never lists");
        assert!(out.contains("<skipped message=\"needs --diff\"/>"));
        assert!(out.contains("<system-out>"));
        assert!(out.contains("tests=\"4\" failures=\"1\" skipped=\"1\""));
    }

    /// Adversarial content can't break the XML: `& < >` escape everywhere,
    /// quotes additionally in attributes, and XML-1.0-illegal control bytes
    /// are stripped.
    #[test]
    fn escaping_neutralizes_adversarial_content() {
        let mut r = Report::new("p", "s");
        r.gates = vec![samples::gate(
            "g\"'",
            "t",
            dbtl_core::Severity::Error,
            GateStatus::Fail,
            "m<&>",
            vec![samples::finding(
                dbtl_core::LintRule::RootModels,
                "model.p.x",
                "msg \u{0007}<&",
                dbtl_core::Severity::Error,
                Some("f.sql"),
                false,
            )],
        )];
        r.summary = Report::summarize(&r.gates);
        let out = render(&r, &ctx(None));
        assert!(!out.contains("m<&>"), "raw specials are escaped: {out}");
        assert!(out.contains("m&lt;&amp;&gt;"));
        assert!(out.contains("name=\"g&quot;&apos;\""), "attr quotes escape");
        assert!(!out.contains('\u{0007}'), "control chars strip: {out}");
    }
}
