//! The CI merge-gate plumbing: the changed-path classifier and the
//! needs-aggregator in `.github/scripts/`, plus the workflows' contract
//! that every gating job is reachable through an aggregate's `needs` list
//! (a job added without `needs` coverage can never slip through).
//!
//! Unix-only: the scripts target bash, like the action's helpers.

#![cfg(unix)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn script(name: &str) -> PathBuf {
    Path::new(ROOT).join(".github/scripts").join(name)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `key=value` lines a script prints, as a map.
fn kv(out: &Output) -> BTreeMap<String, String> {
    stdout(out)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---- changed-paths.sh ------------------------------------------------------

/// Run the classifier over an explicit path list (the `--paths` seam).
fn classify(paths: &[&str]) -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("paths.txt");
    let mut text = paths.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    std::fs::write(&file, text).unwrap();
    let out = Command::new("bash")
        .arg(script("changed-paths.sh"))
        .args(["--paths"])
        .arg(&file)
        .output()
        .expect("run changed-paths.sh");
    assert!(
        out.status.success(),
        "changed-paths failed for {paths:?}: {}",
        stderr(&out)
    );
    kv(&out)
}

fn scopes(map: &BTreeMap<String, String>) -> (bool, bool, bool, bool) {
    (
        map["code"] == "true",
        map["docs"] == "true",
        map["site"] == "true",
        map["docsref"] == "true",
    )
}

#[test]
fn rust_source_is_code_only() {
    assert_eq!(
        scopes(&classify(&["crates/core/src/dag.rs"])),
        (true, false, false, false)
    );
    assert_eq!(
        scopes(&classify(&["src/main.rs", "Cargo.toml", "Cargo.lock"])),
        (true, false, false, false)
    );
}

#[test]
fn top_level_markdown_is_docs_only() {
    for f in [
        "README.md",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "RELEASING.md",
    ] {
        assert_eq!(
            scopes(&classify(&[f])),
            (false, true, false, false),
            "{f} must classify as docs only"
        );
    }
}

#[test]
fn site_paths_are_site_only() {
    assert_eq!(
        scopes(&classify(&["site/src/pages/index.astro"])),
        (false, false, true, false)
    );
    // site Markdown outside the content docs is still just site.
    assert_eq!(
        scopes(&classify(&["site/README.md"])),
        (false, false, true, false)
    );
    // A docs-site.yml-only change never touches code.
    assert_eq!(
        scopes(&classify(&[".github/workflows/docs-site.yml"])),
        (false, false, true, false)
    );
}

#[test]
fn site_content_docs_count_as_docs_and_site() {
    assert_eq!(
        scopes(&classify(&["site/src/content/docs/guide.md"])),
        (false, true, true, false)
    );
    assert_eq!(
        scopes(&classify(&["site/src/content/docs/cli/columns.md"])),
        (false, true, true, false)
    );
}

#[test]
fn docs_and_schema_set_docsref() {
    for f in [
        "docs/reference/cli.md",
        "docs/guide.md",
        "schema/report.schema.json",
    ] {
        assert_eq!(
            scopes(&classify(&[f])),
            (false, true, false, true),
            "{f} must classify as docs + docsref"
        );
    }
}

#[test]
fn markdown_under_input_dirs_is_code() {
    // *.md in tests/, crates/, xtask/, action/, benches/ is a build or
    // test input, not prose — a fixture edit must run the full suite.
    for f in [
        "tests/fixtures/sample_project/models/marts/docs.md",
        "crates/core/README.md",
        "action/NOTES.md",
        "xtask/README.md",
        "benches/data/README.md",
        "demo/guide.md",
    ] {
        assert_eq!(
            scopes(&classify(&[f])),
            (true, false, false, false),
            "{f} must classify as code"
        );
    }
}

