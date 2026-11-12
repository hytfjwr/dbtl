//! The `github` format — GitHub Actions workflow commands
//! (`::error file=…,line=…,title=…::…`) so findings land as file annotations
//! on the run and on PR diffs. Errors print before warnings (GitHub caps the
//! annotations it shows per step, so the worst problems must come first);
//! within a severity the report's own gate/finding order is kept. A trailing
//! `::notice` line carries the counts summary.

use super::*;

pub fn render(report: &Report, ctx: &RenderCtx<'_>) -> String {
    // (severity rank, emission sequence, line) — sorted stably by rank so all
    // errors precede all warnings without scrambling report order.
    let mut items: Vec<(u8, usize, String)> = Vec::new();
    let mut seq = 0usize;
    for gate in &report.gates {
        for f in gate.counted() {
            let (level, rank) = match f.severity {
                dbtl::Severity::Error => ("error", 0u8),
                dbtl::Severity::Warn => ("warning", 1u8),
            };
            let props = match &f.location {
                Some(loc) => {
                    // Every position property emits only when known — a
                    // file-scoped finding is a file-level annotation, never
                    // a guessed `line=1`.
                    let mut props = format!("file={}", escape_prop(&prefixed(&loc.file, ctx)));
                    if let Some(line) = loc.line {
                        props.push_str(&format!(",line={line}"));
                    }
                    if let Some(end_line) = loc.end_line {
                        props.push_str(&format!(",endLine={end_line}"));
                    }
                    if let Some(col) = loc.column {
                        props.push_str(&format!(",col={col}"));
                    }
                    if let Some(end_col) = loc.end_column {
                        props.push_str(&format!(",endColumn={end_col}"));
                    }
                    format!("{props},title={}", escape_prop(f.rule.name()))
                }
                None => format!("title={}", escape_prop(f.rule.name())),
            };
            items.push((
                rank,
                seq,
                format!("::{level} {props}::{}", escape_data(&f.message)),
            ));
            seq += 1;
        }
        // A tripped gate with no counted findings (coverage, docs drift, …)
        // still surfaces — as a location-free annotation carrying its id.
        if gate.counted().next().is_none() {
            match gate.status {
                GateStatus::Fail => {
                    items.push((
                        0,
                        seq,
                        format!(
                            "::error title={}::{}",
                            escape_prop(&gate.id),
                            escape_data(&gate.message)
                        ),
                    ));
                    seq += 1;
                }
                GateStatus::Warn => {
                    items.push((
                        1,
                        seq,
                        format!(
                            "::warning title={}::{}",
                            escape_prop(&gate.id),
                            escape_data(&gate.message)
                        ),
                    ));
                    seq += 1;
                }
                _ => {}
            }
        }
    }
    items.sort_by_key(|i| (i.0, i.1));

    let mut out = String::new();
    for (_, _, line) in &items {
        out.push_str(line);
        out.push('\n');
    }
    let s = &report.summary;
    out.push_str(&format!(
        "::notice title=dbtl::{} error(s), {} warning(s), {} baselined, {} skipped\n",
        s.errors, s.warnings, s.baselined, s.skipped
    ));
    out
}

fn prefixed(file: &str, ctx: &RenderCtx<'_>) -> String {
    match ctx.path_prefix {
        Some(prefix) => format!("{prefix}/{file}"),
        None => file.to_string(),
    }
}

