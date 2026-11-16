//! The CI merge-gate plumbing: the changed-path classifier, the
//! needs-aggregator, the cache pruner, and the runner-loss retry decision
//! in `.github/scripts/`, plus the workflows' contract that every gating
//! job is reachable through an aggregate's `needs` list (a job added
//! without `needs` coverage can never slip through).
//!
//! Unix-only: the scripts target bash, like the action's helpers.

#![cfg(unix)]

mod common;

use common::{stderr, stdout};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn script(name: &str) -> PathBuf {
    Path::new(ROOT).join(".github/scripts").join(name)
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

// ---- prune-caches.sh -------------------------------------------------------

/// One cache-listing row the pruner reads: id, key, created_at, ref.
fn entry(id: u32, key: &str, created: &str) -> String {
    format!("{id}\t{key}\t{created}\trefs/heads/main\n")
}

/// Run the pruner over an explicit cache listing (the `--entries` seam)
/// and return the `(id, key)` pairs it would delete.
fn prune(entries: &str, prefixes: &[&str], keep_other: u32) -> Vec<(u32, String)> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("entries.tsv");
    std::fs::write(&file, entries).unwrap();
    let out = Command::new("bash")
        .arg(script("prune-caches.sh"))
        .arg("--entries")
        .arg(&file)
        .args(prefixes)
        .env("KEEP_OTHER", keep_other.to_string())
        .output()
        .expect("run prune-caches.sh");
    assert!(out.status.success(), "prune-caches: {}", stderr(&out));
    stdout(&out)
        .lines()
        .filter_map(|l| {
            let (id, key) = l.split_once('\t')?;
            Some((id.parse().unwrap(), key.to_string()))
        })
        .collect()
}

#[test]
fn prune_keeps_only_the_newest_per_series() {
    let entries = concat!(
        "1\tv0-rust-test-Linux-x64-2fd77104-ed522558\t2026-11-12T20:23:17Z\trefs/heads/main\n",
        "2\tv0-rust-test-Linux-x64-2fd77104-0bdeb884\t2026-11-13T07:45:43Z\trefs/heads/main\n",
        "3\tv0-rust-test-Linux-x64-2fd77104-5845f4ce\t2026-11-13T03:08:20Z\trefs/heads/main\n",
    );
    let victims = prune(entries, &["v0-rust-test-"], 1);
    assert_eq!(
        victims,
        vec![
            (3, "v0-rust-test-Linux-x64-2fd77104-5845f4ce".to_string()),
            (1, "v0-rust-test-Linux-x64-2fd77104-ed522558".to_string()),
        ],
        "every generation but the newest must be pruned — id 2 is the generation main last wrote"
    );
}

#[test]
fn prune_folds_changed_env_hashes_into_one_series() {
    // A changed CARGO_*/RUST* set rewrites the env-hash segment, but the
    // entries still form one series — the stale-env generation retires.
    let entries = concat!(
        "4\tv0-rust-test-Windows_NT-x64-e70cdc14-ed522558\t2026-11-12T11:16:44Z\trefs/heads/main\n",
        "5\tv0-rust-test-Windows_NT-x64-ea627442-0bdeb884\t2026-11-13T07:46:30Z\trefs/heads/main\n",
    );
    let victims = prune(entries, &["v0-rust-test-"], 1);
    assert_eq!(
        victims,
        vec![(
            4,
            "v0-rust-test-Windows_NT-x64-e70cdc14-ed522558".to_string()
        )]
    );
}

#[test]
fn prune_never_touches_unmatched_keys_or_other_refs() {
    let mut entries = String::new();
    entries.push_str(&entry(
        1,
        "actionlint-Linux-v1.7.12",
        "2026-11-09T09:30:31Z",
    ));
    entries.push_str(&entry(2, "pinact-Linux-v4.1.0", "2026-11-09T09:30:43Z"));
    // A different series than the ones being pruned.
    entries.push_str(&entry(
        3,
        "v0-rust-perf-Linux-x64-2fd77104-5e3c6036",
        "2026-11-13T01:31:12Z",
    ));
    // Same series as a pruned one, but on a branch ref — never a main
    // generation.
    entries.push_str(
        "4\tv0-rust-test-Linux-x64-2fd77104-aaaaaaa\t2026-11-12T00:00:00Z\trefs/heads/side\n",
    );
    entries.push_str(&entry(
        5,
        "v0-rust-test-Linux-x64-2fd77104-0bdeb884",
        "2026-11-13T07:45:43Z",
    ));
    let victims = prune(&entries, &["v0-rust-test-"], 1);
    assert!(
        victims.is_empty(),
        "only main-ref entries under the given prefixes may be pruned: {victims:?}"
    );
}

#[test]
fn prune_content_keyed_entries_keep_the_newest_n() {
    // perf-bin-* bundles are keyed by the input SHA — every entry is a
    // distinct merge base, so the group keeps several recent entries.
    let mut entries = String::new();
    for i in 0..12 {
        entries.push_str(&entry(
            i,
            &format!("perf-bin-Linux-56625f11cb9e645f-{i:040x}"),
            &format!("2026-11-{day:02}T00:00:00Z", day = i + 1),
        ));
    }
    let victims = prune(&entries, &["perf-bin-"], 10);
    assert_eq!(victims.len(), 2, "the oldest two must go, newest ten stay");
    // Victims come out newest-first within the group: day 2, then day 1.
    assert_eq!(victims[0].0, 1);
    assert_eq!(victims[1].0, 0);
}

