//! The `--diff git:<rev>` baseline: materialize the project's tracked files
//! at one git revision into a temp dir via read-only plumbing — `git
//! ls-tree` + `git cat-file --batch`, so the working tree, the index, the
//! refs, and the stash are all untouched — then load it through the same
//! source-mode loader a project-dir baseline takes. The current side stays
//! the live working tree (dirty files included); the baseline is exactly
//! the committed revision.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// File extensions the baseline materializes — the set the source-mode
/// loader reads (`.sql`/`.py` models, `.yml`/`.yaml` schema + `dbt_project.yml`,
/// `.csv` seeds, `.md` docs blocks). Everything else (`.sql.disabled`,
/// compiled artifacts, editor droppings) isn't project source.
const PROJECT_FILE_EXTS: &[&str] = &[".sql", ".py", ".yml", ".yaml", ".csv", ".md"];

/// Directories a repo can track but the loader must never see — build
/// output and vendored packages aren't the project's source.
const IGNORED_ROOT_DIRS: &[&str] = &["target", "dbt_packages", "dbt_internal_packages"];

/// Load the `--diff git:<rev>` baseline: extract the project at `<project_dir>`
/// (the repository-relative path `git rev-parse --show-prefix` reports for
/// it) at revision `<rev>` into a temp dir and parse it from source. The
/// returned label is the revision string verbatim (`git:<rev>`). Every failure
/// is an input error except an obviously invalid `<rev>`, which is a usage
/// error — a rev must never read as a git flag.
pub(super) fn load(
    project_dir: &Path,
    rev: &str,
    scope: dbtl::PackageScope,
) -> Result<(dbtl::Dag, String), CliError> {
    // `-<anything>` would reach git as a flag, not a rev (`--diff git:-p`
    // failing inside git's own flag parser is worse than a usage error);
    // an empty rev is just a `git:` typo.
    if rev.is_empty() || rev.starts_with('-') {
        return Err(CliError::usage(anyhow::anyhow!(
            "--diff: invalid git revision '{rev}'"
        )));
    }
    if !project_dir.is_dir() {
        return Err(CliError::input(anyhow::anyhow!(
            "--diff: project directory '{}' does not exist",
            project_dir.display()
        )));
    }
    let prefix = git_prefix(project_dir)?;
    let commit = git_rev_parse(project_dir, rev)?;
    let entries = git_ls_tree(project_dir, &commit, &prefix)?;

    let tmp = TempDir::create().map_err(|e| {
        CliError::input(anyhow::anyhow!(
            "--diff: cannot create a temp directory under {}: {e}",
            std::env::temp_dir().display()
        ))
    })?;
    let mut warnings = Vec::new();
    let mut saw_project_yml = false;
    let mut batch = CatBatch::spawn(project_dir)?;
    for entry in &entries {
        let Some(rel) = safe_relpath(&entry.path) else {
            warnings.push(format!(
                "skipping unsafe path '{path}' in {rev}",
                path = entry.path
            ));
            continue;
        };
        let bytes = batch.blob(&entry.oid, &entry.path)?;
        let dest = tmp.path().join(rel);
        let written = (|| -> std::io::Result<()> {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, &bytes)
        })();
        match written {
            Ok(()) => saw_project_yml |= rel == Path::new("dbt_project.yml"),
            // A name the filesystem can't host (Windows-reserved
            // characters, an over-long component) is skipped, not fatal —
            // the baseline is best-effort per file, and the loader's own
            // errors still apply to what lands.
            Err(e) => warnings.push(format!("skipping '{}': {e}", entry.path)),
        }
    }
    for w in &warnings {
        eprintln!("warning (--diff baseline): {w}");
    }
    if !saw_project_yml {
        return Err(CliError::input(anyhow::anyhow!(
            "--diff: revision '{rev}' has no dbt project at '{}' (no dbt_project.yml — \
             the project must live inside the repository)",
            if prefix.is_empty() {
                "."
            } else {
                prefix.as_str()
            }
        )));
    }
    let (dag, warnings) = load_dag_from_source_with_warnings(tmp.path())
        .with_context(|| format!("--diff: failed to parse revision '{rev}' as a dbt project"))
        .input()?;
    for w in &warnings {
        eprintln!("warning (--diff baseline): {w}");
    }
    Ok((
        apply_package_scope(dag, scope, true).0,
        format!("git:{rev}"),
    ))
}

