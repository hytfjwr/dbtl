//! The composite action's shell helpers (`action/*.sh`), exercised the way
//! the runner exercises them: `lib.sh` is sourced into bash and each
//! function is called with plain arguments — no network, no workflow. This
//! pins the install-time contract: version resolution, artifact selection,
//! checksum verification, baseline-rev resolution, and `args` splitting.
//!
//! Unix-only: the action's scripts target bash (Windows runners get Git
//! Bash), and the suite assumes a POSIX filesystem for scratch files.

#![cfg(unix)]

mod common;

use common::{lines, stderr, stdout};
use std::path::Path;
use std::process::{Command, Output};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Run `body` in bash with `action/lib.sh` sourced as `$1`.
fn bash(body: &str) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$1\"; {body}"))
        .arg("dbtl-test")
        .arg(format!("{ROOT}/action/lib.sh"))
        .output()
        .expect("spawn bash")
}

// ---- version resolution ----------------------------------------------------

#[test]
fn version_resolves_from_a_version_tag_ref() {
    let out = bash("dbtl_resolve_version '' v0.6.0");
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "v0.6.0");
}

#[test]
fn version_resolves_from_a_bare_semver_ref() {
    let out = bash("dbtl_resolve_version '' 0.6.0");
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "v0.6.0");
}

#[test]
fn version_normalizes_the_explicit_input() {
    for (input, want) in [
        ("0.6.0", "v0.6.0"),
        ("v0.6.0", "v0.6.0"),
        ("v12.3.45", "v12.3.45"),
    ] {
        let out = bash(&format!("dbtl_resolve_version '{input}' ignored"));
        assert!(out.status.success(), "{input}: stderr: {}", stderr(&out));
        assert_eq!(stdout(&out).trim(), want, "input {input}");
    }
}

#[test]
fn version_local_passes_through() {
    let out = bash("dbtl_resolve_version local ''");
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "local");
}

#[test]
fn version_input_wins_over_the_ref() {
    let out = bash("dbtl_resolve_version v1.2.3 v9.9.9");
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "v1.2.3");
}

#[test]
fn version_sha_pinned_ref_is_an_error_asking_for_the_input() {
    let out = bash("dbtl_resolve_version '' 0123456789abcdef0123456789abcdef01234567");
    assert!(!out.status.success(), "a SHA ref must not resolve silently");
    let err = stderr(&out);
    assert!(
        err.contains("version") && err.contains("explicitly"),
        "the error must tell the user to set the version input, got: {err:?}"
    );
}

#[test]
fn version_branch_ref_is_an_error() {
    let out = bash("dbtl_resolve_version '' main");
    assert!(!out.status.success(), "a branch ref has no version");
}

#[test]
fn version_rejects_malformed_values() {
    for bad in [
        "../x",
        "v1",
        "v0.6",
        "0.6.0-rc1",
        "v0.6.0/../evil",
        "",
        "latest",
    ] {
        let out = bash(&format!("dbtl_resolve_version '{bad}' ''"));
        assert!(
            !out.status.success(),
            "{bad:?} must be rejected (it lands in a release URL)"
        );
    }
}

// ---- artifact selection ----------------------------------------------------

#[test]
fn artifact_selection_covers_the_shipped_targets() {
    let cases = [
        ("Linux", "X64", "x86_64-unknown-linux-gnu tar.gz"),
        ("Linux", "ARM64", "aarch64-unknown-linux-gnu tar.gz"),
        ("macOS", "ARM64", "aarch64-apple-darwin tar.gz"),
        ("macOS", "X64", "x86_64-apple-darwin tar.gz"),
        ("Windows", "X64", "x86_64-pc-windows-msvc zip"),
    ];
    for (os, arch, want) in cases {
        let out = bash(&format!("dbtl_select_artifact '{os}' '{arch}'"));
        assert!(out.status.success(), "{os}/{arch}: {}", stderr(&out));
        assert_eq!(stdout(&out).trim(), want, "{os}/{arch}");
    }
}

