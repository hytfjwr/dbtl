//! Shared fixture plumbing for the integration tests.
//!
//! Each `tests/*.rs` target is its own crate, so helpers that every suite
//! needs — locating the committed fixtures, loading the jaffle_finance
//! manifest, spawning the built `dbtl` binary — live here instead of being
//! copy-pasted per file. Not every target uses every helper, so dead code
//! is allowed crate-wide.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

pub mod consistency;

/// The committed jaffle_finance manifest (schema v12) most suites load.
pub const FIXTURE_MANIFEST: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

/// The small synthetic manifest the CLL/consistency suites pin.
pub const SAMPLE_MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/sample_manifest.json"
);

/// The `dbt_source`-mode project dir matching `SAMPLE_MANIFEST`.
pub const SAMPLE_PROJECT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_project");

/// The real-dbt manifest generated from `LOGISTICS_PROJECT`
/// (`cargo xtask gen-fixture-manifest --project tests/fixtures/logistics_project`).
pub const LOGISTICS_MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/logistics_manifest.json"
);

/// The `dbt_source`-mode project dir matching `LOGISTICS_MANIFEST` — a
/// seed-fed logistics project with var-controlled nodes and a local package.
pub const LOGISTICS_PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/logistics_project"
);

/// Absolute path to a file under `tests/fixtures/`.
pub fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// The repository root — suites covering repo files outside
/// `tests/fixtures/` (docs, CI scripts, the changelog) anchor paths here.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The fixture manifest as a `PathBuf` (CLI-spawning suites want paths).
pub fn manifest_path() -> PathBuf {
    fixture_path("manifest.json")
}

/// `SAMPLE_MANIFEST` as a `PathBuf`.
pub fn sample_manifest_path() -> PathBuf {
    fixture_path("sample_manifest.json")
}

/// Copy the fixture manifest into `dir` as `manifest.json` and return its
/// path — `.dbtl.yml` auto-detects beside the data source, so
/// config-driven suites need an owned directory.
pub fn manifest_in(dir: &std::path::Path) -> PathBuf {
    let dst = dir.join("manifest.json");
    std::fs::copy(manifest_path(), &dst).unwrap();
    dst
}

/// Load the fixture manifest into a `Dag`.
pub fn fixture_dag() -> dbtl::Dag {
    dbtl::load_dag(FIXTURE_MANIFEST).expect("fixture manifest must load")
}

/// A `Command` bound to the `dbtl` binary cargo just built.
pub fn dbtl_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dbtl"))
}

/// `dbtl <args>` as a configurable `Command`: stdin nulled — a bare
/// (subcommand-less) invocation's TTY probe then fails instead of parking
/// on the test runner's terminal — and the sealed git env applied: a
/// `--diff git:`/`state:` run's spawned `git` must see the same isolation
/// the test commits ran under. Callers needing `.env`/`.current_dir`/
/// piped-stdio control chain this; everyone else uses [`run`].
pub fn dbtl(args: &[&str]) -> Command {
    let mut cmd = dbtl_cmd();
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    git_isolated_env(&mut cmd);
    cmd
}

/// Run `dbtl <args>` to completion and collect its output.
pub fn run(args: &[&str]) -> std::process::Output {
    dbtl(args).output().expect("run dbtl")
}

