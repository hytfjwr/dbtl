use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args as ClapArgs;

use crate::gen_manifest;

#[derive(ClapArgs)]
pub struct Args {
    /// Pin the dbt-core release used for parsing (e.g. "1.12.6"). By default
    /// uvx resolves the latest release, matching the previous manual recipe.
    #[arg(long)]
    dbt_version: Option<String>,
    /// Write the manifest here instead of tests/fixtures/sample_manifest.json.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Inline YAML/JSON var map passed to `dbt compile --vars` verbatim —
    /// the manifest then records the same rendered values `dbtl --vars`
    /// produces from source.
    #[arg(long, value_name = "YAML|JSON")]
    vars: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let root = gen_manifest::repo_root();
    let (project_dir, out) = resolve_paths(&root, args.out);
    gen_manifest::gen(
        &project_dir,
        &out,
        None,
        args.dbt_version.as_deref(),
        args.vars.as_deref(),
    )
}

/// The fixed (project dir, manifest) pair for the sample fixture — `out`
/// only changes when `--out` redirects a run.
fn resolve_paths(root: &Path, out: Option<PathBuf>) -> (PathBuf, PathBuf) {
    (
        root.join("tests/fixtures/sample_project"),
        out.unwrap_or_else(|| root.join("tests/fixtures/sample_manifest.json")),
    )
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
        args: Args,
    }

    #[test]
    fn args_parse_all_flags() {
        let cli = TestCli::try_parse_from([
            "t",
            "--dbt-version",
            "1.12.6",
            "--out",
            "scratch/m.json",
            "--vars",
            "{a: 1}",
        ])
        .expect("parse");
        assert_eq!(cli.args.dbt_version.as_deref(), Some("1.12.6"));
        assert_eq!(cli.args.out, Some(PathBuf::from("scratch/m.json")));
        assert_eq!(cli.args.vars.as_deref(), Some("{a: 1}"));
        let cli = TestCli::try_parse_from(["t"]).expect("all optional");
        assert_eq!(cli.args.dbt_version, None);
        assert_eq!(cli.args.out, None);
        assert_eq!(cli.args.vars, None);
    }

    #[test]
    fn resolve_paths_defaults_to_the_sample_fixture_pair() {
        let root = Path::new("/repo");
        let (project, out) = resolve_paths(root, None);
        assert_eq!(
            project,
            PathBuf::from("/repo/tests/fixtures/sample_project")
        );
        assert_eq!(
            out,
            PathBuf::from("/repo/tests/fixtures/sample_manifest.json")
        );
        // --out redirects the write; the project stays fixed.
        let (project, out) = resolve_paths(root, Some(PathBuf::from("/tmp/x.json")));
        assert_eq!(
            project,
            PathBuf::from("/repo/tests/fixtures/sample_project")
        );
        assert_eq!(out, PathBuf::from("/tmp/x.json"));
    }
}
