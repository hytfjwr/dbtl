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
    /// `--run-results` — an explicit run_results.json (TUI + `docs`).
    pub run_results: Option<&'a str>,
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
            run_results: cli.run_results.as_deref(),
            scope: cli.package_scope(),
            sql_dialect: cli.sql_dialect,
        }
    }

    /// Load the graph through the shared [`resolve_source`] rule.
    pub fn resolve(&self) -> Result<ResolvedSource, CliError> {
        resolve_source(
            self.manifest,
            self.source,
            self.project,
            self.scope,
            self.diff,
        )
    }

    /// The directory `git rev-parse --show-prefix` runs in for a `--diff
    /// git:<rev>` baseline — `--source` when given, else `--project` (the
    /// current-side project the revision is read back against).
    pub fn project_dir(&self) -> PathBuf {
        self.source
            .as_deref()
            .map_or_else(|| PathBuf::from(self.project), PathBuf::from)
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

/// Which input [`resolve_source`] would load, without loading it — shared
/// with the `.dbtl.yml` probe (`lint --list-rules`/`--explain` decide where
/// the config file lives from the path decision alone, never the data).
pub(super) enum SourcePath {
    /// A manifest.json to load directly.
    Manifest(PathBuf),
    /// A project dir to parse from source.
    Project(PathBuf),
}

/// Whether `--diff` names a git revision baseline (`git:<rev>`).
pub(super) fn is_git_diff(diff: Option<&str>) -> bool {
    diff.is_some_and(|d| d.starts_with("git:"))
}

/// The source-selection half of [`resolve_source`]: explicit `--manifest` /
/// `--source` win, else auto-detect under `project` — `target/manifest.json`
/// (full fidelity) first, then `dbt_project.yml` (source parse). `None` =
/// no data source under `project`.
///
/// `diff` only matters for `git:<rev>`: a git baseline parses the revision's
/// project from SOURCE, so the current side resolves symmetrically — the
/// `target/manifest.json` auto-detect is skipped (a manifest-vs-source
/// comparison would report loader differences as changes). An explicit
/// `--manifest` still wins — [`resolve_source`] warns that the sides then
/// load through different loaders (the warning lives there, not here:
/// this function is also the `.dbtl.yml` probe's pure path oracle, and a
/// pure decision must not print).
pub(super) fn source_path(
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    diff: Option<&str>,
) -> Option<SourcePath> {
    let git_diff = is_git_diff(diff);
    if let Some(path) = manifest {
        return Some(SourcePath::Manifest(PathBuf::from(path)));
    }
    if let Some(dir) = source {
        return Some(SourcePath::Project(PathBuf::from(dir)));
    }
    let project = PathBuf::from(project);
    // `<project>/target` resolves through the shared sibling-artifact rule
    // (`dbtl::default_target_dir`) — the same root the run_results / catalog
    // / compiled side maps read.
    let compiled = if git_diff {
        None
    } else {
        dbtl::default_target_dir(None, Some(&project))
            .map(|target| target.join("manifest.json"))
            .filter(|m| m.is_file())
    };
    if let Some(manifest) = compiled {
        return Some(SourcePath::Manifest(manifest));
    }
    if project.join("dbt_project.yml").is_file() {
        return Some(SourcePath::Project(project));
    }
    None
}

/// The shared data-source resolution, identical for the TUI and the
/// subcommands. Explicit `manifest` / `source` win; otherwise auto-detect
/// under `project` per [`source_path`]. The TUI ([`build_app`]) and the
/// subcommands all go through here so the precedence can never diverge.
/// Every failure here is an input error — a named file/dir that could not
/// be read.
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
    diff: Option<&str>,
) -> Result<ResolvedSource, CliError> {
    // An explicit --manifest overrides the symmetric-source rule — say so:
    // manifest-vs-source loader differences can read as changes.
    if manifest.is_some() && is_git_diff(diff) {
        eprintln!(
            "warning: --diff git: reads the baseline from project source; \
             an explicit --manifest current side may report loader \
             differences as changes"
        );
    }
    let Some(path) = source_path(manifest, source, project, diff) else {
        // A git baseline compares against project source — a compiled
        // manifest can sit next to it without being read, so the missing
        // thing to report is `dbt_project.yml`, not "no data source".
        if is_git_diff(diff) {
            return Err(CliError::input(anyhow::anyhow!(
                "no dbt project under {}: --diff git:<rev> compares the \
                 revision's project files parsed from source, so the current \
                 side needs dbt_project.yml too (or pass --source <dir> / \
                 --manifest <file>)",
                project
            )));
        }
        return Err(CliError::input(anyhow::anyhow!(
            "no data source under {}: neither target/manifest.json nor dbt_project.yml found \
             (pass --manifest <file> or --source <dir>)",
            project
        )));
    };
    let (dag, mut warnings, label, is_source) = match &path {
        SourcePath::Manifest(path) => {
            let (dag, warnings) = load_dag_with_warnings(path).input()?;
            (dag, warnings, path.display().to_string(), false)
        }
        SourcePath::Project(dir) => {
            let (dag, warnings) = load_dag_from_source_with_warnings(dir).input()?;
            (dag, warnings, dir.display().to_string(), true)
        }
    };
    // The auto-detected manifest gets a staleness probe — an explicit
    // `--manifest`/`--source` already named the data source deliberately,
    // but the silent `<project>/target/manifest.json` pick can hide
    // post-parse edits behind a stale graph. `manifest.is_none()` here IS
    // the auto-detect case (`--diff git:` never picks a manifest).
    if manifest.is_none() {
        if let SourcePath::Manifest(path) = &path {
            if let Some(w) = stale_manifest_warning(path, &PathBuf::from(project)) {
                warnings.push(w);
            }
        }
    }
    warn_each("", &warnings);
    let (dag, scope_w) = apply_package_scope(dag, scope, is_source);
    warnings.extend(scope_w);
    Ok(ResolvedSource {
        dag,
        label,
        is_source,
        warnings,
    })
}

