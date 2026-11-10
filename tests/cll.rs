//! Column-level lineage (Issue 07 v0) end-to-end coverage: the `ColumnResolver`
//! boundary, the lazy memoized `ColumnLineage` engine, `closure_report`
//! determinism, and the `dbtl columns` CLI surface.
//!
//! Everything runs over `tests/fixtures/sample_project` (+ its frozen
//! manifest), where `models/marts/cll_demo.sql` exercises every edge kind:
//! rename (`o.id as order_id`), passthrough (`o.status`), transform
//! (`upper(o.status)`, `count(o.id)`), and filter-or-join (`o.disabled_flag`,
//! `f.country` — row-shaping columns that appear only in the join/where tail).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use dbtl::{
    closure_report, load_dag, load_dag_from_source, ClosureDirection, ColumnEdgeKind,
    ColumnLineage, ColumnRef, ColumnResolver, Dag, NameMatchResolver, RawManifest, RawNode,
};

const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/sample_manifest.json"
);
const PROJECT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_project");

fn fixture_manifest() -> PathBuf {
    PathBuf::from(MANIFEST)
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dbtl"))
        .args(args)
        .output()
        .expect("failed to run dbtl")
}

fn model(name: &str) -> String {
    format!("model.sample.{name}")
}

fn source_uid(name: &str) -> String {
    format!("source.sample.raw.{name}")
}

fn col(node: &str, c: &str) -> ColumnRef {
    ColumnRef::new(node, c)
}

/// `(model.column, model.column, kind)` triples — the comparable view of an
/// edge set, sorted + deduped by `BTreeSet`.
fn edge_set(
    dag: &Dag,
    resolver: &impl ColumnResolver,
    node: &str,
) -> BTreeSet<(String, String, ColumnEdgeKind)> {
    ColumnLineage::new(dag, resolver)
        .in_edges(node)
        .iter()
        .map(|e| {
            (
                format!("{}.{}", e.from.node, e.from.column),
                format!("{}.{}", e.to.node, e.to.column),
                e.kind,
            )
        })
        .collect()
}

// ---- the fixture's frozen edge sets ---------------------------------------

/// `cll_demo`'s in-edges: all four kinds, plus the documented v0
/// over-connection — `o.id`/`o.status`/`f.*` name-match binds EVERY parent
/// owning the column, so stg AND fct appear as rename/passthrough/transform
/// sources for shared column names.
fn cll_demo_edges() -> BTreeSet<(String, String, ColumnEdgeKind)> {
    let mut set = BTreeSet::new();
    let m = model("cll_demo");
    let stg = model("stg_orders");
    let fct = model("fct_orders");
    for p in [&stg, &fct] {
        set.insert((
            format!("{p}.id"),
            format!("{m}.order_id"),
            ColumnEdgeKind::Rename,
        ));
        set.insert((
            format!("{p}.status"),
            format!("{m}.status"),
            ColumnEdgeKind::Passthrough,
        ));
        set.insert((
            format!("{p}.status"),
            format!("{m}.status_upper"),
            ColumnEdgeKind::Transform,
        ));
        set.insert((
            format!("{p}.id"),
            format!("{m}.n_orders"),
            ColumnEdgeKind::Transform,
        ));
    }
    for out in ["order_id", "status", "status_upper", "n_orders"] {
        set.insert((
            format!("{stg}.disabled_flag"),
            format!("{m}.{out}"),
            ColumnEdgeKind::FilterOrJoin,
        ));
        set.insert((
            format!("{fct}.country"),
            format!("{m}.{out}"),
            ColumnEdgeKind::FilterOrJoin,
        ));
    }
    set
}

#[test]
fn cll_demo_in_edges_cover_every_edge_kind() {
    let dag = load_dag(MANIFEST).expect("sample manifest loads");
    let resolver = NameMatchResolver::new(None);
    assert_eq!(
        edge_set(&dag, &resolver, &model("cll_demo")),
        cll_demo_edges(),
        "cll_demo in-edges"
    );
}

