//! `--diff git:<rev>` — the baseline materialized from a git revision and
//! parsed from source. These tests build real repositories (see
//! `tests/common`'s `GitRepo`): init, commit the source fixture, mutate,
//! commit again — then run `dbtl` against revisions (`HEAD~1`, a sha, a
//! branch, a tag), in a subdirectory, against a dirty working tree, and
//! against the error shapes (unknown rev, no repo, no project at the rev,
//! a `-`-leading rev, `git` absent from `PATH`).

mod common;
use common::{copy_tree, dbtl_cmd, git_isolated_env, GitRepo, SAMPLE_PROJECT};
use std::path::Path;
use std::process::Command;

/// `dbtl` with the sealed git config (its spawned `git` children must see
/// the same isolation the test commits ran under) and a nulled stdin.
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

/// A repo holding the sample project at `rel`, committed twice: the
/// fixture verbatim (the baseline revision) and then with
/// `stg_orders.sql` extended (the change HEAD carries). Returns the repo
/// and the first commit's sha.
fn two_commit_repo(rel: &str) -> (GitRepo, String) {
    let repo = GitRepo::init();
    repo.copy_in(Path::new(SAMPLE_PROJECT), rel);
    let base = repo.commit(&[rel], "import fixture");
    let sql = repo.root.join(rel).join("models/staging/stg_orders.sql");
    let text = std::fs::read_to_string(&sql).unwrap();
    std::fs::write(&sql, format!("{text}\n-- touched\n")).unwrap();
    repo.commit(&[rel], "touch stg_orders");
    (repo, base)
}

// ---- happy paths -----------------------------------------------------------

#[test]
fn impact_reads_baseline_from_a_git_revision() {
    let (repo, _base) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    let out = run(&["impact", "--project", &root, "--diff", "git:HEAD~1"]);
    assert!(
        out.status.success(),
        "impact runs, stderr: {}",
        stderr(&out)
    );
    let out = stdout(&out);
    assert!(
        out.contains("stg_orders"),
        "the HEAD~1→HEAD change reports, got: {out:?}"
    );
    // The baseline's own label is the spec string verbatim.
    assert!(out.contains("`git:HEAD~1`"), "the rev label, got: {out:?}");
}

#[test]
fn impact_accepts_sha_branch_and_tag_revs() {
    let (repo, base_sha) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    repo.git().args(["branch", "baseline-br", &base_sha]).run();
    repo.git().args(["tag", "baseline-tag", &base_sha]).run();
    for rev in [&base_sha[..12], "baseline-br", "baseline-tag"] {
        let spec = format!("git:{rev}");
        let out = run(&["impact", "--project", &root, "--diff", &spec]);
        assert!(
            out.status.success(),
            "--diff {spec} resolves, stderr: {}",
            stderr(&out)
        );
        let out = stdout(&out);
        assert!(
            out.contains("stg_orders"),
            "--diff {spec} sees the baseline model, got: {out:?}"
        );
    }
}

#[test]
fn git_baseline_in_a_repo_subdirectory() {
    // The project lives BELOW the repo root — `git rev-parse
    // --show-prefix` yields `sub/proj/` and the listing/filter must scope
    // to it (a sibling tree it must never read exists beside it).
    let (repo, base) = two_commit_repo("sub/proj");
    repo.write("other/unrelated.sql", "select 1\n");
    repo.write("README.md", "# not the project\n");
    repo.commit(&["other", "README.md"], "sibling noise");
    let proj = repo.root.join("sub/proj").to_str().unwrap().to_string();
    let spec = format!("git:{base}");
    let out = run(&["impact", "--source", &proj, "--diff", &spec]);
    assert!(
        out.status.success(),
        "subdir project resolves, stderr: {}",
        stderr(&out)
    );
    let out = stdout(&out);
    assert!(
        out.contains("stg_orders") && !out.contains("unrelated"),
        "scoped to sub/proj, got: {out:?}"
    );
}

