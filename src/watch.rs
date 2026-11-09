//! The `--watch` poller: a polled (not notified) change detector over the data
//! source. Manifest mode stamps the single file's mtime; source mode stamps the
//! MAX `(mtime, file count)` over the project's `.sql`/`.yml`/`.yaml`/`.csv`
//! files (the count catches a deletion, which can only lower the max mtime).
//! Checks are throttled to one scan per 2s of idle ticks, so the 250ms event
//! poll never busy-walks a big project tree.

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
/// (`.sql`/`.yml`/`.yaml`/`.csv`) under the tree — skipping dot-dirs and
/// `target/` (dbt writes artifacts there on every invocation, which would
/// self-trigger). `None` when the root is unreadable.
pub fn fs_stamp(root: &std::path::Path, recursive: bool) -> Option<(std::time::SystemTime, usize)> {
    fn walk(dir: &std::path::Path, best: &mut Option<std::time::SystemTime>, count: &mut usize) {
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
                if !name.starts_with('.') && name != "target" {
                    walk(&path, best, count);
                }
                continue;
            }
            let data_file = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("sql" | "yml" | "yaml" | "csv")
            );
            if !data_file {
                continue;
            }
            *count += 1;
            if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                if best.is_none_or(|b| mtime > b) {
                    *best = Some(mtime);
                }
            }
        }
    }
    if !recursive {
        return std::fs::metadata(root)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| (t, 1));
    }
    let mut best = None;
    let mut count = 0;
    walk(root, &mut best, &mut count);
    best.map(|t| (t, count))
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
        // Artifacts under target/ and non-data files never count.
        std::fs::write(dir.join("target/manifest.json"), "{}").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();

        let (_, count) = fs_stamp(&dir, true).expect("stamp exists");
        assert_eq!(count, 2, "a.sql + dbt_project.yml only");

        let mut w = Watch::new(dir.clone(), true);
        assert!(!w.poll(), "throttled: no immediate re-scan");
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(!w.poll(), "unchanged tree is not a change");
        // A new model file changes the count (even with a coarse mtime clock).
        std::fs::write(dir.join("models/b.sql"), "select 2").unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "new data file detected");
        // Deleting it lowers the count back — also a change.
        std::fs::remove_file(dir.join("models/b.sql")).unwrap();
        w.last_check = std::time::Instant::now() - Duration::from_secs(3);
        assert!(w.poll(), "deletion detected via the file count");

        // Single-file (manifest) mode stamps that file alone.
        let manifest = dir.join("target/manifest.json");
        assert_eq!(fs_stamp(&manifest, false).map(|(_, c)| c), Some(1));
        assert_eq!(fs_stamp(&dir.join("missing"), false), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
