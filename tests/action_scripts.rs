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

// ---- script bodies -----------------------------------------------------------
//
// The four scripts the action's steps execute, run end-to-end under bash
// with every external command replaced by a stub from tests/stubs/:
// `gh`, `curl`, `git`, and `dbtl` log their argv to $STUB_LOG, take their
// failure switches from $STUB_DIR/responses.env, and serve canned bytes —
// no network, no API, no real repository. The environment is hermetic
// (env_clear): an ambient GITHUB_* — a CI step's GITHUB_OUTPUT included —
// or credential can neither leak in nor out.

/// Run `action/<script>` under bash with the stubs first on PATH and the
/// scratch dir as STUB_DIR/HOME/TMPDIR/RUNNER_TEMP.
fn run_script(script: &str, tmp: &tempfile::TempDir, envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(format!("{ROOT}/action/{script}"))
        .current_dir(tmp.path())
        .env_clear()
        .env(
            "PATH",
            format!(
                "{ROOT}/tests/stubs:{}",
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", tmp.path())
        .env("TMPDIR", tmp.path())
        .env("RUNNER_TEMP", tmp.path())
        .env("STUB_DIR", tmp.path())
        .env("STUB_LOG", tmp.path().join("stub.log"));
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn bash")
}

/// Write the stub behavior knobs (`responses.env` is sourced by every stub).
fn stub_responses(tmp: &tempfile::TempDir, body: &str) {
    std::fs::write(tmp.path().join("responses.env"), body).unwrap();
}

/// The concatenated argv log of every stub call the script made (empty
/// when no stub ran — the file is only created on first append).
fn stub_log(tmp: &tempfile::TempDir) -> String {
    std::fs::read_to_string(tmp.path().join("stub.log")).unwrap_or_default()
}

/// The line `<name>=<value>`'s value from a GITHUB_OUTPUT-shaped file.
fn output_value<'a>(outputs: &'a str, name: &str) -> Option<&'a str> {
    outputs
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{name}=")))
}

/// sha256 via lib.sh's own `dbtl_sha256` — the same helper the script
/// verifies with, so the test never assumes sha256sum vs shasum.
fn sha256(file: &Path) -> String {
    let out = bash(&format!("dbtl_sha256 '{}'", file.display()));
    assert!(out.status.success(), "dbtl_sha256: {}", stderr(&out));
    stdout(&out).trim().to_string()
}

/// Stage a fake release in the curl stub's file dir: a `dbtl-v<ver>-<target>`
/// payload dir containing `files` (the happy path's `dbtl` is a shell
/// script reporting `<reported>`), tarred as `dbtl-v<ver>-<target>.tar.gz`,
/// plus a checksums.txt naming it. Returns the artifact basename.
fn stage_artifact(
    tmp: &tempfile::TempDir,
    version: &str,
    target: &str,
    files: &[(&str, &str)],
) -> String {
    let artifact = format!("dbtl-v{version}-{target}.tar.gz");
    let dir = format!("dbtl-v{version}-{target}");
    let payload_root = tmp.path().join("payload");
    for (rel, contents) in files {
        let p = payload_root.join(&dir).join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }
    let curl_dir = tmp.path().join("curl");
    std::fs::create_dir_all(&curl_dir).unwrap();
    let tgz = curl_dir.join(&artifact);
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&tgz)
        .arg("-C")
        .arg(&payload_root)
        .arg(&dir)
        .status()
        .expect("run tar");
    assert!(status.success(), "tar the fake artifact");
    let sha = sha256(&tgz);
    std::fs::write(
        curl_dir.join("checksums.txt"),
        format!("{sha}  {artifact}\n"),
    )
    .unwrap();
    artifact
}

/// The release artifact whose bundled `dbtl --version` reports `<reported>`.
fn stage_release(tmp: &tempfile::TempDir, version: &str, target: &str, reported: &str) -> String {
    let bin = format!("#!/usr/bin/env bash\necho 'dbtl {reported}'\n");
    stage_artifact(tmp, version, target, &[("dbtl", &bin)])
}