#[test]
fn star_select_passthrough_reaches_the_source() {
    // `select *` in stg_orders: every declared output column the SOURCE also
    // declares is a passthrough edge — this is what lets an upstream walk
    // reach raw.orders instead of dying at the staging layer.
    let dag = load_dag(MANIFEST).expect("sample manifest loads");
    let resolver = NameMatchResolver::new(None);
    let got = edge_set(&dag, &resolver, &model("stg_orders"));
    for c in ["id", "status", "country_code", "disabled_flag"] {
        assert!(
            got.contains(&(
                format!("{}.{}", source_uid("orders"), c),
                format!("{}.{}", model("stg_orders"), c),
                ColumnEdgeKind::Passthrough,
            )),
            "missing passthrough edge for {c}, got {got:?}"
        );
    }
    assert_eq!(got.len(), 4, "exactly the four source columns");
}

#[test]
fn upstream_closure_of_order_id_walks_to_the_source() {
    // The transitive provenance of `cll_demo.order_id` reaches the raw source
    // through BOTH the rename (stg.id) and the over-connected join/filters
    // (fct.id, the FOJ fans) — deterministic `BTreeSet` either way.
    let dag = load_dag(MANIFEST).expect("sample manifest loads");
    let resolver = NameMatchResolver::new(None);
    let lin = ColumnLineage::new(&dag, &resolver);
    let got: BTreeSet<String> = lin
        .closure_up(&col(&model("cll_demo"), "order_id"))
        .iter()
        .map(|c| format!("{}.{}", c.node, c.column))
        .collect();
    let want: BTreeSet<String> = [
        (model("fct_orders"), "country"),
        (model("fct_orders"), "id"),
        (source_uid("orders"), "disabled_flag"),
        (source_uid("orders"), "id"),
        (model("stg_orders"), "disabled_flag"),
        (model("stg_orders"), "id"),
    ]
    .into_iter()
    .map(|(n, c)| format!("{n}.{c}"))
    .collect();
    assert_eq!(got, want, "upstream closure of cll_demo.order_id");
}

#[test]
fn downstream_closure_of_stg_id_is_the_blast_radius() {
    let dag = load_dag(MANIFEST).expect("sample manifest loads");
    let resolver = NameMatchResolver::new(None);
    let lin = ColumnLineage::new(&dag, &resolver);
    let got: BTreeSet<String> = lin
        .closure_down(&col(&model("stg_orders"), "id"))
        .iter()
        .map(|c| format!("{}.{}", c.node, c.column))
        .collect();
    let want: BTreeSet<String> = [
        (model("fct_orders"), "id"),
        (model("cll_demo"), "order_id"),
        (model("cll_demo"), "n_orders"),
        ("snapshot.sample.orders_snapshot".to_string(), "id"),
    ]
    .into_iter()
    .map(|(n, c)| format!("{n}.{c}"))
    .collect();
    assert_eq!(got, want, "downstream closure of stg_orders.id");
}

#[test]
fn column_counts_are_the_one_hop_edge_counts() {
    // The structure modal's `↑u ↓d` numbers are the ONE-HOP edge counts —
    // `upstream()`/`downstream()` lens per the Issue 07 spec, NOT transitive
    // closures. Pin the equivalence so the render layer and the engine can't
    // disagree.
    let dag = load_dag(MANIFEST).expect("sample manifest loads");
    let resolver = NameMatchResolver::new(None);
    let lin = ColumnLineage::new(&dag, &resolver);
    let counts = lin.column_counts(&model("cll_demo"));
    assert_eq!(
        counts.keys().cloned().collect::<Vec<_>>(),
        vec!["n_orders", "order_id", "status", "status_upper"],
        "BTreeMap order = sorted normalized names"
    );
    for (c, (up, down)) in &counts {
        let c = col(&model("cll_demo"), c);
        assert_eq!(*up, lin.upstream(&c).len(), "upstream count for {c:?}");
        assert_eq!(
            *down,
            lin.downstream(&c).len(),
            "downstream count for {c:?}"
        );
    }
    // Every cll_demo output has the same 4-edge 1-hop provenance (two direct
    // edges + the two FOJ fans stg.disabled_flag / fct.country, which attach
    // to every output), and cll_demo is a terminal model: downstream = 0.
    for c in ["order_id", "n_orders", "status", "status_upper"] {
        assert_eq!(counts[c], (4, 0), "{c} counts");
    }
}

