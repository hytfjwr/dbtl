//! The `--watch` poller: a polled (not notified) change detector over the data
//! source. Manifest mode stamps the single file's mtime; source mode stamps the
//! MAX `(mtime, file count)` over the files `dbt parse` reads into the
//! manifest — the count catches a deletion, which can only lower the max mtime.
//! Checks are throttled to one scan per 2s of idle ticks, so the 250ms event
//! poll never busy-walks a big project tree.
//!
//! The same scan backs [`project_files_newer_than`], the auto-detected
//! manifest's staleness probe — one shared input set owns the "what counts
//! as a dbt input" rule so the two scans can't drift.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

pub struct Watch {
    pub root: PathBuf,
    pub recursive: bool,
    stamp: Option<(std::time::SystemTime, usize)>,
    last_check: std::time::Instant,
}

impl Watch {
    pub fn new(root: PathBuf, recursive: bool) -> Self {
        let stamp = fs_stamp(&root, recursive);
        Watch {
            root,
            recursive,
            stamp,
            last_check: std::time::Instant::now(),
        }
    }

    /// Whether the source changed since the last accepted stamp (throttled).
    /// A vanished source (`None` stamp — e.g. mid-`dbt compile` manifest swap)
    /// is NOT a change; the next successful stamp triggers the reload.
    pub fn poll(&mut self) -> bool {
        if self.last_check.elapsed() < Duration::from_secs(2) {
            return false;
        }
        self.last_check = std::time::Instant::now();
        let cur = fs_stamp(&self.root, self.recursive);
        if cur.is_some() && cur != self.stamp {
            self.stamp = cur;
            return true;
        }
        false
    }
}

/// The on-disk stamp `(max mtime, file count)` for a watch root: the file's own
/// mtime (count 1) when `recursive` is false, else over the inputs `dbt parse`
/// reads — see [`scan`]. `None` when the root is unreadable.
pub fn fs_stamp(root: &std::path::Path, recursive: bool) -> Option<(std::time::SystemTime, usize)> {
    if !recursive {
        return std::fs::metadata(root)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| (t, 1));
    }
    let mut best = None;
    let mut count = 0;
    scan(root, true, None, &mut best, &mut count);
    best.map(|(_, t)| (t, count))
}

/// Project input files strictly newer than `since`: `(count, the newest
/// file's path)` — the auto-detected manifest's staleness evidence.
/// Shares [`fs_stamp`]'s scan but skips `target/` WHOLESALE: `compiled/`
/// and `run/` artifacts regenerate alongside the manifest, so one being
/// newer can't prove it stale — only real sources can. A file whose mtime
/// can't be read doesn't count (undeterminable is not stale, same rule
/// `compiled::is_stale` follows).
pub fn project_files_newer_than(
    root: &std::path::Path,
    since: std::time::SystemTime,
) -> (usize, Option<PathBuf>) {
    let mut best = None;
    let mut count = 0;
    scan(root, false, Some(since), &mut best, &mut count);
    (count, best.map(|(p, _)| p))
}