/// The `::…::DATA` side: `%`, CR, and LF would corrupt the command framing.
pub fn escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// The `key=value` property side: data escapes plus `:` and `,` — the two
/// characters that terminate a property.
pub fn escape_prop(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::report::samples::*;

    /// Command escapes — data side `%` CR LF; property side adds `:` `,`.
    #[test]
    fn escapes_cover_each_special_char() {
        assert_eq!(escape_data("100%\r\nx"), "100%25%0D%0Ax");
        assert_eq!(escape_prop("a:b,c"), "a%3Ab%2Cc");
        assert_eq!(escape_prop("50%"), "50%25");
    }

    /// Errors precede warnings, a file-less finding keeps its `title`, a
    /// tripped gate without findings emits a location-free line, and the
    /// notice summarizes.
    #[test]
    fn ordering_and_line_shapes() {
        let out = render(&report(), &ctx(None));
        let lines: Vec<&str> = out.lines().collect();
        let err = lines.iter().position(|l| l.starts_with("::error")).unwrap();
        let warn = lines
            .iter()
            .position(|l| l.starts_with("::warning"))
            .unwrap();
        assert!(err < warn, "errors print before warnings: {out}");
        assert!(
            lines.contains(&"::error file=models/a.sql,line=7,endLine=7,col=3,endColumn=5,title=root_models::a has no parents"),
            "the full position properties: {out}"
        );
        // The file-less finding keeps the rule title without a location.
        assert!(lines.contains(&"::warning title=undocumented_models::d has no description"));
        // The skipped gate and the baselined finding emit nothing.
        assert!(!out.contains("breaking_columns"));
        assert!(!out.contains("c has no parents"));
        assert!(lines.last().unwrap().starts_with("::notice title=dbtl::"));
    }

    /// A file-scoped finding (`file` only, no line) is a file-level
    /// annotation — `file=` and `title=` with no position properties at
    /// all, never a guessed `line=1`.
    #[test]
    fn unpositioned_finding_is_a_file_level_annotation() {
        let mut r = Report::new("proj", "root project only");
        r.gates = vec![gate(
            "lint:root_models",
            "lint root_models",
            dbtl::Severity::Error,
            GateStatus::Fail,
            "lint root_models: 1 violation(s): a",
            vec![unpositioned(
                dbtl::LintRule::RootModels,
                "model.proj.a",
                "a has no parents",
                dbtl::Severity::Error,
                "models/a.sql",
                false,
            )],
        )];
        r.summary = Report::summarize(&r.gates);
        let out = render(&r, &ctx(None));
        assert!(
            out.contains("::error file=models/a.sql,title=root_models::a has no parents"),
            "file + title only: {out}"
        );
        assert!(!out.contains("line="), "no line guess: {out}");
    }

    /// A tripped gate with no counted findings (coverage, docs drift, …)
    /// still emits a location-free annotation keyed by its gate id — CI
    /// surfaces it on the run even though no file can be pointed at.
    #[test]
    fn finding_free_gate_emits_a_title_only_annotation() {
        let mut r = Report::new("proj", "root project only");
        r.gates = vec![
            GateResult::new(
                "coverage",
                "coverage",
                dbtl::Severity::Error,
                GateStatus::Fail,
                "coverage 33% below --coverage-min 90% (1/3)".to_string(),
            ),
            GateResult::new(
                "docs_drift",
                "docs drift",
                dbtl::Severity::Error,
                GateStatus::Warn,
                "docs drift: 2 difference(s)".to_string(),
            ),
            GateResult::skip(
                "breaking_columns",
                "breaking column changes",
                "needs --diff".into(),
            ),
        ];
        r.summary = Report::summarize(&r.gates);
        let out = render(&r, &ctx(None));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.contains(
                &"::error title=coverage::coverage 33%25 below --coverage-min 90%25 (1/3)"
            ),
            "the gate-level annotation (% escapes to %25): {out}"
        );
        // A WARN-status finding-free gate warns; the skipped gate emits none.
        assert!(lines.contains(&"::warning title=docs_drift::docs drift: 2 difference(s)"));
        assert!(!out.contains("breaking_columns"));
    }

    /// `--path-prefix` lands on the file property.
    #[test]
    fn path_prefix_applies() {
        let out = render(&report(), &ctx(Some("sub")));
        assert!(out.contains("file=sub/models/a.sql"));
    }
}
