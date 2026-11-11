//! The CLI's machine contract, exercised against the real binary: the
//! exit-code classes (`0` success / `1` gate failure / `2` usage / `3`
//! input) and the global-flag surface — data-input flags accepted before OR
//! after the subcommand, flags a command doesn't consume rejected rather
//! than silently ignored.

mod common;

use common::{
    dbtl_cmd, manifest_path as fixture_manifest, sample_manifest_path as sample_manifest,
    SAMPLE_PROJECT,
};
use std::path::{Path, PathBuf};

fn run(args: &[&str]) -> std::process::Output {
    dbtl_cmd()
        .args(args)
        // Null stdin keeps a bare (no-subcommand) invocation from seeing the
        // test runner's TTY and parking on the interactive event loop.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

/// Spawn + poll `try_wait`, killing a child that parks (the pattern
/// `package_scope.rs` uses for TUI-startup paths — a console-having host
/// could otherwise block forever on the interactive event loop).
fn spawn_and_wait(args: &[&str]) -> std::process::Output {
    let mut child = dbtl_cmd()
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn dbtl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if child.try_wait().expect("poll child").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if child.try_wait().expect("poll child").is_none() {
        child.kill().expect("kill parked child");
    }
    child.wait_with_output().expect("collect child output")
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---- exit code 0 / 1: success and gate failures -------------------------

#[test]
fn successful_check_exits_zero() {
    let m = fixture_manifest();
    let out = run(&["check", "--manifest", m.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a passing check is exit 0, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn coverage_gate_failure_exits_one() {
    let m = fixture_manifest();
    // The fixture isn't 100%-tested — demanding 100% must fail the gate.
    let out = run(&[
        "check",
        "--manifest",
        m.to_str().unwrap(),
        "--coverage-min",
        "100",
    ]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a gate failure is exit 1, stderr: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("FAIL coverage"));
}

#[test]
fn docs_check_divergence_exits_one() {
    let m = fixture_manifest();
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().join("docs");
    let out = run(&[
        "docs",
        "--manifest",
        m.to_str().unwrap(),
        "--out",
        out_dir.to_str().unwrap(),
        "--quiet",
    ]);
    assert_eq!(out.status.code(), Some(0), "generate: {}", stderr(&out));
    // Doctor one generated page → `docs --check` reports the difference as
    // a gate failure, not a usage or input error.
    let readme = out_dir.join("README.md");
    std::fs::write(
        &readme,
        format!("{}\n", std::fs::read_to_string(&readme).unwrap()),
    )
    .unwrap();
    let out = run(&[
        "docs",
        "--manifest",
        m.to_str().unwrap(),
        "--out",
        out_dir.to_str().unwrap(),
        "--check",
    ]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "divergent docs are a gate failure, stderr: {}",
        stderr(&out)
    );
}

// ---- exit code 2: usage / configuration errors --------------------------

#[test]
fn usage_errors_exit_two() {
    let m = fixture_manifest();
    let m_str = m.to_str().unwrap();
    let cases: Vec<Vec<String>> = vec![
        // An unknown lint rule name.
        vec!["check", "--manifest", m_str, "--rules", "not_a_rule"]
            .into_iter()
            .map(String::from)
            .collect(),
        // `impact` without its required --diff baseline.
        vec!["impact", "--manifest", m_str]
            .into_iter()
            .map(String::from)
            .collect(),
        // `columns` on a model that doesn't exist.
        vec!["columns", "no_such_model.id", "--manifest", m_str]
            .into_iter()
            .map(String::from)
            .collect(),
        vec!["columns", "stg_orders.no_such_col", "--manifest", m_str]
            .into_iter()
            .map(String::from)
            .collect(),
        // --manifest and --source are mutually exclusive wherever each sits.
        vec!["--manifest", m_str, "check", "--source", SAMPLE_PROJECT]
            .into_iter()
            .map(String::from)
            .collect(),
        vec!["--source", SAMPLE_PROJECT, "check", "--manifest", m_str]
            .into_iter()
            .map(String::from)
            .collect(),
    ];
    for args in cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(&refs);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} is a usage error, stderr: {}",
            stderr(&out)
        );
    }

    // A malformed .dbtl.yml supplied to `check --lint` — the config lives
    // next to the manifest, so the fixture is copied to a tempdir.
    let tmp = tempfile::tempdir().expect("tempdir");
    let copied = tmp.path().join("manifest.json");
    std::fs::copy(&m, &copied).unwrap();
    std::fs::write(
        tmp.path().join(".dbtl.yml"),
        "lint:\n  rules:\n    bogus_rule: false\n",
    )
    .unwrap();
    let out = run(&["check", "--manifest", copied.to_str().unwrap(), "--lint"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "malformed .dbtl.yml is a usage error, stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("unknown lint rule 'bogus_rule'"),
        "the parse reason reaches stderr: {}",
        stderr(&out)
    );
}

// ---- exit code 3: missing / unreadable inputs ---------------------------

#[test]
fn input_errors_exit_three() {
    let m = fixture_manifest();
    let m_str = m.to_str().unwrap();
    let nope = "/nonexistent/dbtl_contract_input";
    let cases: Vec<Vec<String>> = vec![
        // A missing manifest, on both sides of the subcommand.
        vec!["check", "--manifest", nope]
            .into_iter()
            .map(String::from)
            .collect(),
        vec!["--manifest", nope, "check"]
            .into_iter()
            .map(String::from)
            .collect(),
        vec!["docs", "--manifest", nope, "--out", "/tmp/dbtl_nope_docs"]
            .into_iter()
            .map(String::from)
            .collect(),
        // A missing --diff baseline.
        vec!["check", "--manifest", m_str, "--diff", nope]
            .into_iter()
            .map(String::from)
            .collect(),
        vec!["impact", "--manifest", m_str, "--diff", nope]
            .into_iter()
            .map(String::from)
            .collect(),
        // An explicitly named --catalog that can't be read — a real fixture
        // uid, so the load error (not a name miss) is what fails.
        vec![
            "columns",
            "model.jaffle_finance.pos_txn.id",
            "--manifest",
            m_str,
            "--catalog",
            nope,
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    ];
    for args in cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(&refs);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{args:?} is an input error, stderr: {}",
            stderr(&out)
        );
    }

    // An explicit --run-results that can't be read: the TUI startup path —
    // the error is classified before any terminal init.
    let out = spawn_and_wait(&["--manifest", m_str, "--run-results", nope]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "explicit --run-results is an input error: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("error:"));
}

// ---- the global-flag surface: position-independent ----------------------

/// Two arg orders must produce byte-identical stdout and the same exit code.
fn assert_position_equivalent(before: &[String], after: &[String]) {
    let before_refs: Vec<&str> = before.iter().map(String::as_str).collect();
    let after_refs: Vec<&str> = after.iter().map(String::as_str).collect();
    let a = run(&before_refs);
    let b = run(&after_refs);
    assert_eq!(
        a.status.code(),
        b.status.code(),
        "exit codes diverge: {before:?} vs {after:?}"
    );
    assert_eq!(
        a.stdout, b.stdout,
        "stdout diverges between flag positions: {before:?} vs {after:?}"
    );
}

#[test]
fn data_flags_parse_before_or_after_the_subcommand() {
    let m = fixture_manifest();
    let m_str = m.to_str().unwrap();

    // --manifest: the headline bug — `dbtl --manifest M check` used to
    // silently drop the flag (dispatch read the subcommand's own field).
    assert_position_equivalent(
        &["--manifest".into(), m_str.into(), "check".into()],
        &["check".into(), "--manifest".into(), m_str.into()],
    );

    // --source, --project, --diff, --sql-dialect, --catalog, --run-results
    // (TUI), --include-packages: same contract wherever they sit.
    assert_position_equivalent(
        &["--source".into(), SAMPLE_PROJECT.into(), "check".into()],
        &["check".into(), "--source".into(), SAMPLE_PROJECT.into()],
    );

    // --project auto-detect needs a dir carrying target/manifest.json.
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(&m, target.join("manifest.json")).unwrap();
    let dir = tmp.path().to_str().unwrap().to_string();
    assert_position_equivalent(
        &["--project".into(), dir.clone(), "check".into()],
        &["check".into(), "--project".into(), dir],
    );

    // --diff on `check` (and `impact` below).
    assert_position_equivalent(
        &[
            "--manifest".into(),
            m_str.into(),
            "--diff".into(),
            m_str.into(),
            "check".into(),
        ],
        &[
            "check".into(),
            "--manifest".into(),
            m_str.into(),
            "--diff".into(),
            m_str.into(),
        ],
    );
    assert_position_equivalent(
        &[
            "--manifest".into(),
            m_str.into(),
            "--diff".into(),
            m_str.into(),
            "impact".into(),
            "--format".into(),
            "json".into(),
        ],
        &[
            "impact".into(),
            "--manifest".into(),
            m_str.into(),
            "--diff".into(),
            m_str.into(),
            "--format".into(),
            "json".into(),
        ],
    );

    // `docs` too: the data flag lands the same whichever side it sits.
    let tmp2 = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp2.path().join("docs").to_str().unwrap().to_string();
    assert_position_equivalent(
        &[
            "--manifest".into(),
            m_str.into(),
            "docs".into(),
            "--out".into(),
            out_dir.clone(),
            "--quiet".into(),
        ],
        &[
            "docs".into(),
            "--manifest".into(),
            m_str.into(),
            "--out".into(),
            out_dir,
            "--quiet".into(),
        ],
    );

    // --sql-dialect / --include-packages on commands that consume them.
    // `stg_orders` lives in the SAMPLE manifest, not the jaffle fixture.
    let sm = sample_manifest();
    let sm_str = sm.to_str().unwrap();
    assert_position_equivalent(
        &[
            "--sql-dialect".into(),
            "generic".into(),
            "--manifest".into(),
            sm_str.into(),
            "columns".into(),
            "stg_orders.id".into(),
        ],
        &[
            "columns".into(),
            "stg_orders.id".into(),
            "--manifest".into(),
            sm_str.into(),
            "--sql-dialect".into(),
            "generic".into(),
        ],
    );
    assert_position_equivalent(
        &[
            "--include-packages".into(),
            "--manifest".into(),
            m_str.into(),
            "check".into(),
        ],
        &[
            "check".into(),
            "--manifest".into(),
            m_str.into(),
            "--include-packages".into(),
        ],
    );
}

// ---- flags a subcommand doesn't consume are rejected ---------------------

#[test]
fn unsupported_flags_are_rejected_not_ignored() {
    let m = fixture_manifest();
    let m_str = m.to_str().unwrap();
    // (args, flag-spelling-in-stderr, subcommand-name-in-stderr)
    let mut cases: Vec<(Vec<String>, &str, &str)> = Vec::new();
    // The TUI-only flags are rejected under every subcommand — each typed
    // BEFORE the subcommand (after it they fail clap's own unknown-arg
    // check, still exit 2).
    let tui_flags: &[(&[&str], &str)] = &[
        (&["--select", "x"], "--select"),
        (&["--watch"], "--watch"),
        (&["--theme", "default"], "--theme"),
        (&["--list-themes"], "--list-themes"),
        (&["--ascii"], "--ascii"),
        (&["--unicode"], "--unicode"),
    ];
    let subs: &[(&str, &[&str])] = &[
        ("docs", &["--out", "/tmp/dbtl_flag_docs"]),
        ("check", &["--manifest", m_str]),
        ("impact", &["--manifest", m_str, "--diff", m_str]),
        ("columns", &["a.b", "--manifest", m_str]),
    ];
    for (flag_args, spelled) in tui_flags {
        for (sub, sub_args) in subs {
            let mut args: Vec<String> = flag_args.iter().map(|s| s.to_string()).collect();
            args.push(sub.to_string());
            args.extend(sub_args.iter().map(|s| s.to_string()));
            cases.push((args, spelled, sub));
        }
    }
    // Global flags a command doesn't consume — before AND after the
    // subcommand alike (global args parse in either position).
    for (args, spelled, sub) in [
        (
            vec!["--run-results", "r.json", "check", "--manifest", m_str],
            "--run-results",
            "check",
        ),
        (
            vec!["check", "--manifest", m_str, "--run-results", "r.json"],
            "--run-results",
            "check",
        ),
        (
            vec![
                "--run-results",
                "r.json",
                "columns",
                "a.b",
                "--manifest",
                m_str,
            ],
            "--run-results",
            "columns",
        ),
        (
            vec![
                "--run-results",
                "r.json",
                "docs",
                "--out",
                "/tmp/dbtl_flag_docs",
            ],
            "--run-results",
            "docs",
        ),
        (
            vec![
                "--run-results",
                "r.json",
                "impact",
                "--manifest",
                m_str,
                "--diff",
                m_str,
            ],
            "--run-results",
            "impact",
        ),
        (
            vec![
                "--catalog",
                "c.json",
                "docs",
                "--out",
                "/tmp/dbtl_flag_docs",
            ],
            "--catalog",
            "docs",
        ),
        (
            vec![
                "--sql-dialect",
                "generic",
                "docs",
                "--out",
                "/tmp/dbtl_flag_docs",
            ],
            "--sql-dialect",
            "docs",
        ),
        (
            vec!["--diff", m_str, "docs", "--out", "/tmp/dbtl_flag_docs"],
            "--diff",
            "docs",
        ),
        (
            vec!["columns", "a.b", "--manifest", m_str, "--diff", m_str],
            "--diff",
            "columns",
        ),
    ] {
        cases.push((args.iter().map(|s| s.to_string()).collect(), spelled, sub));
    }

    for (args, flag, sub) in cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(&refs);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} must be a usage error, stderr: {}",
            stderr(&out)
        );
        let err = stderr(&out);
        assert!(
            err.contains(flag),
            "{args:?}: stderr names the flag '{flag}', got: {err}"
        );
        assert!(
            err.contains(sub),
            "{args:?}: stderr names the subcommand '{sub}', got: {err}"
        );
    }
}

