use super::*;

/// The `meta` key the affected-owners set reads: `.dbtl.yml`'s top-level
/// `owner_meta_key` when the project config names one, else
/// `~/.config/dbtl/config.yml`'s, else the `owner` default — ONE resolution
/// the `check`/`impact` gates and the TUI share, so a CI comment and the
/// `D` modal never disagree on who to page. `project` is the already-parsed
/// `ProjectConfig::owner_meta_key`.
///
/// `user_config` controls whether `config.yml` is consulted at all: the
/// gates and the TUI resolve `owner_meta_key` project-first-then-user, but
/// generated docs must be reproducible from the project inputs alone — a
/// committed `owners.md` must never depend on a maintainer's home dir — so
/// `docs` passes `false` (`.dbtl.yml`, else `owner`).
pub(super) fn owner_meta_key(project: Option<&str>, user_config: bool) -> String {
    if let Some(key) = project {
        return key.to_string();
    }
    if !user_config {
        return "owner".to_string();
    }
    match crate::state::load_config() {
        Ok(c) => c.owner_meta_key.unwrap_or_else(|| "owner".to_string()),
        // Same policy as the TUI's startup: a malformed config warns and
        // falls back to defaults — it never blocks the subcommand.
        Err(e) => {
            eprintln!("warning: ignoring dbtl config.yml: {e}");
            "owner".to_string()
        }
    }
}

/// The catalog a resolved current source loads under the shared
/// `--catalog`-or-auto-detect rule [`dbtl::resolve_catalog_path`] — the
/// same one the App's startup follows. An explicit `--catalog` is a
/// REQUIRED input: unreadable fails as an input error; the auto-detected
/// sibling stays best-effort (missing or unreadable just means no catalog
/// data — `Requires::Catalog` lint rules then report `skipped`).
fn load_cli_catalog(
    resolved: &ResolvedSource,
    catalog_flag: Option<&str>,
) -> Result<Option<std::rc::Rc<dbtl::Catalog>>, CliError> {
    let (manifest_arg, project_arg) = if resolved.is_source {
        (None, Some(std::path::Path::new(&resolved.label)))
    } else {
        (Some(std::path::Path::new(&resolved.label)), None)
    };
    match (
        catalog_flag,
        dbtl::resolve_catalog_path(
            catalog_flag.map(std::path::Path::new),
            manifest_arg,
            project_arg,
        ),
    ) {
        (Some(_), Some(p)) => Ok(Some(std::rc::Rc::new(
            dbtl::Catalog::load(&p)
                .with_context(|| format!("failed to load catalog {}", p.display()))
                .input()?,
        ))),
        (_, Some(p)) => Ok(dbtl::Catalog::load(&p).ok().map(std::rc::Rc::new)),
        (_, None) => Ok(None),
    }
}

