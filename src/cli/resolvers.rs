use super::*;

/// The `meta` key the affected-owners set reads, from
/// `~/.config/dbtl/config.yml`'s `owner_meta_key` (default `owner`) — the
/// same config the TUI reads, so a `dbtl impact` comment and the `D` modal
/// never disagree on who to page.
pub(super) fn owner_meta_key() -> String {
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

/// The CLL resolver for a resolved CURRENT source, built exactly like the
/// app's: an explicit `--catalog` wins over the auto-detected
/// `target/catalog.json` sibling (one [`dbtl::resolve_catalog_path`] rule the
/// App follows too), and the catalog tops the column/type priority chain;
/// `--sql-dialect` pins the parse dialect, else it auto-selects from the
/// manifest's adapter (an unknown adapter lands on generic with a stderr
/// notice — the override suppresses it). Shared by `columns`, `impact`, and
/// `check` so every surface resolves identically.
///
/// An explicit catalog is a REQUIRED input — unreadable fails as an input
/// error; the auto-detected sibling stays best-effort (missing or
/// unreadable just means no catalog data).
pub(super) fn cll_resolver(
    resolved: &ResolvedSource,
    sql_dialect: Option<dbtl::SqlDialect>,
    catalog_flag: Option<&str>,
) -> Result<dbtl::SqlParseResolver, CliError> {
    let (manifest_arg, project_arg) = if resolved.is_source {
        (None, Some(std::path::Path::new(&resolved.label)))
    } else {
        (Some(std::path::Path::new(&resolved.label)), None)
    };
    let catalog = match (
        catalog_flag,
        dbtl::resolve_catalog_path(
            catalog_flag.map(std::path::Path::new),
            manifest_arg,
            project_arg,
        ),
    ) {
        (Some(_), Some(p)) => Some(std::rc::Rc::new(
            dbtl::Catalog::load(&p)
                .with_context(|| format!("failed to load catalog {}", p.display()))
                .input()?,
        )),
        (_, Some(p)) => dbtl::Catalog::load(&p).ok().map(std::rc::Rc::new),
        (_, None) => None,
    };
    let dialect = sql_dialect.unwrap_or_else(|| dbtl::SqlDialect::from_dag(&resolved.dag));
    if sql_dialect.is_none() {
        if let Some(a) = dbtl::SqlDialect::unrecognized_adapter(&resolved.dag) {
            eprintln!(
                "warning: column lineage: unknown adapter '{a}' — using the generic SQL dialect"
            );
        }
    }
    Ok(dbtl::SqlParseResolver::new(catalog, dialect))
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

/// The lint config for `check --lint`: `.dbtl.yml` beside the resolved data
/// source (same auto-detect as the TUI/docs), a malformed file fatal, and
/// `--rules` overlaid as the authoritative rule selector (exactly the named
/// rules run — the file still supplies thresholds and naming prefixes).
/// Every failure is a usage error — a config the caller handed us that we
/// cannot interpret.
pub(super) fn load_lint_config(
    resolved: &ResolvedSource,
    rules: &[String],
) -> Result<dbtl::LintConfig, CliError> {
    let (manifest_arg, project_arg) = if resolved.is_source {
        (None, Some(std::path::Path::new(&resolved.label)))
    } else {
        (Some(std::path::Path::new(&resolved.label)), None)
    };
    let path = dbtl::lint_config_path(manifest_arg, project_arg);
    let mut cfg = match &path {
        Some(p) => dbtl::LintConfig::load(p)
            .with_context(|| format!("check --lint: failed to read {}", p.display()))
            .usage()?,
        None => dbtl::LintConfig::default(),
    };
    if !rules.is_empty() {
        for name in rules {
            if dbtl::LintRule::from_name(name).is_none() {
                return Err(CliError::usage(anyhow::anyhow!(
                    "unknown lint rule '{name}' (valid rules: {})",
                    dbtl::LintRule::ALL
                        .iter()
                        .map(|r| r.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        for r in dbtl::LintRule::ALL {
            cfg.rules
                .insert(r.name().to_string(), rules.iter().any(|n| n == r.name()));
        }
    }
    Ok(cfg)
}
