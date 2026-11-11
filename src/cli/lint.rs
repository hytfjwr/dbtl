//! The `dbtl lint` subcommand and the ONE lint evaluation `check --lint`
//! shares: config resolution, rule execution ([`dbtl::run_rules`]), the
//! baseline file, `--diff`'s new-violation narrowing, and the `fail_on`
//! severity threshold — computed here once, so the two commands can never
//! count or gate differently.

use std::collections::BTreeSet;

use super::*;

/// A gate's verdict: `fail` trips the exit code, `warn` reports without
/// counting (findings exist but `fail_on` doesn't reach them), `pass` is
/// clean. Shared so `check`'s per-check lines and `lint`'s verdict line
/// always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateStatus {
    Pass,
    Warn,
    Fail,
}

impl GateStatus {
    /// The line prefix (`check`'s per-check lines, `lint`'s verdict).
    pub(super) fn word(self) -> &'static str {
        match self {
            GateStatus::Pass => "PASS",
            GateStatus::Warn => "WARN",
            GateStatus::Fail => "FAIL",
        }
    }
    /// The `--format json` status string.
    pub(super) fn json(self) -> &'static str {
        match self {
            GateStatus::Pass => "pass",
            GateStatus::Warn => "warn",
            GateStatus::Fail => "fail",
        }
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

