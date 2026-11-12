//! The shared machine report behind `dbtl check` and `dbtl lint`: one
//! [`Report`] model (`dbtl.report/v1`) describing the run — the gates that
//! evaluated, their findings, the diff/docs payloads — rendered by one pure
//! function per output format. `check` used to hand-build a text line list
//! and a parallel JSON shape in lockstep; both now derive from this single
//! structure, so the console, the CI annotations, and the uploaded files can
//! never disagree about what the gates said.

mod codeclimate;
mod github;
mod json;
mod junit;
mod markdown;
mod sarif;
mod text;

use super::*;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

/// The schema marker every JSON-shaped report carries — lets a consumer pin
/// the layout version before reading fields.
pub const REPORT_SCHEMA: &str = "dbtl.report/v1";

/// One gate's outcome inside a [`Report`]. `id` is the stable machine name
/// (`coverage`, `lint:root_models`, `docs_drift` — the values junit `name`
/// attributes and GitHub `title=` properties use); `title` is the display
/// label the text/markdown rows lead with. `severity` is the gate's own
/// severity (`error` for the fixed gates, the rule's configured severity for
/// lint gates) — distinct from `status`, which applies the `--fail-on`
/// threshold (a demoted error gate reads `warn` but stays error-severity).
/// `message` is the complete line body the text renderer prints after the
/// status word (for pass/skip: `{title}: {message}`; for fail/warn: the
/// sentence as-is, matching the pre-report wording verbatim).
#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    pub id: String,
    pub title: String,
    pub severity: dbtl::Severity,
    pub status: GateStatus,
    pub message: String,
    /// Findings the gate surfaced: counted (non-baselined) findings plus the
    /// baselined ones marked `baselined`, in evaluation order. Empty for the
    /// aggregate gates (coverage, diff risk flags, docs drift).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<dbtl::Finding>,
    /// Gate-shaped detail lines that aren't lint [`dbtl::Finding`]s — the
    /// docs drift's `missing:`/`changed:`/`stale:` paths. Rendered under the
    /// gate line the way findings are.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl GateResult {
    pub fn new(
        id: &str,
        title: &str,
        severity: dbtl::Severity,
        status: GateStatus,
        message: String,
    ) -> Self {
        GateResult {
            id: id.to_string(),
            title: title.to_string(),
            severity,
            status,
            message,
            findings: Vec::new(),
            details: Vec::new(),
        }
    }

    /// A gate that was requested but could not evaluate (e.g. `--no-breaking`
    /// without `--diff`) — reported as `skip`, never counted as a failure.
    pub fn skip(id: &str, title: &str, message: String) -> Self {
        Self::new(id, title, dbtl::Severity::Error, GateStatus::Skip, message)
    }

    /// The findings that actually count — baselined ones are suppressed.
    pub fn counted(&self) -> impl Iterator<Item = &dbtl::Finding> {
        self.findings.iter().filter(|f| !f.baselined)
    }

    /// The suffix the text/markdown gate lines print: the gate severity plus
    /// a baselined tally when any (`error, 3 baselined`).
    pub fn severity_tag(&self) -> String {
        let n = self.findings.iter().filter(|f| f.baselined).count();
        if n > 0 {
            format!("{}, {n} baselined", self.severity.name())
        } else {
            self.severity.name().to_string()
        }
    }
}

/// The generated-docs freshness diff [`DocsDrift::lines`] renders and the
/// `docs_drift` gate reports. The three lists are always sorted (the same
/// `missing:`/`changed:`/`stale:` lines `dbtl docs --check` prints), so the
/// report is deterministic regardless of disk order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DocsDrift {
    pub missing: Vec<String>,
    pub changed: Vec<String>,
    pub stale: Vec<String>,
}

impl DocsDrift {
    pub fn total(&self) -> usize {
        self.missing.len() + self.changed.len() + self.stale.len()
    }

    /// The flat `kind: path` lines in `docs --check` order (`changed` <
    /// `missing` < `stale`, each sorted) — the exact strings that command's
    /// diff prints.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.total());
        out.extend(self.changed.iter().map(|p| format!("changed: {p}")));
        out.extend(self.missing.iter().map(|p| format!("missing: {p}")));
        out.extend(self.stale.iter().map(|p| format!("stale: {p}")));
        out
    }
}