// ---- install.sh --------------------------------------------------------------

/// The release install end-to-end: checksums and the tarball download,
/// the sha256 gate, extraction, the binary's own `--version` probe, and
/// the bin-dir line appended to GITHUB_PATH.
#[test]
fn install_downloads_verifies_and_publishes_the_bin_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let artifact = stage_release(&tmp, "1.2.3", "x86_64-unknown-linux-gnu", "1.2.3");
    let github_path = tmp.path().join("github_path.txt");
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("DBTL_ACTION_REPOSITORY", "o/r"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
            ("GITHUB_PATH", github_path.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.contains(&format!("Verified sha256 of {artifact}")),
        "{so}"
    );
    assert!(so.contains("dbtl v1.2.3 ready"), "{so}");
    // GITHUB_PATH got exactly the extracted bin dir, which really carries
    // the runnable binary.
    let bin_dir = std::fs::read_to_string(&github_path).unwrap();
    let bin_dir = bin_dir.trim_end();
    assert!(
        bin_dir.ends_with("dbtl-v1.2.3-x86_64-unknown-linux-gnu"),
        "GITHUB_PATH line: {bin_dir}"
    );
    let ver = Command::new(Path::new(bin_dir).join("dbtl"))
        .arg("--version")
        .output()
        .expect("run the installed dbtl");
    assert_eq!(stdout(&ver).trim(), "dbtl 1.2.3");
    // Both downloads went through the curl stub — no real network.
    let log = stub_log(&tmp);
    assert!(log.contains("checksums.txt"), "{log}");
    assert!(log.contains(&artifact), "{log}");
}

/// `version: local` skips the whole download — the dbtl already on PATH
/// is the installed one.
#[test]
fn install_local_uses_the_path_dbtl() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_script("install.sh", &tmp, &[("DBTL_VERSION", "local")]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("Using dbtl from PATH:"),
        "{}",
        stdout(&out)
    );
    assert!(
        !stub_log(&tmp).lines().any(|l| l.starts_with("curl")),
        "no download should happen: {}",
        stub_log(&tmp)
    );
}

/// A version that isn't a release tag never reaches the download step.
#[test]
fn install_rejects_a_non_semver_version() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_script("install.sh", &tmp, &[("DBTL_VERSION", "nightly")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("::error::invalid version"),
        "{}",
        stderr(&out)
    );
}

/// An unsupported runner triple is an explicit error, not a guess.
#[test]
fn install_rejects_an_unsupported_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "ARM32"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unsupported runner Linux/ARM32"),
        "{}",
        stderr(&out)
    );
}

/// The release must list the runner's artifact in checksums.txt — the
/// check runs before the tarball download so "no artifact for your
/// runner" reads as such, not as a bare digest mismatch.
#[test]
fn install_requires_the_artifacts_checksum_line() {
    let tmp = tempfile::tempdir().unwrap();
    stage_release(&tmp, "1.2.3", "x86_64-unknown-linux-gnu", "1.2.3");
    std::fs::write(
        tmp.path().join("curl/checksums.txt"),
        format!(
            "{}  dbtl-v1.2.3-aarch64-apple-darwin.tar.gz\n",
            "0".repeat(64)
        ),
    )
    .unwrap();
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("DBTL_ACTION_REPOSITORY", "o/r"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("has no artifact for x86_64-unknown-linux-gnu"),
        "{}",
        stderr(&out)
    );
}

/// A corrupted download fails the sha256 gate — the binary is never run
/// or put on PATH.
#[test]
fn install_fails_on_a_checksum_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let artifact = stage_release(&tmp, "1.2.3", "x86_64-unknown-linux-gnu", "1.2.3");
    std::fs::write(
        tmp.path().join("curl/checksums.txt"),
        format!("{}  {artifact}\n", "0".repeat(64)),
    )
    .unwrap();
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("DBTL_ACTION_REPOSITORY", "o/r"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!("::error::checksum mismatch for {artifact}")),
        "{}",
        stderr(&out)
    );
}