/// The one project scan [`fs_stamp`] and [`project_files_newer_than`] ride:
/// count the files `dbt parse` would read into the manifest — nothing a
/// run regenerates can trip a stale verdict or a reload.
///
/// With a parseable `dbt_project.yml`, that means exactly dbt's inputs:
/// every `.yml`/`.yaml` and `.dbtignore` directly at the project root
/// (`vars.yml`, `packages.yml`, `selectors.yml`, a root `profiles.yml`, …)
/// plus the data files under the resource paths [`dbtl::project_resource_dirs`]
/// resolves (the `model-paths`/`seed-paths`/`snapshot-paths`/`macro-paths`/
/// `analysis-paths`/`test-paths`/`docs-paths` keys, dbt's defaults, and the
/// `source-paths`/`data-paths` aliases). Trees that only ever carry
/// generated or vendored content — `logs/` (dbt's default `log-path`),
/// `dbt_packages/`/`dbt_internal_packages/`, `target/`, `venv/`,
/// `node_modules/` — never sit in those lists, so they can't count;
/// `target/compiled/` IS added back for `fs_stamp` alone (`include_compiled`
/// — a recompile's expanded SQL should refresh the `s` preview).
///
/// An unreadable `dbt_project.yml` falls back to the broad walk — the
/// whole tree minus the generated directory names — so an undecidable
/// project shape degrades to the same scan it always had (the
/// "unprovable is not stale" rule: never count generated trees even when
/// the inputs can't be listed).
fn scan(
    root: &std::path::Path,
    include_compiled: bool,
    newer: Option<std::time::SystemTime>,
    best: &mut Option<(PathBuf, std::time::SystemTime)>,
    count: &mut usize,
) {
    let mut seen = HashSet::new();
    match dbtl::project_resource_dirs(root) {
        Some(dirs) => {
            scan_root_files(root, newer, best, count, &mut seen);
            for dir in dirs {
                walk_scoped(&dir, newer, best, count, &mut seen);
            }
            if include_compiled {
                if let Some(dir) = dbtl::default_compiled_dir(None, Some(root)) {
                    walk_scoped(&dir, newer, best, count, &mut seen);
                }
            }
        }
        None => walk_fallback(root, include_compiled, newer, best, count, &mut seen),
    }
}

/// The project root's own manifest inputs — non-recursive: every
/// `.yml`/`.yaml` sibling of `dbt_project.yml` (`vars.yml`,
/// `packages.yml`, `package-lock.yml`, `selectors.yml`, `dependencies.yml`,
/// a root `profiles.yml`, …) plus `.dbtignore`, a file with no extension,
/// so it matches by name.
fn scan_root_files(
    root: &std::path::Path,
    newer: Option<std::time::SystemTime>,
    best: &mut Option<(PathBuf, std::time::SystemTime)>,
    count: &mut usize,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path();
        let is_input = entry.file_name().to_string_lossy() == ".dbtignore"
            || matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            );
        if !is_input {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        count_file(path, mtime, newer, best, count, seen);
    }
}

/// Recursive pass over one resolved resource directory, counting the data
/// files `dbt parse` reads: `.sql`/`.py` models/snapshots/macros,
/// `.yml`/`.yaml` properties and configs, `.csv` seeds, and `.md`
/// `{% docs %}` bodies (descriptions render through them — a docs edit
/// is a manifest input). A resource path of `"."` puts the whole project
/// in scope, so generated/vendored names and `target/` are skipped here
/// too — the compiled subtree is walked as its own root by the caller.
fn walk_scoped(
    dir: &std::path::Path,
    newer: Option<std::time::SystemTime>,
    best: &mut Option<(PathBuf, std::time::SystemTime)>,
    count: &mut usize,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // file_type() does not follow symlinks, so a symlinked directory
        // (e.g. a link loop back into the tree) is never walked into.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if name != "target" && !generated_dir(&name) {
                walk_scoped(&path, newer, best, count, seen);
            }
            continue;
        }
        if !resource_file(&path) {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        count_file(path, mtime, newer, best, count, seen);
    }
}

/// The fallback walk for an unreadable `dbt_project.yml`: the whole tree
/// minus the generated directory names, with the pre-resolution data
/// extensions (`.sql`/`.py`/`.yml`/`.yaml`/`.csv` — no `.md`, which is only
/// an input under a resource path we couldn't resolve). `include_compiled`
/// controls `target/`'s reach like before: `fs_stamp` counts
/// `target/compiled/` (a recompile should reload the view); the staleness
/// probe passes `false` — a generated artifact can never make its own
/// generator stale.
fn walk_fallback(
    dir: &std::path::Path,
    include_compiled: bool,
    newer: Option<std::time::SystemTime>,
    best: &mut Option<(PathBuf, std::time::SystemTime)>,
    count: &mut usize,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if name == "target" {
                if include_compiled {
                    // `target/` itself is still skipped wholesale — only
                    // its `compiled/` subtree carries reload-worthy user
                    // data (the expanded SQL + stale state the SQL modal
                    // shows).
                    walk_fallback(&path.join("compiled"), true, newer, best, count, seen);
                }
            } else if !generated_dir(&name) {
                walk_fallback(&path, include_compiled, newer, best, count, seen);
            }
            continue;
        }
        let data_file = matches!(
            path.extension().and_then(|e| e.to_str()),
            // `.py` too: Python models' sources + compiled artifacts.
            Some("sql" | "py" | "yml" | "yaml" | "csv")
        );
        if !data_file {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        count_file(path, mtime, newer, best, count, seen);
    }
}

