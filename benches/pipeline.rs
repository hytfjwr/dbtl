//! Pipeline benchmarks over a synthetic manifest (see `common::synth_manifest`):
//! `Dag::build`, `subgraph_view`, the whole-graph overview layout, the styled
//! lineage layout the event loop draws from, `dbtl docs` generation, the
//! manifest/source load paths, column-level lineage, selector evaluation,
//! diffing, linting, and a full `ui::draw` frame.
//!
//! Sizes follow the reference points (500 / 2000 / 8000 models);
//! fan-out and layer mix stay on `SynthParams::default()` unless a case is
//! specifically about shape. Run: `cargo bench` (a subset via
//! `cargo bench -- dag_build/2000`).

mod common;

use common::{manifest_json, synth_manifest, write_project, SynthParams};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use dbtl::ui::{draw as tui_draw, RenderCtx, UiState};
use dbtl::{
    build_model_list, compute_diff, generate_docs, layout_density, load_dag_from_source,
    load_manifest, run_rules, select, App, ColumnLineage, ColumnRef, Dag, Density, GlyphMode,
    LintConfig, PackageScope, RawManifest, RawNode, SortMode, SqlDialect, SqlParseResolver,
};
use ratatui::{backend::TestBackend, Terminal};
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

/// Params for the "rich" benches: sources, declared columns, tests,
/// exposures, and ref-bearing SQL all on — the shape `cargo xtask synth`
/// writes to disk and the load/CLL/lint paths all exercise.
fn rich_params(n: usize) -> SynthParams {
    SynthParams {
        models: n,
        fan_out: 8,
        sources: 8,
        columns: 4,
        tests_per_model: 2,
        exposures: 16,
        sql: true,
        ..SynthParams::default()
    }
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

fn bench_manifest_parse(c: &mut Criterion) {
    let mut g = c.benchmark_group("manifest_parse");
    for n in [2000usize, 8000] {
        let dir = tempfile::tempdir().expect("bench tempdir");
        let path = dir.path().join("manifest.json");
        std::fs::write(&path, manifest_json(&synth_manifest(&rich_params(n))))
            .expect("write synth manifest");
        g.bench_with_input(BenchmarkId::from_parameter(n), &path, |b, p| {
            b.iter(|| load_manifest(black_box(p)).expect("parse"))
        });
    }
    g.finish();
}

fn bench_source_load(c: &mut Criterion) {
    let mut g = c.benchmark_group("source_load");
    for n in [500usize, 2000] {
        let dir = tempfile::tempdir().expect("bench tempdir");
        write_project(&rich_params(n), dir.path()).expect("write synth project");
        g.bench_with_input(BenchmarkId::from_parameter(n), dir.path(), |b, d| {
            b.iter(|| load_dag_from_source(black_box(d)).expect("load"))
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
            b.iter(|| generate_docs(black_box(d), "bench", "synthetic", PackageScope::Root))
        });
    }
    g.finish();
}

/// Column-level lineage over ref-bearing SQL: a fresh `ColumnLineage` per
/// iteration so the per-node memo stays cold and each sample pays the real
/// transitive-closure cost. `mar_00300`'s stride picks all land in the
/// intermediate layer (an all-`int` parent cone — the join edges fan the
/// walk); `tbl_0000`'s downstream cone reaches most of the graph.
fn bench_cll(c: &mut Criterion) {
    let dag = Dag::build(&synth_manifest(&rich_params(2000)));
    let resolver = SqlParseResolver::new(None, SqlDialect::Generic);
    let mut g = c.benchmark_group("cll");
    g.bench_function("closure_up", |b| {
        b.iter(|| {
            let cll = ColumnLineage::new(&dag, &resolver);
            cll.closure_up(black_box(&ColumnRef::new("model.bench.mar_00300", "id")))
        })
    });
    g.bench_function("closure_down", |b| {
        b.iter(|| {
            let cll = ColumnLineage::new(&dag, &resolver);
            cll.closure_down(black_box(&ColumnRef::new(
                "source.bench.raw.tbl_0000",
                "id",
            )))
        })
    });
    g.finish();
}

fn bench_select(c: &mut Criterion) {
    let dag = dag(8000);
    let mut g = c.benchmark_group("select");
    let cases: [(&str, String); 5] = [
        ("hops", "+mar_02399+".to_string()),
        ("tag", "tag:nightly".to_string()),
        ("path", "path:marts".to_string()),
        ("materialized", "config.materialized:table".to_string()),
        ("union", "+int_00100+ tag:hourly".to_string()),
    ];
    for (label, spec) in cases {
        g.bench_function(label, |b| {
            b.iter(|| select(black_box(&dag), None, black_box(&spec)))
        });
    }
    g.finish();
}

/// Deterministically mutate a manifest for the diff bench: change `raw_code`
/// on every 37th node, drop every 211th node outright, and add fresh marts
/// leaves — the changed/removed/added mix a real PR produces. Uids are
/// visited in sorted order so the mutation is a pure function of the input.
fn mutated(manifest: &RawManifest) -> RawManifest {
    let mut cur = manifest.clone();
    let mut uids: Vec<String> = cur.nodes.keys().cloned().collect();
    uids.sort();
    for (i, uid) in uids.iter().enumerate() {
        if i % 37 == 0 {
            if let Some(node) = cur.nodes.get_mut(uid) {
                node.raw_code = Some(format!("select {i} as id -- changed"));
            }
        }
        if i % 211 == 0 {
            cur.nodes.remove(uid);
            cur.parent_map.remove(uid);
            cur.child_map.remove(uid);
        }
    }
    let mut marts: Vec<String> = cur
        .nodes
        .keys()
        .filter(|u| u.starts_with("model.bench.mar_"))
        .cloned()
        .collect();
    marts.sort();
    for (j, parent) in marts.iter().take(4).enumerate() {
        let uid = format!("model.bench.mar_new_{j:03}");
        cur.parent_map.insert(uid.clone(), vec![parent.clone()]);
        cur.child_map
            .entry(parent.clone())
            .or_default()
            .push(uid.clone());
        cur.nodes.insert(
            uid.clone(),
            RawNode {
                name: format!("mar_new_{j:03}"),
                resource_type: "model".to_string(),
                path: Some(format!("marts/mar_new_{j:03}.sql")),
                original_file_path: Some(format!("models/marts/mar_new_{j:03}.sql")),
                raw_code: Some(format!("select {j} as id")),
                ..Default::default()
            },
        );
    }
    cur
}

fn bench_diff(c: &mut Criterion) {
    let base = Dag::build(&synth_manifest(&rich_params(2000)));
    let current = Dag::build(&mutated(&synth_manifest(&rich_params(2000))));
    let mut g = c.benchmark_group("diff");
    g.bench_function("2000", |b| {
        b.iter(|| compute_diff(black_box(&base), black_box(&current)))
    });
    g.finish();
}

/// Every registered rule in one `run_rules` pass — the same entry point
/// `dbtl check --lint` uses.
fn bench_lint(c: &mut Criterion) {
    let dag = Dag::build(&synth_manifest(&rich_params(4000)));
    let mut g = c.benchmark_group("lint");
    g.bench_function("4000", |b| {
        b.iter(|| run_rules(black_box(&dag), &LintConfig::default()))
    });
    g.finish();
}

/// One full `ui::draw` frame on a 160x44 test backend: list + lineage +
/// status — the per-keystroke render cost at overview scale.
fn bench_draw(c: &mut Criterion) {
    let dag = dag(8000);
    let list = build_model_list(&dag, SortMode::Layer);
    let state = UiState::new(list.len());
    let layout = layout_density(&dag.full_subgraph(), GlyphMode::Unicode, Density::Compact);
    let mut terminal = Terminal::new(TestBackend::new(160, 44)).expect("terminal");
    let mut g = c.benchmark_group("draw");
    g.bench_function("160x44", |b| {
        b.iter(|| {
            let ctx = RenderCtx::new(&list, &state, Some(&layout));
            // `draw` returns a frame that borrows the terminal — drop it
            // inside the iteration so the closure returns nothing.
            terminal.draw(|f| tui_draw(f, &ctx)).expect("draw");
        })
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_dag_build,
    bench_manifest_parse,
    bench_source_load,
    bench_subgraph,
    bench_overview_layout,
    bench_styled_lineage,
    bench_docs,
    bench_cll,
    bench_select,
    bench_diff,
    bench_lint,
    bench_draw,
);
criterion_main!(benches);
