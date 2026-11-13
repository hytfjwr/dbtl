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

/// `?` full-text search over a 10k-model project where EVERY model carries
/// compiled SQL — the compiled-search worst case. Per-keystroke cost must
/// stay under 2x the compiled-free run (the pre-feature cost): the lowered
/// compiled text is memoized per uid, so a keystroke adds one `contains`
/// scan per node, not a fresh lowering pass.
#[test]
#[ignore]
fn perf_fulltext_search_keystroke_scales() {
    use dbtl::app::{apply_action, App};
    use dbtl::Action;

    let mut manifest = synth_manifest(&SynthParams {
        models: 10_000,
        sql: true,
        ..SynthParams::default()
    });
    // Give every model a compiled blob the size of its raw SQL — the
    // largest per-node payload the compiled path can see.
    for n in manifest.nodes.values_mut() {
        n.compiled_code = n.raw_code.clone();
    }
    let with_compiled = Dag::build(&manifest);
    for n in manifest.nodes.values_mut() {
        n.compiled_code = None;
    }
    let without_compiled = Dag::build(&manifest);

    // Type `?zzz_nomatch` — every keystroke refilters the whole list and
    // misses on every model, so each pass walks all four match surfaces.
    let per_keystroke = |dag: Dag| -> Vec<std::time::Duration> {
        let mut app = App::new(dag, std::path::PathBuf::from("synth"));
        apply_action(&mut app, Action::SearchOpen);
        let mut timings = Vec::new();
        for c in "?zzz_nomatch".chars() {
            let t = Instant::now();
            apply_action(&mut app, Action::SearchType(c));
            timings.push(t.elapsed());
        }
        timings
    };
    let before = per_keystroke(without_compiled);
    let after = per_keystroke(with_compiled);
    eprintln!("raw-only keystrokes:    {before:?}");
    eprintln!("raw+compiled keystrokes: {after:?}");
    // Skip the first keystroke — it pays the one-time lowering of all
    // compiled blobs, amortized across every later keystroke.
    let steady = |v: &[std::time::Duration]| v[3..].iter().sum::<std::time::Duration>();
    let (b, a) = (steady(&before), steady(&after));
    eprintln!("steady-state: raw-only {b:?} vs raw+compiled {a:?}");
    assert!(
        a <= b * 2,
        "compiled-SQL search costs {a:?} vs the raw-only {b:?} — over 2x"
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
