//! The `--watch` poller: a polled (not notified) change detector over the data
//! source. Manifest mode stamps the single file's mtime; source mode stamps the
//! MAX `(mtime, file count)` over the project's `.sql`/`.py`/`.yml`/`.yaml`/`.csv`
//! files (the count catches a deletion, which can only lower the max mtime).
//! Checks are throttled to one scan per 2s of idle ticks, so the 250ms event
//! poll never busy-walks a big project tree.
//!
//! The same walk backs [`project_files_newer_than`], the auto-detected
//! manifest's staleness probe — one tree walk owns the "what counts as a
//! project source file" rule so the two scans can't drift.

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
/// mtime (count 1) when `recursive` is false, else over every data file
/// (`.sql`/`.py`/`.yml`/`.yaml`/`.csv`) under the tree — skipping dot-dirs and
/// `target/` EXCEPT `target/compiled/` (a recompile's expanded `.sql`/`.py`
/// SHOULD refresh the compiled-SQL view; every other artifact — `run/`, logs,
/// `manifest.json` — writes on every invocation and would self-trigger).
/// `None` when the root is unreadable.
pub fn fs_stamp(root: &std::path::Path, recursive: bool) -> Option<(std::time::SystemTime, usize)> {
    if !recursive {
        return std::fs::metadata(root)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| (t, 1));
    }
    let mut best = None;
    let mut count = 0;
    walk_tree(root, true, None, &mut best, &mut count);
    best.map(|(_, t)| (t, count))
}

/// Project data files strictly newer than `since`: `(count, the newest
/// file's path)` — the auto-detected manifest's staleness evidence.
/// Shares `fs_stamp`'s walk but skips `target/` WHOLESALE: `compiled/` and
/// `run/` artifacts regenerate alongside the manifest, so one being newer
/// can't prove it stale — only real sources can. A file whose mtime can't
/// be read doesn't count (undeterminable is not stale, same rule
/// `compiled::is_stale` follows).
pub fn project_files_newer_than(
    root: &std::path::Path,
    since: std::time::SystemTime,
) -> (usize, Option<PathBuf>) {
    let mut best = None;
    let mut count = 0;
    walk_tree(root, false, Some(since), &mut best, &mut count);
    (count, best.map(|(p, _)| p))
}

/// The one project-tree walk [`fs_stamp`] and [`project_files_newer_than`]
/// ride. `include_compiled` controls `target/`'s reach: `fs_stamp` counts
/// `target/compiled/` (a recompile should reload the view); the staleness
/// probe passes `false` — a generated artifact can never make its own
/// generator stale. `newer` restricts counting to files with mtime
/// strictly past it (the probe's threshold); `None` counts everything.
/// `best` accumulates the newest counted `(path, mtime)`; `count` the
/// counted files. Symlinked directories are never followed (a link loop
/// back into the tree can't recurse forever).
fn walk_tree(
    dir: &std::path::Path,
    include_compiled: bool,
    newer: Option<std::time::SystemTime>,
    best: &mut Option<(PathBuf, std::time::SystemTime)>,
    count: &mut usize,
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
            if name == "target" {
                if include_compiled {
                    // `target/` itself is still skipped wholesale — only its
                    // `compiled/` subtree carries reload-worthy user data (the
                    // expanded SQL + stale state the SQL modal shows).
                    walk_tree(&path.join("compiled"), true, newer, best, count);
                }
            } else if !name.starts_with('.') {
                walk_tree(&path, include_compiled, newer, best, count);
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
        // The threshold probe only counts what it can PROVE is newer — an
        // unreadable or not-strictly-newer mtime means "can't establish
        // stale" stays silent.
        if let Some(s) = newer {
            match mtime {
                Some(m) if m > s => {}
                _ => continue,
            }
        }
        *count += 1;
        if let Some(m) = mtime {
            if best.as_ref().is_none_or(|(_, b)| m > *b) {
                *best = Some((path, m));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_stamp_tracks_data_files_and_watch_detects_change() {
        let dir = std::env::temp_dir().join(format!("dbtl_watch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("models/a.sql"), "select 1").unwrap();
        std::fs::write(dir.join("dbt_project.yml"), "name: t").unwrap();
        // Artifacts under target/ and non-data files never count — EXCEPT
        // target/compiled/*.sql (and *.py — Python models' compiled artifacts),
        // which a recompile refreshes (the SQL modal shows it, and a stale
        // flag depends on its mtime).
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
        std::fs::write(dir.join("README.md"), "x").unwrap();

        let (_, count) = fs_stamp(&dir, true).expect("stamp exists");
        assert_eq!(
            count, 4,
            "a.sql + dbt_project.yml + the compiled .sql + .py (run/ + manifest skipped)"
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

        // Single-file (manifest) mode stamps that file alone.
        let manifest = dir.join("target/manifest.json");
        assert_eq!(fs_stamp(&manifest, false).map(|(_, c)| c), Some(1));
        assert_eq!(fs_stamp(&dir.join("missing"), false), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The staleness probe: counts only files strictly newer than the
    /// threshold, reports the newest's path, and — unlike `fs_stamp` —
    /// ignores ALL of `target/` (artifacts regenerate with the manifest,
    /// they can't prove it stale).
    #[test]
    fn project_files_newer_than_counts_only_what_the_manifest_predates() {
        let dir = std::env::temp_dir().join(format!("dbtl_newer_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("target/compiled/t/models")).unwrap();
        let set = |rel: &str, body: &str, t: std::time::SystemTime| {
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
        let t0 = std::time::SystemTime::now() - Duration::from_secs(3600);
        let t1 = t0 + Duration::from_secs(60);
        let t2 = t0 + Duration::from_secs(120);
        set("models/old.sql", "select 1", t0);
        set("models/new.sql", "select 2", t1);
        set("models/newest.sql", "select 3", t2);
        // A newer compiled artifact is NOT evidence — it regenerates with
        // the manifest.
        set("target/compiled/t/models/a.sql", "select 4", t2);
        set("README.md", "x", t2); // non-data never counts

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
}
