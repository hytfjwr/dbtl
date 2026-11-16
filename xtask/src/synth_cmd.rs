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

#[cfg(test)]
mod tests {
    use super::*;

    /// run() maps the flags onto SynthParams and writes a real project —
    /// the same one `cargo xtask synth` produces, one dir deep.
    #[test]
    fn run_writes_a_complete_project() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("proj");
        run(Args {
            models: 4,
            out: out.clone(),
            sources: 2,
            columns: 1,
            tests_per_model: 1,
            exposures: 2,
            fan_out: 2,
            package: "b".to_string(),
        })
        .expect("run synth");
        // The source tree and the pre-built manifest both landed.
        let project_yml = std::fs::read_to_string(out.join("dbt_project.yml")).unwrap();
        assert!(project_yml.contains("name: b"), "{project_yml}");
        let manifest = out.join("target/manifest.json");
        assert!(manifest.is_file(), "manifest written");
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        // The tree and the manifest describe the same graph: four models,
        // two source tables, two exposures.
        let models = m["nodes"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|u| u.starts_with("model.b."))
            .count();
        assert_eq!(models, 4, "one node per model");
        assert_eq!(m["sources"].as_object().unwrap().len(), 2);
        assert_eq!(m["exposures"].as_object().unwrap().len(), 2);
        // One .sql file per model, all under the layer dirs.
        let sqls = glob_sql(&out.join("models"));
        assert_eq!(sqls.len(), 4, "{sqls:?}");
        assert!(out.join("models/staging/schema.yml").is_file());
    }

    /// Recursive .sql file listing (the models tree is two dirs deep).
    fn glob_sql(dir: &PathBuf) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for layer in std::fs::read_dir(dir).unwrap() {
            let layer = layer.unwrap().path();
            if layer.is_dir() {
                out.extend(
                    std::fs::read_dir(&layer)
                        .unwrap()
                        .map(|e| e.unwrap().path())
                        .filter(|p| p.extension().is_some_and(|e| e == "sql")),
                );
            }
        }
        out
    }
}
