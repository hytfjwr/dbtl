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
    /// them to the App's notification log as unread WARNING entries (Issue
    /// 08) so they stay visible after the TUI opens; non-TUI callers ignore
    /// them — stderr output is unchanged.
    pub warnings: Vec<String>,
}

/// The shared data-source resolution, identical for the TUI and the `docs`
/// subcommand. Explicit `manifest` / `source` win; otherwise auto-detect under
/// `project`: a compiled `target/manifest.json` if present (full fidelity), else
/// parse the project from source. The TUI ([`build_app`]) and docs both go
/// through here so the precedence can never diverge.
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
) -> Result<ResolvedSource> {
    if let Some(path) = manifest {
        let (dag, mut warnings) = load_dag_with_warnings(path)?;
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
        let (dag, mut warnings) = load_dag_from_source_with_warnings(dir)?;
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
        let (dag, mut warnings) = load_dag_with_warnings(&manifest)?;
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
        let (dag, mut warnings) = load_dag_from_source_with_warnings(&project)?;
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
        anyhow::bail!(
            "no data source under {}: neither target/manifest.json nor dbt_project.yml found \
             (pass --manifest <file> or --source <dir>)",
            project.display()
        )
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
pub fn build_app(cli: &Cli) -> Result<App> {
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
    // An explicit --run-results overrides the auto-detected sibling; its load
    // error IS user-facing (unlike the silent auto-detect).
    if let Some(rr) = &cli.run_results {
        app.set_run_results(PathBuf::from(rr))?;
    }
    Ok(app)
}