/// A release that serves nothing (a bad tag, a deleted release) is an
/// install error naming the URL, not a silent skip.
#[test]
fn install_fails_when_the_release_download_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("DBTL_ACTION_REPOSITORY", "o/r"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("could not download checksums.txt"),
        "{}",
        stderr(&out)
    );
}

/// A binary that can't run on the runner (a wrong-arch tarball, say) is
/// caught by the post-install `--version` probe.
#[test]
fn install_fails_when_the_binary_reports_a_different_version() {
    let tmp = tempfile::tempdir().unwrap();
    stage_release(&tmp, "1.2.3", "x86_64-unknown-linux-gnu", "9.9.9");
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("DBTL_ACTION_REPOSITORY", "o/r"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("installed dbtl reports version '9.9.9', expected '1.2.3'"),
        "{}",
        stderr(&out)
    );
}

/// A tarball with no dbtl inside is an explicit error — not an empty
/// PATH line that fails mysteriously in the next step.
#[test]
fn install_fails_when_the_archive_has_no_binary() {
    let tmp = tempfile::tempdir().unwrap();
    stage_artifact(
        &tmp,
        "1.2.3",
        "x86_64-unknown-linux-gnu",
        &[("README", "no binary here\n")],
    );
    let out = run_script(
        "install.sh",
        &tmp,
        &[
            ("DBTL_VERSION", "1.2.3"),
            ("DBTL_ACTION_REPOSITORY", "o/r"),
            ("RUNNER_OS", "Linux"),
            ("RUNNER_ARCH", "X64"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("found no dbtl binary inside"),
        "{}",
        stderr(&out)
    );
}

// ---- run.sh ------------------------------------------------------------------

/// The happy path: inputs become `dbtl check` arguments (visible in both
/// the echoed `+` line and the stub's argv log), and the three outputs
/// are published — `sarif` empty when not requested.
#[test]
fn run_invokes_dbtl_check_and_publishes_outputs() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let outputs = tmp.path().join("outputs.txt");
    std::fs::write(tmp.path().join("dbtl_report.txt"), "# report body\n").unwrap();
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("DBTL_BASE_REF", "main"),
            ("DBTL_LINT", "true"),
            ("DBTL_FAIL_ON", "warn"),
            ("DBTL_STEP_SUMMARY", "true"),
            ("DBTL_ARGS", "--include-packages --select marts+"),
            ("GITHUB_OUTPUT", outputs.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("+ dbtl check"), "{so}");
    assert!(so.contains("dbtl check exited 0"), "{so}");
    // Every input reached the dbtl command line.
    let log = stub_log(&tmp);
    let dbtl_line = log
        .lines()
        .find(|l| l.starts_with("dbtl check"))
        .unwrap_or_else(|| panic!("dbtl check was not invoked: {log}"));
    for arg in [
        format!("--project {}", proj.display()),
        "--format github".to_string(),
        "--report markdown:".to_string(),
        format!("--path-prefix {}", proj.display()),
        "--diff git:main".to_string(),
        "--lint".to_string(),
        "--fail-on warn".to_string(),
        "--step-summary".to_string(),
        "--include-packages --select marts+".to_string(),
    ] {
        assert!(dbtl_line.contains(&arg), "missing {arg:?} in: {dbtl_line}");
    }
    // `main` is not a 40-hex base — no git resolution happened.
    assert!(!log.lines().any(|l| l.starts_with("git ")), "{log}");

    let written = std::fs::read_to_string(&outputs).unwrap();
    assert!(written.contains("exit-code=0\n"), "{written}");
    let report = output_value(&written, "report").unwrap_or_default();
    assert!(
        !report.is_empty() && Path::new(report).is_file(),
        "report=<{report}> must name a written file: {written}"
    );
    assert_eq!(std::fs::read_to_string(report).unwrap(), "# report body\n");
    assert!(written.contains("sarif=\n"), "{written}");
}

/// dbtl's verdict travels in the `exit-code` output while the script
/// itself always exits 0 — the action's last step re-raises it after
/// write-back had its chance.
#[test]
fn run_reports_dbtls_failure_as_the_exit_code_output() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let outputs = tmp.path().join("outputs.txt");
    stub_responses(&tmp, "dbtl_check_exit=4\n");
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("GITHUB_OUTPUT", outputs.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("dbtl check exited 4"),
        "{}",
        stdout(&out)
    );
    let written = std::fs::read_to_string(&outputs).unwrap();
    assert!(written.contains("exit-code=4\n"), "{written}");
    // The report is still published — dbtl writes it on gate failures.
    let report = output_value(&written, "report").unwrap_or_default();
    assert!(Path::new(report).is_file(), "{written}");
}