/// [`run`] with extra env vars scoped to the child — the only safe way to
/// point e.g. `XDG_CONFIG_HOME` at a fixture dir.
pub fn run_env(args: &[&str], envs: &[(&str, &std::path::Path)]) -> std::process::Output {
    let mut cmd = dbtl(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("run dbtl")
}

/// stdout of a finished `Output` as an owned String.
pub fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stderr of a finished `Output` as an owned String.
pub fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// stdout as owned lines.
pub fn lines(out: &std::process::Output) -> Vec<String> {
    stdout(out).lines().map(str::to_string).collect()
}

// ---- temporary git repositories -------------------------------------------
//
// The git-baseline tests materialize real revisions, so they need real
// repositories: `git init`, commit a fixture, mutate, commit again. Every
// `git` invocation below goes through [`git_in`], which pins identity and
// isolation env so a developer's `~/.gitconfig` (signing, hooks, autocrlf,
// even an absent name/email) can never change what the test commits — or
// block it on a machine with no configured identity.

/// A scratch dir that also owns a git-repo-independent scratch `TMPDIR`
/// (the baseline's temp tree must land somewhere observable — see the
/// empty-after check in the git tests).
pub struct GitRepo {
    /// The repository root (and `git -C` target).
    pub root: PathBuf,
    _tmp: tempfile::TempDir,
}

impl GitRepo {
    /// `git init` an empty repository inside a fresh tempdir.
    pub fn init() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root).args(["init", "--quiet"]).run();
        Self { root, _tmp: tmp }
    }

    /// Copy `src` (a dir's CONTENTS, or a single file) into `rel` under the
    /// repo root — the way a fixture lands at a subdirectory path.
    pub fn copy_in(&self, src: &std::path::Path, rel: &str) {
        let dst = self.root.join(rel);
        if src.is_dir() {
            copy_tree(src, &dst);
        } else {
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(src, &dst).unwrap();
        }
    }

    /// Write `contents` to `rel` under the repo root (parents created).
    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }

    /// `git add -A <paths>` + `git commit`. Returns the new commit sha.
    pub fn commit(&self, paths: &[&str], message: &str) -> String {
        let mut add = vec!["add", "-A", "--"];
        add.extend(paths.iter().copied());
        git(&self.root).args(add).run();
        git(&self.root)
            .args(["commit", "--quiet", "-m", message])
            .run();
        git(&self.root)
            .args(["rev-parse", "HEAD"])
            .out()
            .trim()
            .to_string()
    }

    /// `git -C <root> <args>` with the isolated environment.
    pub fn git(&self) -> GitCmd {
        git(&self.root)
    }
}

/// A configured `git` invocation — identity + isolation env applied, so the
/// user's/system's git configuration can't leak in.
pub struct GitCmd {
    cmd: Command,
}

impl GitCmd {
    /// Chain args.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.cmd.args(args);
        self
    }

    /// Run to completion; the command must succeed.
    pub fn run(&mut self) {
        let out = self.cmd.output().expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            self.cmd,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Run to completion and return stdout as a String.
    pub fn out(&mut self) -> String {
        let out = self.cmd.output().expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            self.cmd,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `git -C <dir>` with the test-isolated environment: a fixed
/// identity (the developer machine needn't have one), no signing, no
/// autocrlf rewriting, and global/system config sealed off entirely.
pub fn git(dir: &std::path::Path) -> GitCmd {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd.env("GIT_AUTHOR_NAME", "dbtl tests")
        .env("GIT_AUTHOR_EMAIL", "dbtl-tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "dbtl tests")
        .env("GIT_COMMITTER_EMAIL", "dbtl-tests@example.invalid")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_1", "false")
        // Seal off ~/.gitconfig and /etc/gitconfig — their presence must
        // be a no-op on the committed result.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    GitCmd { cmd }
}

/// The same isolation env applied to a `dbtl` invocation — the binary
/// spawns `git` itself for `--diff git:<rev>`, and that child must see the
/// sealed config too.
pub fn git_isolated_env(cmd: &mut Command) {
    cmd.env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_1", "false")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

/// Write `(relative path, contents)` pairs under `dir` — a scratch
/// source-mode project's files (parents created) — and return `dir`.
pub fn write_project(dir: &std::path::Path, files: &[(&str, &str)]) -> PathBuf {
    for (rel, contents) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }
    dir.to_path_buf()
}

/// A recursive directory copy — `std::fs` has none, and the diff/git tests
/// need an editable twin of a fixture project.
pub fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// Set a file's mtime (Rust ≥1.75 `File::set_modified`) — staleness tests
/// stamp explicitly: a CI checkout doesn't preserve mtimes, and `fs::copy`
/// stamps the copy time.
pub fn stamp(path: &std::path::Path, t: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}