#[test]
fn artifact_selection_rejects_unsupported_runners() {
    for (os, arch) in [
        ("Windows", "ARM64"),
        ("Linux", "ARM"),
        ("FreeBSD", "X64"),
        ("macOS", "ARM"),
    ] {
        let out = bash(&format!("dbtl_select_artifact '{os}' '{arch}'"));
        assert!(
            !out.status.success(),
            "{os}/{arch} must be an explicit error, got {}",
            stdout(&out)
        );
    }
}

// ---- checksums --------------------------------------------------------------

/// Write `contents` into a fresh temp file and return (dir, path).
fn temp_file(name: &str, contents: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    (dir, path.to_string_lossy().into_owned())
}

#[test]
fn sha256_of_a_file_matches_the_known_digest() {
    // sha256("hello\n") is fixed; the helper's answer must be the real one.
    let (_dir, file) = temp_file("data.txt", "hello\n");
    let out = bash(&format!("dbtl_sha256 '{file}'"));
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
    );
}

#[test]
fn expected_sha256_reads_text_and_binary_mode_lines() {
    let (_dir, sums) = temp_file(
        "checksums.txt",
        "aaa111  dbtl-v0.6.0-x86_64-unknown-linux-gnu.tar.gz\n\
         bbb222 *dbtl-v0.6.0-x86_64-pc-windows-msvc.zip\n\
         ccc333  dbtl-v0.6.0-aarch64-apple-darwin.tar.gz\n",
    );
    for (name, want) in [
        ("dbtl-v0.6.0-x86_64-unknown-linux-gnu.tar.gz", "aaa111"),
        ("dbtl-v0.6.0-x86_64-pc-windows-msvc.zip", "bbb222"),
        ("dbtl-v0.6.0-aarch64-apple-darwin.tar.gz", "ccc333"),
    ] {
        let out = bash(&format!("dbtl_expected_sha256 '{sums}' '{name}'"));
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert_eq!(stdout(&out).trim(), want, "{name}");
    }
}

#[test]
fn expected_sha256_of_an_unlisted_artifact_is_empty() {
    let (_dir, sums) = temp_file("checksums.txt", "aaa111  dbtl-v0.6.0-x.tar.gz\n");
    let out = bash(&format!(
        "dbtl_expected_sha256 '{sums}' 'dbtl-v0.6.0-y.tar.gz'"
    ));
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "", "no line, no digest");
}

#[test]
fn require_sha256_names_the_missing_artifact() {
    let (_dir, sums) = temp_file("checksums.txt", "aaa111  dbtl-v0.6.0-x.tar.gz\n");
    let out = bash(&format!(
        "dbtl_require_sha256 '{sums}' 'dbtl-v0.6.0-x86_64-pc-windows-msvc.zip' 'v0.6.0' 'x86_64-pc-windows-msvc'"
    ));
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("release v0.6.0 has no artifact for x86_64-pc-windows-msvc"),
        "the error names the release and target, got: {err:?}"
    );
}

#[test]
fn verify_sha256_accepts_a_match_and_rejects_a_mismatch() {
    let (_dir, file) = temp_file("artifact.tar.gz", "hello\n");
    let good = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
    let (_dir2, sums) = temp_file(
        "checksums.txt",
        &format!("{good}  artifact.tar.gz\nbadbad  other.tar.gz\n"),
    );
    let out = bash(&format!(
        "dbtl_verify_sha256 '{sums}' 'artifact.tar.gz' '{file}'"
    ));
    assert!(out.status.success(), "a match verifies: {}", stderr(&out));
    let out = bash(&format!(
        "dbtl_verify_sha256 '{sums}' 'other.tar.gz' '{file}'"
    ));
    assert!(!out.status.success(), "a mismatch fails");
    assert!(
        stderr(&out).contains("checksum mismatch"),
        "got: {:?}",
        stderr(&out)
    );
}

// ---- baseline resolution -----------------------------------------------------

#[test]
fn base_resolves_a_pull_request_to_its_base_sha() {
    let sha = "a".repeat(40);
    let out = bash(&format!(
        "dbtl_resolve_base '' pull_request '{sha}' ignored"
    ));
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), sha);
}

