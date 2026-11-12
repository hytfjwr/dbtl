use super::*;
use std::path::{Path, PathBuf};

/// Run the `impact` subcommand: resolve the current source + the `--diff`
/// baseline, compute the shared [`dbtl::diff_report`] payload, and print it as
/// Markdown (the same bytes the TUI's `D`→`e` export writes) or pretty JSON —
/// to stdout, no TTY needed, so CI can post it as a PR comment. `--report
/// markdown:<path>` / `--report json:<path>` additionally write those exact
/// bytes to files (parent directories created, existing files overwritten).
pub fn run_impact(
    format: ImpactFormat,
    reports: &[String],
    select: &[String],
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
    // A malformed --select is a usage error — checked before the load so it
    // can't hide behind a missing-input error.
    validate_selector_specs(select, "--select")?;
    let scope = src.scope;
    let resolved = src.resolve()?;
    let (base, label) = load_baseline(diff_path, scope, &src.project_dir())?;
    // `.dbtl.yml` (resolved next to the input, the same probe `check` uses)
    // supplies `owner_meta_key` and the changed-hubs threshold — so `dbtl
    // impact` and `dbtl check`'s hub gate can't disagree.
    let (manifest_arg, project_arg) = resolved_config_args(&resolved);
    let pc = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?;
    let owner_key = owner_meta_key(pc.config.owner_meta_key.as_deref(), true);
    // The same resolver pair the TUI diffs through (catalog-backed current,
    // catalog-less baseline) so the report's column analysis matches the
    // `D` modal byte-for-byte.
    let diff = selector_diff(src, &resolved, &base)?;
    // --select narrows the whole payload: `added`/`modified`/`column_changes`
    // intersect the selection while `removed` and the edge listings stay
    // (they name nodes the current dag can't resolve). An empty match warns
    // — the report then shows an empty change set, still a valid answer.
    let selection = if select.is_empty() {
        None
    } else {
        let sel = selector_set(&resolved.dag, Some(&diff), select);
        if sel.is_empty() {
            eprintln!("warning: --select '{}' matched no nodes", select.join(" "));
        }
        Some(sel)
    };
    let report = dbtl::diff_report(
        &resolved.dag,
        Some(&base),
        &diff,
        &label,
        scope,
        &dbtl::ImpactOpts {
            owner_key: &owner_key,
            hub_min_downstream: pc.config.check.min_downstream(),
            selection: selection.as_ref(),
        },
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
        ImpactFormat::Markdown => write_stdout(&markdown, anstream::ColorChoice::Auto)?,
        ImpactFormat::Json => {
            write_stdout(&json_bytes(&mut json_cache)?, anstream::ColorChoice::Auto)?
        }
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
    /// `--coverage-min` — the coverage gate's `min` override (`None` =
    /// `.dbtl.yml` / the default decides).
    pub coverage_min: Option<u8>,
    /// `--gate <id>=<severity>` — per-gate severity overrides (repeatable).
    pub gate: &'a [(dbtl::CheckGate, Option<dbtl::Severity>)],
    /// `--column-coverage-min` — the column-coverage gate's `min` override.
    pub column_coverage_min: Option<u8>,
    /// `--doc-coverage-min` — the doc-coverage gate's `min` override.
    pub doc_coverage_min: Option<u8>,
    /// `--max-blast-radius` — the blast-radius gate's `max` override.
    pub max_blast_radius: Option<usize>,
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
    /// `--color` — when the stdout `text`/`concise` report may be styled.
    pub color: ColorArg,
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
    /// `--select` — the dbt-selector specs every gate intersects.
    pub select: &'a [String],
}

/// Merge `.dbtl.yml`'s `check:` section with the CLI overrides into the
/// effective [`dbtl::CheckConfig`]. The named param flags
/// (`--coverage-min`, `--column-coverage-min`, `--doc-coverage-min`,
/// `--max-blast-radius`, `--docs-dir`) set their gate's parameter — and a
/// param flag on an `off` gate lifts it to `error` severity (an explicit
/// flag is an explicit enable, like `--no-breaking` has always been for
/// `breaking_columns`). `--gate <id>=<sev>` is applied LAST so it has the
/// final word on any gate's severity.
fn gate_policy(pc_check: &dbtl::CheckConfig, args: &CheckArgs) -> dbtl::CheckConfig {
    use dbtl::CheckGate as G;
    let mut cfg = pc_check.clone();
    fn entry(cfg: &mut dbtl::CheckConfig, gate: G) -> &mut dbtl::GateSetting {
        cfg.gates.entry(gate).or_insert_with(|| dbtl::GateSetting {
            severity: gate.default_severity(),
            ..dbtl::GateSetting::default()
        })
    }
    /// A param flag on an `off` gate enables it at `error` — the flag is
    /// the explicit ask.
    fn lift(s: &mut dbtl::GateSetting) {
        if s.severity.is_none() {
            s.severity = Some(dbtl::Severity::Error);
        }
    }
    if let Some(min) = args.coverage_min {
        let s = entry(&mut cfg, G::Coverage);
        s.min = Some(usize::from(min));
        lift(s);
    }
    if let Some(min) = args.column_coverage_min {
        let s = entry(&mut cfg, G::ColumnCoverage);
        s.min = Some(usize::from(min));
        lift(s);
    }
    if let Some(min) = args.doc_coverage_min {
        let s = entry(&mut cfg, G::DocCoverage);
        s.min = Some(usize::from(min));
        lift(s);
    }
    if let Some(max) = args.max_blast_radius {
        let s = entry(&mut cfg, G::BlastRadius);
        s.max = Some(max);
        lift(s);
    }
    if args.no_breaking {
        entry(&mut cfg, G::BreakingColumns).severity = Some(dbtl::Severity::Error);
    }
    if let Some(dir) = args.docs_dir {
        let s = entry(&mut cfg, G::DocsDrift);
        s.dir = Some(PathBuf::from(dir));
        lift(s);
    }
    // `--gate` is the last word on severity — it beats the named flags'
    // implied `error` (an explicit `=warn`/`=off` means exactly what it says).
    for (gate, severity) in args.gate {
        entry(&mut cfg, *gate).severity = *severity;
    }
    cfg
}

/// Push one gate row at its resolved severity: `Pass` when `ok` holds, else
/// the severity's `gate_status` verdict (`Fail`/`Warn` under `--fail-on`).
/// `Skip` rows are pushed by the caller — they don't evaluate.
fn push_gate(
    gates: &mut Vec<GateResult>,
    gate: dbtl::CheckGate,
    severity: dbtl::Severity,
    fail_on: dbtl::FailOn,
    ok: bool,
    ok_msg: String,
    trip_msg: String,
) {
    let (status, message) = if ok {
        (GateStatus::Pass, ok_msg)
    } else {
        (gate_status(severity, fail_on), trip_msg)
    };
    gates.push(GateResult::new(
        gate.id(),
        gate.title(),
        severity,
        status,
        message,
    ));
}

/// Run the `check` subcommand: evaluate the CI gates over the resolved
/// source, emit the shared [`Report`] through the selected formats, and
/// return `Err` (exit 1 via `main`) when any gate fails.
///
/// The gate set is `.dbtl.yml`'s `check:` section plus the CLI overrides,
/// resolved over the [`dbtl::CheckGate`] registry defaults (see
/// [`gate_policy`]): the coverage family (model test coverage, declared-
/// column coverage, description coverage), and — only with `--diff` — the
/// diff gates (new layer violations, untested changes, breaking column
/// changes, changed hubs, affected exposures, blast radius), all read off
/// the single [`dbtl::diff_report`] analysis the `impact` command prints.
/// `check.lint`/`--lint`/`--rules` add one gate per enabled static-lint
/// rule through the shared [`evaluate_lint`] — the SAME counted-finding
/// set `dbtl lint` reports. A diff gate the user explicitly enabled but
/// can't evaluate (no `--diff`) reports `skip`, never a failure — same for
/// an enabled `docs_drift` with no directory.
///
/// Every gate is severity-aware: a configured `warn` gate trips as WARN
/// (which `--fail-on warn` still fails), `off` removes the row, and
/// `--gate <id>=<sev>` overrides any source. The `scope:` line and the
/// `baseline:`/`affected owners:` context lines never gate; `--quiet`
/// prints nothing (exit code only).
pub fn run_check(src: &DataSourceArgs, args: &CheckArgs<'_>) -> Result<(), CliError> {
    use dbtl::CheckGate as G;
    let scope = src.scope;
    // `--rules` implies linting (a rule list with nothing to run is a no-op
    // flag, so honour the intent instead). An unknown name is a usage error —
    // checked before the data load so it can't hide behind a missing-input
    // error. Same for a malformed --report spec or a --path-prefix with no
    // path-carrying format selected.
    validate_rule_names(args.rules)?;
    // A malformed --select is a usage error (a CI gate must never silently
    // check an empty set), and a `state:` atom needs the `--diff` baseline
    // it compares against — both checked before the data load.
    validate_selector_specs(args.select, "--select")?;
    require_diff_for_state(args.select, src.diff.is_some())?;
    let plan = OutputPlan::build(
        args.format,
        args.quiet,
        args.color,
        args.reports,
        args.step_summary,
        args.path_prefix,
        &ReportFormat::ALL,
    )?;
    let resolved = src.resolve()?;
    let dag = &resolved.dag;
    // `.dbtl.yml` feeds EVERY gate — the `check:` section picks severities
    // and parameters, `lint:` the rule set, top-level `fail_on` the verdict
    // threshold — so it's loaded once up front: a malformed file is a usage
    // error (a CI gate can't silently run the wrong policy).
    let (manifest_arg, project_arg) = resolved_config_args(&resolved);
    let pc = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?;
    let fail_on = resolve_fail_on(args.fail_on, pc.config.fail_on);
    let baseline_explicit = pc.config.baseline_explicit;
    let config_found = pc.path.is_some();
    // `.dbtl.yml`'s check: + the CLI overrides → the effective gate set.
    let check_cfg = gate_policy(&pc.config.check, args);
    // `.dbtl.yml` owner_meta_key > config.yml's > `owner` — the ONE
    // resolution `dbtl impact` and the TUI `D` modal share.
    let owner_key = owner_meta_key(pc.config.owner_meta_key.as_deref(), true);
    let lint_active = args.lint || !args.rules.is_empty() || check_cfg.lint;

    // The baseline loads once and feeds EVERYTHING diff-aware — the diff
    // gates, the lint new-violation comparison, and `--select`'s `state:`
    // atoms — and the DagDiff is computed once through the same resolver
    // pair `dbtl impact` uses, so all three see the identical change set.
    let base_ctx = match src.diff {
        Some(d) => Some(load_baseline(d, scope, &src.project_dir())?),
        None => None,
    };
    let diff = match &base_ctx {
        Some((base, _)) => Some(selector_diff(src, &resolved, base)?),
        None => None,
    };
    // `--select` evaluates once into the uid set every gate intersects:
    // the coverage counts shrink to it, the diff gates read a
    // selection-narrowed `diff_report`, lint keeps only findings inside
    // it. `docs_drift` deliberately ignores it — a docs freshness
    // comparison is whole-set by definition. An empty match warns and
    // runs on (a gate on zero nodes passes).
    let selection = if args.select.is_empty() {
        None
    } else {
        let sel = selector_set(dag, diff.as_ref(), args.select);
        if sel.is_empty() {
            eprintln!(
                "warning: --select '{}' matched no nodes",
                args.select.join(" ")
            );
        }
        Some(sel)
    };

    // The shared report: gates in evaluation order, the context lines
    // (`baseline:`/`affected owners:`) anchored where the line report has
    // always printed them — after `scope:` + the coverage family.
    let mut report = Report::new(dag.project_name().unwrap_or("dbt"), scope.describe());
    let mut gates: Vec<GateResult> = Vec::new();

    // ---- the coverage family (no baseline needed) --------------------
    // Each reads a shared `*_counts` core function — the same numbers the
    // TUI gauges and `dbtl docs` summaries quote, so the gate can never
    // count differently from the surfaces. `selection` narrows the counted
    // population; `None` is the untouched whole-project count.
    if let Some(sev) = check_cfg.severity(G::Coverage) {
        let (tested, total) = dbtl::coverage_counts(dag, selection.as_ref());
        // No buildable nodes → nothing untested → 100%.
        let pct = (tested * 100).checked_div(total).unwrap_or(100);
        let min = check_cfg.min(G::Coverage);
        push_gate(
            &mut gates,
            G::Coverage,
            sev,
            fail_on,
            pct >= min,
            format!("{tested}/{total} ({pct}%)"),
            format!("coverage {pct}% below the {min}% minimum ({tested}/{total})"),
        );
    }
    if let Some(sev) = check_cfg.severity(G::ColumnCoverage) {
        let (covered, declared) = dbtl::column_coverage_counts(dag, selection.as_ref());
        // No declared columns → nothing to cover → 100%.
        let pct = (covered * 100).checked_div(declared).unwrap_or(100);
        let min = check_cfg.min(G::ColumnCoverage);
        push_gate(
            &mut gates,
            G::ColumnCoverage,
            sev,
            fail_on,
            pct >= min,
            format!("{covered}/{declared} ({pct}%)"),
            format!("column coverage {pct}% below the {min}% minimum ({covered}/{declared})"),
        );
    }
    if let Some(sev) = check_cfg.severity(G::DocCoverage) {
        let (documented, total) = dbtl::doc_coverage_counts(dag, selection.as_ref());
        let pct = (documented * 100).checked_div(total).unwrap_or(100);
        let min = check_cfg.min(G::DocCoverage);
        push_gate(
            &mut gates,
            G::DocCoverage,
            sev,
            fail_on,
            pct >= min,
            format!("{documented}/{total} ({pct}%)"),
            format!("doc coverage {pct}% below the {min}% minimum ({documented}/{total})"),
        );
    }

    // ---- the diff gates ----------------------------------------------
    // The same baseline/DagDiff pair `--select`'s `state:` atoms and the
    // lint narrowing already read — one change set per run.
    if let Some((base, label)) = &base_ctx {
        // Resolver-aware diff — the same pair `run_impact` and the TUI's `D`
        // modal use — so the breaking-columns gate rides the identical column
        // analysis the reviewer reads, and the report's `impact` block IS
        // the same payload `dbtl impact` prints (the hub list included —
        // `min_downstream` comes from the same `check:` resolution, and the
        // `selection` is the same set `--select` narrowed the coverage
        // counts to).
        let diff = diff.as_ref().expect("a baseline always yields a diff");
        let payload = dbtl::diff_report(
            dag,
            Some(base),
            diff,
            label,
            scope,
            &dbtl::ImpactOpts {
                owner_key: &owner_key,
                hub_min_downstream: check_cfg.min_downstream(),
                selection: selection.as_ref(),
            },
        );
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
        for gate in G::ALL.iter().copied().filter(|g| g.needs_diff()) {
            let Some(sev) = check_cfg.severity(gate) else {
                continue;
            };
            match gate {
                G::NewLayerViolations => push_gate(
                    &mut gates,
                    gate,
                    sev,
                    fail_on,
                    pr.new_layer_violations.is_empty(),
                    "none".to_string(),
                    format!(
                        "{} new layer violation(s): {}",
                        pr.new_layer_violations.len(),
                        pr.new_layer_violations
                            .iter()
                            .map(|(p, c)| format!("{p} -> {c}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                G::UntestedChanges => push_gate(
                    &mut gates,
                    gate,
                    sev,
                    fail_on,
                    pr.untested_changes.is_empty(),
                    "none".to_string(),
                    format!(
                        "{} changed node(s) without tests: {}",
                        pr.untested_changes.len(),
                        pr.untested_changes.join(", ")
                    ),
                ),
                // Fail only on BREAKING column changes whose downstream
                // still references the old column (removed/renamed with
                // unmigrated consumers). Partial moves (type/expr) report
                // in the impact pack, never here. Renames print
                // `old -> new` so the log shows the exact migration to ask
                // for.
                G::BreakingColumns => {
                    // Like every diff gate: the breaking changes ON
                    // selected nodes — a column break outside the
                    // selection doesn't gate.
                    let breaking: Vec<_> = diff
                        .breaking_referenced()
                        .into_iter()
                        .filter(|ch| selection.as_ref().is_none_or(|s| s.contains(&ch.node)))
                        .collect();
                    let names = breaking
                        .iter()
                        .map(|ch| match &ch.renamed_from {
                            Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                            None => format!("{}.{}", ch.node_name, ch.column),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    push_gate(
                        &mut gates,
                        gate,
                        sev,
                        fail_on,
                        breaking.is_empty(),
                        "none".to_string(),
                        format!(
                            "{} breaking column change(s) referenced downstream: {names}",
                            breaking.len()
                        ),
                    );
                }
                // The SAME hub list the report's `## Hubs` section prints —
                // `pr_impact` already applied the configured
                // `min_downstream`, so the gate and the pack can't disagree.
                G::ChangedHubs => {
                    let min = check_cfg.min_downstream();
                    push_gate(
                        &mut gates,
                        gate,
                        sev,
                        fail_on,
                        pr.changed_hubs.is_empty(),
                        "none".to_string(),
                        format!(
                            "{} changed hub(s) (>= {min} downstream): {}",
                            pr.changed_hubs.len(),
                            pr.changed_hubs
                                .iter()
                                .map(|(name, k)| format!("{name} ({k})"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                }
                G::AffectedExposures => push_gate(
                    &mut gates,
                    gate,
                    sev,
                    fail_on,
                    pr.affected_exposures.is_empty(),
                    "none".to_string(),
                    format!(
                        "{} affected exposure(s): {}",
                        pr.affected_exposures.len(),
                        pr.affected_exposures.join(", ")
                    ),
                ),
                G::BlastRadius => {
                    let max = check_cfg.max(G::BlastRadius);
                    push_gate(
                        &mut gates,
                        gate,
                        sev,
                        fail_on,
                        pr.affected_models <= max,
                        format!("{} affected model(s) (max {max})", pr.affected_models),
                        format!(
                            "blast radius {} affected model(s) exceeds max {max}",
                            pr.affected_models
                        ),
                    );
                }
                // Only `needs_diff` gates iterate — the coverage family and
                // docs_drift have their own slots.
                _ => unreachable!("non-diff gate in the diff loop"),
            }
        }
        report.impact = Some(payload);
    }
    // An explicitly-enabled diff gate without `--diff` has nothing to
    // evaluate — report `skip`: visible to CI parsers, never counted as a
    // failure. A default-on diff gate nobody asked for just isn't listed,
    // exactly as before.
    if base_ctx.is_none() {
        for gate in G::ALL.iter().copied().filter(|g| g.needs_diff()) {
            if check_cfg.explicit(gate) {
                if let Some(sev) = check_cfg.severity(gate) {
                    gates.push(GateResult::new(
                        gate.id(),
                        gate.title(),
                        sev,
                        GateStatus::Skip,
                        "needs --diff".to_string(),
                    ));
                }
            }
        }
    }

    // `docs_drift` — the same freshness comparison `dbtl docs --check`
    // runs: regenerate the file set in memory and diff it against the
    // directory (`--docs-dir` or `check.gates.docs_drift.dir`; the flag
    // wins). The drift detail rides as the gate's `details` lines AND the
    // report's `docs_drift` block, so every format shows the divergence.
    if let Some(sev) = check_cfg.severity(G::DocsDrift) {
        match check_cfg.docs_dir().map(Path::to_path_buf) {
            Some(dir) => {
                // No --lineage-depth flag on check — the gate resolves
                // depth from `.dbtl.yml`/defaults exactly like `docs
                // --check` without the flag. `src` feeds the aux lint
                // inputs AND the explicit-artifact opt-ins `docs --check`
                // honours, so this gate can never disagree with it. And
                // --select never narrows it: a docs freshness comparison
                // is whole-set by definition (`DocsScope::default` = no
                // narrowing).
                let files = docs_file_set(src, &resolved, scope, None, &DocsScope::default())?;
                let drift = docs_differences(&dir, &files);
                if drift.total() == 0 {
                    gates.push(GateResult::new(
                        G::DocsDrift.id(),
                        G::DocsDrift.title(),
                        sev,
                        GateStatus::Pass,
                        format!("up to date ({} files)", files.len()),
                    ));
                } else {
                    let mut gate = GateResult::new(
                        G::DocsDrift.id(),
                        G::DocsDrift.title(),
                        sev,
                        gate_status(sev, fail_on),
                        format!("docs drift: {} difference(s)", drift.total()),
                    );
                    gate.details = drift.lines();
                    gates.push(gate);
                }
                report.docs_drift = Some(drift);
            }
            // An enabled gate nobody pointed at a directory can't
            // evaluate — `skip`, never a failure.
            None if check_cfg.explicit(G::DocsDrift) => gates.push(GateResult::new(
                G::DocsDrift.id(),
                G::DocsDrift.title(),
                sev,
                GateStatus::Skip,
                "needs --docs-dir or check.gates.docs_drift.dir".to_string(),
            )),
            None => {}
        }
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
        let candidates =
            dbtl::project_config_candidates(manifest_arg.as_deref(), project_arg.as_deref());
        let baseline = load_lint_baseline_file(
            &lint_baseline_path(&cfg.baseline, config_found, &candidates),
            baseline_explicit,
        )?;
        // The same aux inputs `dbtl lint` builds — the lazy resolver and
        // the catalog — so the gate and the report can never evaluate
        // differently.
        let (lint_catalog, lint_resolver) = lint_aux(&resolved, src.sql_dialect, src.catalog)?;
        let lint_base_resolver = base_ctx
            .as_ref()
            .map(|(b, _)| src.baseline_resolver(dag, b));
        let mut outcome = evaluate_lint(
            dag,
            &cfg,
            base_ctx.as_ref().map(|(b, _)| b),
            baseline.as_ref(),
            fail_on,
            LintAux {
                sql: Some(&lint_resolver),
                catalog: lint_catalog.as_deref(),
                base_sql: lint_base_resolver.as_ref(),
            },
            selection.as_ref(),
        );
        // The same best-effort yml line fill `dbtl lint` reports —
        // filled before `lint_gates` lifts the findings into the Report.
        dbtl::fill_finding_lines(&mut outcome.findings, dag, &lint_project_root(&candidates));
        gates.extend(lint_gates(dag, &outcome));
        report.lint = Some(LintTally {
            pre_existing: outcome.rules.iter().map(|r| r.pre_existing).sum(),
            has_diff: outcome.has_diff,
            skipped: outcome.skipped,
            unparsed_models: outcome.unparsed.len(),
        });
    }

    let failures = gates
        .iter()
        .filter(|g| g.status == GateStatus::Fail)
        .count();
    report.gates = gates;
    report.summary = Report::summarize(&report.gates);
    // The same `.dbtl.yml`-candidate root `fill_finding_lines` resolved
    // `location.file` against — `.yml` excerpts read from disk here.
    let lint_root = lint_project_root(&dbtl::project_config_candidates(
        manifest_arg.as_deref(),
        project_arg.as_deref(),
    ));
    plan.emit(
        &report,
        "check",
        &RenderEnv {
            dag,
            project_root: &lint_root,
        },
    )?;
    if failures == 0 {
        Ok(())
    } else {
        Err(CliError::gate(anyhow::anyhow!(
            "{failures} check(s) failed"
        )))
    }
}