#[test]
fn prune_empty_listing_is_a_no_op() {
    let victims = prune("", &["v0-rust-test-"], 1);
    assert!(victims.is_empty());
}

// ---- run-shellcheck.sh -----------------------------------------------------

fn run_shellcheck(args: &[&str]) -> Output {
    Command::new("bash")
        .arg(script("run-shellcheck.sh"))
        .args(args)
        .output()
        .expect("run run-shellcheck.sh")
}

/// Every tracked `*.sh` is a lint target, and every reported target is a
/// real file that is shell by name or by shebang.
#[test]
fn lint_targets_cover_every_tracked_script() {
    let expected = common::git(Path::new(ROOT))
        .args(["ls-files", "--", "*.sh"])
        .out();
    let out = run_shellcheck(&["--print-targets"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let targets: BTreeSet<String> = stdout(&out).lines().map(String::from).collect();
    for f in expected.lines() {
        assert!(targets.contains(f), "{f} must be a lint target");
    }
    for t in &targets {
        let p = Path::new(ROOT).join(t);
        assert!(p.is_file(), "reported target {t} must exist");
        if !t.ends_with(".sh") {
            let first = std::fs::read_to_string(&p)
                .unwrap()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            assert!(
                first.starts_with("#!"),
                "{t} is a lint target but has no shebang"
            );
        }
    }
}

/// The target scan is enumeration, not a list: a tracked file with a shell
/// shebang joins the set whatever its name, an untracked file never does.
#[test]
fn lint_targets_follow_the_shebang_not_the_extension() {
    let repo = common::GitRepo::init();
    repo.write("x.sh", "echo hi\n");
    repo.write("tool", "#!/usr/bin/env bash\necho hi\n");
    repo.write("helper", "#!/bin/sh\necho hi\n");
    repo.write("envs", "#!/usr/bin/env -S bash\necho hi\n");
    repo.write("note", "#!/usr/bin/env python3\nprint()\n");
    repo.write("x.ps1", "#!/usr/bin/env pwsh\nWrite-Output hi\n");
    repo.write("x.fish", "#!/usr/bin/env fish\necho hi\n");
    repo.write("plain.txt", "not a script\n");
    repo.write("untracked.sh", "echo hi\n");
    repo.commit(
        &[
            "x.sh",
            "tool",
            "helper",
            "envs",
            "note",
            "x.ps1",
            "x.fish",
            "plain.txt",
        ],
        "seed",
    );

    let out = Command::new("bash")
        .arg(script("run-shellcheck.sh"))
        .arg("--print-targets")
        .arg(&repo.root)
        .output()
        .expect("run run-shellcheck.sh");
    assert!(out.status.success(), "{}", stderr(&out));
    let targets: BTreeSet<String> = stdout(&out).lines().map(String::from).collect();
    assert_eq!(
        targets,
        set(&["x.sh", "tool", "helper", "envs"]),
        "tracked .sh plus shell-shebang files, nothing else"
    );
}

/// The composite action's `run:` values are linted as snippets — each
/// extracted command must be a complete single line.
#[test]
fn extract_runs_pulls_each_action_step_command() {
    let action = Path::new(ROOT).join("action.yml");
    let out = run_shellcheck(&["--extract-runs", action.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let body = stdout(&out);
    let snippets: Vec<&str> = body
        .lines()
        .map(|l| l.split_once('\t').expect("LINE<TAB>command").1)
        .collect();
    assert_eq!(
        snippets.len(),
        7,
        "the action's script-calling steps must all be extracted: {snippets:?}"
    );
    for text in &snippets {
        assert!(!text.is_empty(), "an extracted run: must not be empty");
        assert!(
            !text.starts_with('|') && !text.starts_with('>'),
            "a block scalar must never reach linting: {text}"
        );
    }
}

/// A multi-line `run:` can't be reconstructed faithfully in shell — the
/// extractor fails closed and tells the author to move it into a tracked
/// script instead.
#[test]
fn extract_runs_rejects_multiline_blocks() {
    for (name, body) in [
        (
            "block scalar",
            "runs:\n  steps:\n    - run: |\n        echo a\n        echo b\n",
        ),
        (
            "folded scalar",
            "runs:\n  steps:\n    - run: >-\n        echo a\n",
        ),
        ("empty run", "runs:\n  steps:\n    - run:\n"),
        ("continuation", "runs:\n  steps:\n    - run: echo a &&\n"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("action.yml");
        std::fs::write(&f, body).unwrap();
        let out = run_shellcheck(&["--extract-runs", f.to_str().unwrap()]);
        assert!(!out.status.success(), "{name} must fail: {}", stdout(&out));
        assert!(
            stderr(&out).contains("multi-line"),
            "{name} must explain the failure: {}",
            stderr(&out)
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
    // Coverage, TUI E2E, and cache housekeeping report without gating —
    // everything else a merge waits on must be inside the aggregate's
    // needs.
    assert_needs_cover(&wf, "ci-result", &["coverage", "tui-e2e", "cache-prune"]);
}

#[test]
fn dogfood_result_covers_every_job() {
    let wf = workflow("dogfood.yml");
    assert_needs_cover(&wf, "dogfood-result", &["cache-prune"]);
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
    assert!(
        needs(&dogfood, "dogfood").contains("changes"),
        "dogfood needs changes"
    );
    assert_eq!(
        job(&dogfood, "dogfood")["if"].as_str(),
        Some(code_only),
        "dogfood"
    );
}

/// The build and both consumers of its binary share one job, so the order of
/// its steps is the contract the old job graph (`needs: release-build`)
/// used to carry: the binary exists and is on PATH before anything runs it,
/// and the action self-test comes before the E2E.
#[test]
fn dogfood_job_builds_before_it_runs_the_binary() {
    let wf = workflow("dogfood.yml");
    let steps = job(&wf, "dogfood")["steps"]
        .as_sequence()
        .expect("dogfood steps");
    let pos = |name: &str| {
        steps
            .iter()
            .position(|s| s["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("dogfood has no step named {name:?}"))
    };
    let order = [
        "Build dbtl (release)",
        "Put dbtl on PATH",
        "Run the action (passing check)",
        "Run the action (failing check)",
        "Run the full check report",
        "Exercise the git-revision baseline",
    ];
    let positions: Vec<usize> = order.iter().map(|n| pos(n)).collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "dogfood steps must run in this order: {order:?} (got positions {positions:?})"
    );
    assert!(
        steps
            .iter()
            .all(|s| s["parallel"].is_null() && s["background"].is_null()),
        "the self-test runs a composite action — the dogfood steps stay sequential"
    );
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

/// The cache-prune jobs run only after their own workflow's writers on a
/// main push — a pruned series must always keep the generation main last
/// wrote, and the delete scope needs `actions: write` on that job alone.
#[test]
fn cache_prune_jobs_wait_for_their_writers() {
    for (wf_name, writers) in [
        (
            "ci.yml",
            &["changes", "lint", "build-test", "msrv", "hygiene"][..],
        ),
        ("dogfood.yml", &["changes", "dogfood"][..]),
        ("perf.yml", &["perf-prebuild"][..]),
        ("wasm-cache.yml", &["engine-build"][..]),
    ] {
        let wf = workflow(wf_name);
        let prune = job(&wf, "cache-prune");
        assert_eq!(
            needs(&wf, "cache-prune"),
            writers.iter().map(|s| s.to_string()).collect(),
            "{wf_name}:cache-prune must wait on exactly its workflow's writers"
        );
        assert_eq!(
            prune["if"].as_str(),
            Some("always() && github.event_name == 'push' && github.ref == 'refs/heads/main'"),
            "{wf_name}:cache-prune runs only after a main push"
        );
        assert_eq!(
            prune["permissions"]["actions"].as_str(),
            Some("write"),
            "{wf_name}:cache-prune needs actions: write to delete cache entries"
        );
    }
}

/// `uses:` steps that would run concurrently with another step: every child
/// of a `parallel:` group and every `background: true` step. The runner races
/// concurrent `uses:` steps (shared action downloads, composite-action state,
/// the event payload file), so only `run:` steps may run concurrently. Each
/// returned entry names the offending step.
fn concurrent_uses_steps(wf: &serde_norway::Value) -> Vec<String> {
    let mut bad = Vec::new();
    let Some(jobs) = wf["jobs"].as_mapping() else {
        return bad;
    };
    for (id, job) in jobs {
        let id = id.as_str().unwrap_or("?");
        let Some(steps) = job["steps"].as_sequence() else {
            continue;
        };
        for step in steps {
            let mut concurrent: Vec<&serde_norway::Value> = Vec::new();
            if let Some(group) = step["parallel"].as_sequence() {
                concurrent.extend(group);
            }
            if step["background"].as_bool() == Some(true) {
                concurrent.push(step);
            }
            for s in concurrent {
                if !s["uses"].is_null() {
                    let name = s["name"].as_str().unwrap_or("<unnamed>");
                    bad.push(format!("{id}: {name}"));
                }
            }
        }
    }
    bad
}

#[test]
fn concurrent_steps_are_run_steps_only() {
    let dir = Path::new(ROOT).join(".github/workflows");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("read .github/workflows") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let bad = concurrent_uses_steps(&workflow(&name));
        assert!(
            bad.is_empty(),
            "{name}: `uses:` steps must not sit in a `parallel:` group or run with `background: true`: {bad:?}"
        );
        checked += 1;
    }
    assert!(checked > 0, "no workflows were checked");
}

#[test]
fn concurrent_uses_steps_are_detected() {
    let wf: serde_norway::Value = serde_norway::from_str(
        r#"
jobs:
  a:
    steps:
      - parallel:
          - name: fine
            run: echo ok
          - name: grouped action
            uses: actions/checkout@v1
      - name: background action
        background: true
        uses: ./
      - name: background script
        background: true
        run: echo ok
      - name: plain action
        uses: actions/checkout@v1
      - wait-all:
"#,
    )
    .unwrap();
    assert_eq!(
        concurrent_uses_steps(&wf),
        vec![
            "a: grouped action".to_string(),
            "a: background action".to_string()
        ]
    );
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

/// `shellcheck` over every tracked script and the composite action's run:
/// snippets — the same pass the workflow job runs. CI guarantees shellcheck
/// (ubuntu-latest ships it); locally it is optional.
#[test]
fn shellcheck_passes_on_the_ci_scripts() {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let out = run_shellcheck(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The happy path above proves a clean tree passes; this proves the
/// inverse — a lint finding inside the repo makes the run exit non-zero,
/// so a regression cannot land silently.
#[test]
fn shellcheck_fails_on_a_violating_script() {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let repo = common::GitRepo::init();
    // SC2162: `read` without -r mangles backslashes.
    repo.write("bad.sh", "read x\n");
    repo.commit(&["bad.sh"], "seed");
    let out = Command::new("shellcheck")
        .arg(repo.root.join("bad.sh"))
        .output()
        .expect("run shellcheck");
    assert!(
        !out.status.success(),
        "shellcheck itself must flag the fixture — else this test proves nothing"
    );
    let out = Command::new("bash")
        .arg(script("run-shellcheck.sh"))
        .current_dir(&repo.root)
        .output()
        .expect("run run-shellcheck.sh");
    assert!(
        !out.status.success(),
        "a lint violation must fail the run: {}",
        stdout(&out)
    );
}

/// Action discovery is enumeration, not a single hardcoded path: a
/// composite action under `.github/actions/` joins the run: lint set.
#[test]
fn shellcheck_fails_on_a_nested_action_run() {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let repo = common::GitRepo::init();
    repo.write("clean.sh", "echo hi\n");
    repo.write(
        ".github/actions/x/action.yaml",
        "runs:\n  steps:\n    - run: read x\n",
    );
    repo.commit(&["clean.sh", ".github/actions/x/action.yaml"], "seed");
    let out = Command::new("bash")
        .arg(script("run-shellcheck.sh"))
        .current_dir(&repo.root)
        .output()
        .expect("run run-shellcheck.sh");
    assert!(
        !out.status.success(),
        "a violating nested action run: must fail the run: {}",
        stdout(&out)
    );
}

// ---- retry-runner-failures.sh ----------------------------------------------

/// The annotation GitHub leaves when no hosted runner ever picks the job
/// up — the only signal that separates a runner-acquisition loss from a
/// cancelled-while-queued job, which shares the same zero-step shape.
const ACQUIRED: &str =
    "The job was not acquired by Runner of type hosted even after multiple attempts";

/// A `GET /actions/runs/{id}` body carrying the fields the decision reads.
fn run_json(conclusion: &str, attempt: u32, head_sha: &str) -> String {
    format!(
        r#"{{"id":123,"name":"CI","event":"pull_request","conclusion":"{conclusion}","run_attempt":{attempt},"head_sha":"{head_sha}","head_branch":"feat/x","head_repository":{{"full_name":"o/r"}},"run_number":7,"html_url":"https://example.invalid/runs/123"}}"#
    )
}

/// A `GET /actions/runs/{id}/jobs` body from `(id, name, conclusion)` rows.
fn jobs_json(rows: &[(u64, &str, &str)]) -> String {
    let jobs: Vec<String> = rows
        .iter()
        .map(|(id, name, concl)| {
            format!(
                r#"{{"id":{id},"name":"{name}","conclusion":"{concl}","runner_name":"","steps":[]}}"#
            )
        })
        .collect();
    format!(
        r#"{{"total_count":{},"jobs":[{}]}}"#,
        rows.len(),
        jobs.join(",")
    )
}

/// A `GET /check-runs/{id}/annotations` body from annotation messages.
fn annotations_json(messages: &[&str]) -> String {
    let rows: Vec<String> = messages
        .iter()
        .map(|m| format!(r#"{{"annotation_level":"failure","message":"{m}"}}"#))
        .collect();
    format!("[{}]", rows.join(","))
}

/// Write the decision script's `--fixtures` directory layout into `dir`:
/// `run.json`, `jobs.json`, one `attempt-<n>-jobs.json` per earlier
/// attempt in `attempt_jobs`, one `annotations-<job>.json` per probed
/// candidate, and `branch-head` for the head branch's current tip
/// (`None` = the lookup cannot resolve it).
fn write_run_fixtures(
    dir: &Path,
    run: &str,
    jobs: &str,
    annotations: &[(u64, String)],
    branch_head: Option<&str>,
    attempt_jobs: &[(u32, u64)],
) {
    std::fs::write(dir.join("run.json"), run).unwrap();
    std::fs::write(dir.join("jobs.json"), jobs).unwrap();
    for (id, body) in annotations {
        std::fs::write(dir.join(format!("annotations-{id}.json")), body).unwrap();
    }
    for (attempt, count) in attempt_jobs {
        std::fs::write(
            dir.join(format!("attempt-{attempt}-jobs.json")),
            format!(r#"{{"total_count":{count},"jobs":[]}}"#),
        )
        .unwrap();
    }
    if let Some(sha) = branch_head {
        std::fs::write(dir.join("branch-head"), format!("{sha}\n")).unwrap();
    }
}

/// Replay the decision over recorded API payloads; `attempt_jobs` carries
/// the `total_count` each earlier attempt's jobs endpoint reported.
fn retry_decision(
    run: &str,
    jobs: &str,
    annotations: &[(u64, String)],
    branch_head: Option<&str>,
    attempt_jobs: &[(u32, u64)],
) -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().unwrap();
    write_run_fixtures(
        dir.path(),
        run,
        jobs,
        annotations,
        branch_head,
        attempt_jobs,
    );
    let out = Command::new("bash")
        .arg(script("retry-runner-failures.sh"))
        .arg("--fixtures")
        .arg(dir.path())
        .output()
        .expect("run retry-runner-failures.sh");
    assert!(
        out.status.success(),
        "a decision must always exit cleanly: {}",
        stderr(&out)
    );
    kv(&out)
}

/// The run a hosted-runner loss actually leaves: the affected jobs are
/// `cancelled` with no runner and no steps, and the aggregate check fails
/// because it watches them. Only those runs earn the one retry.
#[test]
fn runner_acquisition_loss_retries_the_run_once() {
    let map = retry_decision(
        &run_json("failure", 1, "aaaa0000"),
        &jobs_json(&[
            (1, "changes", "success"),
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "TUI E2E (macos-latest)", "cancelled"),
            (4, "Markdown tests", "skipped"),
            (5, "CI result", "failure"),
        ]),
        &[
            (2, annotations_json(&[ACQUIRED])),
            (3, annotations_json(&[ACQUIRED])),
        ],
        Some("aaaa0000"),
        &[],
    );
    assert_eq!(map["rerun"], "true", "{}", map["reason"]);
    assert_eq!(
        map["acquired"], "Build & Test (macos-latest), TUI E2E (macos-latest)",
        "the summary lists every lost job"
    );
}

/// One genuine failure among the cancelled jobs vetoes the retry — a test
/// failure is not something a re-run repairs.
#[test]
fn a_real_failure_mixed_in_vetoes_the_retry() {
    let map = retry_decision(
        &run_json("failure", 1, "aaaa0000"),
        &jobs_json(&[
            (1, "changes", "success"),
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "Lint", "failure"),
            (4, "CI result", "failure"),
        ]),
        &[
            (2, annotations_json(&[ACQUIRED])),
            (
                3,
                annotations_json(&["Process completed with exit code 1."]),
            ),
        ],
        Some("aaaa0000"),
        &[],
    );
    assert_eq!(map["rerun"], "false");
    assert!(map["reason"].contains("Lint"), "{}", map["reason"]);
}

/// An approval-gated attempt runs zero jobs, so it never counts as the
/// run's first real attempt: a bot-authored run's approved attempt — the
/// second by number, the first that ran — still earns the one retry.
#[test]
fn an_approval_wait_attempt_does_not_spend_the_retry() {
    let map = retry_decision(
        &run_json("failure", 2, "aaaa0000"),
        &jobs_json(&[
            (1, "changes", "success"),
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "CI result", "failure"),
        ]),
        &[(2, annotations_json(&[ACQUIRED]))],
        Some("aaaa0000"),
        // Attempt 1 sat in the approval gate: zero jobs.
        &[(1, 0)],
    );
    assert_eq!(map["rerun"], "true", "{}", map["reason"]);
}

/// The retry is spent by the first attempt that ran jobs, not by the
/// attempt counter — an earlier attempt that listed jobs means this
/// failure is not the first real attempt and is left to a human. That is
/// also what a second evaluation of an already-retried run sees: the
/// re-run advanced the attempt, so the retry can never loop.
#[test]
fn an_attempt_that_ran_jobs_spends_the_retry() {
    // The second attempt after a first attempt that ran jobs.
    let map = retry_decision(
        &run_json("failure", 2, "aaaa0000"),
        &jobs_json(&[
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "CI result", "failure"),
        ]),
        &[(2, annotations_json(&[ACQUIRED]))],
        Some("aaaa0000"),
        &[(1, 14)],
    );
    assert_eq!(map["rerun"], "false", "{}", map["reason"]);

    // The third attempt: an approval wait, then the retried attempt —
    // the shape a re-evaluated run shows after its one retry was used.
    let map = retry_decision(
        &run_json("failure", 3, "aaaa0000"),
        &jobs_json(&[
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "CI result", "failure"),
        ]),
        &[(2, annotations_json(&[ACQUIRED]))],
        Some("aaaa0000"),
        &[(1, 0), (2, 14)],
    );
    assert_eq!(map["rerun"], "false", "{}", map["reason"]);
    assert!(map["reason"].contains("attempt 2"), "{}", map["reason"]);
}

/// A run whose newest attempt lists no jobs is still waiting — the
/// approval gate a bot-authored run's re-run can land back in — or queued;
/// either way there is nothing to re-run. The conclusion it carries does
/// not matter: an approval wait reports `action_required`, and a run that
/// never spawned jobs can report `failure` (a workflow that could not
/// start), and both stay untouched.
#[test]
fn a_newest_attempt_without_jobs_is_left_alone() {
    for conclusion in ["action_required", "failure"] {
        let map = retry_decision(
            &run_json(conclusion, 3, "aaaa0000"),
            &jobs_json(&[]),
            &[],
            Some("aaaa0000"),
            &[(1, 0), (2, 14)],
        );
        assert_eq!(
            map["rerun"], "false",
            "conclusion {conclusion}: {}",
            map["reason"]
        );
        assert!(
            map["reason"].contains("no jobs"),
            "conclusion {conclusion}: {}",
            map["reason"]
        );
    }
}

/// Cancellations that are not a lost runner — a cancelled run outright,
/// or queued jobs dropped by a concurrency supersede — never carry the
/// annotation, so nothing retries them.
#[test]
fn cancellations_are_left_alone() {
    // A run-level cancel (a person or the concurrency group) ran no jobs
    // at all, so the zero-jobs gate reports it first — it shares the
    // queued/empty shape; the conclusion gate would veto it regardless.
    let map = retry_decision(
        &run_json("cancelled", 1, "aaaa0000"),
        &jobs_json(&[]),
        &[],
        Some("aaaa0000"),
        &[],
    );
    assert_eq!(map["rerun"], "false", "{}", map["reason"]);

    // A superseded queued job shows the same zero-step cancelled shape as
    // a lost runner but is annotated with the cancel reason — or with
    // nothing at all.
    for anns in [
        annotations_json(&[
            "Canceling since a higher priority waiting request for CI-refs/pull/1/merge exists",
            "The operation was canceled.",
        ]),
        annotations_json(&[]),
    ] {
        let map = retry_decision(
            &run_json("failure", 1, "aaaa0000"),
            &jobs_json(&[
                (1, "changes", "success"),
                (2, "Build & Test (macos-latest)", "cancelled"),
                (3, "CI result", "failure"),
            ]),
            &[(2, anns)],
            Some("aaaa0000"),
            &[],
        );
        assert_eq!(map["rerun"], "false", "{}", map["reason"]);
    }
}

/// When the branch tip moved past the run's head commit a newer run is
/// already covering that branch — re-running would cancel it through the
/// shared concurrency group. An unresolvable tip (deleted branch, gone
/// fork) proves nothing either, so it also vetoes.
#[test]
fn a_stale_or_unresolvable_head_vetoes_the_retry() {
    let jobs = jobs_json(&[
        (1, "changes", "success"),
        (2, "Build & Test (macos-latest)", "cancelled"),
        (3, "CI result", "failure"),
    ]);
    for head in [Some("bbbb1111"), None] {
        let map = retry_decision(
            &run_json("failure", 1, "aaaa0000"),
            &jobs,
            &[(2, annotations_json(&[ACQUIRED]))],
            head,
            &[],
        );
        assert_eq!(map["rerun"], "false", "head {head:?}: {}", map["reason"]);
    }
}

/// A run where only the aggregate check failed has no lost runner — its
/// failure is real output (a bug in the aggregation step, or a need that
/// genuinely failed), so it is left for a human.
#[test]
fn an_aggregate_only_failure_is_not_runner_loss() {
    let map = retry_decision(
        &run_json("failure", 1, "aaaa0000"),
        &jobs_json(&[
            (1, "changes", "success"),
            (2, "Lint", "success"),
            (3, "CI result", "failure"),
        ]),
        &[],
        Some("aaaa0000"),
        &[],
    );
    assert_eq!(map["rerun"], "false", "{}", map["reason"]);
}

// ---- retry-runner-sweep.sh -------------------------------------------------

/// One `workflow_runs[]` row of the `GET /actions/runs` listing the sweep
/// reads: id, workflow name, conclusion, the run's actor, created_at.
fn listed_run(id: u64, name: &str, conclusion: &str, actor: &str, created: &str) -> String {
    format!(
        r#"{{"id":{id},"name":"{name}","conclusion":"{conclusion}","actor":{{"login":"{actor}"}},"created_at":"{created}","html_url":"https://example.invalid/runs/{id}"}}"#
    )
}

/// A `GET /actions/runs` body from `listed_run` rows.
fn runs_listing(rows: &[String]) -> String {
    format!(
        r#"{{"total_count":{},"workflow_runs":[{}]}}"#,
        rows.len(),
        rows.join(",")
    )
}

/// Replay the sweep over recorded payloads: `listing` is the
/// `GET /actions/runs` body, and each `dir` named `<run-id>/` under the
/// fixture root holds that run's decision fixtures. Returns the script's
/// stdout lines.
fn sweep(dir: &Path, listing: &str, since: &str, extra_args: &[&str]) -> std::process::Output {
    std::fs::write(dir.join("runs.json"), listing).unwrap();
    let out = Command::new("bash")
        .arg(script("retry-runner-sweep.sh"))
        .arg("--decision")
        .arg(script("retry-runner-failures.sh"))
        .arg("--since")
        .arg(since)
        .arg("--fixtures")
        .arg(dir)
        .args(extra_args)
        .output()
        .expect("run retry-runner-sweep.sh");
    assert!(
        out.status.success(),
        "a sweep must always exit cleanly: {}",
        stderr(&out)
    );
    out
}

/// The shape the release pull request's failing CI run leaves in the
/// listing — a bot actor, the watched workflow, inside the window.
fn bot_run_fixture(dir: &Path) {
    write_run_fixtures(
        dir,
        &run_json("failure", 2, "aaaa0000"),
        &jobs_json(&[
            (1, "changes", "success"),
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "CI result", "failure"),
        ]),
        &[(2, annotations_json(&[ACQUIRED]))],
        Some("aaaa0000"),
        &[(1, 0)],
    );
}

/// The sweep evaluates only failed runs of the watched workflows whose
/// actor is github-actions[bot] inside the window — a human's failure is
/// the event path's business, anything else is nobody's retry to give.
#[test]
fn sweep_evaluates_only_recent_bot_failures() {
    let dir = tempfile::tempdir().unwrap();
    let listing = runs_listing(&[
        listed_run(
            9001,
            "CI",
            "failure",
            "github-actions[bot]",
            "2026-11-17T10:00:00Z",
        ),
        // Outside the window.
        listed_run(
            9002,
            "Dogfood",
            "failure",
            "github-actions[bot]",
            "2026-11-15T10:00:00Z",
        ),
        // A human's run — the event path already covers it.
        listed_run(9003, "CI", "failure", "hytfjwr", "2026-11-17T10:00:00Z"),
        // Not one of the watched workflows.
        listed_run(
            9004,
            "bump",
            "failure",
            "github-actions[bot]",
            "2026-11-17T10:00:00Z",
        ),
        // Not a failure.
        listed_run(
            9005,
            "CI",
            "success",
            "github-actions[bot]",
            "2026-11-17T10:00:00Z",
        ),
    ]);
    let run_dir = dir.path().join("9001");
    std::fs::create_dir(&run_dir).unwrap();
    bot_run_fixture(&run_dir);

    let out = sweep(dir.path(), &listing, "2026-11-17T00:00:00Z", &["--dry-run"]);
    let text = stdout(&out);
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("evaluated="))
            .collect::<Vec<_>>(),
        vec!["evaluated=9001"],
        "only the in-window bot failure of a watched workflow: {text}"
    );
    assert!(
        text.lines().any(|l| l == "rerun=9001"),
        "the decision's verdict surfaces: {text}"
    );
    assert!(
        !text.lines().any(|l| l.starts_with("issued=")),
        "a dry run never dispatches: {text}"
    );
    assert_eq!(kv(&out)["swept"], "1");
}

/// Every watched workflow's bot failures are candidates — the listing
/// filter must admit Dogfood and Security rows, not only CI's.
#[test]
fn sweep_evaluates_each_watched_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let listing = runs_listing(&[
        listed_run(
            9001,
            "CI",
            "failure",
            "github-actions[bot]",
            "2026-11-17T10:00:00Z",
        ),
        listed_run(
            9002,
            "Dogfood",
            "failure",
            "github-actions[bot]",
            "2026-11-17T09:00:00Z",
        ),
        listed_run(
            9003,
            "Security",
            "failure",
            "github-actions[bot]",
            "2026-11-17T08:00:00Z",
        ),
    ]);
    for id in ["9001", "9002", "9003"] {
        let run_dir = dir.path().join(id);
        std::fs::create_dir(&run_dir).unwrap();
        bot_run_fixture(&run_dir);
    }

    let out = sweep(dir.path(), &listing, "2026-11-17T00:00:00Z", &["--dry-run"]);
    let text = stdout(&out);
    // Newest first — the cap reads off the top of this order.
    assert_eq!(
        text.lines()
            .filter(|l| l.starts_with("evaluated="))
            .collect::<Vec<_>>(),
        vec!["evaluated=9001", "evaluated=9002", "evaluated=9003"],
        "all three watched workflows evaluate, newest first: {text}"
    );
    assert_eq!(kv(&out)["swept"], "3");
}

/// A candidate the decision vetoes still counts as evaluated — the sweep
/// reports it and moves on: no `rerun=`, no `issued=`, and no `error=`
/// (a veto is a verdict, not a decision failure).
#[test]
fn sweep_reports_a_vetoed_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let listing = runs_listing(&[listed_run(
        9001,
        "CI",
        "failure",
        "github-actions[bot]",
        "2026-11-17T10:00:00Z",
    )]);
    let run_dir = dir.path().join("9001");
    std::fs::create_dir(&run_dir).unwrap();
    // A real job failure mixed in with the lost runner — vetoed.
    write_run_fixtures(
        &run_dir,
        &run_json("failure", 1, "aaaa0000"),
        &jobs_json(&[
            (1, "changes", "success"),
            (2, "Build & Test (macos-latest)", "cancelled"),
            (3, "Build & Test (ubuntu-latest)", "failure"),
            (4, "CI result", "failure"),
        ]),
        &[
            (2, annotations_json(&[ACQUIRED])),
            (3, annotations_json(&["1 test failed"])),
        ],
        Some("aaaa0000"),
        &[],
    );

    let out = sweep(dir.path(), &listing, "2026-11-17T00:00:00Z", &[]);
    let text = stdout(&out);
    assert!(
        text.lines().any(|l| l == "evaluated=9001"),
        "the vetoed run is still evaluated: {text}"
    );
    assert!(
        !text
            .lines()
            .any(|l| l.starts_with("rerun=") || l.starts_with("issued=")),
        "a vetoed run is never re-issued: {text}"
    );
    assert!(
        !text.lines().any(|l| l.starts_with("error=")),
        "a veto is not a decision error: {text}"
    );
    assert_eq!(kv(&out)["swept"], "1");
}

/// Without `--dry-run` the accepted run is actually re-issued — the
/// schedule's whole point. The listing and the per-run decision are
/// unchanged from the dry-run case.
#[test]
fn sweep_issues_the_retry_when_not_a_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let listing = runs_listing(&[listed_run(
        9001,
        "CI",
        "failure",
        "github-actions[bot]",
        "2026-11-17T10:00:00Z",
    )]);
    let run_dir = dir.path().join("9001");
    std::fs::create_dir(&run_dir).unwrap();
    bot_run_fixture(&run_dir);

    let out = sweep(dir.path(), &listing, "2026-11-17T00:00:00Z", &[]);
    let text = stdout(&out);
    assert!(
        text.lines().any(|l| l == "issued=9001"),
        "the accepted run's re-run is dispatched: {text}"
    );
}

/// `--run-id` skips the listing entirely — and the actor filter with it —
/// so a manual dispatch can pin the evaluation to any run (a human picks
/// the target deliberately).
#[test]
fn sweep_run_id_evaluates_the_pinned_run() {
    let dir = tempfile::tempdir().unwrap();
    // The fixture directory IS the run's own decision fixture here; the
    // listing file is never read in this mode.
    bot_run_fixture(dir.path());
    let out = Command::new("bash")
        .arg(script("retry-runner-sweep.sh"))
        .arg("--decision")
        .arg(script("retry-runner-failures.sh"))
        .arg("--run-id")
        .arg("123")
        .arg("--fixtures")
        .arg(dir.path())
        .arg("--dry-run")
        .output()
        .expect("run retry-runner-sweep.sh --run-id");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.lines().any(|l| l == "evaluated=123"),
        "the pinned run is evaluated by its own fixtures: {text}"
    );
    assert!(text.lines().any(|l| l == "rerun=123"), "{text}");
}

/// The sweep's watched-workflow set lives in the shell script, while the
/// event path's lives in the workflow trigger — the two are the same set
/// by contract, and drift would leave a gating workflow reachable by
/// neither path.
#[test]
fn sweep_watches_the_same_workflows_the_event_path_does() {
    let wf = workflow("retry-runner-failures.yml");
    let mut triggered: Vec<String> = wf["on"]["workflow_run"]["workflows"]
        .as_sequence()
        .expect("workflow_run.workflows is a list")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    triggered.sort();

    let text = std::fs::read_to_string(script("retry-runner-sweep.sh")).unwrap();
    let marker = "WATCHED_NAMES='";
    let start = text.find(marker).expect("the sweep declares WATCHED_NAMES") + marker.len();
    let end = start + text[start..].find('\'').expect("WATCHED_NAMES closes");
    let mut swept: Vec<String> =
        serde_json::from_str(&text[start..end]).expect("WATCHED_NAMES is a JSON list");
    swept.sort();

    assert_eq!(
        triggered, swept,
        "the sweep's listing filter must name exactly the workflow_run set"
    );
}

/// The retry workflow keeps the event path for human-authored runs and
/// carries the scheduled + dispatch sweep for the bot's runs — the two
/// triggers must exist together or one population is never reached.
#[test]
fn retry_workflow_covers_both_paths() {
    let wf = workflow("retry-runner-failures.yml");
    let watched: BTreeSet<String> = wf["on"]["workflow_run"]["workflows"]
        .as_sequence()
        .expect("workflow_run.workflows is a list")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        watched,
        set(&["CI", "Dogfood", "Security"]),
        "the event path watches exactly the gating workflows"
    );
    assert!(
        !wf["on"]["schedule"].as_sequence().unwrap().is_empty(),
        "the bot's runs emit no workflow_run event — a schedule must reach them"
    );
    assert!(
        !wf["on"]["workflow_dispatch"].is_null(),
        "a manual evaluate-only path inspects the sweep"
    );

    // Both jobs evaluate through the same decision with the same
    // permissions — the sweep issues the same write the event path does.
    for id in job_ids(&wf) {
        let perms = &job(&wf, &id)["permissions"];
        assert_eq!(
            perms["actions"].as_str(),
            Some("write"),
            "{id} must hold actions: write to re-issue the failed jobs"
        );
        assert_eq!(
            perms["checks"].as_str(),
            Some("read"),
            "{id} must read check-run annotations"
        );
    }
    let sweep_if = job(&wf, "sweep")["if"].as_str().unwrap_or("");
    assert!(
        sweep_if.contains("schedule"),
        "the sweep job must run on the cron path: {sweep_if}"
    );
}
