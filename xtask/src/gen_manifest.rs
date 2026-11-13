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
}

pub fn run(args: GenArgs) -> Result<()> {
    let root = repo_root();
    let project_dir = resolve(&root, args.project);
    let out = args
        .out
        .map(|p| resolve(&root, p))
        .unwrap_or_else(|| default_out(&root, &project_dir));
    gen(
        &project_dir,
        &out,
        args.profile.as_deref(),
        args.dbt_version.as_deref(),
    )
}

/// Run `dbt deps` (local packages only) then `dbt compile` under a
/// throwaway duckdb profile, normalize the per-run fields, and write the
/// manifest to `out`.
pub fn gen(
    project_dir: &Path,
    out: &Path,
    profile: Option<&str>,
    dbt_version: Option<&str>,
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
            run_dbt(project_dir, tmp.path(), dbt_version, "deps")?;
        }
        // `dbt compile` (not `dbt parse`): only compile puts `compiled_code`
        // in the manifest, which the fixture carries and consumers prefer.
        run_dbt(project_dir, tmp.path(), dbt_version, "compile")?;
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

fn run_dbt(
    project_dir: &Path,
    profiles_dir: &Path,
    dbt_version: Option<&str>,
    subcmd: &str,
) -> Result<()> {
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
        .args(["--project-dir", "."])
        .current_dir(project_dir);
    let status = cmd
        .status()
        .with_context(|| format!("run uvx dbt {subcmd}"))?;
    if !status.success() {
        bail!("dbt {subcmd} failed: uvx exited with {status}");
    }
    Ok(())
}

/// Pin every per-run value so regenerating the same project and dbt
/// version is byte-identical: the six volatile `metadata` fields go null
/// and every resource's `created_at` stamp becomes 0.
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
            "project_id",
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
fn sort_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.sort_keys();
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