#[test]
fn dot_github_markdown_is_one_level_of_docs() {
    assert_eq!(
        scopes(&classify(&[".github/PULL_REQUEST_TEMPLATE.md"])),
        (false, true, false, false)
    );
    // Nested .github Markdown is code, like everything else under .github/.
    assert_eq!(
        scopes(&classify(&[".github/ISSUE_TEMPLATE/bug.md"])),
        (true, false, false, false)
    );
    assert_eq!(
        scopes(&classify(&[".github/workflows/ci.yml"])),
        (true, false, false, false)
    );
    assert_eq!(
        scopes(&classify(&[".github/scripts/changed-paths.sh"])),
        (true, false, false, false)
    );
}

#[test]
fn a_version_bump_runs_everything() {
    // Cargo.toml + Cargo.lock + CHANGELOG.md — the docs scope is also set,
    // but code forces the full suite.
    assert_eq!(
        scopes(&classify(&["Cargo.toml", "Cargo.lock", "CHANGELOG.md"])),
        (true, true, false, false)
    );
}

#[test]
fn mixed_changes_set_every_scope() {
    assert_eq!(
        scopes(&classify(&[
            "src/main.rs",
            "site/src/nav.json",
            "README.md",
            "docs/reference/cli.md",
        ])),
        (true, true, true, true)
    );
}

#[test]
fn an_empty_or_missing_diff_runs_everything() {
    // An empty path list is an anomaly — fall back to the safe side.
    assert_eq!(
        scopes(&classify(&[])),
        (true, true, true, true),
        "an empty diff must report every scope changed"
    );
    // No repository at all: `git diff` cannot resolve the merge parents.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new("bash")
        .arg(script("changed-paths.sh"))
        .arg("pull_request")
        .current_dir(dir.path())
        .output()
        .expect("run changed-paths.sh");
    assert!(
        out.status.success(),
        "an unreachable base must not fail the job: {}",
        stderr(&out)
    );
    assert_eq!(
        scopes(&kv(&out)),
        (true, true, true, true),
        "an unreachable base must report every scope changed"
    );
}

#[test]
fn non_pull_request_events_run_everything() {
    for event in ["push", "schedule", "workflow_dispatch", "release"] {
        let out = Command::new("bash")
            .arg(script("changed-paths.sh"))
            .arg(event)
            .output()
            .expect("run changed-paths.sh");
        assert!(out.status.success(), "{event}: {}", stderr(&out));
        assert_eq!(
            scopes(&kv(&out)),
            (true, true, true, true),
            "event {event} must report every scope changed"
        );
    }
}

/// The real `git diff HEAD^1 HEAD` path, against a scratch repository with
/// an actual merge commit — the same shape `actions/checkout` leaves.
#[test]
fn pull_request_mode_diffs_the_merge_parents() {
    let repo = common::GitRepo::init();
    repo.write("src/lib.rs", "fn a() {}\n");
    repo.write("README.md", "# base\n");
    repo.commit(&["src/lib.rs", "README.md"], "base");
    repo.git().args(["checkout", "-b", "side"]).run();
    repo.write("docs/guide.md", "# guide\n");
    repo.commit(&["docs/guide.md"], "docs work");
    repo.git().args(["checkout", "-"]).run();
    repo.git()
        .args(["merge", "--no-ff", "-m", "merge side", "side"])
        .run();

    let out = Command::new("bash")
        .arg(script("changed-paths.sh"))
        .arg("pull_request")
        .current_dir(&repo.root)
        .output()
        .expect("run changed-paths.sh");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        scopes(&kv(&out)),
        (false, true, false, true),
        "a docs-only merge must classify as docs + docsref"
    );
}

// ---- aggregate-results.sh --------------------------------------------------

fn aggregate(needs_json: &str) -> Output {
    Command::new("bash")
        .arg(script("aggregate-results.sh"))
        .env("NEEDS_JSON", needs_json)
        .output()
        .expect("run aggregate-results.sh")
}

