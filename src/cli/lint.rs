//! The `dbtl lint` subcommand and the ONE lint evaluation `check --lint`
//! shares: config resolution, rule execution ([`dbtl::run_rules`]), the
//! baseline file, `--diff`'s new-violation narrowing, and the `fail_on`
//! severity threshold — computed here once, so the two commands can never
//! count or gate differently.

use std::collections::BTreeSet;

use super::*;

/// A gate's verdict: `fail` trips the exit code, `warn` reports without
/// counting (findings exist but `fail_on` doesn't reach them), `pass` is
/// clean, `skip` means the gate was asked for but could not evaluate. Shared
/// so `check`'s per-check lines and `lint`'s verdict line always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl GateStatus {
    /// The line prefix (`check`'s per-check lines, `lint`'s verdict).
    pub(super) fn word(self) -> &'static str {
        match self {
            GateStatus::Pass => "PASS",
            GateStatus::Warn => "WARN",
            GateStatus::Fail => "FAIL",
            GateStatus::Skip => "SKIP",
        }
    }
    /// The report status string (`gates[].status` in `dbtl.report/v1`).
    pub(super) fn json(self) -> &'static str {
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

/// Does a `severity` finding fail under `fail_on`? `warn` gates on every
/// counted finding (warnings included); `error` on errors only; `never` on
/// nothing — findings still report, the exit just stays 0.
pub(super) fn severity_fails(severity: dbtl::Severity, fail_on: dbtl::FailOn) -> bool {
    match fail_on {
        dbtl::FailOn::Never => false,
        dbtl::FailOn::Warn => true,
        dbtl::FailOn::Error => severity >= dbtl::Severity::Error,
    }
}

/// The verdict for a gate that DID find violations of `severity`: `fail`
/// when [`severity_fails`], else `warn` — reported, never counted.
pub(super) fn gate_status(severity: dbtl::Severity, fail_on: dbtl::FailOn) -> GateStatus {
    if severity_fails(severity, fail_on) {
        GateStatus::Fail
    } else {
        GateStatus::Warn
    }
}

/// `fail_on` resolution — the CLI flag wins over `.dbtl.yml`, which wins
/// over the shipped default (`error`).
pub(super) fn resolve_fail_on(
    cli: Option<dbtl::FailOn>,
    file: Option<dbtl::FailOn>,
) -> dbtl::FailOn {
    cli.or(file).unwrap_or(dbtl::FailOn::Error)
}

/// The verdict for one enabled rule inside a [`LintGateOutcome`]: its effective
/// severity, which findings count, and how they were classified.
pub(super) struct RuleOutcome {
    /// The rule.
    pub rule: dbtl::LintRule,
    /// Its effective severity — uniform across its findings.
    pub severity: dbtl::Severity,
    /// Indices into [`LintGateOutcome::findings`] that count toward the gate:
    /// not baselined, and new under `--diff`.
    pub counted: Vec<usize>,
    /// Non-baselined findings suppressed by `--diff`'s baseline dag.
    pub pre_existing: usize,
    /// Findings frozen by the baseline file.
    pub baselined: usize,
    /// pass / warn / fail from `counted` + `fail_on`.
    pub status: GateStatus,
}

/// The one lint evaluation `check --lint` and `dbtl lint` share: every
/// finding `run_rules` produced, classified into counted / baselined /
/// pre-existing, with the `fail_on` verdict precomputed — so both commands
/// see the same "counted findings" by construction.
pub(super) struct LintGateOutcome {
    /// Every finding `run_rules` produced (exclude/meta suppression already
    /// applied), `baselined` marked — in the deterministic run order.
    pub findings: Vec<dbtl::Finding>,
    /// Enabled rules that didn't run for a missing input (SQL resolver /
    /// catalog), in `LintRule::ALL` order — `check` prints `SKIP` rows and
    /// `lint` a `skipped:` section; they never count toward the gate.
    pub skipped: Vec<dbtl::SkippedRule>,
    /// SQL models `run_rules` couldn't parse (the `unparsed` tally both
    /// reports print — a model that never reached a SQL rule, not a
    /// finding).
    pub unparsed: std::collections::BTreeSet<String>,
    /// Per enabled AND RUNNABLE rule, in `LintRule::ALL` order.
    pub rules: Vec<RuleOutcome>,
    /// Counted findings that meet `fail_on` (the gate's numerator — `0`
    /// when nothing fails).
    pub gating: usize,
    /// `--diff` was in play (drives the "new violations" wording).
    pub has_diff: bool,
    /// The resolved fail threshold.
    pub fail_on: dbtl::FailOn,
    /// A counted finding at or above `fail_on` exists.
    pub failed: bool,
}

