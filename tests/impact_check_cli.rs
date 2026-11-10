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
        // Null stdin keeps a bare (no-subcommand) invocation from seeing the
        // test runner's TTY and parking on the interactive event loop.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

/// Same as [`run`] with extra env vars scoped to the child process — the only
/// safe way to point `~/.config/dbtl/config.yml` at a fixture.
fn run_env(args: &[&str], envs: &[(&str, &std::path::Path)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dbtl"));
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to run dbtl")
}

/// The real-dbt-generated manifest the consistency suite pins (rich `meta`).
fn sample_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_manifest.json")
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

#[test]
fn check_lists_affected_owners_as_an_informational_line() {
    // `dbtl check --diff` prints the blast radius's affected owners for the
    // on-call reader — an informational line like `baseline:`, never a gate
    // (no FAIL prefix, exit code untouched).
    let m = sample_manifest();
    // A modified copy: stg_orders's raw_code differs → "modified" diff, and
    // its downstream model fct_orders carries `owner` meta.
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&m).unwrap()).unwrap();
    manifest["nodes"]["model.sample.stg_orders"]["raw_code"] =
        serde_json::json!("select 2 -- changed");
    // stg_orders → fct_orders (owner alice) is the only downstream model with
    // `owner` meta (dim_customers hangs off source.raw.customers instead).
    let tmp = tempfile::tempdir().unwrap();
    let changed = tmp.path().join("changed_manifest.json");
    std::fs::write(&changed, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "no gates engage — the line must not become one; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("affected owners: alice"),
        "informational owners line, got: {stdout:?}"
    );
    assert!(
        !stdout.lines().any(|l| l.starts_with("FAIL affected")),
        "owners must never gate: {stdout:?}"
    );
    // No --diff → no blast radius → no owners line at all.
    let out = run(&["check", "--manifest", m.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("affected owners:"),
        "absent without --diff: {stdout:?}"
    );
}

#[test]
fn check_uses_owner_meta_key_and_warns_on_malformed_config() {
    // `owner_meta_key` from config.yml picks which `meta` key the owners set
    // reads; a malformed config warns on stderr and falls back to `owner` —
    // the same non-fatal policy the TUI applies at startup, never an abort.
    let m = sample_manifest();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("dbtl");
    std::fs::create_dir_all(&cfg).unwrap();

    // A VALID custom key: fct_orders's `meta.team` lists by team, not owner.
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&m).unwrap()).unwrap();
    manifest["nodes"]["model.sample.stg_orders"]["raw_code"] =
        serde_json::json!("select 2 -- changed");
    let changed = tmp.path().join("changed.json");
    std::fs::write(&changed, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(cfg.join("config.yml"), "owner_meta_key: team\n").unwrap();
    let out = run_env(
        &[
            "check",
            "--manifest",
            changed.to_str().unwrap(),
            "--diff",
            m.to_str().unwrap(),
        ],
        &[("XDG_CONFIG_HOME", tmp.path())],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("affected owners: analytics-eng"),
        "custom key reads meta.team, got: {stdout:?}"
    );

    // A MALFORMED field type: warn + fall back to `owner`, still exit 0.
    std::fs::write(cfg.join("config.yml"), "owner_meta_key: [a, list]\n").unwrap();
    let out = run_env(
        &[
            "check",
            "--manifest",
            changed.to_str().unwrap(),
            "--diff",
            m.to_str().unwrap(),
        ],
        &[("XDG_CONFIG_HOME", tmp.path())],
    );
    assert!(
        out.status.success(),
        "malformed config must not abort, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: ignoring dbtl config.yml"),
        "the warning line, got stderr: {stderr:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("affected owners: alice"),
        "fell back to meta.owner, got: {stdout:?}"
    );
}

// ---- `check --lint` -------------------------------------------------------

#[test]
fn check_lint_reports_one_gate_per_rule() {
    // The frozen fixture carries known violations (frozen counts live in
    // tests/manifest_fixture.rs): --lint FAILs the gate and prints one line
    // per rule, PASS or FAIL.
    let m = fixture_manifest();
    let out = run(&["check", "--manifest", m.to_str().unwrap(), "--lint"]);
    assert!(
        !out.status.success(),
        "fixture lint violations fail the gate, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for rule in [
        "direct_join_to_source",
        "multiple_sources_joined",
        "source_fanout",
        "model_fanout",
        "root_models",
        "rejoining_upstream",
        "marts_depend_on_source",
        "missing_primary_key_tests",
        "public_models_without_contract",
        "undocumented_models",
        "naming_conventions",
        "layer_violation",
    ] {
        assert!(
            stdout.contains(&format!("lint {rule}:")),
            "a line for every rule, got: {stdout:?}"
        );
    }
    assert!(
        stdout.contains("FAIL lint direct_join_to_source: 16 violation(s):"),
        "the frozen count, got: {stdout:?}"
    );
    assert!(
        stdout.contains("PASS lint undocumented_models: none"),
        "clean rules still report PASS"
    );
}

