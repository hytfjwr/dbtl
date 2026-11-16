//! The auto-detected-manifest staleness warning: `--project <dir>` picking
//! `target/manifest.json` warns iff the project tree holds data files newer
//! than the manifest — a `dbt parse`-era artifact silently hiding post-parse
//! edits. Explicit `--manifest`/`--source` never warn (they name the data
//! source deliberately), and a fresh manifest stays quiet.
//!
//! mtimes are stamped explicitly: a CI checkout doesn't preserve them, and
//! `fs::copy` sets the copy time — asserting against untouched files would
//! flake on whichever side of the threshold the copy landed.

mod common;

use common::{copy_tree, fixture_path, run, stamp, stderr, write_project};
use std::path::Path;
use std::time::{Duration, SystemTime};

const PROJECT: &str = "compiled_project";

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

/// Write `path` (parents created) and stamp its mtime — a file landing
/// after the manifest's parse.
fn write_at(path: &Path, contents: &str, t: SystemTime) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    stamp(path, t);
}

/// `ls --project <root>` run + stderr — the probe's warning (or absence)
/// is the only assertion surface these tests read.
fn ls_warn(root: &Path) -> String {
    let out = run(&[
        "ls",
        "*",
        "--project",
        root.to_str().unwrap(),
        "--output",
        "uids",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    stderr(&out).replace('\\', "/")
}

/// A scratch project — `files` plus a copy of the compiled fixture's
/// `target/manifest.json` — with every non-target file stamped old and
/// the manifest stamped `manifest_t`, so only files the test adds after
/// can prove staleness.
fn scratch_project(
    files: &[(&str, &str)],
    manifest_t: SystemTime,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(&tmp.path().join("proj"), files);
    let manifest = root.join("target/manifest.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::copy(
        fixture_path("compiled_project/target/manifest.json"),
        &manifest,
    )
    .unwrap();
    stamp_project_sources(&root, SystemTime::UNIX_EPOCH + T_OLD);
    stamp(&manifest, manifest_t);
    (tmp, root)
}

/// The motivating case: `dbt run`'s own `logs/` regenerates on every run,
/// so logs being the only post-parse write must NOT mark the manifest
/// stale — even when the file's extension looks like a model's.
#[test]
fn auto_detected_manifest_ignores_log_writes() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    write_at(
        &root.join("logs/query_log.sql"),
        "select 1",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    assert!(
        !ls_warn(&root).contains("is older than"),
        "a logs/ write is not a manifest input"
    );
}

/// Generated and vendored trees can never be manifest inputs: `logs/`,
/// `dbt_packages/`/`dbt_internal_packages/` (the parse never reads
/// installed package files into THIS project's staleness), `venv/`,
/// `build/`, `node_modules/` — none of their post-parse writes warn.
#[test]
fn auto_detected_manifest_ignores_generated_and_vendored_trees() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    for rel in [
        "logs/dbt.log.sql",
        "dbt_packages/pkg/models/m.sql",
        "dbt_internal_packages/dbt/models/m.sql",
        "venv/lib/site-packages/x.py",
        "node_modules/dep/index.sql",
        "build/out/m.sql",
    ] {
        write_at(&root.join(rel), "select 1", SystemTime::UNIX_EPOCH + T_NEW);
    }
    assert!(
        !ls_warn(&root).contains("is older than"),
        "generated/vendored trees are outside dbt's input set"
    );
}

/// The probe honors the project's own `model-paths` — a model under a
/// custom root is just as much a manifest input as one under `models/`.
#[test]
fn auto_detected_manifest_counts_a_custom_model_path() {
    let (_tmp, root) = scratch_project(
        &[
            (
                "dbt_project.yml",
                "name: p\nversion: \"1.0.0\"\nconfig-version: 2\nmodel-paths: [\"src/models\"]\n",
            ),
            ("src/models/m.sql", "select 1"),
        ],
        SystemTime::UNIX_EPOCH + T_OLD,
    );
    write_at(
        &root.join("src/models/m.sql"),
        "select 2",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    let err = ls_warn(&root);
    assert!(
        err.contains("target/manifest.json is older than 1 project file"),
        "a custom model-paths file is a manifest input: {err}"
    );
    assert!(
        err.contains("newest: src/models/m.sql"),
        "the warning names it: {err}"
    );
}

/// `model-paths: ["."]` puts the whole project in scope — but `logs/`,
/// `dbt_packages/`, `target/`, and dot-dirs are generated trees wherever
/// they sit, so they still cannot count.
#[test]
fn auto_detected_manifest_with_root_resource_path_still_ignores_logs() {
    let (_tmp, root) = scratch_project(
        &[
            (
                "dbt_project.yml",
                "name: p\nversion: \"1.0.0\"\nconfig-version: 2\nmodel-paths: [\".\"]\n",
            ),
            ("models/m.sql", "select 1"),
        ],
        SystemTime::UNIX_EPOCH + T_OLD,
    );
    for rel in [
        "logs/query_log.sql",
        "dbt_packages/pkg/m.sql",
        ".venv/lib/x.py",
    ] {
        write_at(&root.join(rel), "select 1", SystemTime::UNIX_EPOCH + T_NEW);
    }
    assert!(
        !ls_warn(&root).contains("is older than"),
        "a project-root resource path can't reach generated trees"
    );
}

/// The root-level config files dbt reads at parse — every `.yml`/`.yaml`
/// sibling of `dbt_project.yml`, plus `.dbtignore` — are manifest inputs:
/// `vars.yml` and `package-lock.yml` edits must warn.
#[test]
fn auto_detected_manifest_counts_root_config_files() {
    let (_tmp, root) = stamped_project(SystemTime::UNIX_EPOCH + T_OLD);
    // Distinct stamps so the "newest" the warning names is deterministic.
    write_at(
        &root.join(".dbtignore"),
        "build/\n",
        SystemTime::UNIX_EPOCH + T_NEW - Duration::from_secs(2),
    );
    write_at(
        &root.join("package-lock.yml"),
        "packages: []\n",
        SystemTime::UNIX_EPOCH + T_NEW - Duration::from_secs(1),
    );
    write_at(
        &root.join("vars.yml"),
        "x: 1\n",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    let err = ls_warn(&root);
    assert!(
        err.contains("target/manifest.json is older than 3 project files"),
        "root .yml/.yaml/.dbtignore count as inputs: {err}"
    );
    assert!(
        err.contains("newest: vars.yml"),
        "the warning names the newest: {err}"
    );
}

/// A `{% docs %}` body is a manifest input — `.md` files under
/// `docs-paths` (declared or the resource-path union dbt defaults to)
/// must warn like a model edit does.
#[test]
fn auto_detected_manifest_counts_docs_markdown() {
    let (_tmp, root) = scratch_project(
        &[
            (
                "dbt_project.yml",
                "name: p\nversion: \"1.0.0\"\nconfig-version: 2\ndocs-paths: [\"docs\"]\n",
            ),
            ("models/m.sql", "select 1"),
        ],
        SystemTime::UNIX_EPOCH + T_OLD,
    );
    write_at(
        &root.join("docs/guide.md"),
        "{% docs g %}body{% enddocs %}\n",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    let err = ls_warn(&root);
    assert!(
        err.contains("target/manifest.json is older than 1 project file"),
        "a docs-paths .md is a manifest input: {err}"
    );
    assert!(
        err.contains("newest: docs/guide.md"),
        "the warning names it: {err}"
    );
}

/// An unparsable `dbt_project.yml` can't resolve the resource paths — the
/// probe falls back to the broad data-file walk (a stray `.sql` outside
/// `models/` still warns), still excluding `logs/` and the package dirs.
#[test]
fn auto_detected_manifest_falls_back_when_the_project_file_is_unreadable() {
    let (_tmp, root) = scratch_project(
        &[
            ("dbt_project.yml", "name: [unclosed\n"),
            ("misc/stray.sql", "select 1"),
        ],
        SystemTime::UNIX_EPOCH + T_OLD,
    );
    write_at(
        &root.join("misc/stray.sql"),
        "select 2",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    let err = ls_warn(&root);
    assert!(
        err.contains("is older than 1 project file"),
        "the fallback walk still counts a stray data file: {err}"
    );

    // logs/ stays excluded even without the resource paths to guide the
    // walk — and so do the package dirs.
    std::fs::remove_file(root.join("misc/stray.sql")).unwrap();
    write_at(
        &root.join("logs/query_log.sql"),
        "select 1",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    write_at(
        &root.join("dbt_packages/pkg/m.sql"),
        "select 1",
        SystemTime::UNIX_EPOCH + T_NEW,
    );
    assert!(
        !ls_warn(&root).contains("is older than"),
        "the fallback keeps logs/ and package dirs out of the count"
    );
}