/// The auxiliary inputs a lint run carries — the shared lazy SQL resolver
/// and the warehouse catalog, plus the BASELINE side's resolver under
/// `--diff` (the baseline's own warehouse record is never loaded, matching
/// the diff's resolver policy — catalog rules can't compare and simply
/// don't feed `pre_existing`).
pub(super) struct LintAux<'a> {
    /// The current side's SQL resolver — `Some` feeds `LintInput::sql`.
    pub sql: Option<&'a dbtl::SqlParseResolver>,
    /// The current side's catalog — `None` skips the `Requires::Catalog`
    /// rules (recorded, never a failure).
    pub catalog: Option<&'a dbtl::Catalog>,
    /// The baseline side's SQL resolver under `--diff` (`None` skips SQL
    /// rules there too — their current-side findings then all count as
    /// new, the safe direction for a gate).
    pub base_sql: Option<&'a dbtl::SqlParseResolver>,
}

/// The shared lint evaluation: [`dbtl::run_rules`] over `dag` with the
/// `aux` inputs, `baseline` applied to the findings, and — with `base` —
/// the same run over the baseline dag narrowing gating to NEW findings (a
/// finding whose [`dbtl::Finding::key`] exists on the baseline side is
/// pre-existing noise, the same "new only" idea as the layer-violation
/// gate). With `selection`, only findings whose `node_uid` the selected
/// set contains survive — narrowed BEFORE the baselined/pre-existing
/// bookkeeping, so the report only ever sees selected findings.
pub(super) fn evaluate_lint(
    dag: &dbtl::Dag,
    cfg: &dbtl::LintConfig,
    base: Option<&dbtl::Dag>,
    baseline: Option<&dbtl::Baseline>,
    fail_on: dbtl::FailOn,
    aux: LintAux<'_>,
    selection: Option<&BTreeSet<String>>,
) -> LintGateOutcome {
    let outcome = dbtl::run_rules(
        dbtl::LintInput::new(dag, cfg)
            .with_sql(aux.sql)
            .with_catalog(aux.catalog),
    );
    let mut findings = outcome.findings;
    let skipped = outcome.skipped;
    let mut unparsed = outcome.unparsed;
    if let Some(sel) = selection {
        findings.retain(|f| sel.contains(&f.node_uid));
        // The tally counts models the report's selection could have shown.
        unparsed.retain(|u| sel.contains(u));
    }
    if let Some(b) = baseline {
        b.apply(&mut findings);
    }
    let skipped_rules: BTreeSet<dbtl::LintRule> = skipped.iter().map(|s| s.rule).collect();
    let base_keys: BTreeSet<(dbtl::LintRule, String, Vec<String>)> = match base {
        Some(base) => dbtl::run_rules(dbtl::LintInput::new(base, cfg).with_sql(aux.base_sql))
            .findings
            .iter()
            .map(|f| {
                let (r, n, rel) = f.key();
                (r, n.to_string(), rel.to_vec())
            })
            .collect(),
        None => BTreeSet::new(),
    };
    let pre_existing = |f: &dbtl::Finding| {
        !base_keys.is_empty() && {
            let (r, n, rel) = f.key();
            base_keys.contains(&(r, n.to_string(), rel.to_vec()))
        }
    };
    let mut gating = 0;
    let mut rules = Vec::new();
    for rule in dbtl::LintRule::ALL {
        let Some(severity) = cfg.severity(rule) else {
            continue;
        };
        if skipped_rules.contains(&rule) {
            continue; // a SKIP row, not a rule verdict
        }
        let mut counted = Vec::new();
        let mut rule_pre_existing = 0;
        let mut rule_baselined = 0;
        for (i, f) in findings.iter().enumerate() {
            if f.rule != rule {
                continue;
            }
            if f.baselined {
                rule_baselined += 1;
            } else if pre_existing(f) {
                rule_pre_existing += 1;
            } else {
                counted.push(i);
                if severity_fails(f.severity, fail_on) {
                    gating += 1;
                }
            }
        }
        let status = if counted.is_empty() {
            GateStatus::Pass
        } else {
            gate_status(severity, fail_on)
        };
        rules.push(RuleOutcome {
            rule,
            severity,
            counted,
            pre_existing: rule_pre_existing,
            baselined: rule_baselined,
            status,
        });
    }
    LintGateOutcome {
        gating,
        has_diff: base.is_some(),
        fail_on,
        failed: gating > 0,
        findings,
        skipped,
        unparsed,
        rules,
    }
}

