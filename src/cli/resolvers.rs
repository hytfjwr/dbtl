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
/// app's: the auto-detected `target/catalog.json` (when present) tops the
/// column/type priority chain; `--sql-dialect` pins the parse dialect, else
/// it auto-selects from the manifest's adapter (an unknown adapter lands on
/// generic with a stderr notice — the override suppresses it). Shared by
/// `columns`, `impact`, and `check` so every surface resolves identically.
pub(super) fn cll_resolver(
    resolved: &ResolvedSource,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> dbtl::SqlParseResolver {
    let catalog_path = if resolved.is_source {
        dbtl::default_catalog_path(None, Some(std::path::Path::new(&resolved.label)))
    } else {
        dbtl::default_catalog_path(Some(std::path::Path::new(&resolved.label)), None)
    };
    let catalog = catalog_path
        .filter(|p| p.is_file())
        .and_then(|p| dbtl::Catalog::load(&p).ok())
        .map(std::rc::Rc::new);
    let dialect = sql_dialect.unwrap_or_else(|| dbtl::SqlDialect::from_dag(&resolved.dag));
    if sql_dialect.is_none() {
        if let Some(a) = dbtl::SqlDialect::unrecognized_adapter(&resolved.dag) {
            eprintln!(
                "warning: column lineage: unknown adapter '{a}' — using the generic SQL dialect"
            );
        }
    }
    dbtl::SqlParseResolver::new(catalog, dialect)
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
pub(super) fn load_lint_config(
    resolved: &ResolvedSource,
    rules: &[String],
) -> Result<dbtl::LintConfig> {
    let (manifest_arg, project_arg) = if resolved.is_source {
        (None, Some(std::path::Path::new(&resolved.label)))
    } else {
        (Some(std::path::Path::new(&resolved.label)), None)
    };
    let path = dbtl::lint_config_path(manifest_arg, project_arg);
    let mut cfg = match &path {
        Some(p) => dbtl::LintConfig::load(p)
            .with_context(|| format!("check --lint: failed to read {}", p.display()))?,
        None => dbtl::LintConfig::default(),
    };
    if !rules.is_empty() {
        for name in rules {
            anyhow::ensure!(
                dbtl::LintRule::from_name(name).is_some(),
                "unknown lint rule '{name}' (valid rules: {})",
                dbtl::LintRule::ALL
                    .iter()
                    .map(|r| r.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for r in dbtl::LintRule::ALL {
            cfg.rules
                .insert(r.name().to_string(), rules.iter().any(|n| n == r.name()));
        }
    }
    Ok(cfg)
}