#[test]
fn check_rules_selects_a_subset_and_implies_lint() {
    // `--rules` alone runs linting, reporting ONLY the named rules — the flag
    // is the authoritative selector. Both named rules are clean on the
    // fixture → exit 0.
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--rules",
        "undocumented_models,root_models",
    ]);
    assert!(
        out.status.success(),
        "named clean rules pass, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS lint root_models:"));
    assert!(stdout.contains("PASS lint undocumented_models:"));
    assert!(
        !stdout.contains("lint direct_join_to_source:"),
        "unselected rules don't report, got: {stdout:?}"
    );
}

#[test]
fn check_rules_rejects_an_unknown_rule_name() {
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--rules",
        "not_a_rule",
    ]);
    assert!(!out.status.success(), "unknown rule is a hard error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown lint rule 'not_a_rule'"),
        "the error names the typo + valid set, got: {stderr:?}"
    );
}

#[test]
fn check_lint_diff_gates_on_new_violations_only() {
    // Self-diff: every fixture violation is pre-existing → the gate passes
    // with an explicit "no new violations" line per rule.
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "pre-existing violations never gate, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PASS lint direct_join_to_source: no new violations (16 pre-existing)"),
        "pre-existing count is reported, got: {stdout:?}"
    );

    // A modified manifest: blank pos_txn's description → ONE new
    // undocumented_models finding vs the baseline → the gate fails on it.
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&m).unwrap()).unwrap();
    manifest["nodes"]["model.jaffle_finance.pos_txn"]["description"] = serde_json::json!("");
    let tmp = tempfile::tempdir().unwrap();
    let changed = tmp.path().join("manifest.json");
    std::fs::write(&changed, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--lint",
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert!(
        !out.status.success(),
        "a NEW violation gates, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAIL lint undocumented_models: 1 new violation(s): pos_txn"),
        "only the new finding gates, got: {stdout:?}"
    );
    // The pre-existing violations still don't gate.
    assert!(
        stdout.contains("PASS lint direct_join_to_source: no new violations"),
        "stale violations stay PASS, got: {stdout:?}"
    );
}

#[test]
fn check_lint_format_json_serializes_the_report() {
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--format",
        "json",
    ]);
    assert!(!out.status.success(), "violations still fail the gate");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    assert!(v.get("scope").is_some() && v.get("checks").is_some());
    assert_eq!(v["failures"].as_u64().unwrap(), 6, "6 lint rules fail");
    let checks = v["checks"].as_array().unwrap();
    let djs = checks
        .iter()
        .find(|c| c["check"] == "lint:direct_join_to_source")
        .expect("the lint check item");
    assert_eq!(djs["status"], "fail");
    let findings = djs["findings"].as_array().expect("findings serialize");
    assert_eq!(findings.len(), 16, "the same 16 the line report counts");
    assert_eq!(findings[0]["rule"], "direct_join_to_source");
    assert!(findings[0]["node_uid"].is_string() && findings[0]["message"].is_string());
}

#[test]
fn check_lint_malformed_dbtl_yml_is_fatal() {
    // A `.dbtl.yml` the gate can't interpret must fail loudly — silently
    // linting with defaults would run the wrong rule set. (The config is
    // auto-detected beside the manifest, so the fixture manifest is copied
    // to a tempdir rather than writing into the repo.)
    let m = fixture_manifest();
    let tmp = tempfile::tempdir().unwrap();
    let copied = tmp.path().join("manifest.json");
    std::fs::copy(&m, &copied).unwrap();
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    bogus_rule: false\n",
    )
    .unwrap();
    let out = run(&["check", "--manifest", copied.to_str().unwrap(), "--lint"]);
    assert!(!out.status.success(), "a malformed config is fatal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown lint rule 'bogus_rule'"),
        "the parse reason reaches stderr, got: {stderr:?}"
    );
}

#[test]
fn check_lint_dbtl_yml_disables_rules() {
    // The project-local config disables a rule: the disabled rule prints no
    // line and its violations don't count toward the exit code.
    let m = fixture_manifest();
    let tmp = tempfile::tempdir().unwrap();
    let copied = tmp.path().join("manifest.json");
    std::fs::copy(&m, &copied).unwrap();
    // Disable every failing rule on the fixture → the gate passes.
    let yml = "lint:\n  rules:\n    direct_join_to_source: false\n    \
               multiple_sources_joined: false\n    source_fanout: false\n    \
               model_fanout: false\n    rejoining_upstream: false\n    \
               missing_primary_key_tests: false\n";
    std::fs::write(tmp.path().join(".dbtl.yml"), yml).unwrap();
    let out = run(&["check", "--manifest", copied.to_str().unwrap(), "--lint"]);
    assert!(
        out.status.success(),
        "disabled rules don't gate, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("lint direct_join_to_source:"),
        "a disabled rule prints no line, got: {stdout:?}"
    );
    assert!(stdout.contains("PASS lint undocumented_models: none"));
}
