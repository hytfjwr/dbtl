use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct GenArgs {
    /// dbt project directory, relative to the repository root when not
    /// absolute (e.g. tests/fixtures/logistics_project).
    #[arg(long)]
    project: PathBuf,
    /// Write the manifest here. Defaults to
    /// tests/fixtures/<project>_manifest.json for a `foo_project` dir.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Profile name written to the throwaway profiles.yml. Defaults to the
    /// `profile:` key in the project's dbt_project.yml.
    #[arg(long)]
    profile: Option<String>,
    /// Pin the dbt-core release used for parsing (e.g. "1.12.6"). By
    /// default uvx resolves the latest release.
    #[arg(long)]
    dbt_version: Option<String>,
    /// Inline YAML/JSON var map passed to `dbt compile --vars` verbatim —
    /// the manifest then records the same rendered values `dbtl --vars`
    /// produces from source.
    #[arg(long, value_name = "YAML|JSON")]
    vars: Option<String>,
}

pub fn run(args: GenArgs) -> Result<()> {
    let root = repo_root();
    let (project_dir, out) = resolve_args(&root, args.project, args.out);
    gen(
        &project_dir,
        &out,
        args.profile.as_deref(),
        args.dbt_version.as_deref(),
        args.vars.as_deref(),
    )
}

/// The resolved (project dir, output manifest) pair — `out` falls back to
/// the `<project>_manifest.json` convention when not passed.
fn resolve_args(root: &Path, project: PathBuf, out: Option<PathBuf>) -> (PathBuf, PathBuf) {
    let project_dir = resolve(root, project);
    let out = out
        .map(|p| resolve(root, p))
        .unwrap_or_else(|| default_out(root, &project_dir));
    (project_dir, out)
}

/// Run `dbt deps` (local packages only) then `dbt compile` under a
/// throwaway duckdb profile, normalize the per-run fields, and write the
/// manifest to `out`.
pub fn gen(
    project_dir: &Path,
    out: &Path,
    profile: Option<&str>,
    dbt_version: Option<&str>,
    vars: Option<&str>,
) -> Result<()> {
    check_uvx()?;
    if !project_dir.join("dbt_project.yml").is_file() {
        bail!("{} has no dbt_project.yml", project_dir.display());
    }
    let profile = match profile {
        Some(profile) => profile.to_string(),
        None => read_project_profile(project_dir)?,
    };
    reject_remote_packages(project_dir)?;

    let tmp = tempfile::tempdir().context("create a temporary directory")?;
    let profiles_yml = tmp.path().join("profiles.yml");
    fs::write(
        &profiles_yml,
        format!(
            "{profile}:\n  target: dev\n  outputs:\n    dev:\n      type: duckdb\n      path: {}\n",
            tmp.path().join(format!("dbtl_{profile}.duckdb")).display()
        ),
    )
    .context("write dbt profiles.yml")?;

    let result = (|| {
        if project_dir.join("packages.yml").is_file() {
            run_dbt(project_dir, tmp.path(), dbt_version, "deps", None)?;
        }
        // `dbt compile` (not `dbt parse`): only compile puts `compiled_code`
        // in the manifest, which the fixture carries and consumers prefer.
        run_dbt(project_dir, tmp.path(), dbt_version, "compile", vars)?;
        write_manifest(project_dir, out)
    })();
    cleanup(project_dir);
    result
}

/// The repository root: `xtask/` lives directly under it, and
/// `CARGO_MANIFEST_DIR` is fixed at build time so this works from any cwd.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is at the repository root")
        .to_path_buf()
}

/// A path argument resolves against the repository root when relative, so
/// the task behaves the same from any working directory.
fn resolve(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

/// `foo_project/` pairs with `foo_manifest.json` — the convention every
/// generated fixture manifest follows.
fn default_out(root: &Path, project_dir: &Path) -> PathBuf {
    let stem = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix("_project"))
        .unwrap_or("manifest");
    root.join(format!("tests/fixtures/{stem}_manifest.json"))
}

fn check_uvx() -> Result<()> {
    match Command::new("uvx").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        _ => bail!(
            "`uvx` was not found on PATH. Install uv to regenerate a manifest \
             (see https://docs.astral.sh/uv/ for installation instructions)"
        ),
    }
}