/// The verdict for one enabled rule inside a [`LintOutcome`]: its effective
/// severity, which findings count, and how they were classified.
pub(super) struct RuleOutcome {
    /// The rule.
    pub rule: dbtl::LintRule,
    /// Its effective severity — uniform across its findings.
    pub severity: dbtl::Severity,
    /// Indices into [`LintOutcome::findings`] that count toward the gate:
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
pub(super) struct LintOutcome {
    /// Every finding `run_rules` produced (exclude/meta suppression already
    /// applied), `baselined` marked — in the deterministic run order.
    pub findings: Vec<dbtl::Finding>,
    /// Per enabled rule, in `LintRule::ALL` order.
    pub rules: Vec<RuleOutcome>,
    /// Counted findings by severity.
    pub errors: usize,
    /// Counted warn findings.
    pub warnings: usize,
    /// Findings frozen by the baseline file.
    pub baselined: usize,
    /// Non-baselined findings suppressed by `--diff`'s baseline dag.
    pub pre_existing: usize,
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

impl LintOutcome {
    /// Total counted findings — the gate input.
    pub(super) fn counted(&self) -> usize {
        self.errors + self.warnings
    }
    /// The report verdict: `fail` when findings meet `fail_on`, `warn` when
    /// findings exist below it.
    pub(super) fn status(&self) -> GateStatus {
        if self.failed {
            GateStatus::Fail
        } else if self.counted() > 0 {
            GateStatus::Warn
        } else {
            GateStatus::Pass
        }
    }
}

/// The shared lint evaluation: [`dbtl::run_rules`] over `dag`, `baseline`
/// applied to the findings, and — with `base` — the same run over the
/// baseline dag narrowing gating to NEW findings (a finding whose
/// [`dbtl::Finding::key`] exists on the baseline side is pre-existing
/// noise, the same "new only" idea as the layer-violation gate).
pub(super) fn evaluate_lint(
    dag: &dbtl::Dag,
    cfg: &dbtl::LintConfig,
    base: Option<&dbtl::Dag>,
    baseline: Option<&dbtl::Baseline>,
    fail_on: dbtl::FailOn,
) -> LintOutcome {
    let mut findings = dbtl::run_rules(dbtl::LintInput::new(dag, cfg));
    if let Some(b) = baseline {
        b.apply(&mut findings);
    }
    let base_keys: BTreeSet<(dbtl::LintRule, String, Vec<String>)> = match base {
        Some(base) => dbtl::run_rules(dbtl::LintInput::new(base, cfg))
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
    let mut errors = 0;
    let mut warnings = 0;
    let mut gating = 0;
    let mut total_pre_existing = 0;
    let mut rules = Vec::new();
    for rule in dbtl::LintRule::ALL {
        let Some(severity) = cfg.severity(rule) else {
            continue;
        };
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
                match f.severity {
                    dbtl::Severity::Error => errors += 1,
                    dbtl::Severity::Warn => warnings += 1,
                }
                if severity_fails(f.severity, fail_on) {
                    gating += 1;
                }
            }
        }
        total_pre_existing += rule_pre_existing;
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
    LintOutcome {
        baselined: findings.iter().filter(|f| f.baselined).count(),
        pre_existing: total_pre_existing,
        gating,
        has_diff: base.is_some(),
        fail_on,
        failed: gating > 0,
        findings,
        rules,
        errors,
        warnings,
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

/// The `lint` subcommand's options, bundled so the runner's signature stays
/// readable (the fields map 1:1 onto the clap variant's args).
pub struct LintArgs<'a> {
    /// `--rules` — the authoritative rule selector (empty = the config's set).
    pub rules: &'a [String],
    /// `--fail-on` — overrides `.dbtl.yml`'s `fail_on`.
    pub fail_on: Option<dbtl::FailOn>,
    /// `--format`.
    pub format: LintFormat,
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
}

/// The lint config the query modes (`--list-rules`, `--explain`) print:
/// `.dbtl.yml` is probed from the data-path DECISION alone ([`source_path`])
/// — never a data load. A source that can't be determined yields the
/// shipped defaults instead of a data-source error.
fn query_lint_config(src: &DataSourceArgs, rules: &[String]) -> Result<dbtl::LintConfig, CliError> {
    let (manifest_arg, project_arg) = match source_path(src.manifest, src.source, src.project) {
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
    // must work in an empty directory.
    if args.list_rules || args.explain.is_some() {
        let cfg = query_lint_config(src, args.rules)?;
        if args.list_rules {
            print_rules_table(&cfg);
        }
        if let Some(id) = args.explain {
            explain_rule(id, &cfg)?;
        }
        return Ok(());
    }
    // An unknown --rules name is a usage error — checked before the data
    // load so it can't hide behind a missing-input error.
    validate_rule_names(args.rules)?;
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

    // `--write-baseline`: freeze every current finding (post-suppression,
    // the existing baseline NOT applied — the file written IS the baseline
    // now) and always exit 0.
    if let Some(path_arg) = args.write_baseline {
        let findings = dbtl::run_rules(dbtl::LintInput::new(&resolved.dag, &cfg));
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
            println!("wrote {} finding(s) to {}", baseline.len(), path.display());
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
        Some(d) => Some(load_baseline(d, src.scope)?),
        None => None,
    };
    let outcome = evaluate_lint(
        &resolved.dag,
        &cfg,
        base.as_ref().map(|(b, _)| b),
        baseline.as_ref(),
        fail_on,
    );
    if !args.quiet {
        match args.format {
            LintFormat::Text => print_lint_report(&resolved.dag, &outcome),
            LintFormat::Json => println!(
                "{}",
                serde_json::to_string_pretty(&LintReportJson::from(&outcome))
                    .map_err(|e| CliError::new(e.into()))?
            ),
        }
    }
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

/// The plain-text `dbtl lint` report: one line per counted finding —
/// severity, location (`file:line`, defaulting to line 1; the node name
/// when no file is known), rule id, message — a tally line, then the
/// verdict. Baselined findings collapse to a count; under `--diff` the
/// pre-existing count records what the baseline already carried.
fn print_lint_report(dag: &dbtl::Dag, outcome: &LintOutcome) {
    for ro in &outcome.rules {
        for &i in &ro.counted {
            let f = &outcome.findings[i];
            let loc = f
                .location
                .as_ref()
                .map(|l| format!("{}:{}", l.file, l.line.unwrap_or(1)))
                .unwrap_or_else(|| {
                    dag.get(&f.node_uid)
                        .map(|n| n.name.clone())
                        .unwrap_or_else(|| f.node_uid.clone())
                });
            println!(
                "{} {} {} {}",
                f.severity.name(),
                loc,
                f.rule.name(),
                f.message
            );
        }
    }
    if outcome.counted() == 0 {
        let mut parts = Vec::new();
        if outcome.pre_existing > 0 {
            parts.push(format!("{} pre-existing", outcome.pre_existing));
        }
        if outcome.baselined > 0 {
            parts.push(format!("{} baselined", outcome.baselined));
        }
        let msg = if outcome.has_diff {
            "lint: no new violations"
        } else {
            "lint: no violations"
        };
        if parts.is_empty() {
            println!("{msg}");
        } else {
            println!("{msg} ({})", parts.join(", "));
        }
    } else {
        let mut summary = format!(
            "lint: {} error(s), {} warning(s), {} baselined",
            outcome.errors, outcome.warnings, outcome.baselined
        );
        if outcome.has_diff {
            summary += &format!(", {} pre-existing", outcome.pre_existing);
        }
        println!("{summary}");
    }
    println!("{} lint", outcome.status().word());
}

/// The `dbtl lint --format json` payload: every finding (`baselined`
/// marked per finding), the severity tally, the effective `fail_on`, and
/// the verdict bit — provisional until a shared report shape replaces it.
#[derive(serde::Serialize)]
struct LintReportJson<'a> {
    findings: &'a [dbtl::Finding],
    summary: LintSummaryJson,
    fail_on: dbtl::FailOn,
    failed: bool,
}

#[derive(serde::Serialize)]
struct LintSummaryJson {
    errors: usize,
    warnings: usize,
    baselined: usize,
    /// Non-baselined findings suppressed by `--diff`'s baseline — present
    /// only when a comparison ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pre_existing: Option<usize>,
}

impl<'a> From<&'a LintOutcome> for LintReportJson<'a> {
    fn from(outcome: &'a LintOutcome) -> Self {
        LintReportJson {
            findings: &outcome.findings,
            summary: LintSummaryJson {
                errors: outcome.errors,
                warnings: outcome.warnings,
                baselined: outcome.baselined,
                pre_existing: outcome.has_diff.then_some(outcome.pre_existing),
            },
            fail_on: outcome.fail_on,
            failed: outcome.failed,
        }
    }
}

/// The `--list-rules` table — every registry rule, one line each: id,
/// category, effective severity (`off` when the config disables it), and
/// the one-line summary. Columns align to the widest entry.
fn print_rules_table(cfg: &dbtl::LintConfig) {
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
    println!(
        "{:<id_w$}  {:<cat_w$}  {:<8}  SUMMARY",
        "ID",
        "CATEGORY",
        "SEVERITY",
        id_w = id_w,
        cat_w = cat_w
    );
    for rule in dbtl::LintRule::ALL {
        let severity = cfg.severity(rule).map(|s| s.name()).unwrap_or("off");
        println!(
            "{:<id_w$}  {:<cat_w$}  {:<8}  {}",
            rule.name(),
            rule.info().category.name(),
            severity,
            rule.info().summary,
            id_w = id_w,
            cat_w = cat_w
        );
    }
}

/// `--explain <ID>` — the full registry entry plus the effective severity
/// and parameter values under the resolved config. An unknown id is a
/// usage error listing the valid set.
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
    println!("id: {}", info.id);
    println!("category: {}", info.category.name());
    println!(
        "severity: {}",
        cfg.severity(rule).map(|s| s.name()).unwrap_or("off")
    );
    println!("requires: {}", requires_name(info.requires));
    println!();
    println!("{}", info.summary);
    println!();
    println!("Rationale:");
    println!("{}", info.rationale);
    println!();
    println!("Example:");
    println!("{}", info.example);
    println!();
    println!("Params:");
    if info.params.is_empty() {
        println!("  (none)");
    } else {
        for p in info.params {
            match p.kind {
                dbtl::ParamKind::Int { min, default } => {
                    let current = cfg.int_param(rule, p.name).unwrap_or(default);
                    println!(
                        "  {}  integer  min {}, default {}, current {} — {}",
                        p.name, min, default, current, p.description
                    );
                }
            }
        }
    }
    Ok(())
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
        for (sev, fo, want) in [
            (Severity::Error, FailOn::Error, true),
            (Severity::Error, FailOn::Warn, true),
            (Severity::Error, FailOn::Never, false),
            (Severity::Warn, FailOn::Error, false),
            (Severity::Warn, FailOn::Warn, true),
            (Severity::Warn, FailOn::Never, false),
        ] {
            assert_eq!(severity_fails(sev, fo), want, "{sev:?} under {fo:?}");
            let want_status = if want {
                GateStatus::Fail
            } else {
                GateStatus::Warn
            };
            assert_eq!(gate_status(sev, fo), want_status, "{sev:?} under {fo:?}");
        }
    }