/// The lint baseline path for a data source: the `lint.baseline` /
/// default-name path `.dbtl.yml` resolved at load when a config file was
/// found, else the conventional `.dbtl-baseline.json` beside the first
/// `.dbtl.yml` candidate — the project root in either mode (source: the
/// project dir; manifest: `target/`'s parent).
pub(super) fn lint_baseline_path(
    cfg_baseline: &std::path::Path,
    config_found: bool,
    candidates: &[PathBuf],
) -> PathBuf {
    if config_found {
        return cfg_baseline.to_path_buf();
    }
    candidates
        .first()
        .and_then(|p| p.parent())
        .map(|d| d.join(dbtl::DEFAULT_BASELINE_NAME))
        .unwrap_or_else(|| PathBuf::from(dbtl::DEFAULT_BASELINE_NAME))
}

/// Read the lint baseline file: absent → `None` (an explicitly configured
/// path warns and proceeds without; the conventional default stays silent);
/// unreadable or malformed → an input error — a frozen set we can't read
/// must never silently un-freeze every violation.
pub(super) fn load_lint_baseline_file(
    path: &std::path::Path,
    explicit: bool,
) -> Result<Option<dbtl::Baseline>, CliError> {
    match std::fs::read_to_string(path) {
        Ok(text) => dbtl::Baseline::from_json(&text)
            .map(Some)
            .map_err(|e| CliError::input(anyhow::anyhow!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if explicit {
                eprintln!(
                    "warning: lint baseline {} not found; ignoring",
                    path.display()
                );
            }
            Ok(None)
        }
        Err(e) => Err(CliError::input(
            anyhow::Error::new(e).context(format!("failed to read {}", path.display())),
        )),
    }
}

/// The directory a finding's `location.file` is relative to — the project
/// root the `.dbtl.yml` candidates already encode (source mode: the project
/// dir; a `<root>/target/manifest.json`: the root; a manifest elsewhere:
/// its own dir — files that don't resolve there just keep `line` unset).
pub(super) fn lint_project_root(candidates: &[PathBuf]) -> PathBuf {
    candidates
        .first()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The `lint` subcommand's options, bundled so the runner's signature stays
/// readable (the fields map 1:1 onto the clap variant's args).
pub struct LintArgs<'a> {
    /// `--rules` — the authoritative rule selector (empty = the config's set).
    pub rules: &'a [String],
    /// `--fail-on` — overrides `.dbtl.yml`'s `fail_on`.
    pub fail_on: Option<dbtl::FailOn>,
    /// `--format` — the stdout report format.
    pub format: ReportFormat,
    /// `--color` — when the stdout `text`/`concise` report may be styled.
    pub color: ColorArg,
    /// `--write-baseline` — `Some(None)` = the flag with no PATH argument.
    pub write_baseline: Option<&'a Option<String>>,
    /// `--no-baseline` — never read the baseline file.
    pub no_baseline: bool,
    /// `--list-rules`.
    pub list_rules: bool,
    /// `--explain <ID>`.
    pub explain: Option<&'a str>,
    /// `--quiet` — suppress stdout; the exit code carries the verdict.
    pub quiet: bool,
    /// `--report <FMT>:<PATH>` destinations (repeatable).
    pub reports: &'a [String],
    /// `--step-summary` — append the markdown report to $GITHUB_STEP_SUMMARY.
    pub step_summary: bool,
    /// `--path-prefix` — prepended to file paths in path-carrying formats.
    pub path_prefix: Option<&'a str>,
    /// `--select` — the dbt-selector specs findings intersect.
    pub select: &'a [String],
}

/// The lint config the query modes (`--list-rules`, `--explain`) print:
/// `.dbtl.yml` is probed from the data-path DECISION alone ([`source_path`])
/// — never a data load. A source that can't be determined yields the
/// shipped defaults instead of a data-source error.
fn query_lint_config(src: &DataSourceArgs, rules: &[String]) -> Result<dbtl::LintConfig, CliError> {
    let (manifest_arg, project_arg) =
        match source_path(src.manifest, src.source, src.project, src.diff) {
            Some(SourcePath::Manifest(path)) => (Some(path), None),
            Some(SourcePath::Project(dir)) => (None, Some(dir)),
            None => (None, None),
        };
    let mut cfg = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?
        .config
        .lint;
    apply_rules_overlay(&mut cfg, rules)?;
    Ok(cfg)
}

/// Run the `lint` subcommand: `--list-rules`/`--explain` answer from the
/// registry + config alone (no data load); otherwise resolve, evaluate
/// once through [`evaluate_lint`], print the report, and gate the exit on
/// the resolved `fail_on`.
pub fn run_lint(src: &DataSourceArgs, args: &LintArgs) -> Result<(), CliError> {
    // Query modes never touch the data source — `--explain`/`--list-rules`
    // must work in an empty directory. Flags that only shape a loaded graph
    // (`--diff`, `--include-packages`) have no effect here — reject them
    // rather than silently drop them.
    if args.list_rules || args.explain.is_some() {
        let mode = if args.list_rules {
            "--list-rules"
        } else {
            "--explain"
        };
        for (flag, given) in [
            ("--diff", src.diff.is_some()),
            ("--include-packages", src.scope.includes_packages()),
        ] {
            if given {
                return Err(CliError::usage(anyhow::anyhow!(
                    "the argument '{flag}' cannot be used with '{mode}'"
                )));
            }
        }
        let cfg = query_lint_config(src, args.rules)?;
        if args.list_rules {
            print_rules_table(&cfg)?;
        }
        if let Some(id) = args.explain {
            explain_rule(id, &cfg)?;
        }
        return Ok(());
    }
    // An unknown --rules name is a usage error — checked before the data
    // load so it can't hide behind a missing-input error. Same for a
    // malformed --select or a `state:` atom with no `--diff` baseline.
    validate_rule_names(args.rules)?;
    validate_selector_specs(args.select, "--select")?;
    require_diff_for_state(args.select, src.diff.is_some())?;
    // Same for a malformed --report spec or a --path-prefix with no
    // path-carrying format selected — all usage errors land before the load.
    let plan = OutputPlan::build(
        args.format,
        args.quiet,
        args.color,
        args.reports,
        args.step_summary,
        args.path_prefix,
        &ReportFormat::ALL,
    )?;
    // Freezing findings is a snapshot, not a comparison — a baseline written
    // under --diff would silently encode "new only" semantics it lacks.
    if args.write_baseline.is_some() && src.diff.is_some() {
        return Err(CliError::usage(anyhow::anyhow!(
            "the argument '--write-baseline' cannot be used with '--diff'"
        )));
    }
    let resolved = src.resolve()?;
    let (manifest_arg, project_arg) = resolved_config_args(&resolved);
    let pc = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?;
    let fail_on = resolve_fail_on(args.fail_on, pc.config.fail_on);
    let baseline_explicit = pc.config.baseline_explicit;
    let config_found = pc.path.is_some();
    let mut cfg = pc.config.lint;
    apply_rules_overlay(&mut cfg, args.rules)?;
    let candidates =
        dbtl::project_config_candidates(manifest_arg.as_deref(), project_arg.as_deref());
    // The auxiliary inputs every evaluation path below shares — the lazy
    // SQL resolver and (when one resolved) the catalog, so `Requires::Sql`
    // and `Requires::Catalog` rules evaluate identically here and in `check`.
    let (catalog, sql_resolver) = lint_aux(&resolved, src.sql_dialect, src.catalog)?;

    // `--write-baseline`: freeze every current finding (post-suppression,
    // the existing baseline NOT applied — the file written IS the baseline
    // now) and always exit 0.
    if let Some(path_arg) = args.write_baseline {
        let findings = dbtl::run_rules(
            dbtl::LintInput::new(&resolved.dag, &cfg)
                .with_sql(Some(&sql_resolver))
                .with_catalog(catalog.as_deref()),
        )
        .findings;
        let baseline = dbtl::Baseline::from_findings(&findings);
        let path = match path_arg {
            Some(p) => PathBuf::from(p),
            None => lint_baseline_path(&cfg.baseline, config_found, &candidates),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))
                .input()?;
        }
        std::fs::write(&path, baseline.to_json())
            .with_context(|| format!("failed to write lint baseline {}", path.display()))
            .input()?;
        if !args.quiet {
            write_stdout(
                &format!(
                    "wrote {} finding(s) to {}\n",
                    baseline.len(),
                    path.display()
                ),
                anstream::ColorChoice::Auto,
            )?;
        }
        return Ok(());
    }

    let baseline = if args.no_baseline {
        None
    } else {
        load_lint_baseline_file(
            &lint_baseline_path(&cfg.baseline, config_found, &candidates),
            baseline_explicit,
        )?
    };
    let base = match src.diff {
        Some(d) => Some(load_baseline(d, src.scope, &src.project_dir())?),
        None => None,
    };
    let base_resolver = base
        .as_ref()
        .map(|(b, _)| src.baseline_resolver(&resolved.dag, b));
    // `--select` narrows the report to findings on matching nodes —
    // `state:` atoms compare against the SAME resolver-pair diff `check`
    // computes (loaded only when a spec actually uses one). An empty match
    // warns and reports zero findings.
    let diff = match &base {
        Some((b, _)) if specs_use_state(args.select) => Some(selector_diff(src, &resolved, b)?),
        _ => None,
    };
    let selection = if args.select.is_empty() {
        None
    } else {
        let sel = selector_set(&resolved.dag, diff.as_ref(), args.select);
        if sel.is_empty() {
            eprintln!(
                "warning: --select '{}' matched no nodes",
                args.select.join(" ")
            );
        }
        Some(sel)
    };
    let mut outcome = evaluate_lint(
        &resolved.dag,
        &cfg,
        base.as_ref().map(|(b, _)| b),
        baseline.as_ref(),
        fail_on,
        LintAux {
            sql: Some(&sql_resolver),
            catalog: catalog.as_deref(),
            base_sql: base_resolver.as_ref(),
        },
        selection.as_ref(),
    );
    // Best-effort yml line numbers — the file-reading pass `run_rules`
    // stays free of. Unreadable files simply keep `line` unset. Filled
    // before the Report build so every format carries them.
    let project_root = lint_project_root(&candidates);
    dbtl::fill_finding_lines(&mut outcome.findings, &resolved.dag, &project_root);
    // One gate per rule — the same `lint:*` gates `check --lint` reports,
    // rendered by the same formats.
    let mut report = Report::new(
        resolved.dag.project_name().unwrap_or("dbt"),
        src.scope.describe(),
    );
    report.gates = lint_gates(&resolved.dag, &outcome);
    report.summary = Report::summarize(&report.gates);
    report.lint = Some(LintTally {
        pre_existing: outcome.rules.iter().map(|r| r.pre_existing).sum(),
        has_diff: outcome.has_diff,
        skipped: outcome.skipped.clone(),
        unparsed_models: outcome.unparsed.len(),
    });
    plan.emit(
        &report,
        "lint",
        &RenderEnv {
            dag: &resolved.dag,
            project_root: &project_root,
        },
    )?;
    if outcome.failed {
        Err(CliError::gate(anyhow::anyhow!(
            "lint: {} finding(s) at or above fail-on '{}'",
            outcome.gating,
            outcome.fail_on.name()
        )))
    } else {
        Ok(())
    }
}

