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

/// One gate check in the `--format json` report: its name, pass/fail, the same
/// detail string the line report carries, and — for lint checks — the gating
/// [`dbtl::Finding`] list automation consumes (new violations only under
/// `--diff`).
#[derive(serde::Serialize)]
pub(super) struct CheckJsonItem {
    check: String,
    status: &'static str, // "pass" | "fail"
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<dbtl::Finding>,
}

/// The `check --format json` payload: the scope line, the never-gating context
/// lines (`baseline:`, `affected owners:`), then every check in report order.
/// `failures` lets a consumer read the verdict without re-counting statuses.
#[derive(serde::Serialize)]
pub(super) struct CheckReportJson {
    scope: String,
    info: Vec<String>,
    checks: Vec<CheckJsonItem>,
    failures: usize,
}

/// Run the `check` subcommand: evaluate the CI gates over the resolved source
/// and return `Err` (exit 1 via `main`) when any fails. Coverage below
/// `--coverage-min` (when >0), and — only with `--diff` — newly introduced
/// layer violations, changed buildable nodes without tests, and (with
/// `--no-breaking`) breaking column changes still referenced downstream, all
/// from the single [`dbtl::pr_impact`] / [`dbtl::compute_diff_with`] analyses.
/// `--lint` adds one PASS/FAIL line per static-lint rule (the shared
/// [`dbtl::run_rules`] findings — the SAME vector the TUI lens/worklist and
/// `dbtl docs` quote); under `--diff` the lint gate narrows to findings whose
/// [`dbtl::Finding::key`] is absent from the baseline, so pre-existing
/// violations never fail the gate. Prints one PASS/FAIL line per check to
/// stdout (or the whole report as one JSON object under `--format json`),
/// plus informational lines that never gate (`baseline:` and, when the blast
/// radius has any, `affected owners:`); `--quiet` prints nothing (exit code
/// only).
#[allow(clippy::too_many_arguments)] // the subcommand's flat flag set
pub fn run_check(
    src: &DataSourceArgs,
    coverage_min: u8,
    lint: bool,
    rules: &[String],
    format: ImpactFormat,
    quiet: bool,
    no_breaking: bool,
) -> Result<(), CliError> {
    let scope = src.scope;
    let resolved = src.resolve()?;
    let dag = &resolved.dag;
    // `--rules` implies linting (a rule list with nothing to run is a no-op
    // flag, so honour the intent instead).
    let lint_active = lint || !rules.is_empty();

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
        lines.push(format!("PASS coverage: {msg}"));
        json_checks.push(CheckJsonItem {
            check: "coverage".to_string(),
            status: "pass",
            message: msg,
            findings: Vec::new(),
        });
    } else {
        let msg =
            format!("coverage {pct}% below --coverage-min {coverage_min}% ({tested}/{total})");
        lines.push(format!("FAIL {msg}"));
        failures.push(msg.clone());
        json_checks.push(CheckJsonItem {
            check: "coverage".to_string(),
            status: "fail",
            message: msg,
            findings: Vec::new(),
        });
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
            let msg = "none".to_string();
            lines.push(format!("PASS new layer violations: {msg}"));
            json_checks.push(CheckJsonItem {
                check: "new_layer_violations".to_string(),
                status: "pass",
                message: msg,
                findings: Vec::new(),
            });
        } else {
            let msg = format!(
                "{} new layer violation(s): {}",
                pr.new_layer_violations.len(),
                pr.new_layer_violations
                    .iter()
                    .map(|(p, c)| format!("{p} -> {c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            lines.push(format!("FAIL {msg}"));
            failures.push(msg.clone());
            json_checks.push(CheckJsonItem {
                check: "new_layer_violations".to_string(),
                status: "fail",
                message: msg,
                findings: Vec::new(),
            });
        }
        if pr.untested_changes.is_empty() {
            let msg = "none".to_string();
            lines.push(format!("PASS untested changes: {msg}"));
            json_checks.push(CheckJsonItem {
                check: "untested_changes".to_string(),
                status: "pass",
                message: msg,
                findings: Vec::new(),
            });
        } else {
            let msg = format!(
                "{} changed node(s) without tests: {}",
                pr.untested_changes.len(),
                pr.untested_changes.join(", ")
            );
            lines.push(format!("FAIL {msg}"));
            failures.push(msg.clone());
            json_checks.push(CheckJsonItem {
                check: "untested_changes".to_string(),
                status: "fail",
                message: msg,
                findings: Vec::new(),
            });
        }
        // `--no-breaking`: fail only on BREAKING column changes whose
        // downstream still references the old column (removed/renamed with
        // unmigrated consumers). Partial moves (type/expr) report in the
        // impact pack, never here. Renames print `old -> new` so the log
        // shows the exact migration to ask for.
        if no_breaking {
            let breaking = diff.breaking_referenced();
            if breaking.is_empty() {
                let msg = "none".to_string();
                lines.push(format!("PASS breaking column changes: {msg}"));
                json_checks.push(CheckJsonItem {
                    check: "breaking_columns".to_string(),
                    status: "pass",
                    message: msg,
                    findings: Vec::new(),
                });
            } else {
                let names = breaking
                    .iter()
                    .map(|ch| match &ch.renamed_from {
                        Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                        None => format!("{}.{}", ch.node_name, ch.column),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = format!(
                    "{} breaking column change(s) referenced downstream: {names}",
                    breaking.len()
                );
                lines.push(format!("FAIL {msg}"));
                failures.push(msg.clone());
                json_checks.push(CheckJsonItem {
                    check: "breaking_columns".to_string(),
                    status: "fail",
                    message: msg,
                    findings: Vec::new(),
                });
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

    // Static lint — the same `run_rules` findings the TUI's Lint lens/stats
    // worklist and `dbtl docs` quote. The project-local `.dbtl.yml` supplies
    // enables/thresholds/prefixes; a malformed file is FATAL here (a CI gate
    // must never silently lint the wrong rule set — the TUI's warn+fallback
    // policy is deliberately different). `--rules` overrides the file's
    // enables: it selects exactly which rules run, the file still supplies
    // the thresholds/prefixes.
    if lint_active {
        let cfg = load_lint_config(&resolved, rules)?;
        let findings = dbtl::run_rules(dag, &cfg);
        // Under --diff the gate narrows to NEW violations: a finding whose
        // `key()` already exists in the baseline is pre-existing noise — same
        // "new only" idea as the layer-violation gate.
        let base_keys: std::collections::BTreeSet<(dbtl::LintRule, String, Vec<String>)> =
            match &base_ctx {
                Some((base, _)) => dbtl::run_rules(base, &cfg)
                    .iter()
                    .map(|f| {
                        let (r, n, rel) = f.key();
                        (r, n.to_string(), rel.to_vec())
                    })
                    .collect(),
                None => std::collections::BTreeSet::new(),
            };
        let gating: Vec<&dbtl::Finding> = findings
            .iter()
            .filter(|f| {
                base_ctx.is_none() || {
                    let (r, n, rel) = f.key();
                    !base_keys.contains(&(r, n.to_string(), rel.to_vec()))
                }
            })
            .collect();
        let name = |uid: &str| -> String {
            dag.get(uid)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| uid.to_string())
        };
        for rule in dbtl::LintRule::ALL {
            if !cfg.enabled(rule) {
                continue;
            }
            let rule_findings: Vec<&dbtl::Finding> =
                gating.iter().copied().filter(|f| f.rule == rule).collect();
            let total = findings.iter().filter(|f| f.rule == rule).count();
            let check_name = format!("lint:{}", rule.name());
            if rule_findings.is_empty() {
                // Under --diff a rule whose violations are all pre-existing
                // still passes — the message says so explicitly.
                let msg = if base_ctx.is_some() && total > 0 {
                    format!("no new violations ({total} pre-existing)")
                } else {
                    "none".to_string()
                };
                lines.push(format!("PASS lint {}: {msg}", rule.name()));
                json_checks.push(CheckJsonItem {
                    check: check_name,
                    status: "pass",
                    message: msg,
                    findings: Vec::new(),
                });
            } else {
                let names = rule_findings
                    .iter()
                    .map(|f| name(&f.node_uid))
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = if base_ctx.is_some() {
                    format!(
                        "lint {}: {} new violation(s): {}",
                        rule.name(),
                        rule_findings.len(),
                        names
                    )
                } else {
                    format!(
                        "lint {}: {} violation(s): {}",
                        rule.name(),
                        rule_findings.len(),
                        names
                    )
                };
                lines.push(format!("FAIL {msg}"));
                failures.push(msg);
                json_checks.push(CheckJsonItem {
                    check: check_name,
                    status: "fail",
                    message: format!("{} violation(s)", rule_findings.len()),
                    findings: rule_findings.into_iter().cloned().collect(),
                });
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
