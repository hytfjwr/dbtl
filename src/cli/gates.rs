use super::*;
use std::path::Path;

/// Run the `impact` subcommand: resolve the current source + the `--diff`
/// baseline, compute the shared [`dbtl::diff_report`] payload, and print it as
/// Markdown (the same bytes the TUI's `D`→`e` export writes) or pretty JSON —
/// to stdout, no TTY needed, so CI can post it as a PR comment. `--report
/// markdown:<path>` / `--report json:<path>` additionally write those exact
/// bytes to files (parent directories created, existing files overwritten).
pub fn run_impact(
    format: ImpactFormat,
    reports: &[String],
    src: &DataSourceArgs,
) -> Result<(), CliError> {
    // The baseline is `impact`'s reason to exist — missing is a usage error,
    // checked before any loading happens. Same for a malformed --report spec
    // (impact's payload only exists as markdown or json).
    let diff_path = src
        .diff
        .ok_or_else(|| CliError::usage(anyhow::anyhow!("impact requires '--diff <BASELINE>'")))?;
    const IMPACT_FORMATS: &[ReportFormat] = &[ReportFormat::Markdown, ReportFormat::Json];
    let mut files = Vec::with_capacity(reports.len());
    for spec in reports {
        files.push(parse_report_spec(spec, IMPACT_FORMATS)?);
    }
    let scope = src.scope;
    let resolved = src.resolve()?;
    let (base, label) = load_baseline(diff_path, scope)?;
    // The same resolver pair the TUI diffs through (catalog-backed current,
    // catalog-less baseline) so the report's column analysis matches the
    // `D` modal byte-for-byte.
    let cur_res = src.cll_resolver(&resolved)?;
    let base_res = src.baseline_resolver(&resolved.dag, &base);
    let diff = dbtl::compute_diff_with(&base, &resolved.dag, &base_res, &cur_res);
    let report = dbtl::diff_report(
        &resolved.dag,
        Some(&base),
        &diff,
        &label,
        scope,
        &owner_meta_key(),
    );
    let project = resolved.dag.project_name().unwrap_or("dbt");
    let markdown = dbtl::pr_impact_markdown(project, &report);
    // The JSON bytes are computed once and shared between stdout and every
    // `--report json:` file — both carry the DiffReport verbatim.
    let mut json_cache: Option<String> = None;
    let json_bytes = |cache: &mut Option<String>| -> Result<String, CliError> {
        match cache {
            Some(s) => Ok(s.clone()),
            slot @ None => {
                let s = format!(
                    "{}\n",
                    serde_json::to_string_pretty(&report).map_err(|e| CliError::new(e.into()))?
                );
                *slot = Some(s.clone());
                Ok(s)
            }
        }
    };
    match format {
        ImpactFormat::Markdown => print!("{markdown}"),
        ImpactFormat::Json => print!("{}", json_bytes(&mut json_cache)?),
    }
    for (fmt, path) in &files {
        let contents = match fmt {
            ReportFormat::Markdown => markdown.clone(),
            ReportFormat::Json => json_bytes(&mut json_cache)?,
            // parse_report_spec already rejected every other format.
            _ => unreachable!("impact --report validated to markdown/json"),
        };
        write_report_file(path, &contents)?;
    }
    Ok(())
}

/// The `check` subcommand's options, bundled so the runner's signature stays
/// readable (the fields map 1:1 onto the clap variant's args).
pub struct CheckArgs<'a> {
    /// `--coverage-min` — the coverage gate's minimum percent (0 = off).
    pub coverage_min: u8,
    /// `--lint` — add one gate per enabled lint rule.
    pub lint: bool,
    /// `--no-breaking` — with `--diff`, gate on breaking column changes.
    pub no_breaking: bool,
    /// `--rules` — a lint rule list (implies `--lint`).
    pub rules: &'a [String],
    /// `--fail-on` — overrides `.dbtl.yml`'s `fail_on`.
    pub fail_on: Option<dbtl::FailOn>,
    /// `--format` — the stdout report format.
    pub format: ReportFormat,
    /// `--quiet` — suppress stdout; the exit code carries the verdict.
    pub quiet: bool,
    /// `--report <FMT>:<PATH>` destinations (repeatable).
    pub reports: &'a [String],
    /// `--step-summary` — append the markdown report to $GITHUB_STEP_SUMMARY.
    pub step_summary: bool,
    /// `--path-prefix` — prepended to file paths in path-carrying formats.
    pub path_prefix: Option<&'a str>,
    /// `--docs-dir` — gate on generated-docs freshness in this directory.
    pub docs_dir: Option<&'a str>,
}

