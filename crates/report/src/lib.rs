//! dbtl-report — the shared machine report behind `dbtl check` and `dbtl
//! lint`: one [`Report`] model (`dbtl.report/v1`) describing the run — the
//! gates that evaluated, their findings, the diff/docs payloads — rendered
//! by one pure function per output format. The lint half of the model is
//! assembled here too: `evaluate_lint` classifies `run_rules` findings
//! into counted / baselined / pre-existing, `lint_gates` lifts them into
//! gate rows, and [`lint_report`] packages the whole lint run into a
//! [`Report`] — so the CLI's stdout, the CI annotations, the uploaded
//! files, and any in-memory caller can never disagree about what the gates
//! said.
//!
//! The crate never touches files, the terminal, or argument parsing: a
//! `.yml` file's text arrives through a caller-supplied `read_text`
//! function (the CLI reads under the project root; an in-memory caller
//! answers from its own map), and emission is the caller's job.

mod codeclimate;
mod concise;
mod github;
mod json;
mod junit;
mod markdown;
mod sarif;
mod text;

mod lint;

// The public lint seam: callers hand a `LintRunInputs`/`LintAux` in and get
// `LintAssembly`/`LintRun` data back — those two stay re-exported because a
// `pub fn`'s return type can't be less reachable than the fn itself.
// `evaluate_lint`/`LintGateOutcome` join them for `lint --fix`'s driver,
// which must re-run the SAME classified evaluation over patched dags each
// fixpoint round — anything less (raw `run_rules`) would un-apply baseline,
// selection, and `--diff` pre-existing and diverge from the report. The
// remaining internals (`lint_gates`, `severity_fails`, `RuleOutcome`) stay
// inside the private `lint` module so they never become API.
pub use lint::{
    evaluate_lint, gate_status, lint_assembly, lint_report, LintAssembly, LintAux, LintGateOutcome,
    LintRun, LintRunInputs,
};

use dbtl_core::{Dag, DiffReport, Finding, Severity, SkippedRule};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ops::Range;

/// The schema marker every JSON-shaped report carries — lets a consumer pin
/// the layout version before reading fields.
pub const REPORT_SCHEMA: &str = "dbtl.report/v1";

/// A gate's verdict: `fail` trips the exit code, `warn` reports without
/// counting (findings exist but `fail_on` doesn't reach them), `pass` is
/// clean, `skip` means the gate was asked for but could not evaluate. Shared
/// so `check`'s per-check lines and `lint`'s verdict line always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl GateStatus {
    /// The line prefix (`check`'s per-check lines, `lint`'s verdict).
    pub(crate) fn word(self) -> &'static str {
        match self {
            GateStatus::Pass => "PASS",
            GateStatus::Warn => "WARN",
            GateStatus::Fail => "FAIL",
            GateStatus::Skip => "SKIP",
        }
    }
    /// The report status string (`gates[].status` in `dbtl.report/v1`).
    pub(crate) fn json(self) -> &'static str {
        match self {
            GateStatus::Pass => "pass",
            GateStatus::Warn => "warn",
            GateStatus::Fail => "fail",
            GateStatus::Skip => "skip",
        }
    }
}

impl serde::Serialize for GateStatus {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.json())
    }
}

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
    pub severity: Severity,
    pub status: GateStatus,
    pub message: String,
    /// Findings the gate surfaced: counted (non-baselined) findings plus the
    /// baselined ones marked `baselined`, in evaluation order. Empty for the
    /// aggregate gates (coverage, diff risk flags, docs drift).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// Gate-shaped detail lines that aren't lint [`Finding`]s — the
    /// docs drift's `missing:`/`changed:`/`stale:` paths. Rendered under the
    /// gate line the way findings are.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl GateResult {
    pub fn new(
        id: &str,
        title: &str,
        severity: Severity,
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
        Self::new(id, title, Severity::Error, GateStatus::Skip, message)
    }

    /// The findings that actually count — baselined ones are frozen and
    /// suppressed ones (`--show-suppressed` lists them on the gate) never
    /// counted.
    pub fn counted(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| !f.baselined && f.suppression.is_none())
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
    /// Suppressed findings — present only when at least one was dropped
    /// (additive: a run with none serializes the four fields above only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppressed: Option<SuppressedSummary>,
    /// Models the SQL rules couldn't parse — present only when at least
    /// one model was skipped that way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unparsed_models: Option<UnparsedModels>,
    /// Findings `--fix`/`--fix-dry-run` resolved (or would have) — present
    /// only when a fix run rewrote at least one byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<FixedSummary>,
}