#[test]
fn git_baseline_ignores_the_dirty_working_tree() {
    // Baseline = the committed revision; the current side = the live
    // (dirty) tree. An uncommitted model must appear as ADDED vs the rev,
    // and a committed-then-deleted file as REMOVED.
    let (repo, _base) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    repo.write("models/marts/uncommitted_model.sql", "select 1 as id\n");
    let out = run(&["impact", "--project", &root, "--diff", "git:HEAD"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let out = stdout(&out);
    assert!(
        out.contains("uncommitted_model"),
        "the dirty file is current-side only, got: {out:?}"
    );
}

#[test]
fn git_baseline_leaves_the_repository_untouched() {
    // No checkout, no worktree, no index or stash writes — plumbing reads
    // only. Snapshot the four observables around the run.
    let (repo, _base) = two_commit_repo(".");
    let index_bytes = std::fs::read(repo.root.join(".git/index")).unwrap();
    let before = [
        repo.git().args(["status", "--porcelain"]).out(),
        repo.git().args(["stash", "list"]).out(),
        repo.git().args(["worktree", "list"]).out(),
        String::from_utf8_lossy(&index_bytes).into_owned(),
    ];
    let root = repo.root.to_str().unwrap().to_string();
    let out = run(&["check", "--project", &root, "--diff", "git:HEAD~1"]);
    assert!(
        matches!(out.status.code(), Some(0) | Some(1)),
        "check runs, stderr: {}",
        stderr(&out)
    );
    let index_bytes = std::fs::read(repo.root.join(".git/index")).unwrap();
    let after = [
        repo.git().args(["status", "--porcelain"]).out(),
        repo.git().args(["stash", "list"]).out(),
        repo.git().args(["worktree", "list"]).out(),
        String::from_utf8_lossy(&index_bytes).into_owned(),
    ];
    assert_eq!(before, after, "the run must not mutate the repository");
}

#[test]
fn git_baseline_leaves_no_temp_dir() {
    // The materialization scratch dir is a Drop guard — gone after BOTH
    // the success and the failure path. Point TMPDIR at an observable
    // scratch dir.
    let (repo, _base) = two_commit_repo(".");
    let scratch = tempfile::tempdir().unwrap();
    let root = repo.root.to_str().unwrap().to_string();
    for rev in ["git:HEAD", "git:no_such_rev"] {
        // TMPDIR covers unix; TMP/TEMP cover GetTempPath on Windows — the
        // scratch dir must be wherever `std::env::temp_dir()` lands.
        let out = dbtl(&["impact", "--project", &root, "--diff", rev])
            .env("TMPDIR", scratch.path())
            .env("TMP", scratch.path())
            .env("TEMP", scratch.path())
            .output()
            .expect("run dbtl");
        let _ = out;
        let leftovers: Vec<_> = std::fs::read_dir(scratch.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("dbtl-git-baseline-")
            })
            .collect();
        assert!(leftovers.is_empty(), "no temp dirs leak after {rev}");
    }
}

#[test]
fn git_baseline_reads_a_multi_mb_blob_without_stalling() {
    // `cat-file --batch` request/reply: a blob too big for a pipe buffer
    // must stream through — the regression this guards is a write-side
    // deadlock, which manifests as a hang rather than a failure.
    let repo = GitRepo::init();
    repo.copy_in(Path::new(SAMPLE_PROJECT), ".");
    // ~2.5 MB of SQL: far over the 64 KiB pipe buffer.
    let big = format!("select 1 as id\n-- {}\n", "x".repeat(2_500_000));
    repo.write("models/marts/big_model.sql", &big);
    repo.commit(&["."], "fixture + big blob");
    let root = repo.root.to_str().unwrap().to_string();
    let mut cmd = dbtl(&["impact", "--project", &root, "--diff", "git:HEAD"]);
    let out = cmd.output().expect("run dbtl");
    assert!(
        out.status.success(),
        "a multi-MB rev materializes, stderr: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("baseline"),
        "the diff report prints, got: {:?}",
        stdout(&out)
    );
}

#[test]
fn git_baseline_current_side_parses_source_symmetrically() {
    // A `target/manifest.json` sits next to the project — ordinarily the
    // auto-detect would pick it. With a git baseline the current side must
    // parse source anyway (manifest-vs-source loader differences would
    // masquerade as changes). Prove it by planting a DIFFERENT project's
    // manifest: had it loaded, every jaffle node would read as added.
    let (repo, _base) = two_commit_repo(".");
    repo.copy_in(
        &common::fixture_path("manifest.json"),
        "target/manifest.json",
    );
    let root = repo.root.to_str().unwrap().to_string();
    let out = run(&[
        "impact",
        "--project",
        &root,
        "--diff",
        "git:HEAD~1",
        "--format",
        "json",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let added = v["added"].as_array().unwrap();
    assert!(
        added.is_empty(),
        "manifest-side nodes must not appear (the current side parsed \
         source), added: {added:?}"
    );
}

#[test]
fn git_baseline_explicit_manifest_warns_but_continues() {
    let (repo, base_sha) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    // A manifest of the SAME project (the compiled fixture matches the
    // source fixture), so the run itself succeeds end to end.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.json");
    std::fs::copy(common::sample_manifest_path(), &manifest).unwrap();
    let spec = format!("git:{base_sha}");
    let out = run(&[
        "impact",
        "--manifest",
        manifest.to_str().unwrap(),
        // `--project` scopes the git lookup to the test repo (the rev is
        // resolved against `--source` else `--project`).
        "--project",
        &root,
        "--diff",
        &spec,
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("loader"),
        "the loader-asymmetry warning, got: {:?}",
        stderr(&out)
    );
}

#[test]
fn check_runs_diff_gates_off_a_git_baseline() {
    let (repo, _base) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    let out = run(&["check", "--project", &root, "--diff", "git:HEAD~1"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let out = stdout(&out);
    for gate in [
        "PASS coverage:",
        "baseline: git:HEAD~1",
        "PASS new layer violations:",
        "PASS untested changes:",
    ] {
        assert!(out.contains(gate), "missing {gate:?} in:\n{out}");
    }
}

// ---- errors ----------------------------------------------------------------

#[test]
fn git_baseline_unknown_rev_is_an_input_error_with_a_hint() {
    let (repo, _base) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    let out = run(&["impact", "--project", &root, "--diff", "git:no_such_rev"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("cannot resolve git revision") && err.contains("fetch-depth"),
        "the shallow-clone hint, got: {err:?}"
    );
}

#[test]
fn git_baseline_outside_a_repository_is_an_input_error() {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(Path::new(SAMPLE_PROJECT), &tmp.path().join("proj"));
    let proj = tmp.path().join("proj").to_str().unwrap().to_string();
    let out = run(&["impact", "--source", &proj, "--diff", "git:HEAD"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("not inside a git repository"),
        "got: {:?}",
        stderr(&out)
    );
}

#[test]
fn git_baseline_without_a_project_at_the_rev_is_an_input_error() {
    // The rev exists but holds no `dbt_project.yml` under the prefix.
    let repo = GitRepo::init();
    repo.write("README.md", "# just docs\n");
    repo.commit(&["README.md"], "docs only");
    repo.copy_in(Path::new(SAMPLE_PROJECT), ".");
    repo.commit(&["."], "add project");
    let root = repo.root.to_str().unwrap().to_string();
    let out = run(&["impact", "--project", &root, "--diff", "git:HEAD~1"]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("no dbt project"),
        "got: {:?}",
        stderr(&out)
    );
}

#[test]
fn git_baseline_dash_rev_is_a_usage_error() {
    // `git:-p` dies as a usage error before any repository work — a rev
    // must never reach git as a flag. (No repo needed: the check precedes
    // every git call.)
    let m = common::manifest_path();
    let out = run(&[
        "impact",
        "--manifest",
        m.to_str().unwrap(),
        "--diff",
        "git:-p",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a flag-shaped rev is usage, not input: {}",
        stderr(&out)
    );
}

#[test]
fn git_baseline_missing_git_binary_is_an_input_error() {
    let (repo, _base) = two_commit_repo(".");
    let root = repo.root.to_str().unwrap().to_string();
    // An empty PATH: `git` can't spawn — an input error, never a crash.
    let out = dbtl(&["impact", "--project", &root, "--diff", "git:HEAD"])
        .env("PATH", "/nonexistent-dbtl-test-dir")
        .output()
        .expect("run dbtl");
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("git"),
        "the error names the tool, got: {:?}",
        stderr(&out)
    );
}