#[test]
fn base_resolves_a_push_to_its_before_commit() {
    let sha = "b".repeat(40);
    let out = bash(&format!("dbtl_resolve_base '' push '' '{sha}'"));
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), sha);
}

#[test]
fn base_an_all_zero_before_means_no_baseline() {
    let zero = "0".repeat(40);
    let out = bash(&format!("dbtl_resolve_base '' push '' '{zero}'"));
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "", "a first push has no baseline");
}

#[test]
fn base_without_an_event_source_is_empty() {
    for event in ["workflow_dispatch", "schedule", "release", ""] {
        let out = bash(&format!("dbtl_resolve_base '' '{event}' '' ''"));
        assert!(out.status.success(), "{event}: {}", stderr(&out));
        assert_eq!(stdout(&out).trim(), "", "event {event:?} has no baseline");
    }
}

#[test]
fn base_explicit_input_wins_but_must_not_look_like_a_flag() {
    let out = bash("dbtl_resolve_base 'main' pull_request ignored ignored");
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "main");
    for bad in ["-p", "--force", "-"] {
        let out = bash(&format!("dbtl_resolve_base '{bad}' pull_request x y"));
        assert!(!out.status.success(), "{bad:?} must be rejected");
    }
}

// ---- args splitting ----------------------------------------------------------

#[test]
fn split_args_splits_on_whitespace() {
    let out = bash("dbtl_split_args '--coverage-min 100 --select fct_*'");
    assert!(out.status.success());
    assert_eq!(lines(&out), ["--coverage-min", "100", "--select", "fct_*"]);
}

#[test]
fn split_args_collapses_repeated_whitespace_and_handles_empty() {
    let out = bash("dbtl_split_args '   --a    --b   '");
    assert!(out.status.success());
    assert_eq!(lines(&out), ["--a", "--b"]);
    let out = bash("dbtl_split_args ''");
    assert!(out.status.success());
    assert_eq!(stdout(&out), "", "empty input splits to nothing");
    // Whitespace-only input must not become a single empty argument.
    let out = bash("dbtl_split_args '   '");
    assert!(out.status.success());
    assert_eq!(stdout(&out), "", "whitespace-only input splits to nothing");
}

#[test]
fn split_args_does_not_interpret_quotes_or_globs() {
    // Documented on the input: quoting is NOT interpreted, and globbing must
    // never expand `*` against the filesystem.
    let out = bash("dbtl_split_args '--x \"a b\" *'");
    assert!(out.status.success());
    assert_eq!(lines(&out), ["--x", "\"a", "b\"", "*"]);
}

// ---- the hook manifest -------------------------------------------------------

