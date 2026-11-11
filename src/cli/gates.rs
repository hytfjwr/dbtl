use super::*;

/// Run the `impact` subcommand: resolve the current source + the `--diff`
/// baseline, compute the shared [`dbtl::diff_report`] payload, and print it as
/// Markdown (the same bytes the TUI's `D`→`e` export writes) or pretty JSON —
/// to stdout, no TTY needed, so CI can post it as a PR comment.
pub fn run_impact(format: ImpactFormat, src: &DataSourceArgs) -> Result<(), CliError> {
    // The baseline is `impact`'s reason to exist — missing is a usage error,
    // checked before any loading happens.
    let diff_path = src
        .diff
        .ok_or_else(|| CliError::usage(anyhow::anyhow!("impact requires '--diff <BASELINE>'")))?;
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
    match format {
        ImpactFormat::Markdown => {
            let project = resolved.dag.project_name().unwrap_or("dbt");
            print!("{}", dbtl::pr_impact_markdown(project, &report));
        }
        ImpactFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| CliError::new(e.into()))?
            );
        }
    }
    Ok(())
}

/// One gate check in the `--format json` report: its name, pass/warn/fail,
/// the same detail string the line report carries, and — for lint checks —
/// the gating [`dbtl::Finding`] list automation consumes (new violations
/// only under `--diff`).
#[derive(serde::Serialize)]
pub(super) struct CheckJsonItem {
    check: String,
    status: &'static str, // "pass" | "warn" | "fail"
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<dbtl::Finding>,
}

/// The `check --format json` payload: the scope line, the never-gating context
/// lines (`baseline:`, `affected owners:`), then every check in report order.
/// `failures`/`warnings` let a consumer read the verdict without re-counting
/// statuses (`warnings` = WARN items — reported but never gating).
#[derive(serde::Serialize)]
pub(super) struct CheckReportJson {
    scope: String,
    info: Vec<String>,
    checks: Vec<CheckJsonItem>,
    failures: usize,
    warnings: usize,
}

/// A tripped gate's report payload — what a FAIL/WARN line and its JSON
/// mirror print.
struct GateTrip {
    /// The `FAIL|WARN <message>` body.
    message: String,
    /// The parenthesized line suffix — the findings' severity plus any
    /// extra counts (`error`, `warn, 2 baselined`).
    suffix: String,
    /// The JSON item's `message` (usually a shortened `message`).
    json_message: String,
    /// fail counts toward `failures`; warn reports without counting.
    status: GateStatus,
    /// The findings the JSON item lists (lint checks only).
    findings: Vec<dbtl::Finding>,
}

/// A tripped non-lint gate: fixed error severity, no findings — the same
/// shape the lint rule trips use, so every gate shares the one `fail_on`
/// rule (and `--fail-on never` downgrades it to WARN like everything else).
fn fixed_gate_trip(message: String, fail_on: dbtl::FailOn) -> GateTrip {
    GateTrip {
        status: gate_status(dbtl::Severity::Error, fail_on),
        suffix: "error".to_string(),
        json_message: message.clone(),
        message,
        findings: Vec::new(),
    }
}

/// Append one gate result — the `PASS <label>: <pass_msg>` line when clean,
/// or the `WARN|FAIL <trip.message> (<trip.suffix>)` line when tripped —
/// plus the JSON mirror, keeping both renderings in lockstep by building
/// them from one argument set.
fn push_gate(
    lines: &mut Vec<String>,
    failures: &mut Vec<String>,
    json_checks: &mut Vec<CheckJsonItem>,
    label: &str,
    check_name: &str,
    pass_msg: &str,
    trip: Option<GateTrip>,
) {
    match trip {
        None => {
            lines.push(format!("PASS {label}: {pass_msg}"));
            json_checks.push(CheckJsonItem {
                check: check_name.to_string(),
                status: GateStatus::Pass.json(),
                message: pass_msg.to_string(),
                findings: Vec::new(),
            });
        }
        Some(trip) => {
            lines.push(format!(
                "{} {} ({})",
                trip.status.word(),
                trip.message,
                trip.suffix
            ));
            if trip.status == GateStatus::Fail {
                failures.push(trip.message.clone());
            }
            json_checks.push(CheckJsonItem {
                check: check_name.to_string(),
                status: trip.status.json(),
                message: trip.json_message,
                findings: trip.findings,
            });
        }
    }
}