/// The rollup every format echoes: counted findings split by severity, plus
/// gates with no node-bound findings counted as one event each (a coverage
/// failure is `1 error`), baselined findings, and skipped gates.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReportSummary {
    pub errors: usize,
    pub warnings: usize,
    pub baselined: usize,
    pub skipped: usize,
}

/// The shared report the `check`/`lint` gates serialize into — the
/// `dbtl.report/v1` document. Every field is data the run already computed:
/// `gates` is the ordered gate outcomes, `info` the context lines the text
/// format prints after `info_at` gates (the `baseline:`/`affected owners:`
/// provenance lines), `impact` the shared [`dbtl::DiffReport`] verbatim
/// (only when `--diff` engaged), `docs_drift` the freshness diff (only when
/// `--docs-dir` engaged), `summary` the counts rollup.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub tool_version: &'static str,
    pub project: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub info: Vec<String>,
    /// The gate index the `info` lines print before (`gates.len()` appends
    /// them last). Kept out of the serialized shape — a JSON consumer gets
    /// the context lines as the `info` array and interleaves them itself.
    #[serde(skip)]
    pub info_at: usize,
    pub gates: Vec<GateResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<dbtl::DiffReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_drift: Option<DocsDrift>,
    pub summary: ReportSummary,
}

impl Report {
    pub fn new(project: &str, scope: &str) -> Self {
        Report {
            schema: REPORT_SCHEMA,
            tool_version: env!("CARGO_PKG_VERSION"),
            project: project.to_string(),
            scope: scope.to_string(),
            info: Vec::new(),
            info_at: 0,
            gates: Vec::new(),
            impact: None,
            docs_drift: None,
            summary: ReportSummary::default(),
        }
    }

    /// The per-gate findings rollup + gate-level events, per [`ReportSummary`]'s
    /// contract. Called once the gate list is final — the stored copy is what
    /// every format echoes.
    pub fn summarize(gates: &[GateResult]) -> ReportSummary {
        let mut s = ReportSummary::default();
        for g in gates {
            let mut counted = 0;
            for f in &g.findings {
                if f.baselined {
                    s.baselined += 1;
                } else {
                    counted += 1;
                    match f.severity {
                        dbtl::Severity::Error => s.errors += 1,
                        dbtl::Severity::Warn => s.warnings += 1,
                    }
                }
            }
            match g.status {
                // A tripped gate with no counted findings is itself the event.
                GateStatus::Fail if counted == 0 => s.errors += 1,
                GateStatus::Warn if counted == 0 => s.warnings += 1,
                GateStatus::Skip => s.skipped += 1,
                _ => {}
            }
        }
        s
    }
}

/// The report output formats — the shared `ValueEnum` `--format` and
/// `--report <FMT>:<path>` both parse. `impact` accepts only
/// [`Self::Markdown`]/[`Self::Json`] (its payloads predate the report model
/// and stay frozen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ReportFormat {
    Text,
    Markdown,
    Json,
    Github,
    Sarif,
    Junit,
    Codeclimate,
}

impl ReportFormat {
    /// The formats that resolve a finding's `location.file` into a path a CI
    /// annotation/upload can point at — the ones `--path-prefix` applies to.
    pub fn carries_paths(self) -> bool {
        matches!(
            self,
            ReportFormat::Github | ReportFormat::Sarif | ReportFormat::Codeclimate
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            ReportFormat::Text => "text",
            ReportFormat::Markdown => "markdown",
            ReportFormat::Json => "json",
            ReportFormat::Github => "github",
            ReportFormat::Sarif => "sarif",
            ReportFormat::Junit => "junit",
            ReportFormat::Codeclimate => "codeclimate",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "text" => ReportFormat::Text,
            "markdown" => ReportFormat::Markdown,
            "json" => ReportFormat::Json,
            "github" => ReportFormat::Github,
            "sarif" => ReportFormat::Sarif,
            "junit" => ReportFormat::Junit,
            "codeclimate" => ReportFormat::Codeclimate,
            _ => return None,
        })
    }

    /// Every format `check`/`lint` accept — the `allowed` set their
    /// `--report` specs are validated against.
    pub const ALL: [ReportFormat; 7] = [
        ReportFormat::Text,
        ReportFormat::Markdown,
        ReportFormat::Json,
        ReportFormat::Github,
        ReportFormat::Sarif,
        ReportFormat::Junit,
        ReportFormat::Codeclimate,
    ];
}