/// Run the `check` subcommand: evaluate the CI gates over the resolved
/// source, emit the shared [`Report`] through the selected formats, and
/// return `Err` (exit 1 via `main`) when any gate fails. Coverage below
/// `--coverage-min` (when >0), and — only with `--diff` — newly introduced
/// layer violations, changed buildable nodes without tests, and (with
/// `--no-breaking`) breaking column changes still referenced downstream, all
/// from the single [`dbtl::diff_report`] analysis. `--lint` adds one
/// PASS/WARN/FAIL gate per static-lint rule through the shared
/// [`evaluate_lint`] — the SAME counted-finding set `dbtl lint` reports
/// (non-baselined, new under `--diff`). `--docs-dir` adds the docs-freshness
/// gate (the exact comparison `dbtl docs --check` runs). Every gate is
/// severity-aware: the non-lint gates are `error` by definition and lint
/// findings carry their rule's effective severity, so `--fail-on` (CLI >
/// `.dbtl.yml` > default `error`) decides whether a tripped gate FAILs or
/// only WARNs. The `scope:` line and the `baseline:`/`affected owners:`
/// context lines never gate; `--quiet` prints nothing (exit code only).
pub fn run_check(src: &DataSourceArgs, args: &CheckArgs<'_>) -> Result<(), CliError> {
    let scope = src.scope;
    // `--rules` implies linting (a rule list with nothing to run is a no-op
    // flag, so honour the intent instead). An unknown name is a usage error —
    // checked before the data load so it can't hide behind a missing-input
    // error. Same for a malformed --report spec or a --path-prefix with no
    // path-carrying format selected.
    let lint_active = args.lint || !args.rules.is_empty();
    validate_rule_names(args.rules)?;
    let plan = OutputPlan::build(
        args.format,
        args.quiet,
        args.reports,
        args.step_summary,
        args.path_prefix,
        &ReportFormat::ALL,
    )?;
    let resolved = src.resolve()?;
    let dag = &resolved.dag;
    // `.dbtl.yml` feeds EVERY gate via the top-level `fail_on`, so it's
    // loaded once up front — a malformed file is a usage error even with
    // lint off (its threshold can't be trusted either).
    let (manifest_arg, project_arg) = resolved_config_args(&resolved);
    let pc = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?;
    let fail_on = resolve_fail_on(args.fail_on, pc.config.fail_on);
    let baseline_explicit = pc.config.baseline_explicit;
    let config_found = pc.path.is_some();

    // The shared report: gates in evaluation order, the context lines
    // (`baseline:`/`affected owners:`) anchored where the line report has
    // always printed them — after `scope:` + the coverage gate.
    let mut report = Report::new(dag.project_name().unwrap_or("dbt"), scope.describe());
    let mut gates: Vec<GateResult> = Vec::new();

    // Coverage — always measured (the shared `coverage_counts` base, the same
    // numbers the TUI's `cov` segment quotes); the gate only engages above 0.
    let (tested, total) = dbtl::coverage_counts(dag);
    // No buildable nodes → nothing untested → 100%.
    let pct = (tested * 100).checked_div(total).unwrap_or(100);
    if args.coverage_min == 0 || pct >= usize::from(args.coverage_min) {
        gates.push(GateResult::pass(
            "coverage",
            "coverage",
            format!("{tested}/{total} ({pct}%)"),
        ));
    } else {
        gates.push(GateResult::new(
            "coverage",
            "coverage",
            dbtl::Severity::Error,
            gate_status(dbtl::Severity::Error, fail_on),
            format!(
                "coverage {pct}% below --coverage-min {}% ({tested}/{total})",
                args.coverage_min
            ),
        ));
    }

    // The baseline loads once and feeds BOTH diff gates (layer violations +
    // untested changes) and the lint new-violation comparison.
    let base_ctx = match src.diff {
        Some(d) => Some(load_baseline(d, scope)?),
        None => None,
    };
    if let Some((base, label)) = &base_ctx {
        // Resolver-aware diff — the same pair `run_impact` and the TUI's `D`
        // modal use — so `--no-breaking` gates on the identical column
        // analysis the reviewer reads, and the report's `impact` block IS
        // the same payload `dbtl impact` prints.
        let cur_res = src.cll_resolver(&resolved)?;
        let base_res = src.baseline_resolver(dag, base);
        let diff = dbtl::compute_diff_with(base, dag, &base_res, &cur_res);
        let payload = dbtl::diff_report(dag, Some(base), &diff, label, scope, &owner_meta_key());
        let pr = &payload.pr;
        report.info.push(format!("baseline: {label}"));
        // Informational listing (never a gate): who to page for the blast
        // radius — the same `affected_owners` set the impact report lists.
        if !pr.affected_owners.is_empty() {
            report.info.push(format!(
                "affected owners: {}",
                pr.affected_owners.join(", ")
            ));
        }
        report.info_at = gates.len();
        if pr.new_layer_violations.is_empty() {
            gates.push(GateResult::pass(
                "new_layer_violations",
                "new layer violations",
                "none".to_string(),
            ));
        } else {
            gates.push(GateResult::new(
                "new_layer_violations",
                "new layer violations",
                dbtl::Severity::Error,
                gate_status(dbtl::Severity::Error, fail_on),
                format!(
                    "{} new layer violation(s): {}",
                    pr.new_layer_violations.len(),
                    pr.new_layer_violations
                        .iter()
                        .map(|(p, c)| format!("{p} -> {c}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        if pr.untested_changes.is_empty() {
            gates.push(GateResult::pass(
                "untested_changes",
                "untested changes",
                "none".to_string(),
            ));
        } else {
            gates.push(GateResult::new(
                "untested_changes",
                "untested changes",
                dbtl::Severity::Error,
                gate_status(dbtl::Severity::Error, fail_on),
                format!(
                    "{} changed node(s) without tests: {}",
                    pr.untested_changes.len(),
                    pr.untested_changes.join(", ")
                ),
            ));
        }
        // `--no-breaking`: fail only on BREAKING column changes whose
        // downstream still references the old column (removed/renamed with
        // unmigrated consumers). Partial moves (type/expr) report in the
        // impact pack, never here. Renames print `old -> new` so the log
        // shows the exact migration to ask for.
        if args.no_breaking {
            let breaking = diff.breaking_referenced();
            if breaking.is_empty() {
                gates.push(GateResult::pass(
                    "breaking_columns",
                    "breaking column changes",
                    "none".to_string(),
                ));
            } else {
                let names = breaking
                    .iter()
                    .map(|ch| match &ch.renamed_from {
                        Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                        None => format!("{}.{}", ch.node_name, ch.column),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                gates.push(GateResult::new(
                    "breaking_columns",
                    "breaking column changes",
                    dbtl::Severity::Error,
                    gate_status(dbtl::Severity::Error, fail_on),
                    format!(
                        "{} breaking column change(s) referenced downstream: {names}",
                        breaking.len()
                    ),
                ));
            }
        }
        report.impact = Some(payload);
    }
    // `--no-breaking` is a diff gate — without a baseline there is nothing
    // to classify. A gate that was asked for but could not evaluate is a
    // `skip`: visible to CI parsers, never counted as a failure.
    if args.no_breaking && base_ctx.is_none() {
        gates.push(GateResult::skip(
            "breaking_columns",
            "breaking column changes",
            "needs --diff".to_string(),
        ));
    }

    // `--docs-dir` — the same freshness comparison `dbtl docs --check` runs:
    // regenerate the file set in memory and diff it against the directory.
    // The drift detail rides as the gate's `details` lines AND the report's
    // `docs_drift` block, so every format shows the divergence.
    if let Some(dir) = args.docs_dir {
        // No --lineage-depth flag on check — the gate resolves depth from
        // `.dbtl.yml`/defaults exactly like `docs --check` without the flag.
        let files = docs_file_set(&resolved, scope, None)?;
        let drift = docs_differences(Path::new(dir), &files);
        if drift.total() == 0 {
            gates.push(GateResult::pass(
                "docs_drift",
                "docs drift",
                format!("up to date ({} files)", files.len()),
            ));
        } else {
            let mut gate = GateResult::new(
                "docs_drift",
                "docs drift",
                dbtl::Severity::Error,
                gate_status(dbtl::Severity::Error, fail_on),
                format!("docs drift: {} difference(s)", drift.total()),
            );
            gate.details = drift.lines();
            gates.push(gate);
        }
        report.docs_drift = Some(drift);
    }

    // Static lint — the shared evaluation `dbtl lint` runs verbatim: the
    // same `run_rules` findings, the same baseline file, the same `--diff`
    // new-violation narrowing, so the two commands can never count
    // differently. The project-local `.dbtl.yml` supplies
    // enables/thresholds/prefixes; a malformed file is FATAL here (a CI gate
    // must never silently lint the wrong rule set — the TUI's warn+fallback
    // policy is deliberately different). `--rules` overrides the file's
    // enables: it selects exactly which rules run, the file still supplies
    // the thresholds/prefixes.
    if lint_active {
        let mut cfg = pc.config.lint;
        apply_rules_overlay(&mut cfg, args.rules)?;
        let baseline = load_lint_baseline_file(
            &lint_baseline_path(
                &cfg.baseline,
                config_found,
                &dbtl::project_config_candidates(manifest_arg.as_deref(), project_arg.as_deref()),
            ),
            baseline_explicit,
        )?;
        let outcome = evaluate_lint(
            dag,
            &cfg,
            base_ctx.as_ref().map(|(b, _)| b),
            baseline.as_ref(),
            fail_on,
        );
        gates.extend(lint_gates(dag, &outcome));
    }

    let failures = gates
        .iter()
        .filter(|g| g.status == GateStatus::Fail)
        .count();
    report.gates = gates;
    report.summary = Report::summarize(&report.gates);
    plan.emit(&report, "check")?;
    if failures == 0 {
        Ok(())
    } else {
        Err(CliError::gate(anyhow::anyhow!(
            "{failures} check(s) failed"
        )))
    }
}