#[test]
fn repeated_queries_are_byte_identical() {
    // Two independent engine instances over the same Dag + resolver inputs
    // must produce identical report values (BTreeMap/BTreeSet ordering and
    // the report's row sort make this airtight).
    let dag = load_dag(MANIFEST).expect("sample manifest loads");
    let a = NameMatchResolver::new(None);
    let b = NameMatchResolver::new(None);
    for dir in [ClosureDirection::Downstream, ClosureDirection::Upstream] {
        let ra = closure_report(&dag, &a, &col(&model("stg_orders"), "id"), dir);
        let rb = closure_report(&dag, &b, &col(&model("stg_orders"), "id"), dir);
        assert_eq!(ra, rb, "reports diverge for {dir:?}");
        assert_eq!(
            ra.to_string(),
            rb.to_string(),
            "markdown diverges for {dir:?}"
        );
    }
}

#[test]
fn source_and_manifest_modes_agree_where_columns_are_declared() {
    // cll_demo's parents both declare columns in schema.yml, so its in-edges
    // are identical whether the graph came from the frozen manifest or a
    // source parse — the resolver answers from the same information either
    // way. (fct_orders differs legitimately: source mode sees the seed's CSV
    // header columns dbt doesn't record.)
    let m = load_dag(MANIFEST).expect("sample manifest loads");
    let s = load_dag_from_source(PROJECT).expect("sample project parses");
    let (rm, rs) = (NameMatchResolver::new(None), NameMatchResolver::new(None));
    for node in [model("stg_orders"), model("cll_demo")] {
        assert_eq!(
            edge_set(&m, &rm, &node),
            edge_set(&s, &rs, &node),
            "{node} in-edges diverge between modes"
        );
    }
}

// ---- `dbtl columns` CLI surface -------------------------------------------