#[test]
fn pre_commit_hooks_yaml_declares_the_two_hooks() {
    let path = Path::new(ROOT).join(".pre-commit-hooks.yaml");
    let text = std::fs::read_to_string(&path).expect("read .pre-commit-hooks.yaml");
    let hooks: Vec<serde_norway::Value> =
        serde_norway::from_str(&text).expect(".pre-commit-hooks.yaml must parse as YAML");
    assert_eq!(hooks.len(), 2, "exactly two hooks");
    for (id, language) in [("dbtl-lint", "rust"), ("dbtl-lint-system", "system")] {
        let hook = hooks
            .iter()
            .find(|h| h["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("hook {id} missing"));
        assert_eq!(hook["language"].as_str(), Some(language), "{id}.language");
        assert_eq!(hook["entry"].as_str(), Some("dbtl lint"), "{id}.entry");
        assert_eq!(
            hook["pass_filenames"].as_bool(),
            Some(false),
            "{id}.pass_filenames"
        );
        let files = hook["files"].as_str().unwrap_or_default();
        for name in ["m.sql", "m.py", "s.yml", "s.yaml", "seed.csv", "doc.md"] {
            let out = bash(&format!("echo '{name}' | grep -Eq '{files}'"));
            assert!(
                out.status.success(),
                "{id}.files must match {name}: {files}"
            );
        }
        let out = bash(&format!("echo 'm.txt' | grep -Eq '{files}'"));
        assert!(!out.status.success(), "{id}.files must not match .txt");
    }
}

// ---- the action manifest ------------------------------------------------------

/// The parsed root `action.yml`.
fn action_manifest() -> serde_norway::Value {
    let path = Path::new(ROOT).join("action.yml");
    let text = std::fs::read_to_string(&path).expect("read action.yml");
    serde_norway::from_str(&text).expect("action.yml must parse as YAML")
}

#[test]
fn action_yml_declares_the_contracted_inputs_and_outputs() {
    let manifest = action_manifest();
    for input in [
        "version",
        "project-dir",
        "base-ref",
        "lint",
        "fail-on",
        "args",
        "step-summary",
        "sarif",
        "comment",
        "github-token",
        "dry-run",
    ] {
        assert!(
            manifest["inputs"][input].is_mapping(),
            "input {input} is missing"
        );
    }
    for output in ["exit-code", "report", "sarif"] {
        assert!(
            manifest["outputs"][output].is_mapping(),
            "output {output} is missing"
        );
    }
    assert_eq!(manifest["runs"]["using"].as_str(), Some("composite"));
}

/// The action's contract on `run:` blocks: they must carry no `${{ }}`
/// expression — inputs and event data reach the scripts only through `env:`,
/// so a crafted ref/argument can never become part of the command line.
#[test]
fn action_yml_run_blocks_hold_no_expressions() {
    let manifest = action_manifest();
    let steps = manifest["runs"]["steps"]
        .as_sequence()
        .expect("runs.steps is a list");
    assert!(!steps.is_empty());
    for step in steps {
        let name = step["name"].as_str().unwrap_or("<unnamed>");
        if let Some(run) = step["run"].as_str() {
            assert!(
                !run.contains("${{"),
                "step {name:?} interpolates an expression inside run:"
            );
        }
        if step["run"].is_string() {
            assert_eq!(
                step["shell"].as_str(),
                Some("bash"),
                "step {name:?} must run under bash"
            );
        }
    }
}

/// Forked pull requests get a read-only token: the steps that would write
/// (comment, SARIF upload) are gated on the head repo matching this repo —
/// `github.event.pull_request.head.repo.full_name == github.repository` —
/// and a warning step names the suppression instead. The runner evaluates
/// these expressions; the test pins that they exist and say what the
/// contract needs them to say.
#[test]
fn action_yml_suppresses_write_back_on_forked_pull_requests() {
    let manifest = action_manifest();
    let steps = manifest["runs"]["steps"]
        .as_sequence()
        .expect("runs.steps is a list");
    let fork_guard = |step: &serde_norway::Value| {
        let cond = step["if"].as_str().unwrap_or_default();
        cond.contains("head.repo.full_name")
    };
    let warning = steps
        .iter()
        .find(|s| {
            fork_guard(s)
                && s["run"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("::warning::")
        })
        .unwrap_or_else(|| {
            panic!("a step must warn that forked pull requests suppress write-back")
        });
    assert!(
        warning["if"]
            .as_str()
            .unwrap_or_default()
            .contains("head.repo.full_name != github.repository"),
        "the warning step fires exactly when the head repo differs"
    );
    for gated in ["Comment the report", "Upload the SARIF"] {
        let step = steps
            .iter()
            .find(|s| s["name"].as_str().unwrap_or_default().starts_with(gated))
            .unwrap_or_else(|| panic!("step {gated}* missing"));
        assert!(fork_guard(step), "{gated}* must carry the fork guard");
    }
}

// ---- lint --------------------------------------------------------------------

/// `shellcheck` is a CI guarantee (ubuntu-latest ships it); locally it is
/// optional — skip silently when absent rather than weakening the check.
#[test]
fn shellcheck_passes_on_the_action_scripts() {
    if Command::new("shellcheck")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    for script in [
        "lib.sh",
        "install.sh",
        "run.sh",
        "comment.sh",
        "sarif_dry_run.sh",
    ] {
        let out = Command::new("shellcheck")
            .arg(format!("{ROOT}/action/{script}"))
            .output()
            .expect("run shellcheck");
        assert!(
            out.status.success(),
            "shellcheck {script}:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
