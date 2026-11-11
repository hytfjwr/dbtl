use super::*;

/// Resolve the `--diff` baseline into a loaded `Dag` + a display label:
///
/// - `git:<rev>` — the project's tracked files at that git revision,
///   materialized into a temp dir by [`git_baseline`] and parsed from
///   source. `project_dir` is the directory git resolves the
///   repository-relative prefix from (the current `--source`, else
///   `--project`).
/// - a manifest.json FILE directly, or a dbt project DIR through the same
///   auto-detect `build_app` uses (compiled `target/manifest.json` first,
///   else source parse) — so "diff against that other worktree" just works.
///
/// The same package `scope` as the current graph is applied, so both sides
/// of the diff agree on the node set (a baseline manifest carrying
/// installed packages is pruned the same way). Every failure is an input
/// error — a named baseline that could not be read — except an obviously
/// invalid `<rev>` (a `-`-leading value would reach git as a flag), which
/// is a usage error.
pub fn load_baseline(
    path: &str,
    scope: dbtl::PackageScope,
    project_dir: &std::path::Path,
) -> Result<(dbtl::Dag, String), CliError> {
    if let Some(rev) = path.strip_prefix("git:") {
        return git_baseline::load(project_dir, rev, scope);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// `git -C <dir>` with the same sealed config the integration suite
    /// uses: fixed identity, no signing, no autocrlf, and global/system
    /// config sealed off — a developer machine must not leak into the
    /// commits.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "dbtl tests")
            .env("GIT_AUTHOR_EMAIL", "dbtl-tests@example.invalid")
            .env("GIT_COMMITTER_NAME", "dbtl tests")
            .env("GIT_COMMITTER_EMAIL", "dbtl-tests@example.invalid")
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
            .env("GIT_CONFIG_VALUE_0", "false")
            .env("GIT_CONFIG_KEY_1", "core.autocrlf")
            .env("GIT_CONFIG_VALUE_1", "false")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(dir: &std::path::Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }

    /// `load_baseline("git:<rev>")` — the single entry `dbtl check`,
    /// `dbtl impact`, `dbtl lint`, and the TUI's `--diff` all dispatch
    /// through — materializes the revision's project files and parses
    /// them from source. The committed baseline carries a model the
    /// working tree no longer has, so the difference is visible in the
    /// loaded Dag.
    #[test]
    fn git_baseline_loads_the_revision_from_source() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        write(
            &repo,
            "dbt_project.yml",
            "name: p\nversion: '1.0'\nprofile: p\n",
        );
        write(&repo, "models/old.sql", "select 1 as id\n");
        git(&repo, &["add", "-A", "."]);
        git(&repo, &["commit", "--quiet", "-m", "base"]);
        // The working tree gains a second model the baseline lacks.
        write(&repo, "models/new.sql", "select 2 as id\n");

        let (base, label) = load_baseline("git:HEAD", dbtl::PackageScope::Root, &repo)
            .expect("the git baseline loads");
        assert_eq!(label, "git:HEAD");
        assert!(
            base.get("model.p.old").is_some(),
            "the committed model is in the baseline"
        );
        assert!(
            base.get("model.p.new").is_none(),
            "the uncommitted file never reached the baseline"
        );

        // A `-`-leading rev is a usage error before git sees it.
        let err = load_baseline("git:-p", dbtl::PackageScope::Root, &repo)
            .expect_err("a flag-shaped rev is rejected");
        assert_eq!(err.kind(), Some(super::exit::ExitKind::Usage));
    }
}
