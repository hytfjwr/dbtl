//! The CLI side of the shared machine report: the [`Report`] model, the
//! eight renderers, and the lint-gate evaluation live in `dbtl_report`
//! (pure — no filesystem, terminal, or clap); this module owns what only a
//! CLI binary can do — parse `--report <FMT>:<PATH>` specs and plan the
//! destinations ([`OutputPlan`]), read `.yml` excerpt text under the
//! project root ([`SourceTexts`]), and write stdout / report files / the
//! GitHub step summary. The re-export below keeps the crate's report API
//! one `use super::*` away for the sibling CLI modules.

use super::*;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

pub use dbtl_report::{
    collect_sources, gate_status, lint_assembly, lint_report, render_report, DocsDrift, GateResult,
    GateStatus, LintAux, LintRunInputs, RenderCtx, RenderEnv, Report, ReportFormat,
};

/// The one `.yml` text source a run reads through: `location.file` paths
/// resolve under `root`, each file fetched once and shared between the
/// position fill ([`lint_assembly`]) and the excerpt collection
/// ([`collect_sources`]) — the text a position came from is always the text
/// its excerpt shows.
pub struct SourceTexts<'a> {
    root: &'a Path,
    texts: std::cell::RefCell<BTreeMap<String, Option<String>>>,
}

impl<'a> SourceTexts<'a> {
    /// `root` — the project root `location.file` paths are relative to (the
    /// same root the `.dbtl.yml` candidates already encode).
    pub fn new(root: &'a Path) -> Self {
        SourceTexts {
            root,
            texts: std::cell::RefCell::new(BTreeMap::new()),
        }
    }

    /// The `read_text` `dbtl_report`'s resolvers take — memoized so a file
    /// is read once per run however many findings/excerpts cite it; an
    /// unreadable file is `None`, exactly like before.
    pub fn read_text(&self) -> impl Fn(&str) -> Option<String> + '_ {
        |file| {
            let mut texts = self.texts.borrow_mut();
            texts
                .entry(file.to_string())
                .or_insert_with(|| std::fs::read_to_string(self.root.join(file)).ok())
                .clone()
        }
    }
}

/// Write `body` to stdout through `anstream` — ANSI escapes pass through
/// or strip per `choice` (`auto` follows the terminal/environment
/// detection). A closed pipe (`dbtl lint | head`) is a normal end of
/// output; any other write error surfaces to the caller.
pub(crate) fn write_stdout(body: &str, choice: anstream::ColorChoice) -> Result<(), CliError> {
    let mut out = anstream::AutoStream::new(std::io::stdout(), choice);
    match out.write_all(body.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(CliError::new(e.into())),
    }
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
    /// `--color` for the stdout human formats — resolved to the
    /// `anstream` choice that decides whether ANSI escapes pass through.
    pub color: anstream::ColorChoice,
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
        color: ColorArg,
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
            color: color.into(),
            reports,
            step_summary,
            path_prefix,
        })
    }

    /// Emit the report: stdout (unless `--quiet`), then each `--report` file
    /// (parents created, existing files overwritten — write failures are
    /// input/output errors), then the step-summary append. A `--report`
    /// file's bytes match what the same format would print on stdout apart
    /// from color — files are always unstyled.
    pub fn emit(
        &self,
        report: &Report,
        command: &str,
        env: &RenderEnv<'_>,
    ) -> Result<(), CliError> {
        // The excerpt texts the human formats annotate — gathered once and
        // shared across every render, so a file is never re-read per format.
        let needs_sources =
            self.format.is_human() || self.reports.iter().any(|(f, _)| f.is_human());
        let sources = needs_sources.then(|| collect_sources(report, env));
        let ctx = RenderCtx {
            command,
            path_prefix: self.path_prefix.as_deref(),
            dag: Some(env.dag),
            sources: sources.as_ref(),
            styled: false,
        };
        if !self.quiet {
            // Styling goes in only when the format uses it and the caller
            // didn't disable color — `--color never` renders plain, and the
            // stream strips whatever the environment disallows on `auto`.
            let stdout_ctx = RenderCtx {
                styled: self.format.is_human() && self.color != anstream::ColorChoice::Never,
                ..ctx
            };
            write_stdout(&render_report(self.format, report, &stdout_ctx), self.color)?;
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

#[cfg(test)]
mod tests {
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
            ColorArg::Auto,
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
                ColorArg::Auto,
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
                ColorArg::Auto,
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
            ColorArg::Auto,
            &["sarif:r.sarif".to_string()],
            false,
            Some("p"),
            &ReportFormat::ALL,
        )
        .unwrap();
        assert_eq!(plan.path_prefix.as_deref(), Some("p"));
    }
}