/// The `--fix`/`--fix-dry-run` rollup `summary.fixed` carries — findings
/// resolved and the files they span. `dry_run` is report-internal only —
/// the JSON shape stays just the two counts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FixedSummary {
    /// Findings the fixes resolved (`--fix`) or would resolve
    /// (`--fix-dry-run`) — the disappeared, never the attempts.
    pub findings: usize,
    /// Files at least one fix touched.
    pub files: usize,
    /// `true` on a `--fix-dry-run` — the counts are hypothetical; the
    /// text summary words them "fixable" instead of "fixed".
    #[serde(skip)]
    pub dry_run: bool,
}

/// The suppressed-findings rollup `summary.suppressed` carries — the total
/// plus the mechanism breakdown and the count that recorded no reason.
/// Also the [`LintTally`] shape the text summary line reads.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SuppressedSummary {
    /// Findings `lint.rules.<id>.exclude` / `meta.dbtl.ignore` dropped —
    /// they never counted toward the gate.
    pub total: usize,
    /// The per-mechanism split.
    pub by: SuppressedBy,
    /// How many suppressions recorded no reason.
    pub without_reason: usize,
}

/// Per-mechanism suppressed counts inside [`SuppressedSummary`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct SuppressedBy {
    /// `.dbtl.yml` `lint.rules.<id>.exclude` selector suppressions.
    pub exclude: usize,
    /// Node `meta.dbtl.ignore` suppressions.
    pub meta: usize,
}

/// The unparsable-model rollup `summary.unparsed_models` carries — the
/// same set the text summary's third line names.
#[derive(Debug, Clone, Serialize)]
pub struct UnparsedModels {
    /// `names.len()` echoed for consumers that don't list them.
    pub count: usize,
    /// The models' [`dbtl_core::display_name`]s in `unique_id` order.
    pub names: Vec<String>,
}

/// The lint-run tallies the `text`/`concise` summary lines need that the
/// gate list alone can't express: findings `--diff` suppressed before
/// counting, whether the `new` wording applies, the enabled-but-unrunnable
/// rules, and a slot for the unparsable-SQL tally. Carried on
/// [`Report::lint`] — never serialized; the json report's `summary` block
/// keeps its own shape.
#[derive(Debug, Clone, Default)]
pub struct LintTally {
    /// Findings `--diff` classified as already on the baseline side —
    /// reported in the summary as "not counted", never gated on.
    pub pre_existing: usize,
    /// A `--diff` baseline was compared (drives the `new finding(s)`
    /// wording on lint gate rows).
    pub has_diff: bool,
    /// Enabled lint rules `run_rules` skipped for a missing input, in
    /// `LintRule::ALL` order.
    pub skipped: Vec<SkippedRule>,
    /// Models whose SQL exists but wouldn't parse so the SQL rules skipped
    /// them — the summary's third line — carried as their
    /// [`dbtl_core::display_name`]s in `unique_id` order, populated from
    /// [`dbtl_core::LintOutcome::unparsed`] (each uid once, however many SQL
    /// rules hit the same parse failure). The text renderer lists the
    /// first ten then a `+K more` tail.
    pub unparsed_models: Vec<String>,
    /// The suppressed-finding rollup — total, mechanism split, and the
    /// count carrying no reason. The text summary's fourth line and the
    /// JSON `summary.suppressed` both read it.
    pub suppressed: SuppressedSummary,
    /// The `--fix`/`--fix-dry-run` rollup — set by the CLI's fix loop
    /// (report assembly can't see file edits); lifted into
    /// `summary.fixed` by [`Report::lift_lint_summary`].
    pub fixed: Option<FixedSummary>,
}

