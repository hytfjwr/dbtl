//! The `dbtl lint` subcommand: config resolution, the baseline file, the
//! `--diff`/`--select` narrowing, `--write-baseline`, and the `--list-rules`
//! / `--explain` queries. The evaluation itself — `run_rules` findings
//! classified into counted / baselined / pre-existing and lifted into the
//! `lint:*` gates — lives in `dbtl_report` (`lint_report`/`lint_assembly`),
//! the ONE path `check --lint` shares, so the two commands can never count
//! or gate differently.

use super::*;

/// `fail_on` resolution — the CLI flag wins over `.dbtl.yml`, which wins
/// over the shipped default (`error`).
pub(super) fn resolve_fail_on(
    cli: Option<dbtl::FailOn>,
    file: Option<dbtl::FailOn>,
) -> dbtl::FailOn {
    cli.or(file).unwrap_or(dbtl::FailOn::Error)
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
/// must never silently un-freeze every violation. Entries naming a REMOVED
/// rule never make it into the parsed set — one stderr warning per removed
/// id reports how many rows were ignored.
pub(super) fn load_lint_baseline_file(
    path: &std::path::Path,
    explicit: bool,
) -> Result<Option<dbtl::Baseline>, CliError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let baseline = dbtl::Baseline::from_json(&text)
                .map_err(|e| CliError::input(anyhow::anyhow!("{}: {e}", path.display())))?;
            for (id, count) in &baseline.removed_rule_entries {
                eprintln!("warning: baseline: ignoring {count} entries for removed rule {id}");
            }
            Ok(Some(baseline))
        }
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
    /// `--preset` — the base rule set, overriding `.dbtl.yml`'s
    /// `lint.preset`.
    pub preset: Option<dbtl::LintPreset>,
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
    /// `--show-suppressed` — list the suppressed findings (with reasons)
    /// in the human/json outputs.
    pub show_suppressed: bool,
}

/// The lint config the query modes (`--list-rules`, `--explain`) print:
/// `.dbtl.yml` is probed from the data-path DECISION alone ([`source_path`])
/// — never a data load. A source that can't be determined yields the
/// shipped defaults instead of a data-source error.
fn query_lint_config(
    src: &DataSourceArgs,
    rules: &[String],
    preset: Option<dbtl::LintPreset>,
) -> Result<dbtl::LintConfig, CliError> {
    let (manifest_arg, project_arg) =
        match source_path(src.manifest, src.source, src.project, src.diff) {
            Some(SourcePath::Manifest(path)) => (Some(path), None),
            Some(SourcePath::Project(dir)) => (None, Some(dir)),
            None => (None, None),
        };
    let mut cfg = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())?
        .config
        .lint;
    apply_preset_and_rules(&mut cfg, preset, rules)?;
    Ok(cfg)
}

/// Apply the CLI lint layers on top of the file config: `--preset`
/// replaces `lint.preset` (the categories and per-rule `rules` entries
/// still sit above it), then `--rules` resolves to the authoritative
/// per-rule set. Both are `None`-safe no-ops.
pub(super) fn apply_preset_and_rules(
    cfg: &mut dbtl::LintConfig,
    preset: Option<dbtl::LintPreset>,
    rules: &[String],
) -> Result<(), CliError> {
    if let Some(p) = preset {
        cfg.preset = p;
    }
    apply_rules_overlay(cfg, rules)
}

