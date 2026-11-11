use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Pin the dbt-core release used for parsing (e.g. "1.10.0"). By default
    /// uvx resolves the latest release, matching the previous manual recipe.
    #[arg(long)]
    dbt_version: Option<String>,
    /// Write the manifest here instead of tests/fixtures/sample_manifest.json.
    #[arg(long)]
    out: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let root = repo_root();
    let project_dir = root.join("tests/fixtures/sample_project");
    let out = args
        .out
        .unwrap_or_else(|| root.join("tests/fixtures/sample_manifest.json"));

    check_uvx()?;

    let tmp = tempfile::tempdir().context("create a temporary directory")?;
    let profiles_yml = tmp.path().join("profiles.yml");
    fs::write(
        &profiles_yml,
        format!(
            "sample:\n  target: dev\n  outputs:\n    dev:\n      type: duckdb\n      path: {}\n",
            tmp.path().join("dbtl_sample.duckdb").display()
        ),
    )
    .context("write dbt profiles.yml")?;

    let mut cmd = Command::new("uvx");
    cmd.arg("--with")
        .arg("dbt-duckdb")
        .arg("--from")
        .arg(match &args.dbt_version {
            Some(version) => format!("dbt-core=={version}"),
            None => "dbt-core".to_string(),
        })
        // `dbt compile` (not `dbt parse`): only compile puts `compiled_code`
        // in the manifest, which the fixture carries and consumers prefer.
        .args(["dbt", "compile", "--profiles-dir"])
        .arg(tmp.path())
        .args(["--project-dir", "."])
        .current_dir(&project_dir);
    let status = cmd.status().context("run uvx dbt compile")?;

    let result = if status.success() {
        write_manifest(&project_dir, &out)
    } else {
        Err(anyhow::anyhow!(
            "dbt compile failed: uvx exited with {status}"
        ))
    };
    cleanup(&project_dir);
    result
}

/// The repository root: `xtask/` lives directly under it, and
/// `CARGO_MANIFEST_DIR` is fixed at build time so this works from any cwd.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is at the repository root")
        .to_path_buf()
}

fn check_uvx() -> Result<()> {
    match Command::new("uvx").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        _ => bail!(
            "`uvx` was not found on PATH. Install uv to regenerate the sample manifest \
             (see https://docs.astral.sh/uv/ for installation instructions)"
        ),
    }
}

/// Strip `metadata.user_id` (it embeds a per-run UUID) and write compact JSON.
fn write_manifest(project_dir: &Path, out: &Path) -> Result<()> {
    let generated = project_dir.join("target/manifest.json");
    let text =
        fs::read_to_string(&generated).with_context(|| format!("read {}", generated.display()))?;
    let mut manifest: serde_json::Value =
        serde_json::from_str(&text).context("parse the generated manifest")?;
    if let Some(metadata) = manifest.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        metadata.insert("user_id".to_string(), serde_json::Value::Null);
    }
    fs::write(out, serde_json::to_string(&manifest)? + "\n")
        .with_context(|| format!("write {}", out.display()))?;
    eprintln!("wrote {}", out.display());
    Ok(())
}

/// dbt run litter — removed on success and failure alike so the fixture
/// directory is left exactly as it was found.
fn cleanup(project_dir: &Path) {
    for name in ["target", "logs", ".user.yml"] {
        let path = project_dir.join(name);
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}