/// The shared report the `check`/`lint` gates serialize into — the
/// `dbtl.report/v1` document. Every field is data the run already computed:
/// `gates` is the ordered gate outcomes, `info` the context lines the text
/// format prints after `info_at` gates (the `baseline:`/`affected owners:`
/// provenance lines), `impact` the shared [`DiffReport`] verbatim
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
    pub impact: Option<DiffReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_drift: Option<DocsDrift>,
    pub summary: ReportSummary,
    /// Lint-side tallies for the human formats' summary lines — `Some`
    /// exactly when lint ran (`dbtl lint`, `check --lint`), so `check`
    /// without lint keeps its plain gate list with no summary. Never
    /// serialized.
    #[serde(skip)]
    pub lint: Option<LintTally>,
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
            lint: None,
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
                // Suppressed findings (attached by `--show-suppressed`)
                // never count — their rollup is `summary.suppressed`.
                if f.suppression.is_some() {
                    continue;
                }
                if f.baselined {
                    s.baselined += 1;
                } else {
                    counted += 1;
                    match f.severity {
                        Severity::Error => s.errors += 1,
                        Severity::Warn => s.warnings += 1,
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

    /// Lift the lint tallies into the serialized summary — the suppressed
    /// rollup and the unparsable-model list. Call AFTER `Report::summarize`
    /// and `report.lint` are both set (the rollup counts findings the gate
    /// list never carries, so `summarize` cannot see them). `dbtl lint`
    /// and `check --lint` share this so their JSON shapes never diverge.
    pub fn lift_lint_summary(&mut self) {
        let Some(lint) = &self.lint else {
            return;
        };
        if lint.suppressed.total > 0 {
            self.summary.suppressed = Some(lint.suppressed.clone());
        }
        if !lint.unparsed_models.is_empty() {
            self.summary.unparsed_models = Some(UnparsedModels {
                count: lint.unparsed_models.len(),
                names: lint.unparsed_models.clone(),
            });
        }
        if lint.fixed.is_some() {
            self.summary.fixed = lint.fixed.clone();
        }
    }
}

/// The report output formats — every value `check`/`lint`'s `--format` and
/// `--report <FMT>:<path>` accept. `impact` allows only
/// [`Self::Markdown`]/[`Self::Json`] (its payloads predate the report model
/// and stay frozen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Concise,
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

    /// The human formats — the ones ANSI styling and source excerpts apply
    /// to (`--color` and the excerpt map affect nothing else).
    pub fn is_human(self) -> bool {
        matches!(self, ReportFormat::Text | ReportFormat::Concise)
    }

    pub fn name(self) -> &'static str {
        match self {
            ReportFormat::Text => "text",
            ReportFormat::Concise => "concise",
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
            "concise" => ReportFormat::Concise,
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
    pub const ALL: [ReportFormat; 8] = [
        ReportFormat::Text,
        ReportFormat::Concise,
        ReportFormat::Markdown,
        ReportFormat::Json,
        ReportFormat::Github,
        ReportFormat::Sarif,
        ReportFormat::Junit,
        ReportFormat::Codeclimate,
    ];
}

/// The context a renderer may need that isn't part of the document: the
/// subcommand name (junit `classname`, the markdown title, which report
/// shape `text`/`concise` take), the `--path-prefix` for path-carrying
/// formats, and — for the human formats only — the run's dag, the excerpt
/// texts, and whether ANSI styling goes in.
pub struct RenderCtx<'a> {
    pub command: &'a str,
    pub path_prefix: Option<&'a str>,
    /// The run's dag: `text`/`concise` resolve `related` node names for
    /// `= note:` lines through it. `None` where no dag exists (render
    /// unit tests).
    pub dag: Option<&'a Dag>,
    /// The excerpt texts `text` annotates, keyed by `location.file` —
    /// populated only for the human formats (see [`collect_sources`]).
    pub sources: Option<&'a BTreeMap<String, SourceText>>,
    /// Emit ANSI styling — only ever true for stdout `text`/`concise`
    /// (report files and the step summary always render plain).
    pub styled: bool,
}