/// The parse dialect a resolved current source runs under: `--sql-dialect`
/// wins, else the manifest's adapter auto-selects (an unknown adapter
/// lands on generic with a stderr notice — the override suppresses it).
fn cli_dialect(
    resolved: &ResolvedSource,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> dbtl::SqlDialect {
    let dialect = sql_dialect.unwrap_or_else(|| dbtl::SqlDialect::from_dag(&resolved.dag));
    if sql_dialect.is_none() {
        if let Some(a) = dbtl::SqlDialect::unrecognized_adapter(&resolved.dag) {
            eprintln!(
                "warning: column lineage: unknown adapter '{a}' — using the generic SQL dialect"
            );
        }
    }
    dialect
}

/// The CLL resolver for a resolved CURRENT source, built exactly like the
/// app's: an explicit `--catalog` wins over the auto-detected
/// `target/catalog.json` sibling, and the catalog tops the column/type
/// priority chain; `--sql-dialect` pins the parse dialect, else it
/// auto-selects from the manifest's adapter. Shared by `columns`,
/// `impact`, and `check` so every surface resolves identically.
pub(super) fn cll_resolver(
    resolved: &ResolvedSource,
    sql_dialect: Option<dbtl::SqlDialect>,
    catalog_flag: Option<&str>,
) -> Result<dbtl::SqlParseResolver, CliError> {
    let catalog = load_cli_catalog(resolved, catalog_flag)?;
    let dialect = cli_dialect(resolved, sql_dialect);
    Ok(dbtl::SqlParseResolver::new(catalog, dialect))
}

/// A parser resolver over a catalog the CALLER already resolved — `docs`
/// hands its pages the opt-in catalog (never the aux auto-detect's sibling)
/// so generated output can't pick up warehouse data nobody asked for. The
/// dialect rule is [`cli_dialect`]'s: one notice policy regardless of
/// which catalog the resolver carries.
pub(super) fn parse_resolver(
    resolved: &ResolvedSource,
    catalog: Option<std::rc::Rc<dbtl::Catalog>>,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> dbtl::SqlParseResolver {
    dbtl::SqlParseResolver::new(catalog, cli_dialect(resolved, sql_dialect))
}

/// The auxiliary lint inputs for a resolved current source — the loaded
/// catalog AND the shared lazy [`dbtl::SqlParseResolver`] built over it —
/// so `lint`, `check --lint`, and `docs` feed [`dbtl::LintInput`] the same
/// inputs (`with_sql` always; `with_catalog` when a catalog resolved, else
/// the catalog rules come back `skipped`). Building the resolver is
/// allocation-only: it parses nothing until a SQL rule asks.
pub(super) fn lint_aux(
    resolved: &ResolvedSource,
    sql_dialect: Option<dbtl::SqlDialect>,
    catalog_flag: Option<&str>,
) -> Result<(Option<std::rc::Rc<dbtl::Catalog>>, dbtl::SqlParseResolver), CliError> {
    let catalog = load_cli_catalog(resolved, catalog_flag)?;
    let dialect = cli_dialect(resolved, sql_dialect);
    Ok((
        catalog.clone(),
        dbtl::SqlParseResolver::new(catalog, dialect),
    ))
}

/// The baseline side's diff resolver: no catalog (the baseline's warehouse
/// record isn't loaded — its declared + CLL-inferred columns still resolve
/// through the same priority chain); `--sql-dialect` pins BOTH sides'
/// dialects, else [`dbtl::SqlDialect::for_diff_baseline`] shares the current
/// side's dialect — one parse dialect per diff, or normalized expressions
/// can't compare.
pub(super) fn baseline_resolver(
    current: &dbtl::Dag,
    base: &dbtl::Dag,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> dbtl::SqlParseResolver {
    let dialect = sql_dialect.unwrap_or_else(|| dbtl::SqlDialect::for_diff_baseline(current, base));
    dbtl::SqlParseResolver::new(None, dialect)
}

/// The `.dbtl.yml` in effect for a data source: the parsed file (the
/// shipped defaults when none exists) plus the path it was read from —
/// `None` meaning "no file", which anchors the default lint-baseline path
/// instead.
pub(super) struct ProjectConfigFile {
    pub config: dbtl::ProjectConfig,
    pub path: Option<PathBuf>,
}

/// The (manifest, project-dir) pair `.dbtl.yml` discovery keys off: source
/// mode treats the project dir as the root, manifest mode the manifest's
/// location — the same inputs [`dbtl::project_config_path`] takes.
pub(super) fn resolved_config_args(
    resolved: &ResolvedSource,
) -> (Option<PathBuf>, Option<PathBuf>) {
    if resolved.is_source {
        (None, Some(PathBuf::from(&resolved.label)))
    } else {
        (Some(PathBuf::from(&resolved.label)), None)
    }
}

/// Load `.dbtl.yml` for a (manifest, project-dir) probe — the shared entry
/// `check`, `lint`, and `docs` take so the file's discovery, its
/// `warning: …` stderr lines, and its error class can never drift between
/// commands. A missing file yields the shipped defaults; a malformed file
/// is a usage error — a config a CI gate can't interpret must fail loudly.
pub(super) fn load_project_config(
    manifest_arg: Option<&std::path::Path>,
    project_arg: Option<&std::path::Path>,
) -> Result<ProjectConfigFile, CliError> {
    let path = dbtl::project_config_path(manifest_arg, project_arg);
    let config = match &path {
        Some(p) => {
            let pc = dbtl::ProjectConfig::load(p)
                .with_context(|| format!("failed to read {}", p.display()))
                .usage()?;
            warn_each("", &pc.warnings);
            pc
        }
        None => dbtl::ProjectConfig::default(),
    };
    Ok(ProjectConfigFile { config, path })
}

/// Every `--rules` name must be a registry rule — checked before the data
/// source loads so an unknown name is a usage error, never masked behind a
/// missing-input error. A REMOVED rule id is still a usage error, but the
/// message names the replacement coverage instead of the valid set.
pub(super) fn validate_rule_names(rules: &[String]) -> Result<(), CliError> {
    for name in rules {
        if dbtl::LintRule::from_name(name).is_none() {
            return Err(CliError::usage(anyhow::anyhow!(
                "{}",
                dbtl::removed_rule_error(name).unwrap_or_else(|| format!(
                    "unknown lint rule '{name}' (valid rules: {})",
                    dbtl::LintRule::ALL
                        .iter()
                        .map(|r| r.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            )));
        }
    }
    Ok(())
}

/// Strictly validate a `--select`/`--exclude` spec list through the ONE
/// grammar [`dbtl::validate_selector`] — run BEFORE the data source loads,
/// so a malformed spec is a usage error that can't hide behind a
/// missing-input error. `flag` names the argument in the message
/// (`--select`, `--exclude`, or `ls`'s positional).
pub(super) fn validate_selector_specs(specs: &[String], flag: &str) -> Result<(), CliError> {
    for spec in specs {
        dbtl::validate_selector(spec)
            .map_err(|e| CliError::usage(anyhow::anyhow!("invalid {flag} '{spec}': {e}")))?;
    }
    Ok(())
}

/// Whether any spec carries a `state:` atom — after [`validate_selector_specs`],
/// so a malformed spec reports its own error first.
pub(super) fn specs_use_state(specs: &[String]) -> bool {
    specs.iter().any(|s| dbtl::selector_uses_state(s))
}

/// The `state:` precondition every selector-carrying subcommand shares: a
/// `state:` atom compares against the `--diff` baseline, so one without
/// the other is a usage error — never a silently-empty selection.
pub(super) fn require_diff_for_state(specs: &[String], has_diff: bool) -> Result<(), CliError> {
    if specs_use_state(specs) && !has_diff {
        return Err(CliError::usage(anyhow::anyhow!(
            "a 'state:' selector needs --diff to name the baseline it compares against"
        )));
    }
    Ok(())
}

/// Evaluate the selector list into its `unique_id` set plus each spec's
/// evaluation notes — currently the `path:` legacy resource-relative
/// form, so a non-interactive command prints them once per invocation
/// through [`crate::cli::out::warn_each`]. The set is the union of each
/// spec's [`dbtl::select_report`] result (a repeated flag unions exactly
/// like a whitespace-separated spec). Infallible once
/// [`validate_selector_specs`] passed; `diff` feeds `state:` atoms.
pub(super) fn selector_set_report(
    dag: &dbtl::Dag,
    diff: Option<&dbtl::DagDiff>,
    specs: &[String],
) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let mut set = std::collections::BTreeSet::new();
    let mut notes = std::collections::BTreeSet::new();
    for spec in specs {
        let (matched, spec_notes) = dbtl::select_report(dag, diff, spec);
        set.extend(matched);
        notes.extend(spec_notes);
    }
    (set, notes.into_iter().collect())
}

/// The `DagDiff` a `state:` selector compares against — the SAME resolver
/// pair `impact`/`check` build ([`dbtl::compute_diff_with`]: catalog-backed
/// current side, catalog-less baseline), so `state:modified` selects
/// exactly the change set the impact report counts.
pub(super) fn selector_diff(
    src: &DataSourceArgs,
    resolved: &ResolvedSource,
    base: &dbtl::Dag,
) -> Result<dbtl::DagDiff, CliError> {
    let cur_res = src.cll_resolver(resolved)?;
    let base_res = src.baseline_resolver(&resolved.dag, base);
    Ok(dbtl::compute_diff_with(
        base,
        &resolved.dag,
        &base_res,
        &cur_res,
    ))
}

/// Overlay `--rules` on a lint config: the flag is the authoritative rule
/// SELECTOR — exactly the named rules run; a file-`off` (or preset- or
/// category-`off`) named rule lifts to `On(None)` — its severity resolves
/// to the category's `error`/`warn` if one is configured, else the
/// registry default (a configured non-off severity stands, like the
/// file's thresholds and prefixes do). An unknown name is a usage error
/// listing the valid ids.
pub(super) fn apply_rules_overlay(
    cfg: &mut dbtl::LintConfig,
    rules: &[String],
) -> Result<(), CliError> {
    if rules.is_empty() {
        return Ok(());
    }
    validate_rule_names(rules)?;
    for r in dbtl::LintRule::ALL {
        if rules.iter().any(|n| n == r.name()) {
            if !matches!(cfg.rules.get(&r), Some(dbtl::RuleEnabled::On(_))) {
                cfg.rules.insert(r, dbtl::RuleEnabled::On(None));
            }
        } else {
            cfg.rules.insert(r, dbtl::RuleEnabled::Off);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An empty source-mode ResolvedSource rooted at `dir` (the `.dbtl.yml`
    /// search treats the project dir itself as the root).
    fn resolved_at(dir: &std::path::Path) -> ResolvedSource {
        let manifest = dbtl::RawManifest {
            unresolved_adjacent: Default::default(),
            metadata: Default::default(),
            nodes: HashMap::new(),
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
            docs: Default::default(),
            disabled: Default::default(),
            ref_scope: None,
        };
        ResolvedSource {
            dag: dbtl::Dag::build(&manifest),
            label: dir.to_string_lossy().into_owned(),
            is_source: true,
            warnings: Vec::new(),
        }
    }

    /// The production composition under test: `.dbtl.yml` beside the
    /// resolved source → the `lint:` section → the `--rules` overlay.
    fn lint_cfg_for(resolved: &ResolvedSource, rules: &[String]) -> dbtl::LintConfig {
        let (manifest_arg, project_arg) = resolved_config_args(resolved);
        let mut cfg = load_project_config(manifest_arg.as_deref(), project_arg.as_deref())
            .unwrap()
            .config
            .lint;
        apply_rules_overlay(&mut cfg, rules).unwrap();
        cfg
    }

    #[test]
    fn rules_flag_is_the_authoritative_selector() {
        // Named rules run even when the file turns them off — at the
        // registry's default severity; a file-configured non-off severity
        // stands; unnamed rules go off entirely.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".dbtl.yml"),
            "lint:\n  rules:\n    root_models: false\n    source_fanout: {severity: warn}\n",
        )
        .unwrap();
        let resolved = resolved_at(dir.path());
        let cfg = lint_cfg_for(
            &resolved,
            &["root_models".to_string(), "source_fanout".to_string()],
        );
        assert!(cfg.enabled(dbtl::LintRule::RootModels));
        assert_eq!(
            cfg.severity(dbtl::LintRule::RootModels),
            Some(dbtl::Severity::Warn),
            "a named-but-off rule runs at the registry default severity"
        );
        assert_eq!(
            cfg.severity(dbtl::LintRule::SourceFanout),
            Some(dbtl::Severity::Warn),
            "a named rule keeps its configured non-off severity"
        );
        assert!(
            !cfg.enabled(dbtl::LintRule::ModelFanout),
            "unnamed rules go off under --rules"
        );
        // No --rules → the file's off applies.
        let cfg = lint_cfg_for(&resolved, &[]);
        assert!(!cfg.enabled(dbtl::LintRule::RootModels));
        assert_eq!(
            cfg.severity(dbtl::LintRule::SourceFanout),
            Some(dbtl::Severity::Warn)
        );
    }

    #[test]
    fn rules_flag_lifts_preset_and_category_off_rules() {
        // A named rule off via `preset` or `categories` lifts too — the
        // flag is authoritative over EVERY off layer. Its severity
        // resolves to the category's `error`/`warn` when one is set,
        // else the registry default.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".dbtl.yml"),
            "lint:\n  preset: minimal\n  categories:\n    modeling: off\n    sql: error\n",
        )
        .unwrap();
        let resolved = resolved_at(dir.path());
        let cfg = lint_cfg_for(
            &resolved,
            &["root_models".to_string(), "select_star".to_string()],
        );
        assert_eq!(
            cfg.severity(dbtl::LintRule::RootModels),
            Some(dbtl::Severity::Warn),
            "a modeling:off + minimal-off rule lifts to the registry default"
        );
        assert_eq!(
            cfg.severity(dbtl::LintRule::SelectStar),
            Some(dbtl::Severity::Error),
            "a minimal-off rule under `sql: error` lifts at the category severity"
        );
        assert!(
            !cfg.enabled(dbtl::LintRule::MissingPrimaryKeyTests),
            "unnamed rules go off even when the preset would run them"
        );
        // No --rules → the file's layers apply untouched: `modeling: off`
        // holds, `sql: error` still enables select_star at error above the
        // preset, and the minimal set's error-default rules run.
        let cfg = lint_cfg_for(&resolved, &[]);
        assert!(!cfg.enabled(dbtl::LintRule::RootModels));
        assert_eq!(
            cfg.severity(dbtl::LintRule::SelectStar),
            Some(dbtl::Severity::Error)
        );
        assert!(cfg.enabled(dbtl::LintRule::MissingPrimaryKeyTests));
    }

    #[test]
    fn owner_meta_key_resolves_project_first_then_user_policy() {
        // A `.dbtl.yml` key always wins — over the user config AND over
        // the project-only path docs takes.
        for user in [true, false] {
            assert_eq!(
                owner_meta_key(Some("team"), user),
                "team",
                "the project key wins (user_config={user})"
            );
        }
        // With no project key the docs path (`user_config: false`) can only
        // ever produce the `owner` default — it never opens config.yml, so
        // committed pages can't depend on a maintainer's home dir.
        assert_eq!(owner_meta_key(None, false), "owner");
        // `true` resolves the user config (the gates' path) — still
        // `owner` when no config.yml names a key; what it returns under a
        // configured home dir is the shared file-loading rule, covered by
        // the config loader's own tests.
    }
}
