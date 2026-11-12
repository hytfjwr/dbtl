//! CLI-level coverage for the `dbtl lint` subcommand and the severity-aware
//! lint gate `check --lint` shares: the exit-code classes (0 pass / 1 gate /
//! 2 usage / 3 input), `--fail-on` precedence, the baseline file lifecycle,
//! `--diff`'s new-violation narrowing, and the `--list-rules`/`--explain`
//! query modes — all through the real binary, no TTY involved.

mod common;

use common::{dbtl_cmd, manifest_path as fixture_manifest, sample_manifest_path, SAMPLE_PROJECT};
use std::path::{Path, PathBuf};

fn run(args: &[&str]) -> std::process::Output {
    dbtl_cmd()
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

/// `run` with the working dir pinned — query-mode tests need an empty
/// directory so auto-detect can't find a data source.
fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    dbtl_cmd()
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

fn out_text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Copy the fixture manifest into `dir` and return its path — tests that
/// place `.dbtl.yml` / baseline files need an owned directory to write in
/// (the config lives next to the data source).
fn manifest_in(dir: &Path) -> PathBuf {
    let dst = dir.join("manifest.json");
    std::fs::copy(fixture_manifest(), &dst).unwrap();
    dst
}

fn lint_on(manifest: &Path, extra: &[&str]) -> std::process::Output {
    let mut args: Vec<&str> = vec!["lint", "--manifest"];
    let m = manifest.to_str().unwrap();
    args.push(m);
    args.extend_from_slice(extra);
    run(&args)
}

// ---- the gate exit code: findings + fail_on -------------------------------

#[test]
fn error_findings_exit_one_and_warn_only_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "error findings fail by default, stderr: {}",
        stderr(&out)
    );
    let stdout = out_text(&out);
    assert!(stdout.contains("error["), "verdict line: {stdout}");

    // Downgrade every firing rule to warn → only warnings remain → pass.
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    direct_join_to_source: {severity: warn}\n    multiple_sources_joined: {severity: warn}\n    source_fanout: {severity: warn}\n    model_fanout: {severity: warn}\n    rejoining_upstream: {severity: warn}\n    missing_primary_key_tests: {severity: warn}\n",
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "warn-only findings report but pass, stderr: {}",
        stderr(&out)
    );
    let text = out_text(&out);
    assert!(text.contains("warning["), "verdict line: {text}");
    // …but `--fail-on warn` fails on them again.
    let out = lint_on(&m, &["--fail-on", "warn"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    // …and `--fail-on never` reports without failing even errors.
    std::fs::remove_file(tmp.path().join(".dbtl.yml")).unwrap();
    let out = lint_on(&m, &["--fail-on", "never"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "never is report-only, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn cli_fail_on_beats_the_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    // `fail_on: warn` in the file fails on the fixture's error findings.
    std::fs::write(tmp.path().join(".dbtl.yml"), "fail_on: warn\n").unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the file's fail_on applies, stderr: {}",
        stderr(&out)
    );
    // `fail_on: never` in the file passes even with error findings…
    std::fs::write(tmp.path().join(".dbtl.yml"), "fail_on: never\n").unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    // …but the CLI flag wins over the file either way.
    let out = lint_on(&m, &["--fail-on", "error"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "CLI --fail-on beats the file, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn cli_fail_on_beats_file_fail_on_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    // Every firing rule at warn + `fail_on: warn` in the file: the warnings
    // themselves fail the run under the file's threshold.
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "fail_on: warn\nlint:\n  rules:\n    direct_join_to_source: {severity: warn}\n    multiple_sources_joined: {severity: warn}\n    source_fanout: {severity: warn}\n    model_fanout: {severity: warn}\n    rejoining_upstream: {severity: warn}\n    missing_primary_key_tests: {severity: warn}\n",
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the file's fail_on: warn gates warn findings, stderr: {}",
        stderr(&out)
    );
    // The CLI flag wins back the other direction too: error-only gating
    // ignores the warnings entirely.
    let out = lint_on(&m, &["--fail-on", "error"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "CLI --fail-on error beats the file's warn, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn warn_findings_report_with_their_severity() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    direct_join_to_source: {severity: warn}\n",
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    // The finding header carries the configured severity tag…
    assert!(
        stdout.contains("warning[direct_join_to_source]:"),
        "warn-severity finding headers: {stdout}"
    );
    // …and the block carries its position anchor + fix guidance.
    assert!(
        stdout.contains("--> ") && stdout.contains("= help:"),
        "an annotated block per finding: {stdout}"
    );
}

// ---- usage errors: exit 2 --------------------------------------------------

#[test]
fn usage_errors_exit_two() {
    let m = fixture_manifest();
    let m_str = m.to_str().unwrap();
    for args in [
        // An unknown rule name.
        vec!["lint", "--manifest", m_str, "--rules", "not_a_rule"],
        // An unknown --explain id.
        vec!["lint", "--explain", "not_a_rule"],
        // --list-rules / --explain are mutually exclusive.
        vec!["lint", "--list-rules", "--explain", "root_models"],
        // --write-baseline is a snapshot — it can't combine with query
        // modes or with --diff.
        vec!["lint", "--list-rules", "--write-baseline"],
        vec![
            "lint",
            "--manifest",
            m_str,
            "--write-baseline",
            "b.json",
            "--diff",
            m_str,
        ],
        // Flags that only shape a loaded graph have no effect under the
        // query modes — they are rejected, not silently dropped, on either
        // side of the subcommand.
        vec!["lint", "--list-rules", "--diff", m_str],
        vec!["--diff", m_str, "lint", "--list-rules"],
        vec!["lint", "--explain", "root_models", "--diff", m_str],
        vec!["lint", "--list-rules", "--include-packages"],
        // Global flags lint doesn't consume. (`--catalog`/`--sql-dialect`
        // ARE consumed: they feed the SQL/catalog-backed rules.)
        vec!["lint", "--manifest", m_str, "--run-results", "r.json"],
    ] {
        let out = run(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} is a usage error, stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn unknown_rule_and_explain_errors_list_the_valid_ids() {
    let m = fixture_manifest();
    for args in [
        vec!["lint", "--manifest", m.to_str().unwrap(), "--rules", "nope"],
        vec!["lint", "--explain", "nope"],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
        let err = stderr(&out);
        assert!(
            err.contains("root_models") && err.contains("nope"),
            "the error names the bad id and the valid set: {err}"
        );
    }
}

#[test]
fn unknown_rules_fail_before_the_data_load() {
    // `--rules` validation precedes source resolution — a bad name must be
    // a usage error (2), not masked by the missing manifest's input error.
    let out = run(&[
        "lint",
        "--manifest",
        "/nonexistent/dbtl_lint_input",
        "--rules",
        "not_a_rule",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "usage error before data load, stderr: {}",
        stderr(&out)
    );
    let out = run(&[
        "check",
        "--manifest",
        "/nonexistent/dbtl_lint_input",
        "--rules",
        "not_a_rule",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "check validates first too, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn malformed_config_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    bogus_rule: false\n",
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "malformed .dbtl.yml is usage, stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("bogus_rule"),
        "the parse reason reaches stderr: {}",
        stderr(&out)
    );
    // The query modes load the config too — same classification.
    let out = lint_on(&m, &["--list-rules"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
}

// ---- input errors: exit 3 --------------------------------------------------

#[test]
fn input_errors_exit_three() {
    let out = run(&["lint", "--manifest", "/nonexistent/dbtl_lint_input"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a missing manifest is an input error, stderr: {}",
        stderr(&out)
    );

    // A broken baseline file is input, not usage: a frozen set we can't
    // read must never silently un-freeze every violation.
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(tmp.path().join(".dbtl-baseline.json"), "not json{{").unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a broken baseline is input, stderr: {}",
        stderr(&out)
    );
}

// ---- the baseline file lifecycle -------------------------------------------

#[test]
fn write_baseline_freezes_every_finding_then_passes() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let baseline = tmp.path().join("baseline.json");

    let out = lint_on(&m, &["--write-baseline", baseline.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "writes always exit 0, stderr: {}",
        stderr(&out)
    );
    assert!(
        out_text(&out).contains("wrote") && out_text(&out).contains("finding(s)"),
        "the write message: {}",
        out_text(&out)
    );

    // The written file makes every finding baselined → clean pass.
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        format!("lint:\n  baseline: {}\n", baseline.display()),
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "baselined findings don't fail, stderr: {}",
        stderr(&out)
    );
    let stdout = out_text(&out);
    assert!(stdout.contains("baselined"), "the count shows: {stdout}");

    // `--no-baseline` brings the findings back.
    let out = lint_on(&m, &["--no-baseline"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
}

#[test]
fn write_baseline_is_byte_deterministic_and_defaults_beside_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());

    // Bare `--write-baseline` lands at <manifest dir>/.dbtl-baseline.json.
    let out = lint_on(&m, &["--write-baseline"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let default_path = tmp.path().join(".dbtl-baseline.json");
    let first = std::fs::read(&default_path).expect("baseline written beside the manifest");
    // Same project, second write → byte-identical (deterministic output).
    let out = lint_on(&m, &["--write-baseline"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        std::fs::read(&default_path).unwrap(),
        first,
        "two writes over the same input are byte-identical"
    );
    // …and the default-path file is picked up on the next run with no flag.
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the default baseline path is read back, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn missing_explicit_baseline_warns_but_still_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  baseline: nope.json\n",
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "lint still evaluates without the file, stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("warning"),
        "the missing explicit baseline warns: {}",
        stderr(&out)
    );
}

// ---- --diff: only new violations count --------------------------------------

#[test]
fn check_and_lint_count_the_same_findings() {
    let m = fixture_manifest();
    // `lint --format json`: with no baseline file beside the fixture, the
    // counted set is exactly the non-baselined findings across every gate.
    let out = lint_on(&m, &["--format", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let lint_json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("lint stdout is pure JSON");
    let mut lint_counted: Vec<(String, String)> = lint_json["gates"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| {
            g["findings"]
                .as_array()
                .map(|a| a.iter())
                .unwrap_or_default()
        })
        .filter(|f| f["baselined"] == false)
        .map(|f| {
            (
                f["rule"].as_str().unwrap().to_string(),
                f["node_uid"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    lint_counted.sort();

    // `check --lint --format json`: each lint gate carries exactly the
    // findings that gated — by construction the same counted set.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let check_json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("check stdout is pure JSON");
    let mut check_counted: Vec<(String, String)> = check_json["gates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["id"].as_str().is_some_and(|n| n.starts_with("lint:")))
        .flat_map(|g| {
            g["findings"]
                .as_array()
                .map(|a| a.iter())
                .unwrap_or_default()
        })
        .filter(|f| f["baselined"] == false)
        .map(|f| {
            (
                f["rule"].as_str().unwrap().to_string(),
                f["node_uid"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    check_counted.sort();

    assert_eq!(
        lint_counted, check_counted,
        "lint and check count the same findings from the same input"
    );
}

#[test]
fn baseline_file_and_diff_compose_at_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    // Freeze every current finding at the default path, then self-diff:
    // the frozen findings classify as baselined — the baseline file wins
    // the partition over the diff's pre-existing bucket.
    let out = lint_on(&m, &["--write-baseline"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let out = lint_on(&m, &["--diff", m.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "baselined findings don't count, stderr: {}",
        stderr(&out)
    );
    let stdout = out_text(&out);
    assert!(
        stdout.contains("No findings") && stdout.contains("baselined"),
        "the baselined count reports under --diff: {stdout}"
    );
    assert!(
        !stdout.contains("pre-existing"),
        "frozen findings aren't also counted as pre-existing: {stdout}"
    );
    // `--no-baseline` drops the file → the same findings shift to the
    // diff's pre-existing bucket.
    let out = lint_on(&m, &["--diff", m.to_str().unwrap(), "--no-baseline"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        out_text(&out).contains("pre-existing"),
        "without the file, the diff's pre-existing count reports"
    );
    // `check --lint --diff` reads the same file and lands on the same
    // partition — every suppressed rule reports its baselined count.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        out_text(&out).contains("baselined"),
        "check reports the baselined count too"
    );
}

#[test]
fn diff_counts_only_new_violations() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    // Self-diff: every finding exists on the baseline side → nothing counts.
    let out = lint_on(&m, &["--diff", m.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "no new violations pass, stderr: {}",
        stderr(&out)
    );
    let stdout = out_text(&out);
    assert!(
        stdout.contains("No findings") && stdout.contains("pre-existing"),
        "the diff wording: {stdout}"
    );
}

// ---- --list-rules / --explain ----------------------------------------------

#[test]
fn list_rules_prints_one_row_per_registry_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_in(tmp.path(), &["lint", "--list-rules"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "query mode needs no data source, stderr: {}",
        stderr(&out)
    );
    let stdout = out_text(&out);
    let rows: Vec<&str> = stdout
        .lines()
        .skip(1) // the header
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        rows.len(),
        dbtl::LintRule::ALL.len(),
        "one row per registry rule: {stdout}"
    );
    for rule in dbtl::LintRule::ALL {
        let row = rows
            .iter()
            .find(|r| r.starts_with(rule.name()))
            .unwrap_or_else(|| panic!("{} has a row: {stdout}", rule.name()));
        assert!(
            row.contains(rule.info().category.name()),
            "category column: {row}"
        );
        assert!(
            row.contains("error") || row.contains("warn") || row.contains("off"),
            "severity column: {row}"
        );
        assert!(row.contains(rule.info().summary), "summary column: {row}");
    }
}

#[test]
fn list_rules_shows_configured_severities() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    root_models: {severity: warn}\n    source_fanout: false\n",
    )
    .unwrap();
    // Query mode reads the config next to the data source but never loads
    // the data itself — --manifest only decides where .dbtl.yml lives.
    let out = lint_on(&m, &["--list-rules"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    let row = |name: &str| {
        stdout
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("{name} row: {stdout}"))
    };
    assert!(row("root_models").contains("warn"), "warn severity shows");
    assert!(row("source_fanout").contains("off"), "off severity shows");
}

#[test]
fn explain_prints_the_registry_entry_and_effective_values() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    model_fanout: {threshold: 3}\n",
    )
    .unwrap();
    let out = lint_on(&m, &["--explain", "model_fanout"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    for heading in [
        "id: model_fanout",
        "category:",
        "severity:",
        "requires:",
        "Rationale:",
        "Example:",
        "Help:",
        "Params:",
        "Docs:",
    ] {
        assert!(
            stdout.contains(heading),
            "explain shows {heading}: {stdout}"
        );
    }
    // The param row carries the default and the current (configured) value.
    assert!(
        stdout.contains("default") && stdout.contains("current 3"),
        "param defaults and current values: {stdout}"
    );
    // The fix guidance is the registry's help, and the docs link anchors
    // at the rule's own id.
    assert!(
        stdout.contains(dbtl::LintRule::ModelFanout.info().help),
        "the Help: body: {stdout}"
    );
    assert!(
        stdout.contains("https://hytfjwr.github.io/dbtl/reference/lint-rules/#model_fanout"),
        "the Docs: URL: {stdout}"
    );
}

// ---- text / json output and --quiet -----------------------------------------

#[test]
fn json_output_is_machine_readable_and_stdout_clean() {
    let m = fixture_manifest();
    let out = lint_on(&m, &["--format", "json"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout is pure JSON — no log lines mixed in");
    assert_eq!(json["schema"], "dbtl.report/v1", "the schema marker");
    for key in ["tool_version", "project", "scope", "summary"] {
        assert!(json.get(key).is_some(), "report carries {key}");
    }
    let gates = json["gates"].as_array().expect("gates array");
    assert!(!gates.is_empty(), "gates present");
    let findings: Vec<&serde_json::Value> = gates
        .iter()
        .flat_map(|g| {
            g["findings"]
                .as_array()
                .map(|a| a.iter())
                .unwrap_or_default()
        })
        .collect();
    assert!(!findings.is_empty(), "findings present");
    for f in &findings {
        for key in [
            "rule",
            "node_uid",
            "related",
            "severity",
            "baselined",
            "message",
            "location",
        ] {
            assert!(f.get(key).is_some(), "finding carries {key}: {f}");
        }
    }
    let summary = &json["summary"];
    for key in ["errors", "warnings", "baselined", "skipped"] {
        assert!(summary.get(key).is_some(), "summary carries {key}");
    }
    assert!(summary["errors"].as_u64().unwrap() > 0, "errors counted");
}

#[test]
fn quiet_suppresses_stdout_but_keeps_the_exit_code() {
    let m = fixture_manifest();
    let out = lint_on(&m, &["--quiet"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(out.stdout.is_empty(), "stdout is silent under --quiet");
    // …even for a passing run (the exit code carries the verdict).
    let out = lint_on(&m, &["--quiet", "--fail-on", "never"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(out.stdout.is_empty());
}

#[test]
fn text_output_is_deterministic_across_runs() {
    let m = fixture_manifest();
    let a = lint_on(&m, &[]);
    let b = lint_on(&m, &[]);
    assert_eq!(a.stdout, b.stdout, "same input → byte-identical stdout");
    // One annotated block per counted finding — `severity[rule]: message`
    // over a position line + fix footer.
    let stdout = out_text(&a);
    assert!(
        stdout.contains("error[direct_join_to_source]:"),
        "finding blocks: {stdout}"
    );
}

#[test]
fn no_format_invents_a_line_one_for_file_scoped_findings() {
    // A `.sql` (or unreadable `.yml`) finding names its file only — the
    // human formats never guess line 1 (SARIF/codeclimate's required
    // stand-ins are the documented exception, covered by their own
    // tests).
    let m = fixture_manifest();
    let text = out_text(&lint_on(&m, &["--format", "text"]));
    assert!(
        !text.contains(".sql:1") && !text.contains(".yml:1"),
        "no :1 anchor in text: {text}"
    );
    let github = out_text(&lint_on(&m, &["--format", "github"]));
    assert!(
        !github.contains("line=1,") && !github.contains(",line=1"),
        "no line=1 prop in github annotations: {github}"
    );
    let json = out_text(&lint_on(&m, &["--format", "json"]));
    assert!(
        !json.contains("\"line\":1") && !json.contains("\"line\": 1"),
        "no \"line\": 1 in the json report"
    );
}

// ---- check shares the same gate semantics ----------------------------------

#[test]
fn check_lint_gate_is_severity_aware() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    // `never`: every tripped gate WARNs instead of FAILing — the coverage
    // shortfall (fixture isn't 100% tested) included.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--coverage-min",
        "100",
        "--fail-on",
        "never",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "never reports without failing, stderr: {}",
        stderr(&out)
    );
    let stdout = out_text(&out);
    assert!(
        stdout.lines().any(|l| l.starts_with("WARN coverage")),
        "coverage demoted to WARN: {stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.starts_with("WARN lint ")),
        "lint rules demoted to WARN: {stdout}"
    );
    assert!(!stdout.contains("FAIL"), "no FAIL lines remain: {stdout}");

    // `warn` fails on warnings: a warn-severity rule trips it.
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    direct_join_to_source: {severity: warn}\n    multiple_sources_joined: {severity: warn}\n    source_fanout: {severity: warn}\n    model_fanout: {severity: warn}\n    rejoining_upstream: {severity: warn}\n    missing_primary_key_tests: {severity: warn}\n",
    )
    .unwrap();
    let out = run(&["check", "--manifest", m.to_str().unwrap(), "--lint"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "warn-only findings pass under the default, stderr: {}",
        stderr(&out)
    );
    let text = out_text(&out);
    assert!(text.contains("WARN lint "), "WARN line: {text}");
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--fail-on",
        "warn",
    ]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "warn gates under --fail-on warn, stderr: {}",
        stderr(&out)
    );
    assert!(out_text(&out).contains("FAIL lint "), "FAIL line");
}

#[test]
fn check_json_carries_warn_statuses_and_finding_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    direct_join_to_source: {severity: warn}\n    multiple_sources_joined: {severity: warn}\n    source_fanout: {severity: warn}\n    model_fanout: {severity: warn}\n    rejoining_upstream: {severity: warn}\n    missing_primary_key_tests: {severity: warn}\n",
    )
    .unwrap();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--format",
        "json",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("stdout is pure JSON");
    assert!(
        json["summary"]["warnings"].as_u64().unwrap_or(0) > 0,
        "WARN gates count in the summary: {json}"
    );
    let gates = json["gates"].as_array().unwrap();
    let lint_warn = gates
        .iter()
        .find(|g| g["id"].as_str().is_some_and(|n| n.starts_with("lint:")) && g["status"] == "warn")
        .expect("a warn-status lint gate exists");
    let finding = &lint_warn["findings"].as_array().unwrap()[0];
    for key in [
        "severity",
        "baselined",
        "location",
        "rule",
        "node_uid",
        "related",
    ] {
        assert!(
            finding.get(key).is_some(),
            "finding carries the new fields ({key}): {finding}"
        );
    }
}

// ---- requirement-aware skipping ---------------------------------------------

/// A catalog.json declaring every column of `uid`; written next to `manifest`
/// so the sibling auto-detect finds it.
fn catalog_in(dir: &Path, manifest: &Path) {
    let m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).unwrap()).unwrap();
    let mut nodes = serde_json::Map::new();
    for (uid, node) in m["nodes"].as_object().unwrap() {
        if node["resource_type"] != "model" || node["columns"].as_object().is_none() {
            continue;
        }
        let cols: serde_json::Map<String, serde_json::Value> = node["columns"]
            .as_object()
            .unwrap()
            .keys()
            .map(|c| (c.clone(), serde_json::json!({"type": "TEXT"})))
            .collect();
        nodes.insert(uid.clone(), serde_json::json!({"columns": cols}));
    }
    std::fs::write(
        dir.join("catalog.json"),
        serde_json::json!({"nodes": nodes}).to_string(),
    )
    .unwrap();
}

#[test]
fn catalog_rules_skip_without_a_catalog_and_report_with_one() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());

    // No catalog.json beside the manifest → the two catalog-backed rules
    // report as SKIP gates instead of running — never a failure.
    let out = lint_on(&m, &["--fail-on", "never"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = out_text(&out);
    assert!(
        text.contains("2 skipped (need --catalog)"),
        "the skipped-rule tally lands on the summary line: {text}"
    );

    // A sibling catalog.json is auto-detected → both rules evaluate.
    catalog_in(tmp.path(), &m);
    let out = lint_on(&m, &["--fail-on", "never"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        !out_text(&out).contains("skipped"),
        "with a catalog nothing skips: {}",
        out_text(&out)
    );
}

#[test]
fn check_reports_skipped_lint_rules_as_skip_rows() {
    let m = fixture_manifest();
    // No catalog sibling → SKIP lint <rule>: <reason>, exit per findings only.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--fail-on",
        "never",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "skips never fail the run, stderr: {}",
        stderr(&out)
    );
    let text = out_text(&out);
    assert!(
        text.contains("SKIP lint warehouse_type_drift:"),
        "the SKIP verdict line: {text}"
    );

    // The same shape in JSON: a `lint:` gate entry with status `skip`.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--format",
        "json",
    ]);
    let json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("check stdout is pure JSON");
    let skip = json["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str() == Some("lint:warehouse_type_drift") && c["status"] == "skip")
        .expect("a skip-status lint gate exists");
    assert!(
        skip["message"].as_str().is_some_and(|s| !s.is_empty()),
        "the skip carries its reason: {skip}"
    );

    // `lint --format json` reports the same skip gates — and the summary
    // counts them as skipped, never as failures.
    let out = lint_on(&m, &["--format", "json", "--fail-on", "never"]);
    let json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("lint stdout is pure JSON");
    let gates = json["gates"].as_array().expect("gates array");
    assert!(
        gates
            .iter()
            .any(|g| g["id"] == "lint:warehouse_type_drift" && g["status"] == "skip"),
        "lint JSON carries the skip gate: {json}"
    );
    assert!(
        json["summary"]["skipped"].as_u64().unwrap() >= 2,
        "the summary counts skipped gates: {json}"
    );
}

#[test]
fn lint_and_docs_accept_the_catalog_and_dialect_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    catalog_in(tmp.path(), &m);
    let cat = tmp.path().join("catalog.json");
    let docs_dir = tmp.path().join("docs");
    for args in [
        vec![
            "lint",
            "--manifest",
            m.to_str().unwrap(),
            "--catalog",
            cat.to_str().unwrap(),
            "--sql-dialect",
            "snowflake",
        ],
        vec![
            "check",
            "--manifest",
            m.to_str().unwrap(),
            "--lint",
            "--catalog",
            cat.to_str().unwrap(),
        ],
        vec![
            "docs",
            "--manifest",
            m.to_str().unwrap(),
            "--out",
            docs_dir.to_str().unwrap(),
            "--catalog",
            cat.to_str().unwrap(),
            "--sql-dialect",
            "snowflake",
            "--quiet",
        ],
    ] {
        let out = run(&args);
        assert_ne!(
            out.status.code(),
            Some(2),
            "{args:?} is accepted (the flags feed the SQL/catalog rules), stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn check_diff_marks_pre_existing_findings() {
    let m = fixture_manifest();
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--lint",
        "--diff",
        m.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        out_text(&out).contains("pre-existing"),
        "self-diff suppresses every finding: {}",
        out_text(&out)
    );
}

// ---- finding locations: the yml file + line ---------------------------------

/// A tiny source-mode project in `dir`: one documented model with two
/// undescribed columns (so `undocumented_columns` fires on the first one).
/// `models/schema.yml` line numbers are pinned by the write below.
fn column_project(dir: &Path) -> PathBuf {
    let root = dir.join("proj");
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::write(
        root.join("dbt_project.yml"),
        "name: p\nversion: \"1.0.0\"\nconfig-version: 2\n",
    )
    .unwrap();
    std::fs::write(root.join("models/m.sql"), "select 1\n").unwrap();
    // line 7 is `- name: id`, the first undescribed column.
    std::fs::write(
        root.join("models/schema.yml"),
        "version: 2\n\nmodels:\n  - name: m\n    description: documented\n    columns:\n      - name: id\n      - name: amt\n",
    )
    .unwrap();
    root
}

#[test]
fn lint_prints_the_yml_line_and_column_for_column_findings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = column_project(tmp.path());
    let out = run(&[
        "lint",
        "--source",
        root.to_str().unwrap(),
        "--rules",
        "undocumented_columns",
    ]);
    let stdout = out_text(&out);
    // `- name: id` is line 7 and the `id` value sits at column 15.
    assert!(
        stdout.contains("models/schema.yml:7:15"),
        "the column finding prints the declaring yml + its span: {stdout}"
    );
    assert!(
        stdout.contains("undocumented_columns"),
        "the rule id shows: {stdout}"
    );
}

#[test]
fn check_json_findings_carry_the_yml_span_and_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = column_project(tmp.path());
    let out = run(&[
        "check",
        "--source",
        root.to_str().unwrap(),
        "--lint",
        "--rules",
        "undocumented_columns",
        "--format",
        "json",
    ]);
    let json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("check stdout is pure JSON");
    let finding = json["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "lint:undocumented_columns")
        .and_then(|g| g["findings"].as_array().and_then(|a| a.first()))
        .expect("an undocumented_columns finding exists");
    assert_eq!(
        finding["location"],
        serde_json::json!({
            "file": "models/schema.yml",
            "line": 7,
            "column": 15,
            "end_line": 7,
            "end_column": 17,
        }),
        "the `- name: id` value's whole span: {finding}"
    );
    assert_eq!(finding["label"], "declared here");
    assert_eq!(
        finding["help"],
        dbtl::LintRule::UndocumentedColumns.info().help,
        "the effective help serializes"
    );
    assert_eq!(
        finding["docs_url"],
        "https://hytfjwr.github.io/dbtl/reference/lint-rules/#undocumented_columns"
    );
}

// ---- SQL-rule positions: findings underline the offending SQL -----------------

#[test]
fn lint_select_star_points_at_the_wildcard_in_the_model_file() {
    // `fct_orders`'s `o.*` — the expected line/column is COMPUTED from the
    // fixture (never a hardcoded line number), so the test survives edits
    // that move lines around.
    let file = PathBuf::from(SAMPLE_PROJECT).join("models/marts/fct_orders.sql");
    let raw = std::fs::read_to_string(&file).unwrap();
    let at = raw.find("o.*").expect("fct_orders selects o.*");
    let line = raw[..at].matches('\n').count() + 1;
    let column = raw[..at].rsplit('\n').next().unwrap().chars().count() + 1;

    let out = run(&["lint", "--source", SAMPLE_PROJECT, "--rules", "select_star"]);
    let stdout = out_text(&out);
    assert!(
        stdout.contains(&format!("models/marts/fct_orders.sql:{line}:{column}")),
        "the finding anchors at the wildcard (computed {line}:{column}): {stdout}"
    );

    // JSON carries the full span + the annotation label.
    let out = run(&[
        "lint",
        "--source",
        SAMPLE_PROJECT,
        "--rules",
        "select_star",
        "--format",
        "json",
    ]);
    let json: serde_json::Value =
        serde_json::from_str(&out_text(&out)).expect("lint stdout is pure JSON");
    let finding = json["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "lint:select_star")
        .and_then(|g| {
            g["findings"].as_array().and_then(|a| {
                a.iter()
                    .find(|f| f["node_uid"] == "model.sample.fct_orders")
            })
        })
        .expect("a select_star finding on fct_orders");
    assert_eq!(
        finding["location"],
        serde_json::json!({
            "file": "models/marts/fct_orders.sql",
            "line": line,
            "column": column,
            "end_line": line,
            "end_column": column + 3,
        }),
        "the `o.*` span — end exclusive: {finding}"
    );
    assert_eq!(finding["label"], "wildcard");
}

// ---- --select: findings restricted to the selected nodes ---------------------

#[test]
fn lint_select_restricts_findings_to_the_selection() {
    // naming_conventions fires on agg_country_orders / cll_demo /
    // cll_syntax project-wide. `--select fct_*` picks none of them → the
    // rule passes; `--select cll_demo` keeps exactly its one finding.
    let m = sample_manifest_path();
    let out = lint_on(&m, &["--rules", "naming_conventions", "--select", "fct_*"]);
    assert!(
        out.status.success(),
        "no fct_* node violates the prefix rule, stderr: {}",
        stderr(&out)
    );
    assert!(out_text(&out).contains("No findings"));

    let out = lint_on(
        &m,
        &["--rules", "naming_conventions", "--select", "cll_demo"],
    );
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    assert!(
        stdout.contains("error[naming_conventions]:") && stdout.contains("cll_demo"),
        "only the selected node's finding, got: {stdout}"
    );
    assert!(!stdout.contains("agg_country_orders"), "{stdout}");
}

#[test]
fn lint_select_empty_match_warns_and_reports_zero_findings() {
    let m = sample_manifest_path();
    let out = lint_on(&m, &["--select", "no_such_node"]);
    assert!(
        out.status.success(),
        "zero findings can't fail, stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("matched no nodes"),
        "the empty-selection warning: {}",
        stderr(&out)
    );
}

#[test]
fn lint_select_usage_errors() {
    let m = sample_manifest_path();
    // A malformed spec is a usage error — before the data load.
    let out = lint_on(&m, &["--select", "foo:bar"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("foo:bar"), "stderr: {}", stderr(&out));

    // `state:` without `--diff` — same usage class.
    let out = lint_on(&m, &["--select", "state:modified"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("--diff"), "stderr: {}", stderr(&out));
}

#[test]
fn lint_select_conflicts_with_the_query_and_baseline_modes() {
    // --select narrows a lint RUN — the frozen-baseline write path and the
    // query modes don't produce a findings set to narrow, so pairing them
    // is a usage error rather than a silently ignored flag.
    let m = sample_manifest_path();
    for extra in [
        vec!["--select", "fct_*", "--write-baseline"],
        vec!["--select", "fct_*", "--list-rules"],
        vec!["--select", "fct_*", "--explain", "naming_conventions"],
    ] {
        let out = lint_on(&m, &extra);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{extra:?} is a usage error, stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn dbtl_yml_exclude_specs_are_validated_as_selectors() {
    // A rule `exclude:` entry is a selector — a malformed one fails the
    // config load loudly (usage), it doesn't silently exclude nothing.
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    naming_conventions:\n      exclude: [\"foo:bar\"]\n",
    )
    .unwrap();
    let out = lint_on(&m, &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a malformed exclude spec is fatal, stderr: {}",
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains("naming_conventions") && err.contains("foo:bar"),
        "the rule and the bad spec are named: {err}"
    );
}

#[test]
fn dbtl_yml_exclude_selectors_suppress_matching_findings() {
    // The same grammar narrows the exclusion: a `path:`-scoped exclude
    // drops the marts naming violations the rule would report.
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    std::fs::copy(sample_manifest_path(), &m).unwrap();
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    naming_conventions:\n      exclude: [\"path:marts\"]\n",
    )
    .unwrap();
    let out = lint_on(&m, &["--rules", "naming_conventions"]);
    assert!(
        out.status.success(),
        "every naming violation sits under marts/, stderr: {}",
        stderr(&out)
    );
}