/// The "auto-detected manifest may be stale" warning — `Some(message)` when
/// project data files are strictly newer than the manifest dbt last wrote.
/// The message names the count, the newest file, and the two ways out
/// (`dbt parse` to rebuild, `--source` for a parse-free load). `None` on a
/// fresh manifest, an unreadable mtime, or zero newer files — an
/// unprovable staleness is never a warning, the same rule
/// `compiled::is_stale` follows. The scan itself is [`fs_stamp`]'s shared
/// walk minus `target/` (a generated artifact can't make its own
/// generator stale).
///
/// [`fs_stamp`]: crate::watch::fs_stamp
fn stale_manifest_warning(manifest: &std::path::Path, project: &std::path::Path) -> Option<String> {
    let manifest_mtime = std::fs::metadata(manifest)
        .and_then(|m| m.modified())
        .ok()?;
    let (count, newest) = crate::watch::project_files_newer_than(project, manifest_mtime);
    if count == 0 {
        return None;
    }
    // Project-relative paths when possible — the message reads
    // `target/manifest.json … models/x.sql`, not absolute noise.
    let rel = |p: &std::path::Path| p.strip_prefix(project).unwrap_or(p).display().to_string();
    let newest = newest.map(|p| rel(&p)).unwrap_or_default();
    Some(format!(
        "{} is older than {} project file{} (newest: {}); \
         run `dbt parse` to rebuild it, or pass `--source`",
        rel(manifest),
        count,
        if count == 1 { "" } else { "s" },
        newest,
    ))
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
    warn_each(" (scope)", &warnings);
    for w in &warnings {
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
    let resolved = resolve_source(
        &cli.manifest,
        &cli.source,
        &cli.project,
        scope,
        cli.diff.as_deref(),
    )?;
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