/// `DBTL_THEME` feeds `--theme` through clap's env binding — that is NOT an
/// explicit command-line occurrence, so it must not trip the ignored-flag
/// check under a subcommand.
#[test]
fn env_theme_does_not_count_as_an_explicit_flag() {
    let m = fixture_manifest();
    let out = dbtl_cmd()
        .args(["check", "--manifest", m.to_str().unwrap()])
        .env("DBTL_THEME", "default")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "check still succeeds with DBTL_THEME set, stderr: {}",
        stderr(&out)
    );
}

// ---- --catalog: explicit override over the auto-detected sibling ---------

/// Write `catalog.json` under `dir` declaring `cols` for `uid`; returns the path.
fn write_catalog(dir: &Path, uid: &str, cols: &[&str]) -> PathBuf {
    let columns: serde_json::Map<String, serde_json::Value> = cols
        .iter()
        .map(|c| (c.to_string(), serde_json::json!({"type": "INT64"})))
        .collect();
    let mut nodes = serde_json::Map::new();
    nodes.insert(uid.to_string(), serde_json::json!({ "columns": columns }));
    let path = dir.join("catalog.json");
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({ "nodes": nodes })).unwrap(),
    )
    .unwrap();
    path
}

#[test]
fn catalog_auto_detects_the_target_sibling() {
    let m = sample_manifest();
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let manifest = target.join("manifest.json");
    std::fs::copy(&m, &manifest).unwrap();
    write_catalog(&target, "model.sample.fct_orders", &["sibling_col"]);

    // The sibling catalog tops the column list — a warehouse-only column
    // resolves without any flag.
    let out = run(&[
        "columns",
        "fct_orders.sibling_col",
        "--manifest",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "auto-detected catalog columns resolve, stderr: {}",
        stderr(&out)
    );
}

#[test]
fn explicit_catalog_overrides_the_sibling() {
    let m = sample_manifest();
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let manifest = target.join("manifest.json");
    std::fs::copy(&m, &manifest).unwrap();
    // The sibling declares `sibling_col`; the explicit file declares
    // `explicit_col` — whichever `--catalog` names must win outright.
    write_catalog(&target, "model.sample.fct_orders", &["sibling_col"]);
    let explicit_dir = tmp.path().join("explicit");
    std::fs::create_dir_all(&explicit_dir).unwrap();
    let explicit = write_catalog(&explicit_dir, "model.sample.fct_orders", &["explicit_col"]);

    let out = run(&[
        "columns",
        "fct_orders.explicit_col",
        "--manifest",
        manifest.to_str().unwrap(),
        "--catalog",
        explicit.to_str().unwrap(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the explicit catalog's column resolves, stderr: {}",
        stderr(&out)
    );

    // …and it WINS over the sibling: `sibling_col` is unknown under the
    // explicit file (the catalog is the top column-list tier).
    let out = run(&[
        "columns",
        "fct_orders.sibling_col",
        "--manifest",
        manifest.to_str().unwrap(),
        "--catalog",
        explicit.to_str().unwrap(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "the sibling's column is invisible under --catalog, stderr: {}",
        stderr(&out)
    );
}

// ---- TUI startup paths: classified before terminal init -----------------

#[test]
fn tui_start_with_a_missing_manifest_exits_three() {
    // The data load precedes `try_init`, so a bad --manifest reports as an
    // input error on a plain stderr line — no TTY involved at all.
    let out = spawn_and_wait(&["--manifest", "/nonexistent/m.json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "missing manifest is an input error: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("error:"));
}

#[test]
fn tui_start_with_an_unmatched_select_exits_two() {
    let m = fixture_manifest();
    let out = spawn_and_wait(&[
        "--manifest",
        m.to_str().unwrap(),
        "--select",
        "no_such_model_zzz",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unmatched --select is a usage error: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("not found"),
        "the select miss message: {}",
        stderr(&out)
    );
}
