use super::*;

/// A resolved data source: the loaded `Dag`, a human-readable label of where it
/// came from (a manifest path or a project dir, for docs provenance), whether
/// it was parsed from source (vs a compiled manifest), and every warning the
/// load produced (loader + package scope — each already printed to stderr
/// exactly as before).
pub struct ResolvedSource {
    pub dag: dbtl::Dag,
    pub label: String,
    pub is_source: bool,
    /// The collected warnings (loader first, then scope). `build_app` feeds
    /// them to the App's notification log as unread WARNING entries so they
    /// stay visible after the TUI opens; non-TUI callers ignore
    /// them — stderr output is unchanged.
    pub warnings: Vec<String>,
}

/// The global data-source inputs every non-interactive subcommand consumes —
/// bundled once so `run_*` signatures stay short and every command resolves
/// exactly the way `build_app` does. The fields are all `global` clap args,
/// so [`DataSourceArgs::from_cli`] sees them whether they were typed before
/// or after the subcommand.
pub struct DataSourceArgs<'a> {
    /// `--manifest`.
    pub manifest: &'a Option<String>,
    /// `--source`.
    pub source: &'a Option<String>,
    /// `--project` (`.` when unset).
    pub project: &'a str,
    /// `--diff` — the baseline, for the commands that compare.
    pub diff: Option<&'a str>,
    /// `--catalog` — an explicit catalog.json override.
    pub catalog: Option<&'a str>,
    /// The package scope `--include-packages` computes to.
    pub scope: dbtl::PackageScope,
    /// `--sql-dialect` — the parse-dialect pin, if given.
    pub sql_dialect: Option<dbtl::SqlDialect>,
}

impl<'a> DataSourceArgs<'a> {
    /// Bundle the global flags of a parsed `Cli`.
    pub fn from_cli(cli: &'a Cli) -> Self {
        Self {
            manifest: &cli.manifest,
            source: &cli.source,
            project: &cli.project,
            diff: cli.diff.as_deref(),
            catalog: cli.catalog.as_deref(),
            scope: cli.package_scope(),
            sql_dialect: cli.sql_dialect,
        }
    }

    /// Load the graph through the shared [`resolve_source`] rule.
    pub fn resolve(&self) -> Result<ResolvedSource, CliError> {
        resolve_source(self.manifest, self.source, self.project, self.scope)
    }

    /// The current-side CLL resolver for a resolved source — `--catalog` and
    /// `--sql-dialect` included.
    pub fn cll_resolver(
        &self,
        resolved: &ResolvedSource,
    ) -> Result<dbtl::SqlParseResolver, CliError> {
        cll_resolver(resolved, self.sql_dialect, self.catalog)
    }

    /// The baseline-side diff resolver (no baseline catalog — its warehouse
    /// record is never loaded), sharing the current side's dialect rule.
    pub fn baseline_resolver(
        &self,
        current: &dbtl::Dag,
        base: &dbtl::Dag,
    ) -> dbtl::SqlParseResolver {
        baseline_resolver(current, base, self.sql_dialect)
    }
}

