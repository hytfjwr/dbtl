//! `dbtl check`'s configurable gate policy: `.dbtl.yml`'s `check:` section,
//! the `--gate`/named-parameter CLI overrides, `fail_on`, and the skip rows
//! for gates whose input is absent. The `.dbtl.yml` auto-detects beside the
//! resolved manifest, so the tests stage a manifest copy + the config file
//! in a tempdir rather than touching the repo's own `.dbtl.yml`.

mod common;
use common::{
    dbtl_cmd, manifest_path as fixture_manifest, sample_manifest_path as sample_manifest,
};

fn run(args: &[&str]) -> std::process::Output {
    dbtl_cmd()
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

fn run_env(args: &[&str], envs: &[(&str, &std::path::Path)]) -> std::process::Output {
    let mut cmd = dbtl_cmd();
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to run dbtl")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A tempdir holding a copy of the sample manifest plus a `.dbtl.yml` —
/// the auto-detect binds the config to the manifest's directory.
fn staged(tmp: &tempfile::TempDir, yml: &str) -> std::path::PathBuf {
    let manifest = tmp.path().join("manifest.json");
    std::fs::copy(sample_manifest(), &manifest).unwrap();
    if !yml.is_empty() {
        std::fs::write(tmp.path().join(".dbtl.yml"), yml).unwrap();
    }
    manifest
}

/// The sample manifest with `node`'s `raw_code` mutated — a one-node diff
/// vs the pristine fixture (raw SQL only counts when both sides carry it;
/// the fixture does).
fn changed_manifest(tmp: &tempfile::TempDir, node: &str) -> std::path::PathBuf {
    let m = sample_manifest();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&m).unwrap()).unwrap();
    manifest["nodes"][node]["raw_code"] = serde_json::json!("select 2 -- changed");
    let changed = tmp.path().join("changed.json");
    std::fs::write(&changed, serde_json::to_vec(&manifest).unwrap()).unwrap();
    changed
}

// ---- `.dbtl.yml check:` ---------------------------------------------------

#[test]
fn check_gates_config_sets_severity_and_params() {
    // `column_coverage` as an object (severity + min) reports WARN (which
    // `--fail-on error` does not gate on); `doc_coverage` as a bare
    // severity reports but can't trip without a `min` floor.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = staged(
        &tmp,
        "check:\n  gates:\n    column_coverage:\n      severity: warn\n      min: 90\n    doc_coverage: error\n",
    );
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "a warn trip never gates at fail_on=error, stderr: {}",
        stderr(&out)
    );
    let stdout = stdout(&out);
    assert!(
        stdout.contains("WARN column coverage 26% below the 90% minimum (4/15)"),
        "the configured warn row, got: {stdout:?}"
    );
    assert!(
        stdout.contains("PASS doc coverage:"),
        "the enabled doc gate reports, got: {stdout:?}"
    );
}

#[test]
fn check_lint_in_config_runs_lint_without_the_flag() {
    // `check.lint: true` folds the lint gates into `check` — the same rows
    // `--lint` produces, without passing it.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = staged(&tmp, "check:\n  lint: true\n");
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    let stdout = stdout(&out);
    assert!(
        stdout.contains("lint direct_join_to_source:"),
        "lint rows without --lint, got: {stdout:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "the fixture's lint violations still gate, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn check_gates_param_only_entry_enables_a_default_off_gate() {
    // `blast_radius: {max: 25}` — no severity key — still turns the gate
    // on (at error): a configured parameter is an explicit ask, same as
    // `--max-blast-radius`.
    let tmp = tempfile::tempdir().unwrap();
    staged(&tmp, "check:\n  gates:\n    blast_radius:\n      max: 25\n");
    let changed = changed_manifest(&tmp, "model.sample.stg_orders");
    let base = sample_manifest();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--diff",
        base.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "6 affected <= max 25, stderr: {}",
        stderr(&out)
    );
    let stdout = stdout(&out);
    assert!(
        stdout.contains("PASS blast radius: 6 affected model(s) (max 25)"),
        "the configured ceiling reports, got: {stdout:?}"
    );
}

