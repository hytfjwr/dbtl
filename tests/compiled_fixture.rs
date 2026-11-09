//! Integration tests for compiled-SQL ingestion (`Dag::ingest_compiled` via
//! `load_dag` / `load_dag_from_source`) against the committed
//! `tests/fixtures/compiled_project/` — a tiny hand-written project with a
//! `target/manifest.json` AND `target/compiled/` files, exercising every
//! resolution tier. Stale/fresh cases need controlled mtimes, so they copy the
//! fixture into a tempdir and stamp the files explicitly.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use dbtl::{load_dag, load_dag_from_source, Dag};

const PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/compiled_project"
);
const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/compiled_project/target/manifest.json"
);

// ---- manifest mode ---------------------------------------------------------

#[test]
fn manifest_compiled_code_and_target_file_both_load() {
    let dag = load_dag(MANIFEST).expect("fixture manifest loads");
    // Manifest `compiled_code` (mart_one) AND the `target/compiled/` file
    // fallback (stg_one, which has no compiled_code) both land in the side map.
    let mart = dag
        .compiled_code("model.compiled_demo.mart_one")
        .expect("mart_one has manifest compiled_code");
    assert!(mart.contains("demo.main.stg_one"), "expanded ref: {mart}");
    let stg = dag
        .compiled_code("model.compiled_demo.stg_one")
        .expect("stg_one resolves via target/compiled/");
    assert!(stg.contains("demo.raw.events"), "expanded source(): {stg}");
}

#[test]
fn manifest_compiled_code_beats_the_compiled_file() {
    // mart_one has BOTH a manifest `compiled_code` and a
    // `target/compiled/…/mart_one.sql` file whose body is a poison marker —
    // the manifest must win (the file is never read for it).
    let dag = load_dag(MANIFEST).unwrap();
    let mart = dag.compiled_code("model.compiled_demo.mart_one").unwrap();
    assert!(!mart.contains("WRONG_SHOULD_NEVER_BE_READ"));
}

#[test]
fn missing_compiled_sql_is_none_not_an_error() {
    let dag = load_dag(MANIFEST).unwrap();
    // No manifest field, no target file → absent.
    assert_eq!(dag.compiled_code("model.compiled_demo.lonely_model"), None);
    // Sources never have compiled SQL.
    assert_eq!(dag.compiled_code("source.compiled_demo.raw.events"), None);
    // And raw SQL is untouched by any of this.
    assert!(dag
        .raw_code("model.compiled_demo.lonely_model")
        .unwrap()
        .contains("never_compiled"));
}

#[test]
fn compiled_side_map_prunes_with_filtered() {
    // `Dag::filtered` must drop compiled entries for pruned nodes (same rule
    // as every other side map) — keep only mart_one.
    let dag = load_dag(MANIFEST).unwrap();
    let sub = dag.filtered(|n| n.name == "mart_one");
    assert!(sub.compiled_code("model.compiled_demo.mart_one").is_some());
    assert_eq!(sub.compiled_code("model.compiled_demo.stg_one"), None);
}

// ---- source mode -----------------------------------------------------------

#[test]
fn source_mode_ingests_target_compiled() {
    // `load_dag_from_source` synthesizes nodes with `package_name: None` —
    // the uid's project segment (`compiled_demo`) resolves the same
    // `target/compiled/` files.
    let dag = load_dag_from_source(PROJECT).expect("source project parses");
    let stg = dag
        .compiled_code("model.compiled_demo.stg_one")
        .expect("source-mode stg_one resolves target/compiled/");
    assert!(stg.contains("demo.raw.events"));
    // mart_one has no manifest compiled_code in source mode (there IS no
    // manifest) — the file fallback serves it. Its committed file is the
    // poison-marker one, so assert it loaded at all.
    assert!(dag
        .compiled_code("model.compiled_demo.mart_one")
        .unwrap()
        .contains("WRONG_SHOULD_NEVER_BE_READ"));
    assert_eq!(dag.compiled_code("model.compiled_demo.lonely_model"), None);
}