/// `git rev-parse --show-prefix` in `dir` — the directory's path relative
/// to the repository root (empty at the root, `sub/proj/` nested). Run via
/// `-C` so the answer never depends on the caller's cwd. A non-repo (or a
/// missing `git`) is an input error.
fn git_prefix(dir: &Path) -> Result<String, CliError> {
    let out = git(dir, &["rev-parse", "--show-prefix"])?;
    if !out.status.success() {
        return Err(CliError::input(anyhow::anyhow!(
            "--diff: '{}' is not inside a git repository ({})",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Resolve `<rev>` to a commit sha (`rev-parse --verify --quiet` — quiet so
/// the failure carries OUR message, not git's stderr noise). Anything not
/// naming a commit — an unknown ref, a tree/blob expression, a shallow
/// clone missing the base — is an input error with the shallow-fetch hint
/// CI users need.
fn git_rev_parse(dir: &Path, rev: &str) -> Result<String, CliError> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(dir, &["rev-parse", "--verify", "--quiet", &spec])?;
    if !out.status.success() {
        return Err(CliError::input(anyhow::anyhow!(
            "--diff: cannot resolve git revision '{rev}' to a commit \
             (a shallow clone may lack it — try 'fetch-depth: 0' in \
             actions/checkout or 'git fetch <remote> <base> --depth=1')"
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// One materializable tree entry: the object id and its project-relative
/// path (prefix already stripped).
struct TreeEntry {
    oid: String,
    path: String,
}

/// `git ls-tree -r -z` the commit, filtered to the project's dir and to
/// files the source loader reads. NUL-delimited so any filename — spaces,
/// quotes, non-ASCII — parses cleanly; non-UTF-8 names degrade lossily
/// (they still materialize, just under replacement-char names).
fn git_ls_tree(dir: &Path, commit: &str, prefix: &str) -> Result<Vec<TreeEntry>, CliError> {
    // `--full-tree`: `dir` can sit BELOW the repo root, where a pathspec
    // would resolve relative to the cwd (`sub/proj/` meaning
    // `sub/proj/sub/proj/` — an empty listing). Anchored at the root the
    // `--show-prefix` pathspec names the project's subtree exactly.
    let mut args = vec!["ls-tree", "--full-tree", "-r", "-z", commit];
    // An explicit pathspec limits the listing to the project's subtree;
    // at the repo root there is none (an empty pathspec is not a valid
    // "match everything" everywhere).
    if !prefix.is_empty() {
        args.extend(["--", prefix]);
    }
    let out = git(dir, &args)?;
    if !out.status.success() {
        return Err(CliError::input(anyhow::anyhow!(
            "--diff: git ls-tree failed ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut entries = Vec::new();
    for record in out.stdout.split(|b| *b == 0) {
        if record.is_empty() {
            continue;
        }
        // `<mode> <type> <oid>\t<path>` — the metadata header and the name
        // split on the first TAB only (a filename can contain spaces, and
        // without -z quoting even a TAB).
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let (header, raw_name) = record.split_at(tab);
        let header = String::from_utf8_lossy(header);
        let mut parts = header.split(' ');
        let (mode, ty, oid) = (parts.next(), parts.next(), parts.next());
        // Blobs only — gitlinks (submodule "commit" entries) and trees are
        // not files; symlinks (`120000`) would materialize as text the
        // loader can't use, so they are excluded like the loader does.
        if ty != Some("blob") || mode == Some("120000") {
            continue;
        }
        let Some(oid) = oid else { continue };
        let name = String::from_utf8_lossy(&raw_name[1..]).into_owned();
        let Some(rel) = name.strip_prefix(prefix) else {
            continue;
        };
        // Root-level build/vendor dirs are not project source.
        let first = rel.split('/').next().unwrap_or(rel);
        if IGNORED_ROOT_DIRS.contains(&first) {
            continue;
        }
        if !PROJECT_FILE_EXTS.iter().any(|e| rel.ends_with(e)) {
            continue;
        }
        entries.push(TreeEntry {
            oid: oid.to_string(),
            path: rel.to_string(),
        });
    }
    Ok(entries)
}

/// Every path component must be `Normal` — a `..`, an absolute path, or a
/// Windows drive prefix in a hostile tree must never write outside the
/// temp dir (git won't emit those, but the check is the write's contract).
fn safe_relpath(path: &str) -> Option<&Path> {
    let p = Path::new(path);
    if path.is_empty() {
        return None;
    }
    p.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
        .then_some(p)
}

/// Run `git -C <dir> <args>` to completion. A spawn failure means `git`
/// isn't installed — an input error, not a crash (the binary still gates a
/// path baseline without it).
fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, CliError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            CliError::input(anyhow::anyhow!(
                "failed to run git: {e} (git is required for --diff git:<rev>)"
            ))
        })
}

/// `git cat-file --batch` — one persistent process serving object reads.
/// The request/reply ping-pong (write the oid, read the header, read the
/// exact declared size) can't deadlock on a full pipe the way a batch of
/// oids piped in at once can.
struct CatBatch {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CatBatch {
    fn spawn(dir: &Path) -> Result<Self, CliError> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                CliError::input(anyhow::anyhow!(
                    "failed to run git: {e} (git is required for --diff git:<rev>)"
                ))
            })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    /// Fetch one blob's contents — `<oid> blob <size>` header, then `size`
    /// bytes plus a trailing newline.
    fn blob(&mut self, oid: &str, path: &str) -> Result<Vec<u8>, CliError> {
        fn io_err(what: &str, e: std::io::Error) -> CliError {
            CliError::input(anyhow::anyhow!("--diff: git cat-file {what}: {e}"))
        }
        let io = |what: &'static str| move |e: std::io::Error| io_err(what, e);
        writeln!(self.stdin, "{oid}")
            .and_then(|()| self.stdin.flush())
            .map_err(io("write"))?;
        let mut header = String::new();
        self.stdout.read_line(&mut header).map_err(io("read"))?;
        let mut parts = header.trim_end().split(' ');
        let size = match (parts.next(), parts.next(), parts.next()) {
            (_, Some("blob"), Some(size)) => size.parse::<usize>().map_err(|_| {
                CliError::input(anyhow::anyhow!(
                    "--diff: unexpected git cat-file header '{header}'"
                ))
            })?,
            (Some(_), _, _) => {
                return Err(CliError::input(anyhow::anyhow!(
                    "--diff: git cat-file: '{path}' is not a blob at this revision \
                     ({})",
                    header.trim_end()
                )));
            }
            _ => {
                return Err(CliError::input(anyhow::anyhow!(
                    "--diff: git cat-file could not read '{path}' ({})",
                    header.trim_end()
                )));
            }
        };
        let mut bytes = vec![0u8; size];
        self.stdout.read_exact(&mut bytes).map_err(io("read"))?;
        let mut newline = [0u8; 1];
        self.stdout.read_exact(&mut newline).map_err(io("read"))?;
        Ok(bytes)
    }
}

impl Drop for CatBatch {
    fn drop(&mut self) {
        // `--batch` blocks on stdin forever — waiting with the pipe still
        // open deadlocks (fields drop only after `drop` returns). Kill
        // then reap: a read-only query process has no teardown to lose.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The scratch dir the baseline materializes into — removed on drop on BOTH
/// the success and failure paths (the source parse happens while it lives;
/// nothing references it after `load` returns). Hand-rolled on the standard
/// library: `tempfile` is a dev-dependency only, and the shipped binary gets
/// no new crates.
struct TempDir(PathBuf);

impl TempDir {
    fn create() -> std::io::Result<Self> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for attempt in 0..100u32 {
            let dir = base.join(format!(
                "dbtl-git-baseline-{}-{nanos}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a unique temp directory",
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