/// The run-side inputs the human formats need that the report doesn't
/// carry: the dag for `.sql` excerpts and `related` lookups, and the
/// resolver a `.yml` `location.file` reads through — the same function
/// [`lint_assembly`] filled positions from, so an excerpt always matches
/// the position it annotates. Everything else the renderers need lives in
/// the [`Report`] itself.
///
/// [`lint_assembly`]: crate::lint_assembly
pub struct RenderEnv<'a> {
    /// The run's dag — `.sql` excerpts come from its `raw_code` (the exact
    /// string positions were computed against).
    pub dag: &'a Dag,
    /// Resolve a `location.file` (a project-relative `.yml` path) into the
    /// file's text — `None` leaves the finding excerpt-less, exactly like
    /// an unreadable file.
    pub read_text: &'a dyn Fn(&str) -> Option<String>,
}

/// Dispatch one format's renderer — each is a pure `Report -> String`
/// function under this module.
pub fn render_report(format: ReportFormat, report: &Report, ctx: &RenderCtx<'_>) -> String {
    match format {
        ReportFormat::Text => text::render(report, ctx),
        ReportFormat::Concise => concise::render(report, ctx),
        ReportFormat::Markdown => markdown::render(report, ctx),
        ReportFormat::Json => json::render(report, ctx),
        ReportFormat::Github => github::render(report, ctx),
        ReportFormat::Sarif => sarif::render(report, ctx),
        ReportFormat::Junit => junit::render(report, ctx),
        ReportFormat::Codeclimate => codeclimate::render(report, ctx),
    }
}

/// One excerpt text plus the byte offsets where each of its lines
/// starts: `text` renders slices of it, and (line, column) → byte
/// lookups run against `line_starts` so a large `.yml` isn't rescanned
/// for every finding that lands in it.
pub struct SourceText {
    /// The file's full text — a finding's excerpt slices lines out of it.
    pub text: String,
    /// `line_starts[i]` is the byte offset line `i + 1` (1-based) begins
    /// at — each `\n` byte opens a new line.
    line_starts: Vec<usize>,
}

impl SourceText {
    pub fn new(text: String) -> Self {
        // An empty text has no line 1 — a `(1, 1)` position must report
        // None, not a zero-width span at offset 0.
        let mut line_starts = if text.is_empty() { vec![] } else { vec![0] };
        line_starts.extend(
            text.match_indices('\n')
                .map(|(i, _)| i + 1)
                // A trailing `\n` opens no empty line after it — the same
                // line count `str::split_inclusive`/the snippet's own map
                // see.
                .filter(|&s| s < text.len()),
        );
        SourceText { text, line_starts }
    }

    /// The byte offset of 1-based `(line, column)` — columns count
    /// `char`s like every other position in the report — `None` when
    /// the line doesn't exist or the column sits past its end. Both
    /// `\n` and `\r\n` line endings are handled (the `\r` stays part of
    /// its line).
    pub(crate) fn position_byte(&self, line: u32, column: u32) -> Option<usize> {
        if line == 0 || column == 0 {
            return None;
        }
        let start = *self.line_starts.get(line as usize - 1)?;
        if column == 1 {
            return Some(start);
        }
        // The line's bytes, excluding the `\n` that ends it.
        let end = self
            .line_starts
            .get(line as usize)
            .map_or(self.text.len(), |next| next - 1);
        let mut chars = self.text[start..end].chars();
        let mut offset = start;
        for _ in 1..column {
            offset += chars.next()?.len_utf8();
        }
        Some(offset)
    }