#[test]
fn check_gates_unknown_gate_and_param_are_usage_errors() {
    for (yml, needle) in [
        (
            "check:\n  gates:\n    bogus_gate: warn\n",
            "unknown check gate 'bogus_gate'",
        ),
        (
            "check:\n  gates:\n    coverage: {min: 50, bogus: 1}\n",
            "unknown parameter 'bogus' for gate 'coverage'",
        ),
        (
            "check:\n  gates:\n    coverage: {min: 200}\n",
            "must be an integer in [0, 100]",
        ),
        (
            "check:\n  gates:\n    coverage: sideways\n",
            "must be one of error|warn|off",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = staged(&tmp, yml);
        let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{yml:?} is a usage error, stderr: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains(needle),
            "{yml:?} → {needle:?} in stderr, got: {:?}",
            stderr(&out)
        );
    }
}

// ---- CLI overrides ---------------------------------------------------------

#[test]
fn check_gate_flag_off_warn_and_error() {
    let m = fixture_manifest();
    // `=off` removes the row entirely — even for the default-on coverage.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--gate",
        "coverage=off",
    ]);
    assert!(out.status.success());
    let off = stdout(&out);
    assert!(
        !off.contains("coverage"),
        "an off gate prints no row, got: {off:?}"
    );

    // `=warn` reports but never gates at the default `--fail-on error`.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--coverage-min",
        "100",
        "--gate",
        "coverage=warn",
    ]);
    assert!(
        out.status.success(),
        "warn trips don't fail: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("WARN coverage"));

    // `--fail-on warn` still counts it.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--coverage-min",
        "100",
        "--gate",
        "coverage=warn",
        "--fail-on",
        "warn",
    ]);
    assert_eq!(out.status.code(), Some(1), "warn + fail-on warn gates");
}

#[test]
fn check_named_param_flags_enable_gates_and_override_config() {
    // `.dbtl.yml` sets min 10 (passes at 26%); the flag's 90 wins → FAIL.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = staged(
        &tmp,
        "check:\n  gates:\n    column_coverage:\n      min: 10\n",
    );
    let out = run(&[
        "check",
        "--manifest",
        manifest.to_str().unwrap(),
        "--column-coverage-min",
        "90",
    ]);
    assert_eq!(out.status.code(), Some(1), "the flag param wins");
    let text = stdout(&out);
    assert!(
        text.contains("FAIL column coverage 26% below the 90% minimum"),
        "got: {text:?}"
    );
    // And `--gate` is the last word: `=off` after the flag removes it.
    let out = run(&[
        "check",
        "--manifest",
        manifest.to_str().unwrap(),
        "--column-coverage-min",
        "90",
        "--gate",
        "column_coverage=off",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        !stdout(&out).contains("column coverage"),
        "--gate off beats the flag's implied enable"
    );
}

#[test]
fn check_doc_coverage_min_flag_reports_and_gates() {
    // The sample project documents 6 of its 10 models: a 90% floor fails.
    let m = sample_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--doc-coverage-min",
        "90",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = stdout(&out);
    assert!(
        stdout.contains("FAIL doc coverage 60% below the 90% minimum (6/10)"),
        "got: {stdout:?}"
    );
}

#[test]
fn check_gate_flag_rejects_malformed_values() {
    let m = fixture_manifest();
    for spec in ["notagate=warn", "coverage=bogus", "coverage"] {
        let out = run(&["check", "--manifest", m.to_str().unwrap(), "--gate", spec]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "--gate {spec:?} is a usage error"
        );
    }
}

// ---- diff gates ------------------------------------------------------------

#[test]
fn check_explicitly_enabled_diff_gate_skips_without_diff() {
    // `--gate blast_radius=error` with no `--diff`: the gate reports
    // `skip`, never a silent no-op and never a failure.
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--gate",
        "blast_radius=error",
    ]);
    assert!(
        out.status.success(),
        "a skipped gate doesn't fail, stderr: {}",
        stderr(&out)
    );
    let stdout = stdout(&out);
    assert!(
        stdout.contains("SKIP blast radius: needs --diff"),
        "the skip row, got: {stdout:?}"
    );
}

