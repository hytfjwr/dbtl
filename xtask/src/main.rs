mod gen_docs_cmd;
mod perf_report_cmd;
mod regen_sample_manifest;
mod synth_cmd;

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
    /// Write a deterministic synthetic dbt project to disk (models/*.sql +
    /// schema.yml + target/manifest.json — the manifest and the source tree
    /// describe the same graph).
    Synth(synth_cmd::Args),
    /// Render the Markdown perf comparison for the CI step summary: Criterion
    /// medians for both sides plus command timings and binary sizes.
    PerfReport(perf_report_cmd::Args),
    /// Regenerate the reference docs under docs/reference/ and the config
    /// JSON Schemas under schema/ (`--check` verifies without writing).
    GenDocs(gen_docs_cmd::Args),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::RegenSampleManifest(args) => regen_sample_manifest::run(args),
        Command::Synth(args) => synth_cmd::run(args),
        Command::PerfReport(args) => perf_report_cmd::run(args),
        Command::GenDocs(args) => gen_docs_cmd::run(args),
    }
}