    /// The byte range `line`'s bytes occupy — including its `\n` when
    /// one is there (the last line runs to EOF). `None` past EOF.
    pub(crate) fn line_span(&self, line: u32) -> Option<Range<usize>> {
        let start = *self.line_starts.get(line.checked_sub(1)? as usize)?;
        let end = self
            .line_starts
            .get(line as usize)
            .copied()
            .unwrap_or(self.text.len());
        Some(start..end)
    }

    /// The 1-based line `byte` falls inside — a byte at a line's start
    /// belongs to that line; one at EOF to the last line.
    pub(crate) fn line_of_byte(&self, byte: usize) -> u32 {
        self.line_starts.partition_point(|&s| s <= byte) as u32
    }
}

/// The excerpt texts `text`/`concise` annotate, keyed by `location.file`:
/// `.yml` files resolved once through `env.read_text` (the same source
/// [`fill_finding_lines_with`](dbtl_core::fill_finding_lines_with) fills
/// positions from), `.sql` taken from `Dag::raw_code` — the exact string
/// positions were computed against. A file that can't be resolved stays
/// absent; its findings render position-only.
pub fn collect_sources(report: &Report, env: &RenderEnv<'_>) -> BTreeMap<String, SourceText> {
    let mut sources = BTreeMap::new();
    // Counted findings AND the suppressed ones `--show-suppressed` lists —
    // everything the human formats may annotate. Baselined findings stay
    // out: they render as a count, never a block.
    for f in report
        .gates
        .iter()
        .flat_map(|g| g.findings.iter().filter(|f| !f.baselined))
    {
        let Some(loc) = f.location.as_ref() else {
            continue;
        };
        if sources.contains_key(&loc.file) {
            continue;
        }
        let text = if loc.file.ends_with(".yml") || loc.file.ends_with(".yaml") {
            (env.read_text)(&loc.file)
        } else {
            env.dag.raw_code(&f.node_uid).map(str::to_string)
        };
        if let Some(text) = text {
            sources.insert(loc.file.clone(), SourceText::new(text));
        }
    }
    sources
}