    #[test]
    fn evaluate_partitions_findings_into_counted_baselined_pre_existing() {
        let dag = two_root_models();
        let cfg = cfg_only(&[dbtl::LintRule::RootModels]);
        // Plain run: both findings count, gate fails under the default.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Error);
        assert_eq!(out.counted(), 2);
        assert_eq!(out.errors, 2);
        assert_eq!(out.baselined, 0);
        assert!(out.failed && out.rules[0].status == GateStatus::Fail);
        // A baseline freezing `a` leaves only `b` counted.
        let frozen = dbtl::Baseline::from_findings(
            &out.findings
                .iter()
                .filter(|f| f.node_uid == "model.p.a")
                .cloned()
                .collect::<Vec<_>>(),
        );
        let out = evaluate_lint(&dag, &cfg, None, Some(&frozen), dbtl::FailOn::Error);
        assert_eq!(out.counted(), 1);
        assert_eq!(out.baselined, 1);
        assert_eq!(out.findings[out.rules[0].counted[0]].node_uid, "model.p.b");
        // Self-diff: every finding is pre-existing — nothing counts.
        let out = evaluate_lint(&dag, &cfg, Some(&dag), None, dbtl::FailOn::Error);
        assert_eq!(out.counted(), 0);
        assert_eq!(out.pre_existing, 2);
        assert!(!out.failed && out.rules[0].status == GateStatus::Pass);
        // Diff + baseline: the frozen one is baselined, the rest pre-existing.
        let out = evaluate_lint(&dag, &cfg, Some(&dag), Some(&frozen), dbtl::FailOn::Error);
        assert_eq!(out.counted(), 0);
        assert_eq!(out.pre_existing, 1);
        assert_eq!(out.baselined, 1);
    }

    #[test]
    fn evaluate_honours_fail_on_for_warn_findings() {
        let dag = two_root_models();
        let mut cfg = cfg_only(&[dbtl::LintRule::RootModels]);
        cfg.severities
            .insert(dbtl::LintRule::RootModels, Some(dbtl::Severity::Warn));
        // Warn findings under the default `error` threshold report but pass.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Error);
        assert_eq!(out.warnings, 2);
        assert!(!out.failed);
        assert_eq!(out.status(), GateStatus::Warn);
        assert_eq!(out.rules[0].status, GateStatus::Warn);
        // `--fail-on warn` fails on them.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Warn);
        assert!(out.failed && out.gating == 2);
        // `never` never fails.
        let out = evaluate_lint(&dag, &cfg, None, None, dbtl::FailOn::Never);
        assert!(!out.failed && out.status() == GateStatus::Warn);
    }
}
