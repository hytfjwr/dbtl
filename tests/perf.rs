//! Release-mode performance smoke tests — `#[ignore]`d in normal runs and
//! exercised by the nightly perf job via
//! `cargo test --release --workspace -- --ignored perf_`. Budgets are
//! deliberately loose (orders of magnitude over local timings): these catch
//! pathological regressions, not single-digit-percent drift — that's what
//! the PR-time benchmark report is for.

use std::time::Instant;

use dbtl::{generate_docs, load_dag_from_source, Dag, PackageScope};
use xtask::synth::{synth_manifest, write_project, SynthParams};

/// `Dag::build` over the 8000-model synth graph — the graph shape the TUI
/// overview and the layout benches exercise.
#[test]
#[ignore]
fn perf_dag_build_scales() {
    let params = SynthParams {
        models: 8000,
        ..SynthParams::default()
    };
    let manifest = synth_manifest(&params);
    let start = Instant::now();
    let dag = Dag::build(&manifest);
    assert_eq!(dag.nodes().len(), 8000);
    assert!(
        start.elapsed().as_secs() < 10,
        "Dag::build(8000) took {:?} — expected well under 10s",
        start.elapsed()
    );
}

/// The no-compile source loader over a generated project tree — parses
/// `dbt_project.yml`, every `.sql`, and the schema files.
#[test]
#[ignore]
fn perf_source_load_scales() {
    let dir = tempfile::tempdir().expect("tempdir");
    let params = SynthParams {
        models: 2000,
        fan_out: 8,
        sources: 8,
        columns: 4,
        tests_per_model: 2,
        exposures: 16,
        sql: true,
        ..SynthParams::default()
    };
    write_project(&params, dir.path()).expect("write synth project");
    let start = Instant::now();
    let dag = load_dag_from_source(dir.path()).expect("load");
    // 2000 models + 8 sources + 16 exposures.
    assert_eq!(dag.nodes().len(), 2000 + 8 + 16);
    assert!(
        start.elapsed().as_secs() < 60,
        "source load of a 2000-model project took {:?} — expected well under 60s",
        start.elapsed()
    );
}

/// `dbtl docs` generation over 4000 models — the file set the `docs`
/// subcommand writes.
#[test]
#[ignore]
fn perf_docs_generation_scales() {
    let params = SynthParams {
        models: 4000,
        ..SynthParams::default()
    };
    let dag = Dag::build(&synth_manifest(&params));
    let start = Instant::now();
    let files = generate_docs(&dag, "bench", "synthetic", PackageScope::Root);
    assert!(!files.is_empty());
    assert!(
        start.elapsed().as_secs() < 60,
        "generate_docs(4000) took {:?} — expected well under 60s",
        start.elapsed()
    );
}