/// A requested SARIF report becomes the `sarif` output when dbtl writes
/// the file.
#[test]
fn run_publishes_the_sarif_path_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let outputs = tmp.path().join("outputs.txt");
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("DBTL_SARIF", "true"),
            ("GITHUB_OUTPUT", outputs.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stub_log(&tmp).contains("--report sarif:"),
        "{}",
        stub_log(&tmp)
    );
    let written = std::fs::read_to_string(&outputs).unwrap();
    let sarif = output_value(&written, "sarif").unwrap_or_default();
    assert!(Path::new(sarif).is_file(), "sarif=<{sarif}>: {written}");
    assert!(
        std::fs::read_to_string(sarif).unwrap().contains("\"runs\""),
        "the stub's SARIF body"
    );
}

/// A dbtl that dies before writing its report leaves `report=` empty —
/// the comment step then treats it as "no report produced".
#[test]
fn run_leaves_report_empty_when_dbtl_writes_none() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let outputs = tmp.path().join("outputs.txt");
    stub_responses(&tmp, "dbtl_skip_report=1\ndbtl_check_exit=1\n");
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("GITHUB_OUTPUT", outputs.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let written = std::fs::read_to_string(&outputs).unwrap();
    assert!(written.contains("exit-code=1\n"), "{written}");
    assert!(written.contains("report=\n"), "{written}");
}