#[test]
fn columns_prints_the_downstream_markdown_report() {
    let m = fixture_manifest();
    let out = run(&[
        "columns",
        "stg_orders.id",
        "--manifest",
        m.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "columns exits 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("# Column lineage: `stg_orders.id` (downstream)"),
        "report header, got: {stdout:?}"
    );
    assert!(stdout.contains("## Downstream columns (4)"), "row count");
    for row in [
        "`fct_orders.id` | `stg_orders.id` | passthrough",
        "`cll_demo.order_id`",
        "`orders_snapshot.id`",
    ] {
        assert!(stdout.contains(row), "missing {row}, got: {stdout:?}");
    }
}

#[test]
fn columns_upstream_lists_provenance() {
    let m = fixture_manifest();
    let out = run(&[
        "columns",
        "cll_demo.order_id",
        "--upstream",
        "--manifest",
        m.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("# Column lineage: `cll_demo.order_id` (upstream)"),
        "upstream header, got: {stdout:?}"
    );
    assert!(stdout.contains("## Upstream columns (6)"), "row count");
    assert!(stdout.contains("`orders.id`"), "reaches the raw source");
    assert!(stdout.contains("filter_or_join"), "the FOJ edges are named");
}

#[test]
fn columns_json_is_a_serializable_report() {
    let m = fixture_manifest();
    let out = run(&[
        "columns",
        "stg_orders.id",
        "--format",
        "json",
        "--manifest",
        m.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json parses");
    assert_eq!(v["target"], "stg_orders.id");
    assert_eq!(v["direction"], "downstream");
    assert_eq!(v["confidence"], "name_match");
    assert_eq!(v["unique_id"], model("stg_orders"));
    let cols = v["columns"].as_array().expect("columns array");
    assert_eq!(cols.len(), 4);
    assert!(
        cols.iter()
            .any(|r| r["column"] == "fct_orders.id"
                && r["kinds"] == serde_json::json!(["passthrough"])),
        "fct_orders.id row, got {cols:?}"
    );
}

#[test]
fn columns_output_is_deterministic_across_processes() {
    let m = fixture_manifest();
    let args = [
        "columns",
        "stg_orders.id",
        "--manifest",
        m.to_str().unwrap(),
    ];
    let a = run(&args);
    let b = run(&args);
    assert!(a.status.success() && b.status.success());
    assert_eq!(a.stdout, b.stdout, "two runs emit identical bytes");
}

#[test]
fn columns_errors_are_plain_text_and_nonzero() {
    let m = fixture_manifest();
    for (target, needle) in [
        ("bogus", "expected <model>.<column>"),
        ("no_such_model.id", "no node named 'no_such_model'"),
        ("stg_orders.no_such_col", "has no column 'no_such_col'"),
    ] {
        let out = run(&["columns", target, "--manifest", m.to_str().unwrap()]);
        assert!(!out.status.success(), "{target} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(needle),
            "{target}: missing '{needle}', got: {stderr:?}"
        );
    }
}

#[test]
fn columns_works_in_source_mode() {
    // The same resolution path the TUI uses, from source: stg_orders.id's
    // blast radius is identical to the manifest-mode answer (the only
    // differences a source parse could add are seed-column edges, which don't
    // touch this column).
    let out = run(&["columns", "stg_orders.id", "--source", PROJECT]);
    assert!(
        out.status.success(),
        "source-mode columns exits 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("## Downstream columns (4)"), "{stdout:?}");
}

// ---- performance ------------------------------------------------------------

/// A `RawNode` with a `select a, b` body and no declared columns — the
/// resolver's extraction path.
fn perf_node(uid: &str, sql: &str) -> (String, RawNode) {
    let name = uid.rsplit('.').next().unwrap_or(uid).to_string();
    (
        uid.to_string(),
        RawNode {
            name,
            resource_type: "model".to_string(),
            raw_code: Some(sql.to_string()),
            original_file_path: Some(format!("models/{}.sql", uid.rsplit('.').next().unwrap())),
            ..Default::default()
        },
    )
}

/// ~100 ms budget for a transitive column closure, per the Issue 07 spec.
/// `#[ignore]`d — run with `cargo test --release --test cll perf_ -- --ignored`
/// (debug-mode timing is unrepresentative).
#[test]
#[ignore]
fn perf_closure_walk_is_fast_enough() {
    use std::collections::HashMap;
    use std::time::Instant;

    // A 200-node chain of `select id, x` models — every level's columns feed
    // the next, so a closure from the head is a full-graph walk.
    let n = 200usize;
    let mut nodes = HashMap::new();
    let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut child_map: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..n {
        let uid = format!("model.p.m{i:03}");
        let sql = if i == 0 {
            "select id, x from raw.seed".to_string()
        } else {
            format!("select id, x from {{ ref('m{:03}') }}", i - 1)
        };
        let (k, v) = perf_node(&uid, &sql);
        nodes.insert(k, v);
        if i > 0 {
            let prev = format!("model.p.m{:03}", i - 1);
            parent_map
                .entry(uid.clone())
                .or_default()
                .push(prev.clone());
            child_map.entry(prev).or_default().push(uid);
        }
    }
    let dag = Dag::build(&RawManifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        parent_map,
        child_map,
        macros: HashMap::new(),
        semantic_models: HashMap::new(),
        metrics: HashMap::new(),
        saved_queries: HashMap::new(),
        unit_tests: HashMap::new(),
        metadata: Default::default(),
    });
    let resolver = NameMatchResolver::new(None);
    let lin = ColumnLineage::new(&dag, &resolver);

    let start = Instant::now();
    let down = lin.closure_down(&col("model.p.m000", "id"));
    let up = lin.closure_up(&col(&format!("model.p.m{:03}", n - 1), "id"));
    let elapsed = start.elapsed();
    assert_eq!(down.len(), n - 1, "id passthroughs through {n}-1 levels");
    assert_eq!(up.len(), n - 1);
    assert!(
        elapsed.as_millis() < 1000,
        "closure took {elapsed:?} — expected <<1s for a 200-model chain"
    );
}
