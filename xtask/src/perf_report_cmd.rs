use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use xtask::perf_report::{read_estimates, read_metrics, render_report, Report};

#[derive(ClapArgs)]
pub struct Args {
    /// Criterion output dir (the `CRITERION_HOME` used for the run) of the
    /// BASE side.
    #[arg(long)]
    base: PathBuf,
    /// Criterion output dir of the HEAD side.
    #[arg(long)]
    head: PathBuf,
    /// Directory holding `<cmd>.{base,head}` timing files
    /// (`secs rss_kib exit`) and `size.{base,head}` byte counts.
    #[arg(long)]
    metrics_dir: PathBuf,
    /// Column label for the base side.
    #[arg(long, default_value = "base")]
    base_label: String,
    /// Column label for the head side.
    #[arg(long, default_value = "head")]
    head_label: String,
    /// Set when the change touched the benchmark code itself — the report
    /// then notes that part of the delta is a changed benchmark set.
    #[arg(long)]
    benches_changed: bool,
    /// Append the report to this file instead of stdout (the workflow passes
    /// its scratch report path, then cats the file into the step summary).
    #[arg(long)]
    out: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let metrics = read_metrics(&args.metrics_dir)
        .with_context(|| format!("read metrics under {}", args.metrics_dir.display()))?;
    let report = Report {
        base_label: args.base_label,
        head_label: args.head_label,
        base: read_estimates(&args.base)
            .with_context(|| format!("read {}", args.base.display()))?,
        head: read_estimates(&args.head)
            .with_context(|| format!("read {}", args.head.display()))?,
        commands: metrics.commands,
        binary_size: metrics.binary_size,
        benches_changed: args.benches_changed,
    };
    let md = render_report(&report);
    match &args.out {
        Some(path) => {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("append to {}", path.display()))?;
            f.write_all(md.as_bytes())?;
        }
        None => print!("{md}"),
    }
    Ok(())
}