/// project-dir must exist — the script fails before touching dbtl.
#[test]
fn run_fails_when_the_project_dir_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    let out = run_script(
        "run.sh",
        &tmp,
        &[("DBTL_PROJECT_DIR", missing.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("does not exist"), "{}", stderr(&out));
    assert!(
        !stub_log(&tmp).lines().any(|l| l.starts_with("dbtl")),
        "dbtl must not run: {}",
        stub_log(&tmp)
    );
}

/// A 40-hex base that the shallow checkout lacks is fetched from origin
/// before the check runs.
#[test]
fn run_fetches_a_missing_base_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let outputs = tmp.path().join("outputs.txt");
    let base = "a".repeat(40);
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("DBTL_BASE_REF", &base),
            ("GITHUB_OUTPUT", outputs.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let log = stub_log(&tmp);
    assert!(
        log.contains("rev-parse --verify --quiet") && log.contains(&base),
        "{log}"
    );
    assert!(
        log.contains(&format!("fetch --no-tags --depth=1 origin {base}")),
        "{log}"
    );
    assert!(log.contains(&format!("--diff git:{base}")), "{log}");
    // The pull-request event path lands on the same 40-hex fetch.
    let tmp2 = tempfile::tempdir().unwrap();
    let proj2 = tmp2.path().join("proj");
    std::fs::create_dir_all(&proj2).unwrap();
    let out2 = run_script(
        "run.sh",
        &tmp2,
        &[
            ("DBTL_PROJECT_DIR", proj2.to_str().unwrap()),
            ("DBTL_EVENT_NAME", "pull_request"),
            ("DBTL_PR_BASE_SHA", &base),
            ("GITHUB_OUTPUT", tmp2.path().join("o.txt").to_str().unwrap()),
        ],
    );
    assert!(out2.status.success(), "stderr: {}", stderr(&out2));
    assert!(
        stub_log(&tmp2).contains(&format!("fetch --no-tags --depth=1 origin {base}")),
        "{}",
        stub_log(&tmp2)
    );
}

/// A base already in the checkout is not refetched.
#[test]
fn run_skips_the_fetch_when_the_base_commit_is_present() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let base = "b".repeat(40);
    std::fs::write(tmp.path().join("git_known_shas"), format!("{base}\n")).unwrap();
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("DBTL_BASE_REF", &base),
            ("GITHUB_OUTPUT", tmp.path().join("o.txt").to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let log = stub_log(&tmp);
    assert!(!log.contains("fetch"), "{log}");
    assert!(log.contains(&format!("--diff git:{base}")), "{log}");
}

/// A base-ref that looks like a flag is rejected before any dbtl or git
/// call — the input lands in a command line.
#[test]
fn run_rejects_a_flag_shaped_base_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let out = run_script(
        "run.sh",
        &tmp,
        &[
            ("DBTL_PROJECT_DIR", proj.to_str().unwrap()),
            ("DBTL_BASE_REF", "--checkpoint=x"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("not a valid git revision"),
        "{}",
        stderr(&out)
    );
    assert_eq!(stub_log(&tmp), "", "no stub should have run");
}

// ---- comment.sh --------------------------------------------------------------

/// No matching comment → POST a new one carrying the report body — after
/// listing the existing comments for the marker.
#[test]
fn comment_posts_a_new_comment_when_none_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.md");
    std::fs::write(&report, "# dbtl check\n\nall good\n").unwrap();
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("Posted the dbtl report comment"),
        "{}",
        stdout(&out)
    );
    let log = stub_log(&tmp);
    assert!(
        log.contains("gh api --paginate repos/o/r/issues/7/comments"),
        "{log}"
    );
    assert!(log.contains("-X POST repos/o/r/issues/7/comments"), "{log}");
    assert_eq!(
        std::fs::read(tmp.path().join("last_body.md")).unwrap(),
        b"# dbtl check\n\nall good\n"
    );
}

/// An existing marker comment is updated in place, not duplicated.
#[test]
fn comment_updates_the_existing_marker_comment() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.md");
    std::fs::write(&report, "# dbtl check\n").unwrap();
    stub_responses(&tmp, "gh_existing_id=4242\n");
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("Updated the dbtl report comment 4242"),
        "{}",
        stdout(&out)
    );
    let log = stub_log(&tmp);
    assert!(
        log.contains("-X PATCH repos/o/r/issues/comments/4242"),
        "{log}"
    );
    assert!(!log.contains("-X POST"), "{log}");
}

/// update-only mode never creates: with no matching comment it reports
/// the skip and stops after the GET.
#[test]
fn comment_update_only_without_a_match_posts_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.md");
    std::fs::write(&report, "# dbtl check\n").unwrap();
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("DBTL_COMMENT_UPDATE_ONLY", "true"),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("update-only mode, nothing posted"),
        "{}",
        stdout(&out)
    );
    let log = stub_log(&tmp);
    assert!(
        !log.contains("-X PATCH") && !log.contains("-X POST"),
        "{log}"
    );
}

/// A failed update falls back to a fresh comment (a deleted comment is
/// the usual cause) — unless update-only is set.
#[test]
fn comment_patch_failure_falls_back_to_a_new_comment() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.md");
    std::fs::write(&report, "# dbtl check\n").unwrap();
    stub_responses(&tmp, "gh_existing_id=5\ngh_patch_fail=1\n");
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("could not update comment 5"),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("Posted the dbtl report comment"),
        "{}",
        stdout(&out)
    );
    let log = stub_log(&tmp);
    let patch = log.find("-X PATCH").expect("PATCH attempted");
    let post = log.find("-X POST").expect("POST fallback");
    assert!(patch < post, "PATCH must precede the POST fallback: {log}");
}