/// The profile name lives in the project's own `dbt_project.yml`; reading
/// it keeps the generated profiles.yml consistent without a flag.
fn read_project_profile(project_dir: &Path) -> Result<String> {
    let path = project_dir.join("dbt_project.yml");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("profile:") {
            let profile = rest.trim().trim_matches('"').trim_matches('\'');
            if !profile.is_empty() {
                return Ok(profile.to_string());
            }
        }
    }
    bail!(
        "{} has no top-level `profile:` key — pass --profile",
        path.display()
    )
}

/// Fixtures must be fully reproducible offline: a packages.yml may only
/// list `local:` entries — hub, git, tarball, and private sources would
/// need network access the fixture build refuses to take.
fn reject_remote_packages(project_dir: &Path) -> Result<()> {
    let packages = project_dir.join("packages.yml");
    let Ok(text) = fs::read_to_string(&packages) else {
        return Ok(());
    };
    for line in text.lines() {
        let entry = line.trim_start().trim_start_matches('-').trim_start();
        for key in ["package:", "git:", "tarball:", "private:"] {
            if entry.starts_with(key) {
                bail!(
                    "{} declares a remote package ({key}…); fixture manifests only support local packages",
                    packages.display()
                );
            }
        }
    }
    Ok(())
}

/// The `uvx` invocation for a dbt subcommand — built without spawning so
/// the `--dbt-version` pin (`--from dbt-core==X.Y.Z`) and `--vars`
/// forwarding are unit-testable.
fn dbt_command(
    profiles_dir: &Path,
    dbt_version: Option<&str>,
    subcmd: &str,
    vars: Option<&str>,
) -> Command {
    let mut cmd = Command::new("uvx");
    cmd.arg("--with")
        .arg("dbt-duckdb")
        .arg("--from")
        .arg(match dbt_version {
            Some(version) => format!("dbt-core=={version}"),
            None => "dbt-core".to_string(),
        })
        .arg("dbt")
        .arg(subcmd)
        .args(["--profiles-dir"])
        .arg(profiles_dir)
        .args(["--project-dir", "."]);
    if let Some(vars) = vars {
        cmd.arg("--vars").arg(vars);
    }
    cmd
}

fn run_dbt(
    project_dir: &Path,
    profiles_dir: &Path,
    dbt_version: Option<&str>,
    subcmd: &str,
    vars: Option<&str>,
) -> Result<()> {
    let mut cmd = dbt_command(profiles_dir, dbt_version, subcmd, vars);
    cmd.current_dir(project_dir)
        // dbt copies this into metadata.send_anonymous_usage_stats; pinning it
        // keeps the manifest identical no matter what the caller's shell
        // exports (CI's dbt-compat job sets the same value).
        .env("DBT_SEND_ANONYMOUS_USAGE_STATS", "false");
    let status = cmd
        .status()
        .with_context(|| format!("run uvx dbt {subcmd}"))?;
    if !status.success() {
        bail!("dbt {subcmd} failed: uvx exited with {status}");
    }
    Ok(())
}

/// Pin every per-run value so regenerating the same project and dbt
/// version is byte-identical: the five volatile `metadata` fields go null
/// and every resource's `created_at` stamp becomes 0. (`project_id` stays —
/// it is a deterministic hash of the project name, not a per-run value.)
fn write_manifest(project_dir: &Path, out: &Path) -> Result<()> {
    let generated = project_dir.join("target/manifest.json");
    let text =
        fs::read_to_string(&generated).with_context(|| format!("read {}", generated.display()))?;
    let mut manifest: serde_json::Value =
        serde_json::from_str(&text).context("parse the generated manifest")?;
    if let Some(metadata) = manifest.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        for key in [
            "generated_at",
            "invocation_id",
            "invocation_started_at",
            "run_started_at",
            "user_id",
        ] {
            metadata.insert(key.to_string(), serde_json::Value::Null);
        }
    }
    zero_created_at(&mut manifest);
    sort_keys(&mut manifest);
    fs::write(out, serde_json::to_string(&manifest)? + "\n")
        .with_context(|| format!("write {}", out.display()))?;
    eprintln!("wrote {}", out.display());
    Ok(())
}

