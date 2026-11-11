use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use xtask::synth::{write_project, SynthParams};

#[derive(ClapArgs)]
pub struct Args {
    /// Number of models to generate.
    #[arg(long)]
    models: usize,
    /// Output directory: the generated dbt project tree lands here, with the
    /// manifest at `<dir>/target/manifest.json`.
    #[arg(long)]
    out: PathBuf,
    /// Source tables feeding the staging layer.
    #[arg(long, default_value_t = 8)]
    sources: usize,
    /// Declared `cNNNN` columns per model and source table (on top of `id`).
    #[arg(long, default_value_t = 8)]
    columns: usize,
    /// Schema column tests per model (`not_null`/`unique` alternating).
    #[arg(long, default_value_t = 2)]
    tests_per_model: usize,
    /// Dashboard exposures consuming marts models.
    #[arg(long, default_value_t = 16)]
    exposures: usize,
    /// Parents per non-staging model.
    #[arg(long, default_value_t = 3)]
    fan_out: usize,
    /// Project/package name used in uids and `dbt_project.yml`.
    #[arg(long, default_value = "bench")]
    package: String,
}

pub fn run(args: Args) -> Result<()> {
    let params = SynthParams {
        models: args.models,
        fan_out: args.fan_out,
        sources: args.sources,
        columns: args.columns,
        tests_per_model: args.tests_per_model,
        exposures: args.exposures,
        // The on-disk forms must agree: `.sql` files always carry
        // ref()/source() bodies and the manifest's `raw_code` mirrors them.
        sql: true,
        package: args.package,
        ..SynthParams::default()
    };
    write_project(&params, &args.out)
        .with_context(|| format!("write the synthetic project to {}", args.out.display()))?;
    eprintln!(
        "wrote a {}-model project to {}",
        params.models,
        args.out.display()
    );
    Ok(())
}
