//! CLI-level coverage for the `dbtl impact` / `dbtl check` subcommands: both
//! return before any terminal init, so they exercise the real arg parsing +
//! analysis path in a non-TTY test process (same guarantee `docs` has).

use std::path::PathBuf;
use std::process::Command;

/// The frozen fixture manifest the rest of the suite pins.
fn fixture_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("manifest.json")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dbtl"))
        .args(args)
        .output()
        .expect("failed to run dbtl")
}

#[test]
fn impact_prints_markdown_report_to_stdout() {
    let m = fixture_manifest();
    let out = run(&[
        "impact",
        "--manifest",
        m.to_str().unwrap(),
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "impact exits 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("# PR Impact Report"),
        "markdown report on stdout, got: {stdout:?}"
    );
}

#[test]
fn impact_json_is_a_serializable_diff_report() {
    let m = fixture_manifest();
    let out = run(&[
        "impact",
        "--manifest",
        m.to_str().unwrap(),
        "--diff",
        m.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    for key in [
        "baseline",
        "added",
        "removed",
        "modified",
        "edges_added",
        "edges_removed",
        "pr",
    ] {
        assert!(v.get(key).is_some(), "DiffReport field {key:?} present");
    }
    let pr = &v["pr"];
    for key in [
        "affected_models",
        "affected_marts",
        "affected_exposures",
        "ci_command",
        "untested_changes",
        "changed_hubs",
        "new_layer_violations",
    ] {
        assert!(pr.get(key).is_some(), "PrImpact field {key:?} present");
    }
}

#[test]
fn impact_requires_a_baseline() {
    let m = fixture_manifest();
    let out = run(&["impact", "--manifest", m.to_str().unwrap()]);
    assert!(!out.status.success(), "no --diff → clap usage error");
}

#[test]
fn check_passes_when_no_gates_engage() {
    let m = fixture_manifest();
    // Self-diff: no changes → no untested changes, no new violations; the
    // coverage line reports but does not gate at --coverage-min 0.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "all gates pass, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS coverage:"), "coverage reported");
    assert!(stdout.contains("PASS new layer violations:"));
    assert!(stdout.contains("PASS untested changes:"));
    assert!(!stdout.contains("FAIL"), "no failures, got: {stdout:?}");
}

#[test]
fn check_fails_under_coverage_threshold() {
    let m = fixture_manifest();
    // The fixture is not 100%-tested, so demanding 100% must fail the gate.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--coverage-min",
        "100",
    ]);
    assert!(
        !out.status.success(),
        "coverage gate under the threshold exits non-zero"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAIL coverage"),
        "failure line on stdout, got: {stdout:?}"
    );
}

#[test]
fn check_quiet_suppresses_report_but_keeps_exit_code() {
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--coverage-min",
        "100",
        "--quiet",
    ]);
    assert!(!out.status.success(), "gate still fails");
    assert!(out.stdout.is_empty(), "quiet prints nothing");
}