/// The context a renderer may need that isn't part of the document: the
/// subcommand name (junit `classname`, the markdown title) and the
/// `--path-prefix` for path-carrying formats.
pub struct RenderCtx<'a> {
    pub command: &'a str,
    pub path_prefix: Option<&'a str>,
}

/// Dispatch one format's renderer — each is a pure `Report -> String`
/// function under this module.
pub fn render_report(format: ReportFormat, report: &Report, ctx: &RenderCtx<'_>) -> String {
    match format {
        ReportFormat::Text => text::render(report, ctx),
        ReportFormat::Markdown => markdown::render(report, ctx),
        ReportFormat::Json => json::render(report, ctx),
        ReportFormat::Github => github::render(report, ctx),
        ReportFormat::Sarif => sarif::render(report, ctx),
        ReportFormat::Junit => junit::render(report, ctx),
        ReportFormat::Codeclimate => codeclimate::render(report, ctx),
    }
}

/// A `file[:line[:column]]` anchor for a finding: the project-relative path
/// plus the recorded position — unknown portions drop off rather than
/// printing a guessed `:1`, with `--path-prefix` applied when the format
/// carries paths. Findings without a file location fall back to the node's
/// `unique_id` — a stable identifier, never blank.
pub fn finding_anchor(f: &dbtl::Finding, path_prefix: Option<&str>) -> String {
    match &f.location {
        Some(loc) => {
            let file = match path_prefix {
                Some(prefix) => format!("{prefix}/{}", loc.file),
                None => loc.file.clone(),
            };
            match (loc.line, loc.column) {
                (Some(line), Some(column)) => format!("{file}:{line}:{column}"),
                (Some(line), None) => format!("{file}:{line}"),
                _ => file,
            }
        }
        None => f.node_uid.clone(),
    }
}

/// One `file:line rule message` finding line — the shared shape the text
/// report, the markdown sections, and the junit bodies all list findings as.
/// Baselined findings never reach here (callers list counted findings only).
pub fn finding_line(f: &dbtl::Finding) -> String {
    format!(
        "{} {} {}",
        finding_anchor(f, None),
        f.rule.name(),
        f.message
    )
}

/// The stable identity string behind the machine fingerprints (SARIF
/// `partialFingerprints`, the Code Climate `fingerprint`): rule, node, and
/// the related set — what [`dbtl::Finding::key`] distinguishes, joined so it
/// can ride in a JSON string field.
pub fn finding_key(f: &dbtl::Finding) -> String {
    let (rule, node, related) = f.key();
    format!("{}/{}/{}", rule.name(), node, related.join(","))
}