/// The `--list-rules` table — every registry rule, one line each: id,
/// category, effective severity (`off` when the config disables it), and
/// the one-line summary. Columns align to the widest entry. Written
/// through the same broken-pipe-safe stdout helper the report uses.
fn print_rules_table(cfg: &dbtl::LintConfig) -> Result<(), CliError> {
    let id_w = dbtl::LintRule::ALL
        .iter()
        .map(|r| r.name().len())
        .max()
        .unwrap_or(0)
        .max("ID".len());
    let cat_w = dbtl::LintRule::ALL
        .iter()
        .map(|r| r.info().category.name().len())
        .max()
        .unwrap_or(0)
        .max("CATEGORY".len());
    let mut out = format!(
        "{:<id_w$}  {:<cat_w$}  {:<8}  SUMMARY\n",
        "ID",
        "CATEGORY",
        "SEVERITY",
        id_w = id_w,
        cat_w = cat_w
    );
    for rule in dbtl::LintRule::ALL {
        let severity = cfg.severity(rule).map(|s| s.name()).unwrap_or("off");
        out += &format!(
            "{:<id_w$}  {:<cat_w$}  {:<8}  {}\n",
            rule.name(),
            rule.info().category.name(),
            severity,
            rule.info().summary,
            id_w = id_w,
            cat_w = cat_w
        );
    }
    write_stdout(&out, anstream::ColorChoice::Auto)
}