#[test]
fn deterministic_loading() {
    // Two independent loads must agree exactly (side map included) — the
    // ingest iterates a HashMap but each entry depends only on its own node.
    let a = load_dag(MANIFEST).unwrap();
    let b = load_dag(MANIFEST).unwrap();
    for uid in [
        "model.compiled_demo.stg_one",
        "model.compiled_demo.mart_one",
        "model.compiled_demo.lonely_model",
    ] {
        assert_eq!(a.compiled_code(uid), b.compiled_code(uid), "{uid}");
        assert_eq!(
            a.compiled(uid).map(|c| c.stale),
            b.compiled(uid).map(|c| c.stale)
        );
    }
}

// ---- stale detection (tempdir + controlled mtimes) -------------------------

/// Recursively copy a directory tree (the fixture) into `dst`.
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// Set a file's mtime to an absolute instant (Rust ≥1.75 `File::set_modified`).
fn stamp(path: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Copy the fixture into a tempdir and stamp the two files that decide
/// `stg_one`'s freshness: `src_newer` true → stale, false → fresh.
fn stamped_project(src_newer: bool) -> (tempfile::TempDir, Dag) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    copy_tree(Path::new(PROJECT), &root);
    let src = root.join("models/staging/stg_one.sql");
    let cmp = root.join("target/compiled/compiled_demo/models/staging/stg_one.sql");
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let new = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
    if src_newer {
        stamp(&cmp, old);
        stamp(&src, new);
    } else {
        stamp(&src, old);
        stamp(&cmp, new);
    }
    let dag = load_dag(root.join("target/manifest.json")).unwrap();
    (tmp, dag)
}

#[test]
fn source_newer_than_compiled_marks_stale() {
    let (_tmp, dag) = stamped_project(true);
    assert!(dag.compiled("model.compiled_demo.stg_one").unwrap().stale);
}

#[test]
fn compiled_newer_than_source_is_fresh() {
    let (_tmp, dag) = stamped_project(false);
    assert!(!dag.compiled("model.compiled_demo.stg_one").unwrap().stale);
    // Manifest-carried compiled_code is never stale regardless of mtimes.
    assert!(!dag.compiled("model.compiled_demo.mart_one").unwrap().stale);
}

#[test]
fn missing_source_file_is_not_stale() {
    // When the raw .sql can't be found under the project root the freshness
    // check can't run — the expansion just reports not-stale (absence ≠ stale).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    copy_tree(Path::new(PROJECT), &root);
    fs::remove_file(root.join("models/staging/stg_one.sql")).unwrap();
    let dag = load_dag(root.join("target/manifest.json")).unwrap();
    let c = dag.compiled("model.compiled_demo.stg_one").unwrap();
    assert!(!c.stale && c.code.contains("demo.raw.events"));
}

#[test]
fn absent_compiled_dir_is_none_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    copy_tree(Path::new(PROJECT), &root);
    fs::remove_dir_all(root.join("target/compiled")).unwrap();
    let dag = load_dag(root.join("target/manifest.json")).unwrap();
    assert_eq!(dag.compiled_code("model.compiled_demo.stg_one"), None);
    // …but the manifest's own compiled_code still loads.
    assert!(dag.compiled_code("model.compiled_demo.mart_one").is_some());
}

#[test]
fn manifest_outside_a_target_dir_reads_compiled_but_skips_staleness() {
    // `<dir>/manifest.json` + `<dir>/compiled/` still resolves by the sibling
    // convention regardless of the dir's name — but staleness needs `<dir>`
    // to literally be `target/` so `..` is the project root. A copied
    // manifest elsewhere must not compare mtimes against whatever happens to
    // sit at `dir/..` (a wrong root would happily find a "newer" source).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    copy_tree(Path::new(PROJECT), &root);
    fs::rename(root.join("target"), root.join("artifacts")).unwrap();
    // A newer "source" where a wrong `artifacts/..` derivation would look.
    let cmp = root.join("artifacts/compiled/compiled_demo/models/staging/stg_one.sql");
    let src = root.join("models/staging/stg_one.sql");
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let new = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
    stamp(&cmp, old);
    stamp(&src, new); // newer than the expansion — stale under the wrong root
    let dag = load_dag(root.join("artifacts/manifest.json")).unwrap();
    let c = dag
        .compiled("model.compiled_demo.stg_one")
        .expect("sibling compiled/ still reads");
    assert!(c.code.contains("demo.raw.events"));
    assert!(
        !c.stale,
        "manifest not under target/ → staleness undeterminable → not stale"
    );
}