/// Parse one `--report <FMT>:<PATH>` spec. The split is the FIRST `:` only,
/// so a Windows-style `C:\…` path survives in the right-hand side. Unknown
/// formats and empty paths are usage errors — a mistyped destination must
/// not be silently dropped.
pub fn parse_report_spec(
    spec: &str,
    allowed: &[ReportFormat],
) -> Result<(ReportFormat, PathBuf), CliError> {
    let Some((fmt, path)) = spec.split_once(':') else {
        return Err(CliError::usage(anyhow::anyhow!(
            "--report expects <format>:<path>, got '{spec}'"
        )));
    };
    let Some(format) = ReportFormat::from_name(fmt) else {
        return Err(CliError::usage(anyhow::anyhow!(
            "--report: unknown format '{fmt}' (expected one of: {})",
            allowed
                .iter()
                .map(|f| f.name())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    if !allowed.contains(&format) {
        return Err(CliError::usage(anyhow::anyhow!(
            "--report: format '{fmt}' is not valid for this command (expected one of: {})",
            allowed
                .iter()
                .map(|f| f.name())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if path.is_empty() {
        return Err(CliError::usage(anyhow::anyhow!(
            "--report: empty path in '{spec}'"
        )));
    }
    Ok((format, PathBuf::from(path)))
}

/// Where one run's report goes: the stdout `--format` (suppressed by
/// `--quiet`), every `--report FMT:PATH` file, and the GitHub step summary
/// (`--step-summary` appends the markdown render to `$GITHUB_STEP_SUMMARY`).
/// Validated up front so a bad spec is a usage error before any data loads.
pub struct OutputPlan {
    pub format: ReportFormat,
    pub quiet: bool,
    pub reports: Vec<(ReportFormat, PathBuf)>,
    pub step_summary: bool,
    pub path_prefix: Option<String>,
}

impl OutputPlan {
    /// Validate `--report` specs and the `--path-prefix` precondition for
    /// `check`/`lint` (`allowed` is [`ReportFormat::ALL`]). `--path-prefix`
    /// only makes sense with a path-carrying format selected anywhere —
    /// using it with e.g. `text` alone is a usage error.
    pub fn build(
        format: ReportFormat,
        quiet: bool,
        report_args: &[String],
        step_summary: bool,
        path_prefix: Option<&str>,
        allowed: &[ReportFormat],
    ) -> Result<Self, CliError> {
        let mut reports = Vec::with_capacity(report_args.len());
        for spec in report_args {
            reports.push(parse_report_spec(spec, allowed)?);
        }
        // A `.`/empty/trailing-slash prefix contributes nothing — normalize
        // it away rather than double-slashing every emitted path.
        let path_prefix = path_prefix
            .map(|p| p.trim_end_matches('/'))
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_string);
        if path_prefix.is_some() {
            let any_path_format =
                format.carries_paths() || reports.iter().any(|(f, _)| f.carries_paths());
            if !any_path_format {
                return Err(CliError::usage(anyhow::anyhow!(
                    "--path-prefix requires a file-path carrying format (github, sarif, codeclimate) via --format or --report"
                )));
            }
        }
        Ok(OutputPlan {
            format,
            quiet,
            reports,
            step_summary,
            path_prefix,
        })
    }

    /// Emit the report: stdout (unless `--quiet`), then each `--report` file
    /// (parents created, existing files overwritten — write failures are
    /// input/output errors), then the step-summary append. Every render of a
    /// file matches the bytes the same format would print on stdout.
    pub fn emit(&self, report: &Report, command: &str) -> Result<(), CliError> {
        let ctx = RenderCtx {
            command,
            path_prefix: self.path_prefix.as_deref(),
        };
        if !self.quiet {
            print!("{}", render_report(self.format, report, &ctx));
            let _ = std::io::stdout().flush();
        }
        for (format, path) in &self.reports {
            write_report_file(path, &render_report(*format, report, &ctx))?;
        }
        if self.step_summary {
            self.append_step_summary(report, &ctx);
        }
        Ok(())
    }

    /// `$GITHUB_STEP_SUMMARY` append — a no-op when the variable is unset or
    /// empty (local runs). A write failure is a warning, never an exit-code
    /// change: the summary is a side-channel of the run that already
    /// happened.
    fn append_step_summary(&self, report: &Report, ctx: &RenderCtx<'_>) {
        let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let block = render_report(ReportFormat::Markdown, report, ctx);
        let append = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(block.as_bytes()));
        if let Err(e) = append {
            eprintln!("warning: failed to append the step summary to {path}: {e}");
        }
    }
}

/// Write one report file: missing parent directories are created, existing
/// files overwritten. A write failure is an input/output error (exit 3) —
/// the report destination is part of the invocation's contract.
pub fn write_report_file(path: &Path, contents: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create report directory '{}'", parent.display()))
            .input()?;
    }
    std::fs::write(path, contents)
        .with_context(|| format!("write report '{}'", path.display()))
        .input()?;
    Ok(())
}

/// The per-rule gates a [`LintGateOutcome`] produces — shared verbatim between
/// `dbtl lint` (where they ARE the report) and `dbtl check --lint` (where
/// they follow the structural gates), so the same run can never emit two
/// different wordings. Rules evaluated and rules skipped for a missing
/// input interleave in `LintRule::ALL` order — a skipped rule is a `SKIP`
/// gate (`SKIP lint <rule>: <reason>` in text, `status: "skip"` in JSON),
/// informational only, never counted as a failure.
pub fn lint_gates(dag: &dbtl::Dag, outcome: &LintGateOutcome) -> Vec<GateResult> {
    let name = |f: &dbtl::Finding| dag.get(&f.node_uid).map_or("", |n| n.name.as_str());
    let mut gates = Vec::with_capacity(outcome.rules.len() + outcome.skipped.len());
    let mut rules = outcome.rules.iter().peekable();
    let mut skipped = outcome.skipped.iter().peekable();
    // Merge-join the two `LintRule::ALL`-ordered lists so every emitted
    // gate keeps the registry's fixed order.
    loop {
        let take_skip = match (rules.peek(), skipped.peek()) {
            (Some(r), Some(s)) => s.rule < r.rule,
            (None, Some(_)) => true,
            _ => false,
        };
        if take_skip {
            let s = skipped.next().unwrap();
            let id = format!("lint:{}", s.rule.name());
            let title = format!("lint {}", s.rule.name());
            gates.push(GateResult::skip(&id, &title, s.reason.to_string()));
            continue;
        }
        let Some(ro) = rules.next() else {
            break;
        };
        let id = format!("lint:{}", ro.rule.name());
        let title = format!("lint {}", ro.rule.name());
        // Every finding the report keeps for this rule: the counted ones plus
        // the baselined ones (marked) — pre-existing findings stay out of the
        // report entirely (their count is in the pass message).
        let findings: Vec<dbtl::Finding> = outcome
            .findings
            .iter()
            .enumerate()
            .filter(|(i, f)| f.rule == ro.rule && (ro.counted.contains(i) || f.baselined))
            .map(|(_, f)| f.clone())
            .collect();
        match ro.status {
            GateStatus::Pass => {
                let pass_msg = if outcome.has_diff && (ro.pre_existing > 0 || ro.baselined > 0) {
                    let mut parts = Vec::new();
                    if ro.pre_existing > 0 {
                        parts.push(format!("{} pre-existing", ro.pre_existing));
                    }
                    if ro.baselined > 0 {
                        parts.push(format!("{} baselined", ro.baselined));
                    }
                    format!("no new violations ({})", parts.join(", "))
                } else if ro.baselined > 0 {
                    format!("none ({} baselined)", ro.baselined)
                } else {
                    "none".to_string()
                };
                let mut gate =
                    GateResult::new(&id, &title, ro.severity, GateStatus::Pass, pass_msg);
                gate.findings = findings;
                gates.push(gate);
            }
            // `evaluate_lint` never yields Skip for a rule (skips are for
            // whole checks like --no-breaking without --diff, which live in
            // `run_check` itself) — the arm stays for match-exhaustiveness
            // so a future classification can't silently pass unhandled.
            GateStatus::Skip => {
                gates.push(GateResult::skip(&id, &title, "inputs unavailable".into()));
            }
            GateStatus::Warn | GateStatus::Fail => {
                let names = ro
                    .counted
                    .iter()
                    .map(|&i| name(&outcome.findings[i]))
                    .collect::<Vec<_>>()
                    .join(", ");
                let n = ro.counted.len();
                let msg = format!(
                    "{title}: {n} {}violation(s): {names}",
                    if outcome.has_diff { "new " } else { "" }
                );
                let mut gate = GateResult::new(&id, &title, ro.severity, ro.status, msg);
                gate.findings = findings;
                gates.push(gate);
            }
        }
    }
    gates
}

#[cfg(test)]
pub(super) mod samples {
    //! Synthetic report material the per-format unit tests share — the real
    //! shape (gates + findings + baselined markers) without a data source.

    use super::*;

    /// A counted or baselined lint finding: `file` becomes the `location`
    /// (None → a file-less, manifest-mode finding) carrying the pinned
    /// `(7,3)`–`(7,5)` span the yml-resolved findings report.
    pub(super) fn finding(
        rule: dbtl::LintRule,
        node: &str,
        message: &str,
        severity: dbtl::Severity,
        file: Option<&str>,
        baselined: bool,
    ) -> dbtl::Finding {
        dbtl::Finding {
            rule,
            node_uid: node.to_string(),
            related: Vec::new(),
            message: message.to_string(),
            severity,
            location: file.map(|f| dbtl::Location {
                file: f.to_string(),
                line: Some(7),
                column: Some(3),
                end_line: Some(7),
                end_column: Some(5),
            }),
            label: None,
            labels: Vec::new(),
            help: None,
            baselined,
        }
    }

    /// A finding that names a file but no position — the `.sql`-filed
    /// findings today's graph rules produce.
    pub(super) fn unpositioned(
        rule: dbtl::LintRule,
        node: &str,
        message: &str,
        severity: dbtl::Severity,
        file: &str,
        baselined: bool,
    ) -> dbtl::Finding {
        dbtl::Finding {
            rule,
            node_uid: node.to_string(),
            related: Vec::new(),
            message: message.to_string(),
            severity,
            location: Some(dbtl::Location::new(file)),
            label: None,
            labels: Vec::new(),
            help: None,
            baselined,
        }
    }

    /// A gate with findings attached.
    pub(super) fn gate(
        id: &str,
        title: &str,
        severity: dbtl::Severity,
        status: GateStatus,
        message: &str,
        findings: Vec<dbtl::Finding>,
    ) -> GateResult {
        let mut g = GateResult::new(id, title, severity, status, message.to_string());
        g.findings = findings;
        g
    }

    /// The kitchen sink: a pass, a fail with findings (one baselined), a warn,
    /// and a skip — every status a render must handle.
    pub(super) fn report() -> Report {
        let mut r = Report::new("proj", "root project only");
        r.gates = vec![
            GateResult::new(
                "coverage",
                "coverage",
                dbtl::Severity::Error,
                GateStatus::Pass,
                "5/5 (100%)".to_string(),
            ),
            gate(
                "lint:root_models",
                "lint root_models",
                dbtl::Severity::Error,
                GateStatus::Fail,
                "lint root_models: 2 violation(s): a, b",
                vec![
                    finding(
                        dbtl::LintRule::RootModels,
                        "model.proj.a",
                        "a has no parents",
                        dbtl::Severity::Error,
                        Some("models/a.sql"),
                        false,
                    ),
                    finding(
                        dbtl::LintRule::RootModels,
                        "model.proj.b",
                        "b has no parents",
                        dbtl::Severity::Error,
                        Some("models/b.sql"),
                        false,
                    ),
                    finding(
                        dbtl::LintRule::RootModels,
                        "model.proj.c",
                        "c has no parents",
                        dbtl::Severity::Error,
                        Some("models/c.sql"),
                        true,
                    ),
                ],
            ),
            gate(
                "lint:undocumented_models",
                "lint undocumented_models",
                dbtl::Severity::Warn,
                GateStatus::Warn,
                "lint undocumented_models: 1 violation(s): d",
                vec![finding(
                    dbtl::LintRule::UndocumentedModels,
                    "model.proj.d",
                    "d has no description",
                    dbtl::Severity::Warn,
                    None,
                    false,
                )],
            ),
            GateResult::skip(
                "breaking_columns",
                "breaking column changes",
                "needs --diff".to_string(),
            ),
        ];
        r.summary = Report::summarize(&r.gates);
        r
    }

    pub(super) fn ctx<'a>(prefix: Option<&'a str>) -> RenderCtx<'a> {
        RenderCtx {
            command: "check",
            path_prefix: prefix,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;

    /// `--report` specs split on the FIRST `:` — a Windows drive-letter path
    /// survives intact.
    #[test]
    fn report_spec_splits_on_first_colon() {
        let (fmt, path) = parse_report_spec("json:out/report.json", &ReportFormat::ALL).unwrap();
        assert_eq!(fmt, ReportFormat::Json);
        assert_eq!(path, PathBuf::from("out/report.json"));
        let (fmt, path) = parse_report_spec("sarif:C:\\out\\r.sarif", &ReportFormat::ALL).unwrap();
        assert_eq!(fmt, ReportFormat::Sarif);
        assert_eq!(path, PathBuf::from("C:\\out\\r.sarif"));
    }

    /// Unknown formats, missing colons, and empty paths are all usage errors —
    /// a mistyped destination must not be silently dropped.
    #[test]
    fn report_spec_rejects_bad_input() {
        for spec in ["bogus:x", "json", "json:"] {
            assert!(
                parse_report_spec(spec, &ReportFormat::ALL).is_err(),
                "{spec} must be a usage error"
            );
        }
        // `impact` only allows markdown/json — every other format is refused.
        assert!(
            parse_report_spec(
                "sarif:o.sarif",
                &[ReportFormat::Markdown, ReportFormat::Json]
            )
            .is_err(),
            "a format outside the allowed set is a usage error"
        );
    }

    /// `--path-prefix` normalizes away `.`, a trailing slash, and emptiness,
    /// and requires a path-carrying format somewhere in the selection.
    #[test]
    fn path_prefix_normalization_and_validation() {
        let plan = OutputPlan::build(
            ReportFormat::Github,
            false,
            &[],
            false,
            Some("sub/dir/"),
            &ReportFormat::ALL,
        )
        .unwrap();
        assert_eq!(plan.path_prefix.as_deref(), Some("sub/dir"));

        // `.` and empty prefixes contribute nothing — and don't trip the
        // path-format requirement either.
        for prefix in [".", "", "/"] {
            let plan = OutputPlan::build(
                ReportFormat::Text,
                false,
                &[],
                false,
                Some(prefix),
                &ReportFormat::ALL,
            )
            .unwrap();
            assert_eq!(plan.path_prefix, None, "{prefix:?} normalizes away");
        }

        // No path-carrying format anywhere → usage error.
        assert!(
            OutputPlan::build(
                ReportFormat::Text,
                false,
                &[],
                false,
                Some("p"),
                &ReportFormat::ALL
            )
            .is_err(),
            "--path-prefix without a path-carrying format is a usage error"
        );
        // …but a --report destination carrying paths satisfies it.
        let plan = OutputPlan::build(
            ReportFormat::Text,
            false,
            &["sarif:r.sarif".to_string()],
            false,
            Some("p"),
            &ReportFormat::ALL,
        )
        .unwrap();
        assert_eq!(plan.path_prefix.as_deref(), Some("p"));
    }

    /// `summarize` counts counted findings by severity, folds baselined
    /// findings and skipped gates into their own tallies, and counts a
    /// finding-free tripped gate as one event.
    #[test]
    fn summarize_counts_the_kitchen_sink() {
        let r = report();
        assert_eq!(r.summary.errors, 2, "two counted error findings");
        assert_eq!(r.summary.warnings, 1, "one counted warn finding");
        assert_eq!(r.summary.baselined, 1, "one baselined finding");
        assert_eq!(r.summary.skipped, 1, "one skipped gate");
    }

    /// The finding helpers: `file:line:column` anchors, unknown portions
    /// dropped (never a guessed `:1`), uid fallback, and the key format the
    /// fingerprints hash.
    #[test]
    fn finding_helpers_anchor_and_key() {
        let f = finding(
            dbtl::LintRule::RootModels,
            "model.p.n",
            "m",
            dbtl::Severity::Error,
            Some("models/n.sql"),
            false,
        );
        assert_eq!(finding_anchor(&f, None), "models/n.sql:7:3");
        assert_eq!(finding_anchor(&f, Some("sub")), "sub/models/n.sql:7:3");
        assert_eq!(finding_key(&f), "root_models/model.p.n/");

        let no_loc = finding(
            dbtl::LintRule::RootModels,
            "model.p.n",
            "m",
            dbtl::Severity::Error,
            None,
            false,
        );
        assert_eq!(
            finding_anchor(&no_loc, None),
            "model.p.n",
            "a file-less finding anchors on the node uid"
        );
        assert_eq!(finding_line(&f), "models/n.sql:7:3 root_models m");

        // A file-scoped finding (the `.sql` kind today's graph rules
        // produce) anchors on the file alone — no `:1` is invented, and no
        // position portions appear in json's `location` either.
        let unpositioned = unpositioned(
            dbtl::LintRule::RootModels,
            "model.p.n",
            "m",
            dbtl::Severity::Error,
            "models/n.sql",
            false,
        );
        assert_eq!(finding_anchor(&unpositioned, None), "models/n.sql");
        assert_eq!(finding_line(&unpositioned), "models/n.sql root_models m");
    }
}
