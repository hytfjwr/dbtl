mod regen_sample_manifest;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Repository maintenance tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Regenerate tests/fixtures/sample_manifest.json with real dbt (requires uv).
    RegenSampleManifest(regen_sample_manifest::Args),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::RegenSampleManifest(args) => regen_sample_manifest::run(args),
    }
}