/// `--explain <ID>` — the full registry entry plus the effective severity
/// and parameter values under the resolved config, the fix guidance, and
/// the rule's reference link. An unknown id is a usage error listing the
/// valid set.
fn explain_rule(id: &str, cfg: &dbtl::LintConfig) -> Result<(), CliError> {
    let Some(rule) = dbtl::LintRule::from_name(id) else {
        return Err(CliError::usage(anyhow::anyhow!(
            "unknown lint rule '{id}' (valid rules: {})",
            dbtl::LintRule::ALL
                .iter()
                .map(|r| r.name())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    let info = rule.info();
    let mut out = format!("id: {}\n", info.id);
    out += &format!("category: {}\n", info.category.name());
    out += &format!(
        "severity: {}\n",
        cfg.severity(rule).map(|s| s.name()).unwrap_or("off")
    );
    out += &format!("requires: {}\n\n", requires_name(info.requires));
    out += &format!("{}\n\n", info.summary);
    out += &format!("Rationale:\n{}\n\n", info.rationale);
    out += &format!("Example:\n{}\n\n", info.example);
    out += &format!("Help:\n{}\n\n", info.help);
    out += "Params:\n";
    if info.params.is_empty() {
        out += "  (none)\n";
    } else {
        for p in info.params {
            match p.kind {
                dbtl::ParamKind::Int { min, default } => {
                    let current = cfg.int_param(rule, p.name).unwrap_or(default);
                    out += &format!(
                        "  {}  integer  min {}, default {}, current {} — {}\n",
                        p.name, min, default, current, p.description
                    );
                }
                dbtl::ParamKind::StringList { default, allowed } => {
                    let current = cfg
                        .str_list_param(rule, p.name)
                        .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect());
                    let allowed = allowed
                        .map_or_else(String::new, |a| format!(", allowed [{}]", a.join(", ")));
                    out += &format!(
                        "  {}  string-list  default [{}], current [{}]{} — {}\n",
                        p.name,
                        default.join(", "),
                        current.join(", "),
                        allowed,
                        p.description
                    );
                }
                dbtl::ParamKind::RegexList { default } => {
                    let current = cfg
                        .str_list_param(rule, p.name)
                        .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect());
                    out += &format!(
                        "  {}  regex-list  default [{}], current [{}] — {}\n",
                        p.name,
                        default.join(", "),
                        current.join(", "),
                        p.description
                    );
                }
                dbtl::ParamKind::Bool { default } => {
                    let current = cfg.bool_param(rule, p.name).unwrap_or(default);
                    out += &format!(
                        "  {}  boolean  default {}, current {} — {}\n",
                        p.name, default, current, p.description
                    );
                }
            }
        }
    }
    out += &format!("\nDocs:\n{}\n", rule.docs_url());
    write_stdout(&out, anstream::ColorChoice::Auto)
}

/// The display name of a rule's data requirement.
fn requires_name(r: dbtl::Requires) -> &'static str {
    match r {
        dbtl::Requires::Graph => "graph",
        dbtl::Requires::Sql => "graph + sql",
        dbtl::Requires::Catalog => "graph + catalog",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A model node literal — `path` carries the layer dir like a real
    /// manifest (`staging/x.sql`).
    fn model(name: &str, path: &str) -> (String, dbtl::RawNode) {
        (
            format!("model.p.{name}"),
            dbtl::RawNode {
                name: name.into(),
                resource_type: "model".into(),
                path: Some(path.into()),
                ..Default::default()
            },
        )
    }

    /// A manifest assembled from the given models + (parent, child) edges;
    /// `child_map` derives from `edges` so each test writes its graph once.
    fn manifest(nodes: Vec<(String, dbtl::RawNode)>, edges: &[(&str, &str)]) -> dbtl::RawManifest {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
        for (p, c) in edges {
            parent_map
                .entry((*c).to_string())
                .or_default()
                .push((*p).to_string());
            child_map
                .entry((*p).to_string())
                .or_default()
                .push((*c).to_string());
        }
        dbtl::RawManifest {
            metadata: Default::default(),
            nodes: nodes.into_iter().collect(),
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map,
            child_map,
            docs: Default::default(),
        }
    }

    /// A config with ONLY `rules` enabled — everything else off — so a test
    /// sees exactly the findings it means to.
    fn cfg_only(rules: &[dbtl::LintRule]) -> dbtl::LintConfig {
        let mut cfg = dbtl::LintConfig::default();
        for r in dbtl::LintRule::ALL {
            if !rules.contains(&r) {
                cfg.severities.insert(r, None);
            }
        }
        cfg
    }

    /// Two parentless models — two `root_models` findings, nothing else
    /// when scoped to that rule.
    fn two_root_models() -> dbtl::Dag {
        dbtl::Dag::build(&manifest(
            vec![model("a", "marts/a.sql"), model("b", "marts/b.sql")],
            &[],
        ))
    }

    /// No auxiliary inputs — graph-only evaluation (the SQL/catalog rules
    /// would all skip if enabled; the tests scope to graph rules anyway).
    fn no_aux() -> LintAux<'static> {
        LintAux {
            sql: None,
            catalog: None,
            base_sql: None,
        }
    }

    #[test]
    fn fail_on_resolves_cli_over_file_over_default() {
        assert_eq!(resolve_fail_on(None, None), dbtl::FailOn::Error);
        assert_eq!(
            resolve_fail_on(Some(dbtl::FailOn::Warn), None),
            dbtl::FailOn::Warn
        );
        assert_eq!(
            resolve_fail_on(None, Some(dbtl::FailOn::Warn)),
            dbtl::FailOn::Warn
        );
        assert_eq!(
            resolve_fail_on(Some(dbtl::FailOn::Error), Some(dbtl::FailOn::Warn)),
            dbtl::FailOn::Error,
            "the CLI flag beats the file"
        );
    }

    #[test]
    fn the_gate_status_table() {
        use dbtl::{FailOn, Severity};
        // (finding severity, fail_on) → fails?
        for (sev, fail_on, want) in [
            (Severity::Error, FailOn::Error, true),
            (Severity::Error, FailOn::Warn, true),
            (Severity::Error, FailOn::Never, false),
            (Severity::Warn, FailOn::Error, false),
            (Severity::Warn, FailOn::Warn, true),
            (Severity::Warn, FailOn::Never, false),
        ] {
            assert_eq!(
                severity_fails(sev, fail_on),
                want,
                "{sev:?} under {fail_on:?}"
            );
            let want_status = if want {
                GateStatus::Fail
            } else {
                GateStatus::Warn
            };
            assert_eq!(
                gate_status(sev, fail_on),
                want_status,
                "{sev:?} under {fail_on:?}"
            );
        }
    }

    #[test]
    fn evaluate_partitions_findings_into_counted_baselined_pre_existing() {
        let dag = two_root_models();
        let cfg = cfg_only(&[dbtl::LintRule::RootModels]);
        // Plain run: both findings count, gate fails under the default.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Error, no_aux(), None);
        assert_eq!(out.rules[0].counted.len(), 2);
        assert_eq!(out.rules[0].baselined, 0);
        assert!(out.failed && out.rules[0].status == GateStatus::Fail);
        // A baseline freezing `a` leaves only `b` counted.
        let frozen = dbtl::Baseline::from_findings(
            &out.findings
                .iter()
                .filter(|f| f.node_uid == "model.p.a")
                .cloned()
                .collect::<Vec<_>>(),
        );
        let out = evaluate_lint(
            &dag,
            &cfg,
            None,
            Some(&frozen),
            dbtl::FailOn::Error,
            no_aux(),
            None,
        );
        assert_eq!(out.rules[0].counted.len(), 1);
        assert_eq!(out.rules[0].baselined, 1);
        assert_eq!(out.findings[out.rules[0].counted[0]].node_uid, "model.p.b");
        // Self-diff: every finding is pre-existing — nothing counts.
        let out = evaluate_lint(
            &dag,
            &cfg,
            Some(&dag),
            None,
            dbtl::FailOn::Error,
            no_aux(),
            None,
        );
        assert_eq!(out.rules[0].counted.len(), 0);
        assert_eq!(out.rules[0].pre_existing, 2);
        assert!(!out.failed && out.rules[0].status == GateStatus::Pass);
        // Diff + baseline: the frozen one is baselined, the rest pre-existing.
        let out = evaluate_lint(
            &dag,
            &cfg,
            Some(&dag),
            Some(&frozen),
            dbtl::FailOn::Error,
            no_aux(),
            None,
        );
        assert_eq!(out.rules[0].counted.len(), 0);
        assert_eq!(out.rules[0].pre_existing, 1);
        assert_eq!(out.rules[0].baselined, 1);
    }

    #[test]
    fn evaluate_honours_fail_on_for_warn_findings() {
        let dag = two_root_models();
        let mut cfg = cfg_only(&[dbtl::LintRule::RootModels]);
        cfg.severities
            .insert(dbtl::LintRule::RootModels, Some(dbtl::Severity::Warn));
        // Warn findings under the default `error` threshold report but pass.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Error, no_aux(), None);
        assert_eq!(out.rules[0].counted.len(), 2);
        assert!(!out.failed);
        assert_eq!(out.rules[0].status, GateStatus::Warn);
        // `--fail-on warn` fails on them.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Warn, no_aux(), None);
        assert!(out.failed && out.gating == 2);
        // `never` never fails.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Never, no_aux(), None);
        assert!(!out.failed && out.rules[0].status == GateStatus::Warn);
    }
}