/// The shared data-source resolution, identical for the TUI and the
/// subcommands. Explicit `manifest` / `source` win; otherwise auto-detect
/// under `project`: a compiled `target/manifest.json` if present (full
/// fidelity), else parse the project from source. The TUI ([`build_app`])
/// and the subcommands all go through here so the precedence can never
/// diverge. Every failure here is an input error — a named file/dir that
/// could not be read.
///
/// `scope` is applied to the loaded graph on the way out (the default `Root`
/// scope prunes installed-package nodes — manifest mode's `nodes` map mixes
/// them in), so every caller sees the same scoped `Dag` and the package
/// boundary lives in one place.
pub fn resolve_source(
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    scope: dbtl::PackageScope,
) -> Result<ResolvedSource, CliError> {
    if let Some(path) = manifest {
        let (dag, mut warnings) = load_dag_with_warnings(path).input()?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, false);
        warnings.extend(scope_w);
        return Ok(ResolvedSource {
            dag,
            label: path.clone(),
            is_source: false,
            warnings,
        });
    }
    if let Some(dir) = source {
        let (dag, mut warnings) = load_dag_from_source_with_warnings(dir).input()?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, true);
        warnings.extend(scope_w);
        return Ok(ResolvedSource {
            dag,
            label: dir.clone(),
            is_source: true,
            warnings,
        });
    }
    let project = PathBuf::from(project);
    let manifest = project.join("target").join("manifest.json");
    if manifest.is_file() {
        let (dag, mut warnings) = load_dag_with_warnings(&manifest).input()?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, false);
        warnings.extend(scope_w);
        Ok(ResolvedSource {
            dag,
            label: manifest.display().to_string(),
            is_source: false,
            warnings,
        })
    } else if project.join("dbt_project.yml").is_file() {
        let (dag, mut warnings) = load_dag_from_source_with_warnings(&project).input()?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, true);
        warnings.extend(scope_w);
        Ok(ResolvedSource {
            dag,
            label: project.display().to_string(),
            is_source: true,
            warnings,
        })
    } else {
        Err(CliError::input(anyhow::anyhow!(
            "no data source under {}: neither target/manifest.json nor dbt_project.yml found \
             (pass --manifest <file> or --source <dir>)",
            project.display()
        )))
    }
}
/// Apply the loaded graph's package scope: `All` keeps the manifest's full
/// contents (installed-package nodes as dependency context); the default
/// `Root` prunes to the root project — what `--project-only` used to ask for.
/// The scope decision itself (prune predicate, early-out, and the warnings —
/// an emptied graph, or an ambiguous root guess when `metadata.project_name`
/// is absent) lives in [`dbtl::PackageScope::apply_with_warnings`], so the
/// TUI's `reload` re-running the same scope reports the same outcomes. This
/// wrapper prints them to stderr exactly as before AND returns them (the
/// `(scope)` tag mirroring the stderr prefix) so `resolve_source` can hand
/// them to the notification log; the one CLI-side warning left is the
/// `--include-packages`-in-source-mode no-op, which is about the flag, not
/// the graph.
pub(super) fn apply_package_scope(
    dag: dbtl::Dag,
    scope: dbtl::PackageScope,
    is_source: bool,
) -> (dbtl::Dag, Vec<String>) {
    let mut collected = Vec::new();
    if scope.includes_packages() && is_source {
        // Source mode synthesizes the manifest from the local project
        // only — there are never package nodes to include.
        let w = "--include-packages has no effect in source mode \
                 (installed packages are never parsed)"
            .to_string();
        eprintln!("warning: {w}");
        collected.push(w);
    }
    let (dag, warnings) = scope.apply_with_warnings(dag);
    for w in &warnings {
        eprintln!("warning (scope): {w}");
        collected.push(format!("(scope) {w}"));
    }
    (dag, collected)
}

/// Resolve the CLI into a loaded [`App`]. Explicit `--manifest` / `--source` win;
/// otherwise auto-detect under `--project`: a compiled `target/manifest.json` if
/// present (full fidelity), else parse the project from source. The package
/// scope (`--include-packages`) is applied inside [`resolve_source`] and
/// recorded on the App so `reload` re-scopes the same way.
pub fn build_app(cli: &Cli) -> Result<App, CliError> {
    let scope = cli.package_scope();
    let resolved = resolve_source(&cli.manifest, &cli.source, &cli.project, scope)?;
    let ResolvedSource {
        dag,
        label,
        is_source,
        warnings,
    } = resolved;
    let mut app = if is_source {
        App::from_source(dag, PathBuf::from(label))
    } else {
        App::new(dag, PathBuf::from(label))
    };
    app.set_scope(scope);
    // The load/scope warnings that already went to stderr also join
    // the notification log as unread WARNING entries — opening `Ctrl-n`
    // replays what scrolled past before the TUI came up. One stamp per batch
    // (now_hhmmss spawns `date`; per-line stamping would fork per warning).
    app.record_startup_warnings(&warnings, &crate::event_loop::now_hhmmss());
    // An explicit --sql-dialect pins the column-lineage parser dialect;
    // otherwise the resolver auto-selects from the manifest's adapter_type.
    // An adapter_type we don't know lands on the generic dialect — say so
    // as the startup toast (the override suppresses it), at WARNING level so
    // it also counts toward `!N`.
    if let Some(dialect) = cli.sql_dialect {
        app.set_sql_dialect(dialect);
    } else if let Some(a) = dbtl::SqlDialect::unrecognized_adapter(&app.dag) {
        app.set_notice_level(
            dbtl::NoticeLevel::Warning,
            format!("column lineage: unknown adapter '{a}' — using the generic SQL dialect"),
        );
    }
    // Explicit side-maps override the auto-detected siblings; a load error
    // IS user-facing (unlike the silent auto-detect). Both setters preserve
    // each other's state, so the order carries no semantics.
    if let Some(cat) = &cli.catalog {
        app.set_catalog(PathBuf::from(cat)).input()?;
    }
    if let Some(rr) = &cli.run_results {
        app.set_run_results(PathBuf::from(rr)).input()?;
    }
    Ok(app)
}
