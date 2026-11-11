//! `cargo xtask gen-docs [--check] [--locked]` — regenerate the reference
//! docs under `docs/reference/` and the JSON Schemas under `schema/`.
//! `--check` writes nothing: it compares the generated set against the
//! committed files and fails with a per-file diff when they diverge.
//! `--locked` is forwarded to the `cargo run` that renders the CLI doc.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use xtask::gen_docs::{check, generate, write};

#[derive(ClapArgs)]
pub struct Args {
    /// Compare the generated output against the committed files and exit 1
    /// on any difference — writes nothing. The CI freshness gate.
    #[arg(long)]
    check: bool,
    /// Forward `--locked` to the `cargo run` that renders the CLI
    /// reference, so CI never rewrites the lockfile mid-check.
    #[arg(long)]
    locked: bool,
}

/// The workspace root — this crate lives at `<root>/xtask`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under the workspace root")
        .to_path_buf()
}

pub fn run(args: Args) -> Result<()> {
    let root = workspace_root();
    let cli_markdown = cli_reference(&root, args.locked)?;
    let files = generate(&cli_markdown);
    if args.check {
        check(&root, &files).map_err(|report| anyhow::anyhow!("{report}"))
    } else {
        write(&root, &files).context("failed to write the generated docs")
    }
}

/// The `docs/reference/cli.md` body: `dbtl cli-reference`'s stdout — the
/// binary renders the doc from its own clap model so it can't drift from
/// the real CLI.
fn cli_reference(root: &Path, locked: bool) -> Result<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.arg("run").arg("--quiet");
    if locked {
        cmd.arg("--locked");
    }
    cmd.arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .arg("-p")
        .arg("dbtl")
        .arg("--")
        .arg("cli-reference");
    let out = cmd.output().context("failed to run `dbtl cli-reference`")?;
    if !out.status.success() {
        bail!(
            "`dbtl cli-reference` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).context("`dbtl cli-reference` printed non-UTF-8")
}
