//! The auto-detected-manifest staleness warning: `--project <dir>` picking
//! `target/manifest.json` warns iff the project tree holds data files newer
//! than the manifest — a `dbt parse`-era artifact silently hiding post-parse
//! edits. Explicit `--manifest`/`--source` never warn (they name the data
//! source deliberately), and a fresh manifest stays quiet.
//!
//! mtimes are stamped explicitly: a CI checkout doesn't preserve them, and
//! `fs::copy` sets the copy time — asserting against untoucched files would
//! flake on whichever side of the threshold the copy landed.

mod common;

use common::{copy_tree, dbtl_cmd, fixture_path};
use std::path::Path;
use std::time::{Duration, SystemTime};

const PROJECT: &str = "compiled_project";

fn run(args: &[&str]) -> std::process::Output {
    dbtl_cmd()
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Set a file's mtime (Rust ≥1.75 `File::set_modified`).
fn stamp(path: &Path, t: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Stamp every project file the probe could count — everything outside
/// `target/` (the probe reads `.sql`/`.py`/`.yml`/`.yaml`/`.csv`; stamping
/// extras is harmless).
fn stamp_project_sources(root: &Path, t: SystemTime) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "target" {
                stamp_project_sources(&path, t);
            }
        } else {
            stamp(&path, t);
        }
    }
}

const T_OLD: Duration = Duration::from_secs(1_000_000);
const T_NEW: Duration = Duration::from_secs(2_000_000);

/// `compiled_project` copied to a tempdir with the project sources stamped
/// `old` and `target/manifest.json` stamped `manifest_t` — returns the
/// `(tempdir, project root)`.
fn stamped_project(manifest_t: SystemTime) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    copy_tree(&fixture_path(PROJECT), &root);
    stamp_project_sources(&root, SystemTime::UNIX_EPOCH + T_OLD);
    stamp(&root.join("target/manifest.json"), manifest_t);
    (tmp, root)
}

#[test]
fn auto_detected_manifest_warns_when_sources_are_newer() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    // One post-parse edit lands after the manifest's stamp.
    stamp(
        &root.join("models/marts/mart_one.sql"),
        SystemTime::UNIX_EPOCH + T_NEW,
    );

    let out = run(&[
        "ls",
        "*",
        "--project",
        root.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    // The warning prints `display()` paths — `\` separators on Windows —
    // so assert on the normalized shape.
    let err = stderr(&out).replace('\\', "/");
    assert!(
        err.contains("target/manifest.json is older than 1 project file"),
        "expected the stale warning, got: {err}"
    );
    assert!(
        err.contains("newest: models/marts/mart_one.sql"),
        "the warning names the newest file: {err}"
    );
    assert!(
        err.contains("dbt parse"),
        "the warning names the fix: {err}"
    );
    assert!(
        err.contains("--source"),
        "the warning names the escape: {err}"
    );
}

#[test]
fn auto_detected_manifest_stays_quiet_when_fresh() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_NEW);
    let out = run(&[
        "ls",
        "*",
        "--project",
        root.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        !stderr(&out).contains("is older than"),
        "a fresh manifest must not warn: {}",
        stderr(&out)
    );
}

#[test]
fn explicit_manifest_never_warns_even_when_stale() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    stamp(
        &root.join("models/marts/mart_one.sql"),
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    let manifest = root.join("target/manifest.json");
    let out = run(&[
        "ls",
        "*",
        "--manifest",
        manifest.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        !stderr(&out).contains("is older than"),
        "an explicit --manifest names the source deliberately: {}",
        stderr(&out)
    );
}

/// `--source <dir>` walks the project tree itself — the stale manifest
/// sitting under `target/` is irrelevant to it, so it must stay quiet.
#[test]
fn explicit_source_never_warns() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    stamp(
        &root.join("models/marts/mart_one.sql"),
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    let out = run(&[
        "ls",
        "*",
        "--source",
        root.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        !stderr(&out).contains("is older than"),
        "an explicit --source bypasses the manifest deliberately: {}",
        stderr(&out)
    );
}

/// A manifest that IS the newest data file — same instant as the sources —
/// is not "older than" anything (the probe counts strictly-newer files).
#[test]
fn auto_detected_manifest_same_mtime_is_fresh() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    let out = run(&[
        "ls",
        "*",
        "--project",
        root.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(
        !stderr(&out).contains("is older than"),
        "equal mtimes are not newer: {}",
        stderr(&out)
    );
}