/// Run the `check` subcommand: evaluate the CI gates over the resolved source
/// and return `Err` (exit 1 via `main`) when any fails. Coverage below
/// `--coverage-min` (when >0), and — only with `--diff` — newly introduced
/// layer violations, changed buildable nodes without tests, and (with
/// `--no-breaking`) breaking column changes still referenced downstream, all
/// from the single [`dbtl::pr_impact`] / [`dbtl::compute_diff_with`] analyses.
/// `--lint` adds one PASS/WARN/FAIL line per static-lint rule through the
/// shared [`evaluate_lint`] — the SAME counted-finding set `dbtl lint`
/// reports (non-baselined, new under `--diff`). Every gate is severity-aware:
/// the non-lint gates are `error` by definition and lint findings carry their
/// rule's effective severity, so `--fail-on` (CLI > `.dbtl.yml` > default
/// `error`) decides whether a tripped gate FAILs or only WARNs. Prints one
/// line per check to stdout (or the whole report as one JSON object under
/// `--format json`), plus informational lines that never gate (`baseline:`
/// and, when the blast radius has any, `affected owners:`); `--quiet` prints
/// nothing (exit code only).
#[allow(clippy::too_many_arguments)] // the subcommand's flat flag set
pub fn run_check(
    src: &DataSourceArgs,
    coverage_min: u8,
    lint: bool,
    rules: &[String],
    cli_fail_on: Option<dbtl::FailOn>,
    format: ImpactFormat,
    quiet: bool,
    no_breaking: bool,
) -> Result<(), CliError> {
    let scope = src.scope;
    // `--rules` implies linting (a rule list with nothing to run is a no-op
    // flag, so honour the intent instead). An unknown name is a usage error —
    // checked before the data load so it can't hide behind a missing-input
    // error.
    let lint_active = lint || !rules.is_empty();
    validate_rule_names(rules)?;
    let resolved = src.resolve()?;
    let dag = &resolved.dag;
    // `.dbtl.yml` feeds EVERY gate via the top-level `fail_on`, so it's
    // loaded once up front — a malformed file is a usage error even with
    // lint off (its threshold can't be trusted either).
    let (manifest_arg, project_arg) = resolved_config_args(&resolved);
    let pc = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?;
    let fail_on = resolve_fail_on(cli_fail_on, pc.config.fail_on);
    let baseline_explicit = pc.config.baseline_explicit;
    let config_found = pc.path.is_some();

    let mut lines: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    // The JSON mirror of the report, populated alongside each line so both
    // renderings always agree.
    let mut json_info: Vec<String> = Vec::new();
    let mut json_checks: Vec<CheckJsonItem> = Vec::new();

    // The scope line always leads the report: the checked numbers describe
    // the ROOT project under either scope (installed-package nodes, when
    // loaded via --include-packages, are dependency context only), and the
    // line says so verbatim so a pasted CI log can never be misread.
    let scope_desc = scope.describe().to_string();
    lines.push(format!("scope: {scope_desc}"));

    // Coverage — always measured (the shared `coverage_counts` base, the same
    // numbers the TUI's `cov` segment quotes); the gate only engages above 0.
    let (tested, total) = dbtl::coverage_counts(dag);
    // No buildable nodes → nothing untested → 100%.
    let pct = (tested * 100).checked_div(total).unwrap_or(100);
    if coverage_min == 0 || pct >= usize::from(coverage_min) {
        let msg = format!("{tested}/{total} ({pct}%)");
        push_gate(
            &mut lines,
            &mut failures,
            &mut json_checks,
            "coverage",
            "coverage",
            &msg,
            None,
        );
    } else {
        push_gate(
            &mut lines,
            &mut failures,
            &mut json_checks,
            "coverage",
            "coverage",
            "",
            Some(fixed_gate_trip(
                format!("coverage {pct}% below --coverage-min {coverage_min}% ({tested}/{total})"),
                fail_on,
            )),
        );
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
        // analysis the reviewer reads.
        let cur_res = src.cll_resolver(&resolved)?;
        let base_res = src.baseline_resolver(dag, base);
        let diff = dbtl::compute_diff_with(base, dag, &base_res, &cur_res);
        let pr = dbtl::pr_impact(dag, Some(base), &diff, &owner_meta_key());
        lines.push(format!("baseline: {label}"));
        json_info.push(format!("baseline: {label}"));
        // Informational listing (never a gate): who to page for the blast
        // radius — the same `affected_owners` set the impact report lists.
        if !pr.affected_owners.is_empty() {
            let info = format!("affected owners: {}", pr.affected_owners.join(", "));
            lines.push(info.clone());
            json_info.push(info);
        }
        if pr.new_layer_violations.is_empty() {
            push_gate(
                &mut lines,
                &mut failures,
                &mut json_checks,
                "new layer violations",
                "new_layer_violations",
                "none",
                None,
            );
        } else {
            push_gate(
                &mut lines,
                &mut failures,
                &mut json_checks,
                "new layer violations",
                "new_layer_violations",
                "",
                Some(fixed_gate_trip(
                    format!(
                        "{} new layer violation(s): {}",
                        pr.new_layer_violations.len(),
                        pr.new_layer_violations
                            .iter()
                            .map(|(p, c)| format!("{p} -> {c}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    fail_on,
                )),
            );
        }
        if pr.untested_changes.is_empty() {
            push_gate(
                &mut lines,
                &mut failures,
                &mut json_checks,
                "untested changes",
                "untested_changes",
                "none",
                None,
            );
        } else {
            push_gate(
                &mut lines,
                &mut failures,
                &mut json_checks,
                "untested changes",
                "untested_changes",
                "",
                Some(fixed_gate_trip(
                    format!(
                        "{} changed node(s) without tests: {}",
                        pr.untested_changes.len(),
                        pr.untested_changes.join(", ")
                    ),
                    fail_on,
                )),
            );
        }
        // `--no-breaking`: fail only on BREAKING column changes whose
        // downstream still references the old column (removed/renamed with
        // unmigrated consumers). Partial moves (type/expr) report in the
        // impact pack, never here. Renames print `old -> new` so the log
        // shows the exact migration to ask for.
        if no_breaking {
            let breaking = diff.breaking_referenced();
            if breaking.is_empty() {
                push_gate(
                    &mut lines,
                    &mut failures,
                    &mut json_checks,
                    "breaking column changes",
                    "breaking_columns",
                    "none",
                    None,
                );
            } else {
                let names = breaking
                    .iter()
                    .map(|ch| match &ch.renamed_from {
                        Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                        None => format!("{}.{}", ch.node_name, ch.column),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                push_gate(
                    &mut lines,
                    &mut failures,
                    &mut json_checks,
                    "breaking column changes",
                    "breaking_columns",
                    "",
                    Some(fixed_gate_trip(
                        format!(
                            "{} breaking column change(s) referenced downstream: {names}",
                            breaking.len()
                        ),
                        fail_on,
                    )),
                );
            }
        }
    }
    // `--no-breaking` is a diff gate — without a baseline there is nothing
    // to classify. A silent no-op gate is the worst CI pattern, so say so.
    // The note goes to `info`, not `checks`: no check ran, and a
    // skipped-gate entry would look like a result to a CI parser.
    if no_breaking && base_ctx.is_none() {
        let note = "note: --no-breaking needs --diff; skipped".to_string();
        lines.push(note.clone());
        json_info.push(note);
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
        apply_rules_overlay(&mut cfg, rules)?;
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
        let name = |uid: &str| -> String {
            dag.get(uid)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| uid.to_string())
        };
        for ro in &outcome.rules {
            let label = format!("lint {}", ro.rule.name());
            let check_name = format!("lint:{}", ro.rule.name());
            if ro.status == GateStatus::Pass {
                // Under --diff a rule whose violations are all pre-existing
                // still passes — the message says so explicitly.
                let msg = if outcome.has_diff && (ro.pre_existing + ro.baselined) > 0 {
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
                push_gate(
                    &mut lines,
                    &mut failures,
                    &mut json_checks,
                    &label,
                    &check_name,
                    &msg,
                    None,
                );
            } else {
                let names = ro
                    .counted
                    .iter()
                    .map(|&i| name(&outcome.findings[i].node_uid))
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = if outcome.has_diff {
                    format!(
                        "lint {}: {} new violation(s): {}",
                        ro.rule.name(),
                        ro.counted.len(),
                        names
                    )
                } else {
                    format!(
                        "lint {}: {} violation(s): {}",
                        ro.rule.name(),
                        ro.counted.len(),
                        names
                    )
                };
                let mut suffix = ro.severity.name().to_string();
                if ro.baselined > 0 {
                    suffix += &format!(", {} baselined", ro.baselined);
                }
                push_gate(
                    &mut lines,
                    &mut failures,
                    &mut json_checks,
                    &label,
                    &check_name,
                    "",
                    Some(GateTrip {
                        status: ro.status,
                        suffix,
                        json_message: format!("{} violation(s)", ro.counted.len()),
                        message: msg,
                        findings: ro
                            .counted
                            .iter()
                            .map(|&i| outcome.findings[i].clone())
                            .collect(),
                    }),
                );
            }
        }
    }

    if !quiet {
        match format {
            ImpactFormat::Markdown => {
                for line in &lines {
                    println!("{line}");
                }
            }
            ImpactFormat::Json => {
                let report = CheckReportJson {
                    scope: scope_desc,
                    info: json_info,
                    warnings: json_checks.iter().filter(|c| c.status == "warn").count(),
                    checks: json_checks,
                    failures: failures.len(),
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| CliError::new(e.into()))?
                );
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(CliError::gate(anyhow::anyhow!(
            "{} check(s) failed",
            failures.len()
        )))
    }
}