/// Run the `lint` subcommand: `--list-rules`/`--explain` answer from the
/// registry + config alone (no data load); otherwise resolve, evaluate
/// once through [`lint_report`](dbtl_report::lint_report), print the
/// report, and gate the exit on the resolved `fail_on`.
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
        let cfg = query_lint_config(src, args.rules, args.preset)?;
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
    apply_preset_and_rules(&mut cfg, args.preset, args.rules)?;
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
        let outcome = dbtl::run_rules(
            dbtl::LintInput::new(&resolved.dag, &cfg)
                .with_sql(Some(&sql_resolver))
                .with_catalog(catalog.as_deref()),
        );
        let path = match path_arg {
            Some(p) => PathBuf::from(p),
            None => lint_baseline_path(&cfg.baseline, config_found, &candidates),
        };
        // Merge into whatever the file already holds: the rules that ran
        // rewrite their rows, but rows for rules `--rules` narrowed out —
        // or that skipped for a missing input — carry over. Without the
        // merge a subset rewrite would silently un-freeze every violation
        // it never evaluated.
        let ran: std::collections::BTreeSet<dbtl::LintRule> = dbtl::LintRule::ALL
            .iter()
            .copied()
            .filter(|r| cfg.enabled(*r))
            .filter(|r| !outcome.skipped.iter().any(|s| s.rule == *r))
            .collect();
        let mut baseline = load_lint_baseline_file(&path, false)?.unwrap_or_default();
        let fresh = dbtl::Baseline::from_findings(&outcome.findings);
        let new_entries = fresh.len();
        baseline.replace_entries_for(&ran, fresh);
        let kept = baseline.len() - new_entries;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))
                .input()?;
        }
        std::fs::write(&path, baseline.to_json())
            .with_context(|| format!("failed to write lint baseline {}", path.display()))
            .input()?;
        // `exclude:`/selector-param deprecation notes surface on this path
        // too — the baseline branch returns before the shared warning block.
        warn_each("", &outcome.warnings);
        if !args.quiet {
            let carried = if kept > 0 {
                format!(" ({kept} kept for rules that did not run)")
            } else {
                String::new()
            };
            write_stdout(
                &format!(
                    "wrote {} finding(s) to {}{}\n",
                    baseline.len(),
                    path.display(),
                    carried
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
        Some(d) => Some(load_baseline(
            d,
            src.scope,
            &src.project_dir(),
            src.cli_vars()?.as_ref(),
        )?),
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
    let mut select_notes: Vec<String> = Vec::new();
    let selection = if args.select.is_empty() {
        None
    } else {
        let (sel, notes) = selector_set_report(&resolved.dag, diff.as_ref(), args.select);
        select_notes = notes;
        if sel.is_empty() {
            eprintln!(
                "warning: --select '{}' matched no nodes",
                args.select.join(" ")
            );
        }
        Some(sel)
    };
    // Best-effort yml line numbers — the file-reading pass `run_rules`
    // stays free of. The SAME `.yml` text source fills positions inside
    // `lint_report` and excerpts inside `plan.emit` (one memoized read per
    // file, so a position always annotates the text it was found in).
    let project_root = lint_project_root(&candidates);
    let sources = SourceTexts::new(&project_root);
    let read_text = sources.read_text();
    // One gate per rule — the same `lint:*` gates `check --lint` reports,
    // through the shared `dbtl_report` evaluation path.
    let run = lint_report(
        resolved.dag.project_name().unwrap_or("dbt"),
        src.scope.describe(),
        &LintRunInputs {
            dag: &resolved.dag,
            cfg: &cfg,
            base: base.as_ref().map(|(b, _)| b),
            baseline: baseline.as_ref(),
            fail_on,
            aux: LintAux {
                sql: Some(&sql_resolver),
                catalog: catalog.as_deref(),
                base_sql: base_resolver.as_ref(),
            },
            selection: selection.as_ref(),
            show_suppressed: args.show_suppressed,
            read_text: &read_text,
        },
    );
    // `exclude:`/selector-param deprecation notes from the evaluation plus
    // the `--select` notes — deduplicated: a spec shared between the two
    // channels warns once.
    let mut notes: std::collections::BTreeSet<String> = select_notes.into_iter().collect();
    notes.extend(run.warnings.iter().cloned());
    warn_each("", &notes.into_iter().collect::<Vec<_>>());
    plan.emit(
        &run.report,
        "lint",
        &RenderEnv {
            dag: &resolved.dag,
            read_text: &read_text,
        },
    )?;
    if run.failed {
        Err(CliError::gate(anyhow::anyhow!(
            "lint: {} finding(s) at or above fail-on '{}'",
            run.gating,
            run.fail_on.name()
        )))
    } else {
        Ok(())
    }
}

/// The `--list-rules` table — every registry rule, one line each: id,
/// category, the shipped default (`off` for an opt-in rule), the
/// EFFECTIVE severity under the resolved config, whether the registry
/// enables the rule by default, its on/off under each preset, and the
/// one-line summary. Columns align to the widest entry. Written through
/// the same broken-pipe-safe stdout helper the report uses.
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
        "{:<id_w$}  {:<cat_w$}  {:<7}  {:<8}  {:<7}  {:<7}  {:<8}  {:<6}  SUMMARY\n",
        "ID",
        "CATEGORY",
        "DEFAULT",
        "SEVERITY",
        "ENABLED",
        "MINIMAL",
        "STANDARD",
        "STRICT",
        id_w = id_w,
        cat_w = cat_w
    );
    for rule in dbtl::LintRule::ALL {
        let info = rule.info();
        let on_off = |on: bool| if on { "on" } else { "off" };
        let severity = cfg.severity(rule).map(|s| s.name()).unwrap_or("off");
        let default = if info.default_enabled {
            info.default_severity.name()
        } else {
            "off"
        };
        out += &format!(
            "{:<id_w$}  {:<cat_w$}  {:<7}  {:<8}  {:<7}  {:<7}  {:<8}  {:<6}  {}\n",
            rule.name(),
            info.category.name(),
            default,
            severity,
            on_off(info.default_enabled),
            on_off(dbtl::LintPreset::Minimal.enables(info)),
            on_off(dbtl::LintPreset::Standard.enables(info)),
            on_off(dbtl::LintPreset::Strict.enables(info)),
            info.summary,
            id_w = id_w,
            cat_w = cat_w
        );
    }
    write_stdout(&out, anstream::ColorChoice::Auto)
}