#[test]
fn check_blast_radius_and_diff_gates_evaluate() {
    // stg_orders changes → 6-model blast radius, an 8-downstream hub, and
    // the orders_dashboard exposure: every diff gate fires on real data.
    let tmp = tempfile::tempdir().unwrap();
    let changed = changed_manifest(&tmp, "model.sample.stg_orders");
    let base = sample_manifest();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--diff",
        base.to_str().unwrap(),
        "--gate",
        "blast_radius=error",
        "--gate",
        "affected_exposures=error",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = stdout(&out);
    for needle in [
        "WARN 1 changed hub(s) (>= 5 downstream): stg_orders (8)",
        "FAIL 1 affected exposure(s): orders_dashboard",
        "FAIL blast radius 6 affected model(s) exceeds max 0",
    ] {
        assert!(stdout.contains(needle), "missing {needle:?} in:\n{stdout}");
    }
}

#[test]
fn check_untested_changes_gates_on_a_changed_leaf() {
    // agg_country_orders carries no tests — changing it trips the
    // default-on `untested_changes` gate.
    let tmp = tempfile::tempdir().unwrap();
    let changed = changed_manifest(&tmp, "model.sample.agg_country_orders");
    let base = sample_manifest();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--diff",
        base.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("FAIL 1 changed node(s) without tests: agg_country_orders"),
        "got: {:?}",
        stdout(&out)
    );
}

#[test]
fn check_max_blast_radius_flag_boundary() {
    // 6 affected models: `--max-blast-radius 6` passes (<=), `5` fails.
    let tmp = tempfile::tempdir().unwrap();
    let changed = changed_manifest(&tmp, "model.sample.stg_orders");
    let base = sample_manifest();
    for (max, code, expect) in [("6", 0, "PASS"), ("5", 1, "FAIL")] {
        let out = run(&[
            "check",
            "--manifest",
            changed.to_str().unwrap(),
            "--diff",
            base.to_str().unwrap(),
            "--max-blast-radius",
            max,
        ]);
        assert_eq!(
            out.status.code(),
            Some(code),
            "--max-blast-radius {max}: {}",
            stderr(&out)
        );
        assert!(
            stdout(&out).contains(&format!("{expect} blast radius")),
            "max {max} → {expect}, got: {:?}",
            stdout(&out)
        );
    }
}

