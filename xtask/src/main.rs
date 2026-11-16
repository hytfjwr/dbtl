mod changelog_cmd;
mod gen_docs_cmd;
mod gen_manifest;
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
    /// Regenerate the manifest fixture for any project under tests/fixtures
    /// with real dbt (requires uv; local packages only).
    GenFixtureManifest(gen_manifest::GenArgs),
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
    /// Cut a `## [X.Y.Z] - <date>` section out of CHANGELOG's `Unreleased`
    /// entries and repoint the footer compare links (the release workflow
    /// runs this on the release PR).
    ChangelogRelease(changelog_cmd::Args),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::RegenSampleManifest(args) => regen_sample_manifest::run(args),
        Command::GenFixtureManifest(args) => gen_manifest::run(args),
        Command::Synth(args) => synth_cmd::run(args),
        Command::PerfReport(args) => perf_report_cmd::run(args),
        Command::GenDocs(args) => gen_docs_cmd::run(args),
        Command::ChangelogRelease(args) => changelog_cmd::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The subcommand names the repo's docs and workflows invoke — clap
    /// derives them from the enum, so a rename would silently break CI.
    #[test]
    fn subcommands_route_to_their_handlers() {
        for (argv, want) in [
            ("xtask regen-sample-manifest", "regen"),
            ("xtask gen-fixture-manifest --project p", "gen-fixture"),
            ("xtask synth --models 4 --out /tmp/x", "synth"),
            (
                "xtask perf-report --base b --head h --metrics-dir m",
                "perf",
            ),
            ("xtask gen-docs --check", "docs"),
            (
                "xtask changelog-release --version 0.6.3 --date 2024-01-01",
                "changelog",
            ),
        ] {
            let cli = Cli::try_parse_from(argv.split_whitespace().collect::<Vec<_>>())
                .unwrap_or_else(|e| panic!("{argv}: {e}"));
            let got = match &cli.command {
                Command::RegenSampleManifest(_) => "regen",
                Command::GenFixtureManifest(_) => "gen-fixture",
                Command::Synth(_) => "synth",
                Command::PerfReport(_) => "perf",
                Command::GenDocs(_) => "docs",
                Command::ChangelogRelease(_) => "changelog",
            };
            assert_eq!(got, want, "{argv}");
        }
        // An unknown flag is a parse error, not a silent default.
        assert!(Cli::try_parse_from(["xtask", "synth", "--bogus"]).is_err());
    }
}
