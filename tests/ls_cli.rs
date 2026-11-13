//! CLI-level coverage for `dbtl ls` — the non-interactive selector listing:
//! every output format, the union/intersection/hop grammar, `--exclude`,
//! the strict-validation usage errors (and the `state:`/`--diff`
//! preconditions), and the deterministic empty-result shapes — all through
//! the real binary, no TTY involved.

mod common;

use common::{
    dbtl_cmd, git_isolated_env, manifest_path as fixture_manifest, sample_manifest_path, GitRepo,
    SAMPLE_PROJECT,
};
use std::path::Path;
use std::process::Command;

/// `dbtl` with a nulled stdin and the sealed git env (`ls --diff git:` spawns
/// `git` — its child must see the same isolation the test commits ran under).
fn dbtl(args: &[&str]) -> Command {
    let mut cmd = dbtl_cmd();
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    git_isolated_env(&mut cmd);
    cmd
}

fn run(args: &[&str]) -> std::process::Output {
    dbtl(args).output().expect("run dbtl")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `dbtl ls <specs> --manifest <SAMPLE> <extra...>`.
fn ls(specs: &[&str], extra: &[&str]) -> std::process::Output {
    let mut args: Vec<String> = vec!["ls".to_string()];
    args.extend(specs.iter().map(|s| s.to_string()));
    args.push("--manifest".to_string());
    args.push(sample_manifest_path().to_str().unwrap().to_string());
    args.extend(extra.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs)
}

fn lines(out: &std::process::Output) -> Vec<String> {
    stdout(out).lines().map(|l| l.to_string()).collect()
}

// ---- listings: unions, intersections, hops, atoms --------------------------

#[test]
fn ls_names_sort_by_name() {
    let out = ls(&["fct_orders", "stg_orders"], &[]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(lines(&out), ["fct_orders", "stg_orders"]);
}

#[test]
fn ls_unions_within_and_across_specs() {
    // Whitespace-separated terms union; multiple spec args union the same
    // way — `a b` equals `a` `b`.
    let one = ls(&["stg_orders fct_orders"], &[]);
    let two = ls(&["stg_orders", "fct_orders"], &[]);
    assert!(one.status.success() && two.status.success());
    assert_eq!(stdout(&one), stdout(&two));
    assert_eq!(lines(&one), ["fct_orders", "stg_orders"]);
}

#[test]
fn ls_intersection_narrows_within_a_term() {
    // Comma factors intersect: marts-path AND materialized table.
    let out = ls(&["config.materialized:table,path:marts"], &[]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        lines(&out),
        [
            "agg_country_orders",
            "cll_demo",
            "cll_syntax",
            "fct_contracted",
            "fct_customer_history",
            "fct_model_tests",
            "fct_orders",
            "fct_public_api",
        ]
    );
}

#[test]
fn ls_hop_operators_expand_both_directions() {
    // `+fct_orders` — upstream closure: the model, its staging parent, and
    // the seed/source feeding it (a source prints by its table name).
    let out = ls(&["+fct_orders"], &[]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        lines(&out),
        ["country_codes", "fct_orders", "orders", "stg_orders"],
        "upstream closure"
    );

    // `fct_orders+` — downstream closure incl. the exposure.
    let out = ls(&["fct_orders+"], &[]);
    assert_eq!(
        lines(&out),
        [
            "agg_country_orders",
            "cll_demo",
            "cll_syntax",
            "fct_orders",
            "orders_dashboard",
        ]
    );

    // `N+` bounds the upstream walk to N edges.
    let out = ls(&["1+agg_country_orders"], &[]);
    assert_eq!(lines(&out), ["agg_country_orders", "fct_orders"]);
}

#[test]
fn ls_method_atoms_match_their_domains() {
    // `tag:` — the jaffle fixture's staging models carry `staging` (the
    // sample fixture declares no tags).
    let m = fixture_manifest().to_str().unwrap().to_string();
    let out = run(&["ls", "tag:staging", "--manifest", &m]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let names = lines(&out);
    assert_eq!(names.len(), 11, "the staging tag set, got: {names:?}");
    assert!(names.iter().all(|n| n.starts_with("stg_")), "{names:?}");

    // `meta:key` (key presence) and `meta:key=value`.
    let out = ls(&["meta:zone"], &[]);
    assert_eq!(lines(&out), ["fct_orders"]);
    let out = ls(&["meta:owner=alice"], &[]);
    assert_eq!(lines(&out), ["fct_orders"]);
    let out = ls(&["meta:domain=reference"], &[]);
    assert_eq!(lines(&out), ["country_codes", "extra_seed"]);

    // `exposure:` — the exposure plus its upstream closure.
    let out = ls(&["exposure:orders_dashboard"], &[]);
    let names = lines(&out);
    for needle in ["orders_dashboard", "fct_orders", "stg_orders"] {
        assert!(
            names.contains(&needle.to_string()),
            "exposure + parents, got: {names:?}"
        );
    }

    // `path:` — a directory fragment matches sources' declaring yml too
    // (the staging schema files declare all six sources; the duplicate
    // pair shares the `payments` table name three ways).
    let out = ls(&["path:staging"], &[]);
    assert_eq!(
        lines(&out),
        [
            "audit_log",
            "customers",
            "orders",
            "payments",
            "payments",
            "payments",
            "stg_dup_probe",
            "stg_orders",
            "stg_two_configs",
        ]
    );
}

#[test]
fn ls_globs_match_bare_names() {
    let out = ls(&["fct_*"], &[]);
    assert_eq!(
        lines(&out),
        [
            "fct_contracted",
            "fct_customer_history",
            "fct_daily_orders",
            "fct_incr_file",
            "fct_incr_schema",
            "fct_incr_tree",
            "fct_line_items",
            "fct_model_tests",
            "fct_orders",
            "fct_public_api",
        ]
    );
}

// ---- --exclude -------------------------------------------------------------

#[test]
fn ls_exclude_subtracts_after_the_union() {
    let out = ls(&["stg_orders+"], &["--exclude", "fct_orders+"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        lines(&out),
        [
            "extra_snapshot",
            "fct_daily_orders",
            "fct_line_items",
            "orders_snapshot",
            "stg_orders",
        ]
    );
    // A repeated --exclude unions its removals.
    let out = ls(
        &["stg_orders+"],
        &["--exclude", "fct_orders+", "--exclude", "fct_daily_orders"],
    );
    assert_eq!(
        lines(&out),
        [
            "extra_snapshot",
            "fct_line_items",
            "orders_snapshot",
            "stg_orders",
        ]
    );
}

/// `--exclude` takes exactly ONE spec per occurrence — a bare word after it
/// is another positional selector (a union term), not a second exclusion.
/// `dbtl ls a --exclude b c` must never silently eat `c` as an exclusion.
#[test]
fn ls_exclude_takes_one_spec_per_occurrence() {
    // `fct_line_items` lands as a second positional — it joins the INCLUSION
    // union, so it stays listed (under the old variadic flag it would have
    // been swallowed as a second exclusion and disappeared).
    let out = ls(
        &["stg_orders+", "fct_line_items"],
        &["--exclude", "fct_orders+"],
    );
    assert_eq!(
        lines(&out),
        [
            "extra_snapshot",
            "fct_daily_orders",
            "fct_line_items",
            "orders_snapshot",
            "stg_orders",
        ]
    );
    // Same thing with the exclusion flag between the positionals.
    let out = ls(
        &["stg_orders+", "--exclude", "fct_orders+", "fct_line_items"],
        &[],
    );
    assert_eq!(
        lines(&out),
        [
            "extra_snapshot",
            "fct_daily_orders",
            "fct_line_items",
            "orders_snapshot",
            "stg_orders",
        ]
    );
}

// ---- output formats ---------------------------------------------------------

#[test]
fn ls_uids_print_sorted_unique_ids() {
    let out = ls(&["fct_orders"], &["--output", "uids"]);
    assert!(out.status.success());
    assert_eq!(lines(&out), ["model.sample.fct_orders"]);
}

#[test]
fn ls_selector_prints_one_buildable_line() {
    // Downstream of stg_orders: models + snapshots are buildable; the
    // orders_dashboard exposure drops out.
    let out = ls(&["stg_orders+"], &["--output", "selector"]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        "agg_country_orders cll_demo cll_syntax extra_snapshot fct_daily_orders \
         fct_line_items fct_orders orders_snapshot stg_orders\n"
    );
}

#[test]
fn ls_json_prints_deterministic_objects() {
    let out = ls(&["fct_orders"], &["--output", "json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["unique_id"], "model.sample.fct_orders");
    assert_eq!(row["name"], "fct_orders");
    assert_eq!(row["resource_type"], "model");
    assert_eq!(row["materialized"], "table");
    assert_eq!(
        row["path"].as_str().unwrap().replace('\\', "/"),
        "models/marts/fct_orders.sql"
    );
}

// ---- empty results ----------------------------------------------------------

#[test]
fn ls_empty_match_prints_nothing_for_line_formats() {
    for fmt in ["names", "uids", "selector"] {
        let out = ls(&["no_such_node"], &["--output", fmt]);
        assert!(
            out.status.success(),
            "--output {fmt} exits 0, stderr: {}",
            stderr(&out)
        );
        assert_eq!(
            stdout(&out),
            "",
            "--output {fmt} prints nothing on an empty set"
        );
    }
    let out = ls(&["no_such_node"], &["--output", "json"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "[]\n", "json stays parseable");
}

// ---- strict validation --------------------------------------------------------

/// A closed stdout pipe must be a normal end of output — `dbtl ls | head -1`
/// exits 0, never the panic a raw `println!` dies on. Unix-only: the test
/// relies on dropping the pipe's read end out from under a large write.
#[cfg(unix)]
#[test]
fn ls_survives_a_closed_stdout_pipe() {
    // A manifest big enough that `--output uids` far exceeds the pipe
    // buffer — with the reader gone, EVERY write sees the closed pipe.
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("manifest.json");
    std::fs::write(
        &manifest,
        xtask::synth::manifest_json(&xtask::synth::synth_manifest(&xtask::synth::SynthParams {
            models: 8000,
            ..Default::default()
        })),
    )
    .expect("write synth manifest");

    let mut child = dbtl(&[
        "ls",
        "*",
        "--output",
        "uids",
        "--manifest",
        manifest.to_str().unwrap(),
    ])
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("spawn dbtl");
    // Close the read end immediately: the listing's first write lands on a
    // pipe with no readers (BrokenPipe) — a normal end of output, not a
    // panic (101) or a SIGPIPE kill (no exit code).
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("collect child output");
    assert!(
        out.status.success(),
        "closed stdout pipe must exit cleanly, got {:?}, stderr: {}",
        out.status.code(),
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("panic"),
        "no panic on a closed pipe: {}",
        stderr(&out)
    );
}

#[test]
fn ls_malformed_specs_are_usage_errors() {
    for (spec, needle) in [
        // Unknown method.
        ("foo:bar", "unknown selector method"),
        // A config.* key the grammar doesn't take.
        ("config.freshness:x", "unknown config selector"),
        // A state: value outside modified|new.
        ("state:bogus", "unknown state selector"),
        // Empty method values.
        ("tag:", "tag: needs a non-empty value"),
        ("path:", "path: needs a non-empty value"),
        ("meta:", "meta: needs a key"),
        // Non-numeric hop prefix.
        ("a+b", "hop counts must be a number"),
        // A hop with no atom to expand.
        ("2+", "a '+' hop needs a selector to expand"),
        // Empty comma factors — leading, trailing, doubled.
        ("a,,b", "an empty selector term"),
        (",a", "an empty selector term"),
        ("a,", "an empty selector term"),
    ] {
        let out = ls(&[spec], &[]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "'{spec}' is a usage error, stderr: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains(needle),
            "'{spec}' names the problem, stderr: {}",
            stderr(&out)
        );
    }
    // Whitespace-only specs error too.
    let out = ls(&["a"], &["--exclude", "   "]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
}

#[test]
fn ls_error_names_the_failing_fragment() {
    // One bad spec among good ones still fails, quoting exactly the
    // fragment the user must fix.
    let out = ls(&["stg_orders", "foo:bar"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("foo:bar"),
        "the failing fragment is quoted: {}",
        stderr(&out)
    );
}

// ---- state: / --diff preconditions --------------------------------------------

#[test]
fn ls_state_without_diff_is_a_usage_error() {
    for spec in ["state:modified", "state:new", "state:modified+"] {
        let out = ls(&[spec], &[]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "'{spec}' needs --diff, stderr: {}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("--diff"), "stderr: {}", stderr(&out));
    }
}

#[test]
fn ls_diff_without_state_is_a_usage_error() {
    // --diff exists on `ls` ONLY to feed `state:` — pairing either half
    // without the other is rejected, never silently ignored.
    let m = sample_manifest_path().to_str().unwrap().to_string();
    let out = ls(&["fct_orders"], &["--diff", &m]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("state:"),
        "the message names the missing half: {}",
        stderr(&out)
    );
}

// ---- state: against a git baseline ---------------------------------------------

/// The sample project committed twice: the baseline verbatim, then a HEAD
/// carrying one modified model and one added model.
fn state_repo() -> GitRepo {
    let repo = GitRepo::init();
    repo.copy_in(Path::new(SAMPLE_PROJECT), ".");
    repo.commit(&["."], "import fixture");
    let sql = repo.root.join("models/staging/stg_orders.sql");
    let text = std::fs::read_to_string(&sql).unwrap();
    std::fs::write(&sql, format!("{text}\n-- touched\n")).unwrap();
    repo.write("models/marts/brand_new.sql", "select 1 as id\n");
    repo.commit(&["."], "one modified + one added");
    repo
}

#[test]
fn ls_state_new_and_modified_against_git_baseline() {
    let repo = state_repo();
    let proj = repo.root.to_str().unwrap().to_string();
    // `state:new` — added only: brand_new exists at HEAD, not at HEAD~1.
    let out = run(&[
        "ls",
        "state:new",
        "--project",
        &proj,
        "--diff",
        "git:HEAD~1",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(lines(&out), ["brand_new"]);
    // The state: caveat notes the resolved-by-dbtl set on stderr — stdout
    // stays pipeable.
    assert!(stderr(&out).contains("state:"), "stderr: {}", stderr(&out));

    // `state:modified` — added ∪ modified: brand_new + the touched model.
    let out = run(&[
        "ls",
        "state:modified",
        "--project",
        &proj,
        "--diff",
        "git:HEAD~1",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(lines(&out), ["brand_new", "stg_orders"]);
}

#[test]
fn ls_state_modified_hop_covers_the_blast_radius() {
    let repo = state_repo();
    let proj = repo.root.to_str().unwrap().to_string();
    let out = run(&[
        "ls",
        "state:modified+",
        "--project",
        &proj,
        "--diff",
        "git:HEAD~1",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let names = lines(&out);
    // Every node downstream of the modified stg_orders lands — the same
    // fan-out `stg_orders+` gives.
    for needle in [
        "stg_orders",
        "brand_new",
        "fct_orders",
        "agg_country_orders",
        "orders_dashboard",
        "orders_snapshot",
    ] {
        assert!(names.contains(&needle.to_string()), "{needle} in {names:?}");
    }
}

#[test]
fn ls_state_modified_matches_the_impact_change_set() {
    // `state:modified` must select exactly what `dbtl impact` counts as
    // changed — added ∪ modified — no drift between the surfaces.
    let repo = state_repo();
    let proj = repo.root.to_str().unwrap().to_string();
    let out = run(&[
        "impact",
        "--project",
        &proj,
        "--diff",
        "git:HEAD~1",
        "--format",
        "json",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut expected: Vec<String> = v["added"]
        .as_array()
        .unwrap()
        .iter()
        .chain(v["modified"].as_array().unwrap().iter())
        .map(|r| r[0].as_str().unwrap().to_string())
        .collect();
    expected.sort();
    expected.dedup();

    let out = run(&[
        "ls",
        "state:modified",
        "--project",
        &proj,
        "--diff",
        "git:HEAD~1",
    ]);
    assert_eq!(
        lines(&out),
        expected,
        "ls state:modified == impact's change set"
    );
}
