//! Pipeline benchmarks over a synthetic manifest (see `common::synth_manifest`):
//! `Dag::build`, `subgraph_view`, the whole-graph overview layout, the styled
//! lineage layout the event loop draws from, and `dbtl docs` generation.
//!
//! Sizes follow the proposal's reference points (500 / 2000 / 8000 models);
//! fan-out and layer mix stay on `SynthParams::default()` unless a case is
//! specifically about shape. Run: `cargo bench` (a subset via
//! `cargo bench -- dag_build/2000`).

mod common;

use common::{synth_manifest, SynthParams};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use dbtl::{generate_docs, layout_density, App, Dag, Density, GlyphMode};
use std::path::PathBuf;
use std::time::Duration;

fn dag(n: usize) -> Dag {
    Dag::build(&synth_manifest(&SynthParams {
        models: n,
        ..SynthParams::default()
    }))
}

/// The last marts node — the deepest sink, so a full closure touches nearly
/// every node (marts count for `n` under the default 4/3/3 ratio is `n*3/10`,
/// and the generator prefixes names with the layer's first three letters).
fn tail_marts(n: usize) -> String {
    format!("model.bench.mar_{:05}", n * 3 / 10 - 1)
}

fn bench_dag_build(c: &mut Criterion) {
    let mut g = c.benchmark_group("dag_build");
    for n in [500usize, 2000, 8000] {
        let manifest = synth_manifest(&SynthParams {
            models: n,
            ..SynthParams::default()
        });
        g.bench_with_input(BenchmarkId::from_parameter(n), &manifest, |b, m| {
            b.iter(|| Dag::build(black_box(m)))
        });
    }
    g.finish();
}

fn bench_subgraph(c: &mut Criterion) {
    let dag = dag(8000);
    let root = tail_marts(8000); // mar_02399
    let mut g = c.benchmark_group("subgraph_view");
    for depth in [None, Some(3)] {
        let id = match depth {
            None => "full".to_string(),
            Some(d) => format!("depth{d}"),
        };
        g.bench_function(BenchmarkId::new("rooted", id), |b| {
            b.iter(|| dag.subgraph_view(black_box(&root), true, true, black_box(depth)))
        });
    }
    g.finish();
}

fn bench_overview_layout(c: &mut Criterion) {
    let mut g = c.benchmark_group("overview_layout");
    for n in [2000usize, 8000] {
        let dag = dag(n);
        // `dag.full_subgraph()` = exactly what the `w` overview renders
        // (every node + every edge, Compact density).
        let sg = dag.full_subgraph();
        g.bench_with_input(BenchmarkId::from_parameter(n), &sg, |b, sg| {
            b.iter(|| layout_density(black_box(sg), GlyphMode::Unicode, Density::Compact))
        });
    }
    g.finish();
}

fn bench_styled_lineage(c: &mut Criterion) {
    let dag = dag(8000);
    let mut g = c.benchmark_group("styled_lineage_layout");
    for overview in [false, true] {
        let id = if overview { "overview" } else { "rooted" };
        // `iter_batched`: the App rebuild happens OUTSIDE the timed routine —
        // a fresh App's first styled_lineage_layout is always a cache miss,
        // which is the path this bench measures.
        g.bench_function(id, |b| {
            let dag = dag.clone();
            b.iter_batched(
                || {
                    let mut app = App::new(dag.clone(), PathBuf::from("synth"));
                    if overview {
                        app.ui_state.toggle_overview();
                    } else {
                        app.select_by_unique_id(&tail_marts(8000));
                    }
                    app
                },
                |app| black_box(app.styled_lineage_layout()),
                criterion::BatchSize::SmallInput,
            )
        });
    }
    g.finish();
}

fn bench_docs(c: &mut Criterion) {
    let mut g = c.benchmark_group("generate_docs");
    g.measurement_time(Duration::from_secs(20));
    for n in [1000usize, 4000] {
        let dag = dag(n);
        g.bench_with_input(BenchmarkId::from_parameter(n), &dag, |b, d| {
            b.iter(|| generate_docs(black_box(d), "bench", "synthetic"))
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    bench_dag_build,
    bench_subgraph,
    bench_overview_layout,
    bench_styled_lineage,
    bench_docs,
);
criterion_main!(benches);