/// A missing report warns and exits 0 — the verdict is unaffected and
/// nothing reaches the API.
#[test]
fn comment_without_a_report_warns_and_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("report.md");
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", missing.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("no markdown report was produced"),
        "{}",
        stderr(&out)
    );
    assert_eq!(stub_log(&tmp), "", "gh must not run");
}

/// Dry-run prints the calls it would make and never touches gh.
#[test]
fn comment_dry_run_prints_the_plan_without_calling_gh() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.md");
    std::fs::write(&report, "# dbtl check\n").unwrap();
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("DBTL_DRY_RUN", "true"),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("[dry-run] would list comments"), "{so}");
    assert!(
        so.contains("POST /repos/o/r/issues/7/comments with -F body=@"),
        "{so}"
    );
    assert_eq!(stub_log(&tmp), "", "gh must not run in dry-run");
}

/// GitHub caps comment bodies at 65536 chars: an oversized report is cut
/// at the last complete UTF-8 boundary (a multi-byte char straddling the
/// cut is dropped, not mangled) and tagged as truncated.
#[test]
fn comment_truncates_an_oversized_report_at_a_char_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let mut contents = vec![b'A'; 64998];
    contents.extend_from_slice("€".as_bytes()); // E2 82 AC straddles byte 65000
    contents.extend_from_slice(&[b'B'; 200]);
    let report = tmp.path().join("report.md");
    std::fs::write(&report, &contents).unwrap();
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let note =
        "\n\n_Report truncated — see the job summary for the full dbtl check report._\n".as_bytes();
    let body = std::fs::read(tmp.path().join("last_body.md")).unwrap();
    assert_eq!(body.len(), 64998 + note.len(), "cut + note");
    assert_eq!(&body[..64998], vec![b'A'; 64998].as_slice());
    assert_eq!(&body[64998..], note);
}

/// A read-only token (a forked pull request) turns the POST failure into
/// a warning — the check's verdict never depends on write-back.
#[test]
fn comment_api_failure_degrades_to_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.md");
    std::fs::write(&report, "# dbtl check\n").unwrap();
    stub_responses(&tmp, "gh_post_fail=1\n");
    let out = run_script(
        "comment.sh",
        &tmp,
        &[
            ("DBTL_REPORT", report.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
            ("DBTL_PR_NUMBER", "7"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("could not post the dbtl report comment"),
        "{}",
        stderr(&out)
    );
}

// ---- sarif_dry_run.sh --------------------------------------------------------

/// No SARIF file → a warning and a clean exit (the step is informational
/// and must never flip the verdict).
#[test]
fn sarif_dry_run_warns_when_no_file_was_produced() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("none.sarif");
    let out = run_script(
        "sarif_dry_run.sh",
        &tmp,
        &[("DBTL_SARIF_FILE", missing.to_str().unwrap())],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("no SARIF file was produced"),
        "{}",
        stderr(&out)
    );
}

/// With a file present the upload plan names it and the code-scanning
/// endpoint.
#[test]
fn sarif_dry_run_prints_the_upload_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let sarif = tmp.path().join("dbtl.sarif");
    std::fs::write(&sarif, "{\"version\":\"2.1.0\",\"runs\":[]}\n").unwrap();
    let out = run_script(
        "sarif_dry_run.sh",
        &tmp,
        &[
            ("DBTL_SARIF_FILE", sarif.to_str().unwrap()),
            ("GITHUB_REPOSITORY", "o/r"),
        ],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.contains(&format!(
            "[dry-run] would upload SARIF file '{sarif}'",
            sarif = sarif.display()
        )),
        "{so}"
    );
    assert!(so.contains("POST /repos/o/r/code-scanning/sarifs"), "{so}");
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