fn zero_created_at(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if matches!(map.get("created_at"), Some(serde_json::Value::Number(_))) {
                map.insert("created_at".to_string(), serde_json::Value::from(0));
            }
            map.values_mut().for_each(zero_created_at);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(zero_created_at),
        _ => {}
    }
}

/// dbt emits object keys in an order that varies run to run; a canonical
/// sort is what makes two generations of the same manifest byte-identical.
/// The one exception is `columns`: it is a name → column map whose order IS
/// the schema declaration order (the lint findings print it, source mode
/// preserves it), so only its nested entries are canonicalized.
///
/// The same instability hits the set-derived lists `depends_on.nodes`,
/// `depends_on.macros`, and `sources` (a `ref()`/`source()` pair list) —
/// their order carries no meaning, so they are sorted too.
fn sort_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.sort_keys();
            if let Some(serde_json::Value::Object(deps)) = map.get_mut("depends_on") {
                for key in ["nodes", "macros"] {
                    if let Some(serde_json::Value::Array(items)) = deps.get_mut(key) {
                        items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                    }
                }
            }
            if let Some(serde_json::Value::Array(sources)) = map.get_mut("sources") {
                sources.sort_by_key(|s| s.to_string());
            }
            for (key, nested) in map.iter_mut() {
                if key == "columns" {
                    if let serde_json::Value::Object(cols) = nested {
                        cols.values_mut().for_each(sort_keys);
                    }
                } else {
                    sort_keys(nested);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(sort_keys),
        _ => {}
    }
}

