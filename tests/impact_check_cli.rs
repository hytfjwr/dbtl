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

/// The source-mode dbt project fixture (`stg_orders -> fct_orders ->
/// agg_country_orders`, plus the `orders_dashboard` exposure).
fn sample_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_project")
}

/// A recursive directory copy — `std::fs` has none, and the diff tests need
/// an editable twin of the fixture (same `name: sample` → same unique_ids).
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// The `- name: status` block inside `stg_orders`'s declared columns — the
/// exact bytes the source fixture carries, so `replace` can't drift.
const STG_STATUS_DECL: &str = "      - name: status\n        tests:\n          - accepted_values:\n              values: [\"new\", \"shipped\"]\n";

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
        "scope",
        "added",
        "removed",
        "modified",
        "edges_added",
        "edges_removed",
        "column_changes",
        "pr",
    ] {
        assert!(v.get(key).is_some(), "DiffReport field {key:?} present");
    }
    let pr = &v["pr"];
    for key in [
        "affected_models",
        "affected_marts",
        "affected_exposures",
        "affected_owners",
        "ci_command",
        "column_affected_models",
        "ci_command_columns",
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

// ---- `impact`/`check` column-level diff -----------------------------------

/// A sample_project copy with `stg_orders.status` REMOVED (the declared
/// column is the resolver's top tier — dropping the schema.yml entry drops
/// the output). `fct_orders`'s own declared `status` survives but its
/// `select o.*` no longer carries one in: the broken consumer.
fn project_without_stg_status(tmp: &tempfile::TempDir) -> PathBuf {
    let proj = tmp.path().join("proj");
    copy_dir(&sample_project(), &proj);
    let schema = proj.join("models/staging/schema.yml");
    let text = std::fs::read_to_string(&schema).unwrap();
    assert!(
        text.contains(STG_STATUS_DECL),
        "the fixture's status block is intact"
    );
    std::fs::write(&schema, text.replace(STG_STATUS_DECL, "")).unwrap();
    proj
}

#[test]
fn impact_markdown_renders_the_column_changes_section() {
    // Golden-ish coverage of the shared report payload: the same
    // `pr_impact_markdown` the `D` modal's `e` export writes, driven by a
    // real column removal through the source-mode resolver.
    let tmp = tempfile::tempdir().unwrap();
    let proj = project_without_stg_status(&tmp);
    let out = run(&[
        "impact",
        "--source",
        proj.to_str().unwrap(),
        "--diff",
        sample_project().to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "impact exits 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "## Column changes (1)",
        "- `stg_orders.status`: removed - **breaking**",
        // cll_demo + cll_syntax still WRITE `status` refs — the queries can't
        // compile, so every column they emit dies. fct_orders only loses
        // `status` itself (declared, but the `o.*` that fed it shrank) — its
        // other columns live on.
        "  - downstream cols (12): `cll_demo.n_orders`, `cll_demo.order_id`, \
         `cll_demo.status`, `cll_demo.status_upper`, `cll_syntax.country`, \
         `cll_syntax.jp_id`, `cll_syntax.order_id`, `cll_syntax.order_id_next`, \
         `cll_syntax.rn`, `cll_syntax.status`, `cll_syntax.status_upper`, \
         `fct_orders.status`",
        "  - affected models: cll_demo, cll_syntax, fct_orders",
        "  - exposures: orders_dashboard",
        // agg_country_orders (blast-radius member) is NOT column-affected —
        // it reads `country`/`count(*)`, never `status`. That's the
        // narrowing the column analysis buys.
        "column-level (3 models reached via changed columns)",
        "dbt build --select cll_demo cll_syntax fct_orders stg_orders",
    ] {
        assert!(stdout.contains(needle), "missing {needle:?} in:\n{stdout}");
    }
}

#[test]
fn impact_json_carries_column_changes_and_the_narrow_selector() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = project_without_stg_status(&tmp);
    let out = run(&[
        "impact",
        "--source",
        proj.to_str().unwrap(),
        "--diff",
        sample_project().to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let changes = v["column_changes"].as_array().expect("column_changes");
    assert_eq!(changes.len(), 1, "one classified row: {changes:?}");
    let row = &changes[0];
    assert_eq!(row["node_name"], "stg_orders");
    assert_eq!(row["column"], "status");
    assert_eq!(row["kind"], "removed");
    assert_eq!(row["class"], "breaking");
    assert_eq!(row["affected_columns"].as_array().unwrap().len(), 12);
    assert_eq!(
        row["affected_columns"].as_array().unwrap().last().unwrap(),
        "fct_orders.status"
    );
    assert!(row["affected_exposures"][0]
        .as_str()
        .unwrap()
        .starts_with("orders_dashboard"));
    // The narrowed selector: the changed node PLUS exactly the
    // column-reached models (not the `+` fan-out the model-granular command
    // emits — agg_country_orders drops out entirely).
    assert_eq!(v["pr"]["column_affected_models"], 3);
    assert_eq!(
        v["pr"]["ci_command_columns"].as_str().unwrap(),
        "dbt build --select cll_demo cll_syntax fct_orders stg_orders"
    );
}

#[test]
fn impact_narrow_selector_keeps_fanout_for_column_invisible_sql() {
    // A WHERE-clause edit produces `SQL changed` with ZERO column rows —
    // the change escapes the column surface entirely, so the narrowed
    // selector must keep the `+` fan-out: `dbt build --select stg_orders`
    // alone would leave fct_orders (and everything downstream) stale.
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    copy_dir(&sample_project(), &proj);
    let sql = proj.join("models/staging/stg_orders.sql");
    let text = std::fs::read_to_string(&sql).unwrap();
    std::fs::write(&sql, format!("{text}\nwhere disabled_flag = true\n")).unwrap();
    let out = run(&[
        "impact",
        "--source",
        proj.to_str().unwrap(),
        "--diff",
        sample_project().to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(
        out.status.success(),
        "impact exits 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(
        v["pr"]["ci_command_columns"].as_str().unwrap(),
        "dbt build --select stg_orders+",
        "a column-invisible SQL change keeps the fan-out: {v}"
    );
}

#[test]
fn check_no_breaking_fails_on_a_referenced_column_removal() {
    // The same removal: `--no-breaking` gates on exactly the dangling set.
    let tmp = tempfile::tempdir().unwrap();
    let proj = project_without_stg_status(&tmp);
    let out = run(&[
        "check",
        "--source",
        proj.to_str().unwrap(),
        "--diff",
        sample_project().to_str().unwrap(),
        "--no-breaking",
    ]);
    assert!(
        !out.status.success(),
        "the gate must fail, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .contains("FAIL 1 breaking column change(s) referenced downstream: stg_orders.status"),
        "the failing line names the column, got: {stdout:?}"
    );
}

#[test]
fn check_no_breaking_passes_when_nothing_dangles() {
    // A `data_type` move is partial-breaking — reported, never gated. The
    // same copy shape: only stg_orders.id's declared type changes.
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    copy_dir(&sample_project(), &proj);
    let schema = proj.join("models/staging/schema.yml");
    let text = std::fs::read_to_string(&schema).unwrap();
    std::fs::write(
        &schema,
        text.replacen("data_type: integer", "data_type: bigint", 1),
    )
    .unwrap();
    let out = run(&[
        "check",
        "--source",
        proj.to_str().unwrap(),
        "--diff",
        sample_project().to_str().unwrap(),
        "--no-breaking",
    ]);
    assert!(
        out.status.success(),
        "a partial change never trips the gate, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PASS breaking column changes: none"),
        "the pass line, got: {stdout:?}"
    );
    assert!(
        !stdout.contains("FAIL breaking column"),
        "no breaking FAIL, got: {stdout:?}"
    );
}

#[test]
fn check_no_breaking_without_diff_notes_the_skip() {
    // A silent no-op gate is the worst CI pattern — say what was skipped.
    let m = fixture_manifest();
    let out = run(&["check", "--manifest", m.to_str().unwrap(), "--no-breaking"]);
    assert!(
        out.status.success(),
        "no gates engaged, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("note: --no-breaking needs --diff"),
        "the skip note, got: {stdout:?}"
    );
}

#[test]
fn check_no_breaking_rename_prints_old_to_new() {
    // `status -> order_status` via an explicit alias: fct_orders reads
    // `o.*`, so its declared `status` output dangles under the old name →
    // the gate fires, and the FAIL line prints `old -> new` so the log
    // shows the migration to ask for.
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    copy_dir(&sample_project(), &proj);
    let schema = proj.join("models/staging/schema.yml");
    let text = std::fs::read_to_string(&schema).unwrap();
    std::fs::write(
        &schema,
        text.replace(STG_STATUS_DECL, "      - name: order_status\n"),
    )
    .unwrap();
    let sql = proj.join("models/staging/stg_orders.sql");
    std::fs::write(
        &sql,
        "select\n    id,\n    status as order_status,\n    country_code,\n    disabled_flag\nfrom {{ source('raw', 'orders') }}\n",
    )
    .unwrap();
    let out = run(&[
        "check",
        "--source",
        proj.to_str().unwrap(),
        "--diff",
        sample_project().to_str().unwrap(),
        "--no-breaking",
    ]);
    assert!(
        !out.status.success(),
        "an unmigrated rename gates, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stg_orders.status -> order_status"),
        "old -> new in the failure line, got: {stdout:?}"
    );
}
