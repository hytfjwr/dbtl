use std::path::PathBuf;

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
}

pub fn run(args: Args) -> Result<()> {
    let root = gen_manifest::repo_root();
    let project_dir = root.join("tests/fixtures/sample_project");
    let out = args
        .out
        .unwrap_or_else(|| root.join("tests/fixtures/sample_manifest.json"));
    gen_manifest::gen(&project_dir, &out, None, args.dbt_version.as_deref())
}