#[test]
fn all_success_and_skipped_passes() {
    let out = aggregate(
        r#"{"changes":{"result":"success"},"lint":{"result":"success"},"build":{"result":"skipped"}}"#,
    );
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn failure_and_cancelled_fail_the_gate() {
    for result in ["failure", "cancelled"] {
        let json = format!(r#"{{"a":{{"result":"success"}},"b":{{"result":"{result}"}}}}"#);
        assert!(
            !aggregate(&json).status.success(),
            "a {result} need must fail the aggregate"
        );
    }
}

#[test]
fn missing_or_empty_results_fail_closed() {
    for json in [
        r#"{"a":{"result":""}}"#,
        r#"{"a":{}}"#,
        r#"{"a":{"result":"weird"}}"#,
        "{}",
        "not json",
    ] {
        assert!(
            !aggregate(json).status.success(),
            "payload {json:?} must not pass the gate"
        );
    }
}

// ---- workflow contract -----------------------------------------------------

fn workflow(name: &str) -> serde_norway::Value {
    let text = std::fs::read_to_string(Path::new(ROOT).join(".github/workflows").join(name))
        .unwrap_or_else(|e| panic!("read {name}: {e}"));
    serde_norway::from_str(&text).unwrap_or_else(|e| panic!("{name} must parse as YAML: {e}"))
}

fn job_ids(wf: &serde_norway::Value) -> BTreeSet<String> {
    wf["jobs"]
        .as_mapping()
        .expect("jobs is a map")
        .keys()
        .map(|k| k.as_str().unwrap().to_string())
        .collect()
}

fn needs(wf: &serde_norway::Value, job: &str) -> BTreeSet<String> {
    match &wf["jobs"][job]["needs"] {
        serde_norway::Value::String(s) => [s.clone()].into_iter().collect(),
        serde_norway::Value::Sequence(seq) => seq
            .iter()
            .map(|v| v.as_str().expect("needs entries are strings").to_string())
            .collect(),
        serde_norway::Value::Null => BTreeSet::new(),
        other => panic!("{job}.needs is neither a string nor a list: {other:?}"),
    }
}

fn job<'a>(wf: &'a serde_norway::Value, id: &str) -> &'a serde_norway::Value {
    &wf["jobs"][id]
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Every job in the workflow is either inside the aggregate's `needs`, the
/// aggregate itself, or explicitly report-only — anything else means a new
/// job was added without deciding whether it gates merges.
fn assert_needs_cover(wf: &serde_norway::Value, aggregate: &str, report_only: &[&str]) {
    let mut expected = job_ids(wf);
    expected.remove(aggregate);
    for id in report_only {
        expected.remove(*id);
    }
    assert_eq!(
        needs(wf, aggregate),
        expected,
        "{aggregate}.needs must cover every job except {report_only:?}"
    );
}

#[test]
fn ci_result_covers_every_gating_job() {
    let wf = workflow("ci.yml");
    // Coverage and TUI E2E report without gating — everything else a merge
    // waits on must be inside the aggregate's needs.
    assert_needs_cover(&wf, "ci-result", &["coverage", "tui-e2e"]);
}

#[test]
fn dogfood_result_covers_every_job() {
    let wf = workflow("dogfood.yml");
    assert_needs_cover(&wf, "dogfood-result", &[]);
}

#[test]
fn security_result_covers_every_gating_job() {
    let wf = workflow("security.yml");
    // advisories stays out: new advisories appear without any commit.
    assert_needs_cover(&wf, "security-result", &["advisories"]);
}

#[test]
fn aggregates_have_the_fixed_names_and_always_run() {
    for (wf_name, job_id, display) in [
        ("ci.yml", "ci-result", "CI result"),
        ("dogfood.yml", "dogfood-result", "Dogfood result"),
        ("security.yml", "security-result", "Security result"),
    ] {
        let wf = workflow(wf_name);
        assert_eq!(
            job(&wf, job_id)["name"].as_str(),
            Some(display),
            "{wf_name}:{job_id} display name"
        );
        assert_eq!(
            job(&wf, job_id)["if"].as_str(),
            Some("always()"),
            "{wf_name}:{job_id} must run whatever the needs did"
        );
    }
}

#[test]
fn changes_job_classifies_the_merge_parents() {
    for wf_name in ["ci.yml", "dogfood.yml"] {
        let wf = workflow(wf_name);
        let changes = job(&wf, "changes");
        assert!(
            changes["if"].is_null(),
            "{wf_name}:changes must run on every trigger"
        );
        let outputs: BTreeSet<String> = changes["outputs"]
            .as_mapping()
            .expect("changes.outputs is a map")
            .keys()
            .map(|k| k.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            outputs,
            set(&["code", "docs", "site", "docsref"]),
            "{wf_name}:changes outputs"
        );
        // The diff base is the merge commit's parents — the checkout must
        // keep them reachable.
        let steps = changes["steps"].as_sequence().expect("steps list");
        let checkout = steps
            .iter()
            .find(|s| {
                s["uses"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("actions/checkout@"))
            })
            .expect("changes checks out");
        assert_eq!(
            checkout["with"]["fetch-depth"].as_u64(),
            Some(2),
            "{wf_name}:changes needs both merge parents"
        );
    }
}

#[test]
fn scoped_jobs_skip_on_the_right_scope() {
    let code_only = "needs.changes.outputs.code != 'false'";
    let ci = workflow("ci.yml");
    for id in ["lint", "build-test", "msrv", "coverage", "tui-e2e"] {
        assert!(
            needs(&ci, id).contains("changes"),
            "ci.yml:{id} must need changes"
        );
        assert_eq!(job(&ci, id)["if"].as_str(), Some(code_only), "ci.yml:{id}");
    }
    assert_eq!(
        job(&ci, "docs-reference")["if"].as_str(),
        Some("needs.changes.outputs.code != 'false' || needs.changes.outputs.docsref != 'false'"),
    );
    assert_eq!(
        job(&ci, "markdown-tests")["if"].as_str(),
        Some("needs.changes.outputs.code == 'false' && needs.changes.outputs.docs == 'true'"),
    );

    let dogfood = workflow("dogfood.yml");
    for id in ["release-build", "action-self-test", "dogfood-e2e"] {
        assert!(
            needs(&dogfood, id).contains("changes"),
            "{id} needs changes"
        );
        assert_eq!(job(&dogfood, id)["if"].as_str(), Some(code_only), "{id}");
    }
    // The shared release binary fans out: both consumers must wait on the
    // job that produces it.
    for id in ["action-self-test", "dogfood-e2e"] {
        assert!(
            needs(&dogfood, id).contains("release-build"),
            "{id} must wait on the shared release build"
        );
    }
}

#[test]
fn every_downstream_job_needs_changes() {
    for (wf_name, aggregate) in [("ci.yml", "ci-result"), ("dogfood.yml", "dogfood-result")] {
        let wf = workflow(wf_name);
        for id in job_ids(&wf) {
            if id == "changes" || id == aggregate {
                continue;
            }
            assert!(
                needs(&wf, &id).contains("changes"),
                "{wf_name}:{id} must need changes so it can scope-skip"
            );
        }
    }
}

#[test]
fn hygiene_runs_only_the_spell_check_without_code() {
    let wf = workflow("ci.yml");
    let steps = job(&wf, "hygiene")["steps"]
        .as_sequence()
        .expect("hygiene steps");
    let always = ["Checkout", "Spell check"];
    for (i, step) in steps.iter().enumerate() {
        let name = step["name"].as_str().unwrap_or("<unnamed>");
        if i < always.len() {
            assert!(
                name.contains(always[i]),
                "hygiene step {i} should be {}, got {name}",
                always[i]
            );
            assert!(
                step["if"].is_null(),
                "hygiene step {name:?} must run on every change shape"
            );
        } else {
            assert_eq!(
                step["if"].as_str(),
                Some("needs.changes.outputs.code != 'false'"),
                "hygiene step {name:?} must wait for the code scope"
            );
        }
    }
}

// ---- lint ------------------------------------------------------------------

/// `shellcheck` over the CI scripts — CI guarantees it (ubuntu-latest ships
/// it); locally it is optional, same policy as the action scripts.
#[test]
fn shellcheck_passes_on_the_ci_scripts() {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    for name in ["changed-paths.sh", "aggregate-results.sh"] {
        let out = Command::new("shellcheck")
            .arg(script(name))
            .output()
            .expect("run shellcheck");
        assert!(
            out.status.success(),
            "shellcheck {name}:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