/// dbt run litter — removed on success and failure alike so the fixture
/// directory is left exactly as it was found.
fn cleanup(project_dir: &Path) {
    for name in [
        "target",
        "logs",
        "dbt_packages",
        ".user.yml",
        "package-lock.yml",
    ] {
        let path = project_dir.join(name);
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// `clap::Args` types can't parse standalone — a flatten wrapper gives
    /// them a `Parser` front so tests exercise the real flag definitions.
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: GenArgs,
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    // ---- flag parsing --------------------------------------------------------

    #[test]
    fn gen_args_parse_every_flag() {
        let cli = TestCli::try_parse_from([
            "t",
            "--project",
            "tests/fixtures/foo_project",
            "--out",
            "out/m.json",
            "--profile",
            "mine",
            "--dbt-version",
            "1.12.6",
            "--vars",
            "{env: prod}",
        ])
        .expect("parse");
        assert_eq!(
            cli.args.project,
            PathBuf::from("tests/fixtures/foo_project")
        );
        assert_eq!(cli.args.out, Some(PathBuf::from("out/m.json")));
        assert_eq!(cli.args.profile.as_deref(), Some("mine"));
        assert_eq!(cli.args.dbt_version.as_deref(), Some("1.12.6"));
        assert_eq!(cli.args.vars.as_deref(), Some("{env: prod}"));
    }

    #[test]
    fn gen_args_optional_flags_default_to_none() {
        let cli = TestCli::try_parse_from(["t", "--project", "p"]).expect("parse");
        assert_eq!(cli.args.out, None);
        assert_eq!(cli.args.profile, None);
        assert_eq!(cli.args.dbt_version, None);
        assert_eq!(cli.args.vars, None);
    }

    // ---- output path decisions -------------------------------------------------

    #[test]
    fn resolve_anchors_relative_paths_at_the_root() {
        let root = Path::new("/repo");
        assert_eq!(
            resolve(root, PathBuf::from("tests/fixtures/p")),
            PathBuf::from("/repo/tests/fixtures/p")
        );
        assert_eq!(
            resolve(root, PathBuf::from("/abs/p")),
            PathBuf::from("/abs/p"),
            "absolute paths pass through untouched"
        );
    }

    #[test]
    fn default_out_follows_the_project_manifest_convention() {
        let root = Path::new("/repo");
        assert_eq!(
            default_out(root, Path::new("/repo/tests/fixtures/logistics_project")),
            PathBuf::from("/repo/tests/fixtures/logistics_manifest.json")
        );
        // A dir that does not end in _project falls back to `manifest`.
        assert_eq!(
            default_out(root, Path::new("/repo/tests/fixtures/misc")),
            PathBuf::from("/repo/tests/fixtures/manifest_manifest.json")
        );
    }

    #[test]
    fn resolve_args_prefers_an_explicit_out() {
        let root = Path::new("/repo");
        let (project_dir, out) = resolve_args(
            root,
            PathBuf::from("p/foo_project"),
            Some(PathBuf::from("o/x.json")),
        );
        assert_eq!(project_dir, PathBuf::from("/repo/p/foo_project"));
        assert_eq!(out, PathBuf::from("/repo/o/x.json"));
        let (_, out) = resolve_args(root, PathBuf::from("p/foo_project"), None);
        assert_eq!(out, PathBuf::from("/repo/tests/fixtures/foo_manifest.json"));
    }

    // ---- --dbt-version / --vars forwarding ----------------------------------------

    #[test]
    fn dbt_command_pins_the_requested_dbt_core() {
        let cmd = dbt_command(Path::new("/profiles"), Some("1.12.6"), "compile", None);
        assert_eq!(cmd.get_program(), "uvx");
        let args = argv(&cmd);
        assert_eq!(
            &args[..5],
            ["--with", "dbt-duckdb", "--from", "dbt-core==1.12.6", "dbt"]
        );
        assert_eq!(
            &args[5..],
            [
                "compile",
                "--profiles-dir",
                "/profiles",
                "--project-dir",
                "."
            ]
        );
    }

    #[test]
    fn dbt_command_defaults_to_the_latest_dbt_core() {
        let cmd = dbt_command(Path::new("/p"), None, "deps", None);
        let args = argv(&cmd);
        assert!(args.contains(&"dbt-core".to_string()), "{args:?}");
        assert!(
            !args.iter().any(|a| a.starts_with("dbt-core==")),
            "no pin without --dbt-version: {args:?}"
        );
    }

    #[test]
    fn dbt_command_forwards_vars_verbatim() {
        let cmd = dbt_command(Path::new("/p"), None, "compile", Some("{env: prod}"));
        let args = argv(&cmd);
        let pos = args.iter().position(|a| a == "--vars").expect("--vars");
        assert_eq!(args[pos + 1], "{env: prod}");
        // --vars is a flag+value pair that must come last — nothing is appended after it.
        assert_eq!(pos + 2, args.len());
        // Without vars the flag is absent entirely (dbt would parse "" too).
        assert!(!argv(&dbt_command(Path::new("/p"), None, "compile", None))
            .contains(&"--vars".to_string()));
    }

    // ---- project/profile/package gates (filesystem, no external commands) --------

    #[test]
    fn read_project_profile_parses_bare_and_quoted_names() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |body: &str| fs::write(tmp.path().join("dbt_project.yml"), body).unwrap();
        write("name: p\nprofile: my_profile\n");
        assert_eq!(read_project_profile(tmp.path()).unwrap(), "my_profile");
        write("profile: 'quoted'\n");
        assert_eq!(read_project_profile(tmp.path()).unwrap(), "quoted");
        write("name: p\n");
        let err = read_project_profile(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("no top-level `profile:` key"), "{err}");
        // An empty value does not count as a profile.
        write("profile: \n");
        assert!(read_project_profile(tmp.path()).is_err());
    }

    #[test]
    fn reject_remote_packages_passes_without_a_packages_yml() {
        let tmp = tempfile::tempdir().unwrap();
        reject_remote_packages(tmp.path()).unwrap();
        fs::write(
            tmp.path().join("packages.yml"),
            "packages:\n  - local: ../sibling\n",
        )
        .unwrap();
        reject_remote_packages(tmp.path()).unwrap();
    }

    #[test]
    fn reject_remote_packages_names_each_remote_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let packages = tmp.path().join("packages.yml");
        for (body, needle) in [
            ("packages:\n  - git: https://x/y.git\n", "git:"),
            ("packages:\n    - tarball: https://x/y.tgz\n", "tarball:"),
            ("packages:\n  - private: org/repo\n", "private:"),
            ("package: hub/package\n", "package:"),
        ] {
            fs::write(&packages, body).unwrap();
            let err = reject_remote_packages(tmp.path()).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
            assert!(err.contains("local packages"), "{err}");
        }
    }

    #[test]
    fn cleanup_removes_dbt_litter_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("target")).unwrap();
        fs::create_dir_all(tmp.path().join("dbt_packages")).unwrap();
        fs::write(tmp.path().join("logs"), "x").unwrap();
        fs::write(tmp.path().join(".user.yml"), "x").unwrap();
        fs::write(tmp.path().join("package-lock.yml"), "x").unwrap();
        fs::write(tmp.path().join("keep.sql"), "select 1").unwrap();
        cleanup(tmp.path());
        for name in [
            "target",
            "logs",
            "dbt_packages",
            ".user.yml",
            "package-lock.yml",
        ] {
            assert!(!tmp.path().join(name).exists(), "{name} removed");
        }
        assert!(tmp.path().join("keep.sql").exists(), "project files stay");
    }

    // ---- the manifest normalization ---------------------------------------------

    /// write_manifest's determinism contract: volatile metadata nulled,
    /// every `created_at` zeroed, keys canonical-sorted, `columns`' order
    /// (which IS the schema declaration order) preserved, and the
    /// set-derived `depends_on`/`sources` lists sorted.
    #[test]
    fn write_manifest_normalizes_the_per_run_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("p");
        fs::create_dir_all(project.join("target")).unwrap();
        let generated = r#"{
            "z_last_key": {"created_at": 1},
            "metadata": {
                "generated_at": "2024-01-01T00:00:00Z",
                "invocation_id": "abc",
                "invocation_started_at": "2024-01-01T00:00:00Z",
                "run_started_at": "2024-01-01T00:00:00Z",
                "user_id": "u",
                "project_id": "keep-me",
                "adapter_type": "duckdb"
            },
            "a_first_key": {
                "created_at": 123,
                "created_at_text": "created_at",
                "depends_on": {"nodes": ["z", "a"], "macros": ["m2", "m1"], "other": 1},
                "sources": [{"b": 1}, {"a": 1}],
                "columns": {"z_col": {"created_at": 5}, "a_col": {"created_at": 5}}
            }
        }"#;
        fs::write(project.join("target/manifest.json"), generated).unwrap();
        let out = tmp.path().join("out.json");
        write_manifest(&project, &out).unwrap();

        let text = fs::read_to_string(&out).unwrap();
        assert!(text.ends_with('\n'));
        let m: serde_json::Value = serde_json::from_str(&text).unwrap();
        // Top-level keys canonical-sorted; nested maps too.
        let keys: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["a_first_key", "metadata", "z_last_key"]);
        for key in [
            "generated_at",
            "invocation_id",
            "invocation_started_at",
            "run_started_at",
            "user_id",
        ] {
            assert_eq!(m["metadata"][key], serde_json::Value::Null, "{key} nulled");
        }
        assert_eq!(m["metadata"]["project_id"], "keep-me");
        assert_eq!(m["metadata"]["adapter_type"], "duckdb");
        // Every created_at stamp zeroed, at any depth — and ONLY the
        // numeric ones.
        assert_eq!(m["a_first_key"]["created_at"], 0);
        assert_eq!(m["a_first_key"]["created_at_text"], "created_at");
        assert_eq!(m["z_last_key"]["created_at"], 0);
        // Set-derived lists canonicalized; other maps untouched.
        assert_eq!(
            m["a_first_key"]["depends_on"]["nodes"],
            serde_json::json!(["a", "z"])
        );
        assert_eq!(
            m["a_first_key"]["depends_on"]["macros"],
            serde_json::json!(["m1", "m2"])
        );
        assert_eq!(
            m["a_first_key"]["sources"],
            serde_json::json!([{"a": 1}, {"b": 1}])
        );
        // `columns` order is the schema declaration order — z_col stays first.
        let cols: Vec<&str> = m["a_first_key"]["columns"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(cols, ["z_col", "a_col"], "columns order preserved");
        // But the zeroing/sorting inside each column still applies.
        assert_eq!(m["a_first_key"]["columns"]["z_col"]["created_at"], 0);
    }
}