/// Directory names that are generated or vendored rather than manifest
/// inputs — skipped at any depth inside either walk. `target/` is not
/// here: each walk decides it separately (`fs_stamp` counts
/// `target/compiled/` as a root of its own so a recompile refreshes the
/// compiled-SQL view; the staleness probe skips the whole tree).
fn generated_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "logs" | "dbt_packages" | "dbt_internal_packages")
}

/// The data-file extensions dbt reads under its resource paths.
fn resource_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("sql" | "py" | "yml" | "yaml" | "csv" | "md")
    )
}

/// The shared counting step: `seen` dedupes files reachable through two
/// resource roots (`model-paths: [".", "models"]` reaches `models/x.sql`
/// twice) so each counts once; `newer` restricts counting to files with
/// mtime strictly past it (the staleness probe's threshold — an unreadable
/// or not-strictly-newer mtime means "can't establish stale" stays
/// silent); `best` accumulates the newest counted `(path, mtime)` and
/// `count` the counted files.
fn count_file(
    path: PathBuf,
    mtime: Option<std::time::SystemTime>,
    newer: Option<std::time::SystemTime>,
    best: &mut Option<(PathBuf, std::time::SystemTime)>,
    count: &mut usize,
    seen: &mut HashSet<PathBuf>,
) {
    if !seen.insert(path.clone()) {
        return;
    }
    if let Some(s) = newer {
        match mtime {
            Some(m) if m > s => {}
            _ => return,
        }
    }
    *count += 1;
    if let Some(m) = mtime {
        if best.as_ref().is_none_or(|(_, b)| m > *b) {
            *best = Some((path, m));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// Write (rel, contents) pairs under `dir`, parents created.
    fn write(dir: &std::path::Path, files: &[(&str, &str)]) {
        for (rel, body) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
    }

    /// `fs_stamp`'s counted-file tally — the assertions below care about
    /// WHICH trees land in the count, stated as a number.
    fn stamp_count(root: &std::path::Path) -> usize {
        fs_stamp(root, true).map(|(_, c)| c).unwrap_or(0)
    }

    #[test]
    fn fs_stamp_tracks_data_files_and_watch_detects_change() {
        let dir = std::env::temp_dir().join(format!("dbtl_watch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("models/a.sql"), "select 1").unwrap();
        std::fs::write(dir.join("dbt_project.yml"), "name: t").unwrap();
        // Generated trees never count — EXCEPT target/compiled/*.sql (and
        // *.py — Python models' compiled artifacts), which a recompile
        // refreshes (the SQL modal shows it, and a stale flag depends on
        // its mtime). logs/, dbt_packages/, and vendored dirs are outside
        // every resource path now, so they drop out of the count entirely.
        std::fs::write(dir.join("target/manifest.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("target/run/t")).unwrap();
        std::fs::write(dir.join("target/run/t/a.sql"), "select 1").unwrap();
        std::fs::create_dir_all(dir.join("target/compiled/t/models")).unwrap();
        std::fs::write(dir.join("target/compiled/t/models/a.sql"), "select 1").unwrap();
        std::fs::write(
            dir.join("target/compiled/t/models/m.py"),
            "def model(dbt, s): ...",
        )
        .unwrap();
        write(
            &dir,
            &[
                ("logs/query_log.sql", "select 1"),
                ("dbt_packages/pkg/m.sql", "select 1"),
                ("venv/lib/site.py", "x = 1"),
                ("README.md", "x"),
            ],
        );

        let (_, count) = fs_stamp(&dir, true).expect("stamp exists");
        assert_eq!(
            count, 4,
            "a.sql + dbt_project.yml + the compiled .sql + .py (run/, manifest, logs, packages, venv skipped)"
        );

        let mut w = Watch::new(dir.clone(), true);
        assert!(!w.poll(), "throttled: no immediate re-scan");
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(!w.poll(), "unchanged tree is not a change");
        // A new model file changes the count (even with a coarse mtime clock).
        std::fs::write(dir.join("models/b.sql"), "select 2").unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "new data file detected");
        // A fresh compiled file changes the count too — a `dbt compile`
        // refreshes the view like a source edit.
        std::fs::remove_file(dir.join("models/b.sql")).unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "deletion detected via the file count");
        std::fs::write(dir.join("target/compiled/t/models/b.sql"), "select 2").unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "new target/compiled file detected");
        // A log write is NOT a change — dbt's own run artifacts are not
        // manifest inputs — and it leaves the stamp byte-identical. A
        // root `vars.yml` (a real parse input) moves both mtime and count.
        std::fs::write(dir.join("logs/query_log2.sql"), "select 3").unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(!w.poll(), "log writes must not reload");
        let before = fs_stamp(&dir, true);
        write(&dir, &[("logs/query_log3.sql", "select 4")]);
        assert_eq!(fs_stamp(&dir, true), before, "logs/ leaves the stamp");
        write(&dir, &[("vars.yml", "x: 1")]);
        assert_ne!(fs_stamp(&dir, true), before, "a root vars.yml moves it");

        // Single-file (manifest) mode stamps that file alone.
        let manifest = dir.join("target/manifest.json");
        assert_eq!(fs_stamp(&manifest, false).map(|(_, c)| c), Some(1));
        assert_eq!(fs_stamp(&dir.join("missing"), false), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The staleness probe: counts only files strictly newer than the
    /// threshold, reports the newest's path, and — unlike `fs_stamp` —
    /// ignores ALL of `target/` (artifacts regenerate with the manifest,
    /// they can't prove it stale). This project carries no
    /// `dbt_project.yml`, so it exercises the broad fallback walk.
    #[test]
    fn project_files_newer_than_counts_only_what_the_manifest_predates() {
        let dir = std::env::temp_dir().join(format!("dbtl_newer_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("target/compiled/t/models")).unwrap();
        let set = |rel: &str, body: &str, t: SystemTime| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        let t0 = SystemTime::now() - Duration::from_secs(3600);
        let t1 = t0 + Duration::from_secs(60);
        let t2 = t0 + Duration::from_secs(120);
        set("models/old.sql", "select 1", t0);
        set("models/new.sql", "select 2", t1);
        set("models/newest.sql", "select 3", t2);
        // A newer compiled artifact is NOT evidence — it regenerates with
        // the manifest.
        set("target/compiled/t/models/a.sql", "select 4", t2);
        set("README.md", "x", t2); // non-data never counts
                                   // Even in the fallback walk, dbt's generated trees can't count.
        set("logs/query_log.sql", "select 5", t2);
        set("dbt_packages/pkg/m.sql", "select 6", t2);

        // Threshold at t0: old.sql isn't strictly newer, the two newer
        // sources are, the compiled artifact is ignored entirely.
        let (count, newest) = project_files_newer_than(&dir, t0);
        assert_eq!(count, 2);
        assert_eq!(
            newest.as_deref(),
            Some(dir.join("models/newest.sql").as_path())
        );

        // A threshold at/after the newest file: nothing proves stale.
        let (count, newest) = project_files_newer_than(&dir, t2);
        assert_eq!((count, newest.is_none()), (0, true));

        // Missing dirs don't panic — they just have nothing newer.
        let (count, newest) = project_files_newer_than(&dir.join("absent"), t0);
        assert_eq!((count, newest.is_none()), (0, true));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The resolved-inputs walk: with a readable `dbt_project.yml`, the
    /// probe counts only the resource paths' data files and the root
    /// config files — generated trees, vendored dirs, and artifacts are
    /// outside the input set entirely.
    #[test]
    fn probe_counts_only_dbt_inputs_when_the_project_file_resolves() {
        let dir = std::env::temp_dir().join(format!("dbtl_scoped_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t0 = SystemTime::now() - Duration::from_secs(3600);
        let t1 = t0 + Duration::from_secs(60);
        let set = |rel: &str, body: &str, t: SystemTime| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        write(&dir, &[("dbt_project.yml", "name: p\n")]);
        // dbt inputs, newer than the threshold — every one counts:
        // a model under model-paths, a docs .md, and the root config files.
        set("models/m.sql", "select 1", t1);
        set("models/guide.md", "{% docs g %}body{% enddocs %}", t1);
        set("vars.yml", "x: 1", t1);
        set("package-lock.yml", "packages: []", t1);
        set(".dbtignore", "build/", t1);
        // Everything else is newer too — and NONE of it counts.
        for rel in [
            "logs/query_log.sql",
            "dbt_packages/pkg/m.sql",
            "dbt_internal_packages/dbt/m.sql",
            "venv/lib/x.py",
            "node_modules/pkg/i.sql",
            "build/out.sql",
            "misc/stray.sql", // not under any resource path
            "models/nested/logs/x.sql",
            "README.md", // .md only counts under a resource path
            "scratch.txt",
        ] {
            set(rel, "select 1", t1);
        }
        // A target/compiled artifact is never stale-evidence either.
        set("target/compiled/p/models/m.sql", "select 1", t1);

        let (count, newest) = project_files_newer_than(&dir, t0);
        assert_eq!(
            count, 6,
            "dbt_project.yml + model + docs .md + 3 root config files"
        );
        assert_eq!(
            newest.as_deref(),
            Some(dir.join("dbt_project.yml").as_path()),
            "the just-written project file is strictly the newest input"
        );
        // A threshold past every input's mtime: nothing proves stale.
        let (count, _) = project_files_newer_than(&dir, t0 + Duration::from_secs(7200));
        assert_eq!(count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Resource paths other than the defaults are honored — a model under
    /// `model-paths: ["src/models"]` counts — while `model-paths: ["."]`
    /// still can't see `logs/`/`dbt_packages/`/`target/`/dot-dirs, which a
    /// project-root path would otherwise expose to the walk.
    #[test]
    fn custom_and_root_resource_paths_resolve_the_input_set() {
        let dir = std::env::temp_dir().join(format!("dbtl_paths_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            &[
                (
                    "dbt_project.yml",
                    "name: p\nmodel-paths: [\"src/models\"]\n",
                ),
                ("src/models/m.sql", "select 1"),
                ("models/m.sql", "select 1"), // outside the declared paths
                ("logs/query_log.sql", "select 1"),
            ],
        );
        // dbt_project.yml + src/models/m.sql — the undeclared models/ dir
        // and logs/ don't count.
        assert_eq!(stamp_count(&dir), 2);

        std::fs::write(
            dir.join("dbt_project.yml"),
            "name: p\nmodel-paths: [\".\"]\n",
        )
        .unwrap();
        // Now the whole project is one resource path — both model files
        // count (and the root .sql), but the generated names inside still
        // don't.
        assert_eq!(stamp_count(&dir), 3, "project yml + 2 model files");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unparsable `dbt_project.yml` degrades to the broad walk — the
    /// whole tree minus generated dirs — rather than guessing a resource
    /// set (a stray `.sql` outside `models/` still counts; `logs/` never
    /// does).
    #[test]
    fn unreadable_project_file_falls_back_to_the_broad_walk() {
        let dir = std::env::temp_dir().join(format!("dbtl_broken_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir,
            &[
                ("dbt_project.yml", "name: [unclosed\n"),
                ("models/m.sql", "select 1"),
                ("misc/stray.sql", "select 1"),
                ("logs/query_log.sql", "select 1"),
                ("dbt_packages/pkg/m.sql", "select 1"),
                ("venv/lib/x.py", "x = 1"),
                ("notes.md", "hi"), // .md counts only under a resource path
            ],
        );
        assert_eq!(
            stamp_count(&dir),
            4,
            "models/m.sql + misc/stray.sql + venv/lib/x.py + dbt_project.yml"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
