use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Version to cut, without the leading `v` (e.g. `0.6.3`).
    #[arg(long)]
    version: String,
    /// Release date stamped on the new section (`YYYY-MM-DD`).
    #[arg(long)]
    date: String,
    /// Changelog file to rewrite in place.
    #[arg(long, default_value = "CHANGELOG.md")]
    file: PathBuf,
    /// Cut the section even when `## [Unreleased]` holds no entries (a
    /// release with no user-facing changes gets a `- No notable changes.`
    /// line).
    #[arg(long)]
    allow_empty: bool,
}

pub fn run(args: Args) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("read {}", args.file.display()))?;
    let out = xtask::changelog::release_section(&text, &args.version, &args.date, args.allow_empty)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::write(&args.file, out).with_context(|| format!("write {}", args.file.display()))?;
    eprintln!(
        "cut [{}] - {} in {}",
        args.version,
        args.date,
        args.file.display()
    );
    Ok(())
}
