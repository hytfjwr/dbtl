use super::*;

/// Resolve the `--diff` baseline into a loaded `Dag` + a display label: a
/// manifest.json FILE directly, or a dbt project DIR through the same
/// auto-detect `build_app` uses (compiled `target/manifest.json` first, else
/// source parse) — so "diff against that other worktree" just works. The same
/// package `scope` as the current graph is applied, so both sides of the diff
/// agree on the node set (a baseline manifest carrying installed packages is
/// pruned the same way). Every failure is an input error — a named baseline
/// that could not be read.
pub fn load_baseline(
    path: &str,
    scope: dbtl::PackageScope,
) -> Result<(dbtl::Dag, String), CliError> {
    let p = PathBuf::from(path);
    if p.is_file() {
        let (dag, warnings) = load_dag_with_warnings(&p)
            .with_context(|| format!("--diff: failed to load {path}"))
            .input()?;
        for w in &warnings {
            eprintln!("warning (--diff baseline): {w}");
        }
        return Ok((apply_package_scope(dag, scope, false).0, path.to_string()));
    }
    if p.is_dir() {
        let manifest = p.join("target").join("manifest.json");
        if manifest.is_file() {
            let (dag, warnings) = load_dag_with_warnings(&manifest)
                .with_context(|| format!("--diff: failed to load {}", manifest.display()))
                .input()?;
            for w in &warnings {
                eprintln!("warning (--diff baseline): {w}");
            }
            return Ok((
                apply_package_scope(dag, scope, false).0,
                manifest.display().to_string(),
            ));
        }
        if p.join("dbt_project.yml").is_file() {
            let (dag, warnings) = load_dag_from_source_with_warnings(&p)
                .with_context(|| format!("--diff: failed to parse project {path}"))
                .input()?;
            for w in &warnings {
                eprintln!("warning (--diff baseline): {w}");
            }
            return Ok((apply_package_scope(dag, scope, true).0, path.to_string()));
        }
    }
    Err(CliError::input(anyhow::anyhow!(
        "--diff: {path} is neither a manifest.json file nor a dbt project dir \
         (no target/manifest.json or dbt_project.yml found)"
    )))
}