#[test]
fn check_changed_hubs_threshold_matches_the_impact_report() {
    // The gate's `min_downstream` is the SAME number the report's hub list
    // uses — set it above stg_orders's 8 and BOTH must drop the hub.
    let tmp = tempfile::tempdir().unwrap();
    staged(
        &tmp,
        "check:\n  gates:\n    changed_hubs:\n      min_downstream: 9\n",
    );
    let changed = changed_manifest(&tmp, "model.sample.stg_orders");
    let base = sample_manifest();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--diff",
        base.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    // `.dbtl.yml` bound to `changed.json`'s dir — the staged manifest's.
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["impact"]["pr"]["changed_hubs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "min_downstream 9 hides an 8-fanout hub everywhere: {v}"
    );
    let hubs = v["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "changed_hubs")
        .expect("the gate row");
    assert_eq!(hubs["message"], "none");
}

// ---- fail_on / owner_meta_key precedence -----------------------------------

#[test]
fn check_fail_on_from_config_and_flag() {
    let tmp = tempfile::tempdir().unwrap();
    // `fail_on: never` — even an error-severity trip reports WARN, exit 0.
    let manifest = staged(
        &tmp,
        "fail_on: never\ncheck:\n  gates:\n    column_coverage:\n      min: 99\n",
    );
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "fail_on: never keeps the exit clean, stderr: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("WARN column coverage"));
    // `fail_on: warn` — a warn-severity trip counts.
    let manifest = staged(
        &tmp,
        "fail_on: warn\ncheck:\n  gates:\n    column_coverage:\n      severity: warn\n      min: 99\n",
    );
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    // And the `--fail-on` flag beats the file.
    let out = run(&[
        "check",
        "--manifest",
        manifest.to_str().unwrap(),
        "--fail-on",
        "error",
    ]);
    assert!(
        out.status.success(),
        "--fail-on error demotes the warn trip, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn check_owner_meta_key_dbtl_yml_beats_config_yml() {
    // `.dbtl.yml`'s `owner_meta_key` outranks `config.yml`'s — `team` vs
    // `squad`, so the owners line reads meta.team (`analytics-eng`).
    let tmp = tempfile::tempdir().unwrap();
    staged(&tmp, "owner_meta_key: team\n");
    let changed = changed_manifest(&tmp, "model.sample.stg_orders");
    let cfg_root = tmp.path().join("cfg");
    std::fs::create_dir_all(cfg_root.join("dbtl")).unwrap();
    std::fs::write(cfg_root.join("dbtl/config.yml"), "owner_meta_key: squad\n").unwrap();
    let base = sample_manifest();
    let out = run_env(
        &[
            "check",
            "--manifest",
            changed.to_str().unwrap(),
            "--diff",
            base.to_str().unwrap(),
        ],
        &[("XDG_CONFIG_HOME", cfg_root.as_path())],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("affected owners: analytics-eng"),
        "the .dbtl.yml key wins, got: {:?}",
        stdout(&out)
    );
}

// ---- docs_drift --------------------------------------------------------------

#[test]
fn check_docs_drift_gate() {
    // `docs_drift.dir` resolves relative to `.dbtl.yml` — generate the
    // docs into that relative dir, then drift it.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.json");
    std::fs::copy(sample_manifest(), &manifest).unwrap();
    let gen = run(&[
        "docs",
        "--manifest",
        manifest.to_str().unwrap(),
        "--out",
        tmp.path().join("site").to_str().unwrap(),
    ]);
    assert!(gen.status.success(), "docs generates: {}", stderr(&gen));
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "check:\n  gates:\n    docs_drift:\n      dir: site\n",
    )
    .unwrap();
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    assert!(out.status.success(), "fresh docs pass: {}", stderr(&out));
    assert!(
        stdout(&out).contains("PASS docs drift: up to date"),
        "got: {:?}",
        stdout(&out)
    );
    // A tampered file trips the gate with the changed path as a detail.
    let page = tmp.path().join("site/models/model.sample.fct_orders.md");
    std::fs::write(
        &page,
        format!("{}\ntampered\n", std::fs::read_to_string(&page).unwrap()),
    )
    .unwrap();
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let drift = stdout(&out);
    assert!(
        drift.contains("FAIL docs drift: 1 difference(s)")
            && drift.contains("changed: models/model.sample.fct_orders.md"),
        "the drift detail names the file, got: {drift:?}"
    );
    // An enabled gate nobody pointed at a directory skips, never fails.
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "check:\n  gates:\n    docs_drift: error\n",
    )
    .unwrap();
    let out = run(&["check", "--manifest", manifest.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("SKIP docs drift: needs --docs-dir or check.gates.docs_drift.dir"),
        "got: {:?}",
        stdout(&out)
    );
}

// ---- report consistency -----------------------------------------------------

#[test]
fn check_json_report_serializes_gates_and_impact() {
    // The `--format json` report is the shared `Report`: per-gate
    // id/severity/status plus the SAME `impact` payload `dbtl impact`
    // prints — so the gate rows and the analysis can't disagree.
    let tmp = tempfile::tempdir().unwrap();
    let changed = changed_manifest(&tmp, "model.sample.stg_orders");
    let base = sample_manifest();
    let out = run(&[
        "check",
        "--manifest",
        changed.to_str().unwrap(),
        "--diff",
        base.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "dbtl.report/v1");
    let gates: std::collections::BTreeMap<_, _> = v["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["id"].as_str().unwrap().to_string(), g))
        .collect();
    for id in [
        "coverage",
        "new_layer_violations",
        "untested_changes",
        "changed_hubs",
        "affected_exposures",
    ] {
        assert!(gates.contains_key(id), "a {id} gate row");
    }
    assert_eq!(gates["changed_hubs"]["status"], "warn");
    assert_eq!(gates["affected_exposures"]["severity"], "warn");
    // The impact block IS `dbtl impact`'s DiffReport — same numbers.
    assert_eq!(v["impact"]["pr"]["affected_models"], 6);
    assert_eq!(v["impact"]["baseline"], base.to_str().unwrap());
}