/// `--explain <ID>` — the full registry entry plus the effective severity
/// and parameter values under the resolved config, the fix guidance, and
/// the rule's reference link. An unknown id is a usage error listing the
/// valid set; a removed id is a usage error naming the replacement rules.
fn explain_rule(id: &str, cfg: &dbtl::LintConfig) -> Result<(), CliError> {
    let Some(rule) = dbtl::LintRule::from_name(id) else {
        return Err(CliError::usage(anyhow::anyhow!(
            "{}",
            dbtl::removed_rule_error(id).unwrap_or_else(|| format!(
                "unknown lint rule '{id}' (valid rules: {})",
                dbtl::LintRule::ALL
                    .iter()
                    .map(|r| r.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        )));
    };
    let info = rule.info();
    let mut out = format!("id: {}\n", info.id);
    out += &format!("category: {}\n", info.category.name());
    out += &format!(
        "severity: {}\n",
        cfg.severity(rule).map(|s| s.name()).unwrap_or("off")
    );
    if info.default_enabled {
        out += &format!(
            "default: {} (enabled by default)\n",
            info.default_severity.name()
        );
    } else {
        out += &format!(
            "default: off (opt-in) — enable with `lint.preset: strict`, \
             `lint.categories.{}: error|warn`, `lint.rules.{}: true`, \
             or `--rules {}`\n",
            info.category.name(),
            info.id,
            info.id
        );
    }
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
                dbtl::ParamKind::SelectorList { default } => {
                    let current = cfg
                        .str_list_param(rule, p.name)
                        .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect());
                    out += &format!(
                        "  {}  selector-list  default [{}], current [{}] — {}\n",
                        p.name,
                        default.join(", "),
                        current.join(", "),
                        p.description
                    );
                }
                dbtl::ParamKind::LayerList { default, builtins } => {
                    let current = cfg
                        .str_list_param(rule, p.name)
                        .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect());
                    out += &format!(
                        "  {}  layer-list  default [{}], current [{}], built-in [{}] + lint.naming_prefixes layers — {}\n",
                        p.name,
                        default.join(", "),
                        current.join(", "),
                        builtins.join(", "),
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
}