/// A `file[:line[:column]]` anchor for a finding: the project-relative path
/// plus the recorded position — unknown portions drop off rather than
/// printing a guessed `:1`, with `--path-prefix` applied when the caller
/// passes one (only the github/sarif/codeclimate formats do — `text` and
/// `concise` keep the project-relative path). Findings without a file
/// location fall back to the node's `unique_id` — a stable identifier,
/// never blank.
pub fn finding_anchor(f: &Finding, path_prefix: Option<&str>) -> String {
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
pub fn finding_line(f: &Finding) -> String {
    format!(
        "{} {} {}",
        finding_anchor(f, None),
        f.rule.name(),
        f.message
    )
}

/// The stable identity string behind the machine fingerprints (SARIF
/// `partialFingerprints`, the Code Climate `fingerprint`): rule, node, and
/// the related set — what [`Finding::key`] distinguishes, joined so it
/// can ride in a JSON string field.
pub fn finding_key(f: &Finding) -> String {
    let (rule, node, related) = f.key();
    format!("{}/{}/{}", rule.name(), node, related.join(","))
}

#[cfg(test)]
pub(crate) mod samples {
    //! Synthetic report material the per-format unit tests share — the real
    //! shape (gates + findings + baselined markers) without a data source.

    use super::*;

    /// A counted or baselined lint finding: `file` becomes the `location`
    /// (None → a file-less, manifest-mode finding) carrying the pinned
    /// `(7,3)`–`(7,5)` span the yml-resolved findings report.
    pub(crate) fn finding(
        rule: dbtl_core::LintRule,
        node: &str,
        message: &str,
        severity: Severity,
        file: Option<&str>,
        baselined: bool,
    ) -> Finding {
        Finding {
            rule,
            node_uid: node.to_string(),
            related: Vec::new(),
            message: message.to_string(),
            severity,
            location: file.map(|f| dbtl_core::Location {
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
            suppression: None,
        }
    }

    /// A finding that names a file but no position — the `.sql`-filed
    /// findings today's graph rules produce.
    pub(crate) fn unpositioned(
        rule: dbtl_core::LintRule,
        node: &str,
        message: &str,
        severity: Severity,
        file: &str,
        baselined: bool,
    ) -> Finding {
        Finding {
            rule,
            node_uid: node.to_string(),
            related: Vec::new(),
            message: message.to_string(),
            severity,
            location: Some(dbtl_core::Location::new(file)),
            label: None,
            labels: Vec::new(),
            help: None,
            baselined,
            suppression: None,
        }
    }

    /// A gate with findings attached.
    pub(crate) fn gate(
        id: &str,
        title: &str,
        severity: Severity,
        status: GateStatus,
        message: &str,
        findings: Vec<Finding>,
    ) -> GateResult {
        let mut g = GateResult::new(id, title, severity, status, message.to_string());
        g.findings = findings;
        g
    }

    /// The kitchen sink: a pass, a fail with findings (one baselined), a warn,
    /// and a skip — every status a render must handle.
    pub(crate) fn report() -> Report {
        let mut r = Report::new("proj", "root project only");
        r.gates = vec![
            GateResult::new(
                "coverage",
                "coverage",
                Severity::Error,
                GateStatus::Pass,
                "5/5 (100%)".to_string(),
            ),
            gate(
                "lint:root_models",
                "lint root_models",
                Severity::Error,
                GateStatus::Fail,
                "lint root_models: 2 violation(s): a, b",
                vec![
                    finding(
                        dbtl_core::LintRule::RootModels,
                        "model.proj.a",
                        "a has no parents",
                        Severity::Error,
                        Some("models/a.sql"),
                        false,
                    ),
                    finding(
                        dbtl_core::LintRule::RootModels,
                        "model.proj.b",
                        "b has no parents",
                        Severity::Error,
                        Some("models/b.sql"),
                        false,
                    ),
                    finding(
                        dbtl_core::LintRule::RootModels,
                        "model.proj.c",
                        "c has no parents",
                        Severity::Error,
                        Some("models/c.sql"),
                        true,
                    ),
                ],
            ),
            gate(
                "lint:undocumented_models",
                "lint undocumented_models",
                Severity::Warn,
                GateStatus::Warn,
                "lint undocumented_models: 1 violation(s): d",
                vec![finding(
                    dbtl_core::LintRule::UndocumentedModels,
                    "model.proj.d",
                    "d has no description",
                    Severity::Warn,
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

    pub(crate) fn ctx<'a>(prefix: Option<&'a str>) -> RenderCtx<'a> {
        RenderCtx {
            command: "check",
            path_prefix: prefix,
            dag: None,
            sources: None,
            styled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;

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
            dbtl_core::LintRule::RootModels,
            "model.p.n",
            "m",
            Severity::Error,
            Some("models/n.sql"),
            false,
        );
        assert_eq!(finding_anchor(&f, None), "models/n.sql:7:3");
        assert_eq!(finding_anchor(&f, Some("sub")), "sub/models/n.sql:7:3");
        assert_eq!(finding_key(&f), "root_models/model.p.n/");

        let no_loc = finding(
            dbtl_core::LintRule::RootModels,
            "model.p.n",
            "m",
            Severity::Error,
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
            dbtl_core::LintRule::RootModels,
            "model.p.n",
            "m",
            Severity::Error,
            "models/n.sql",
            false,
        );
        assert_eq!(finding_anchor(&unpositioned, None), "models/n.sql");
        assert_eq!(finding_line(&unpositioned), "models/n.sql root_models m");
    }

    /// An empty source has no line 1 — positions against it report
    /// `None`, never a zero-width span at offset 0.
    #[test]
    fn source_text_empty_has_no_lines() {
        let st = SourceText::new(String::new());
        assert_eq!(st.position_byte(1, 1), None);
        assert_eq!(st.line_span(1), None);
        let st = SourceText::new("a\nb\n".to_string());
        assert_eq!(st.position_byte(1, 1), Some(0));
        assert_eq!(st.line_span(1), Some(0..2));
        assert_eq!(st.line_span(3), None);
    }
}
