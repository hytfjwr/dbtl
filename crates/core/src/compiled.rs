//! `target/compiled/` — the Jinja-expanded SQL `dbt compile` / `dbt build`
//! writes per node as `<package>/<original_file_path>`. A `dbt parse`
//! manifest carries no `compiled_code` (the field only fills when a manifest
//! is written BY a compile/build), so the on-disk files are the fallback
//! source for every parse-produced manifest — and the only source in source
//! mode. Side-map like [`crate::run_results`] / [`crate::catalog`]:
//! auto-detected, never an error when absent.
//!
//! This module also owns [`default_target_dir`]: the single "where dbt writes
//! its target artifacts for this data source" resolution shared by every
//! sibling-artifact loader (`run_results.json`, `catalog.json`, and the
//! `compiled/` tree) — the rule was written three times; now once.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Where dbt writes its target artifacts for a data source: the manifest's
/// own parent dir (a compiled `target/manifest.json` lives inside `target/`),
/// or `<project>/target` in source mode. `None` when neither coordinate is
/// given (the loaders always pass one).
pub fn default_target_dir(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(m) = manifest_path {
        m.parent().map(|dir| dir.to_path_buf())
    } else {
        project_dir.map(|dir| dir.join("target"))
    }
}

/// The `target/compiled/` dir for a data source — [`default_target_dir`] plus
/// the `compiled` subtree dbt fills on `compile`/`build`/`run`/`test`.
pub fn default_compiled_dir(
    manifest_path: Option<&Path>,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    default_target_dir(manifest_path, project_dir).map(|dir| dir.join("compiled"))
}

/// A node's compiled (Jinja-expanded) SQL plus its freshness relative to the
/// source file — the [`Dag`](crate::Dag) `compiled` side-map entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompiledSql {
    /// The expanded SQL text.
    pub code: String,
    /// `true` when the on-disk source was NEWER than this expansion at load
    /// time — the compiled text may not match what the model says now (a
    /// `dbt compile` would refresh it). Always `false` for manifest-carried
    /// `compiled_code`: it ships in the same artifact as `raw_code`, so it is
    /// exactly as fresh as the manifest itself.
    pub stale: bool,
}

/// The on-disk compiled file for a node:
/// `<compiled_dir>/<package>/<original_file_path>`. `None` when the
/// coordinates can't name a file safely — a package that isn't a single
/// normal path segment, or an `original_file_path` that escapes (absolute,
/// `..`): both come from an untrusted manifest, so the check is the same
/// lexical rule the `$EDITOR` jump applies (never `canonicalize` — the file
/// legitimately may not exist yet).
pub fn compiled_file_path(
    compiled_dir: &Path,
    package: &str,
    original_file_path: &str,
) -> Option<PathBuf> {
    // dbt package names are identifiers — a `package` carrying a separator or
    // `..` would let a crafted manifest point the read outside `compiled_dir`.
    let mut pkg_segs = Path::new(package).components();
    if !matches!(pkg_segs.next(), Some(Component::Normal(_))) || pkg_segs.next().is_some() {
        return None;
    }
    let ofp = Path::new(original_file_path);
    if ofp
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    Some(compiled_dir.join(package).join(ofp))
}

/// Whether the compiled file is older than the node's source — i.e. a
/// `dbt compile` would rewrite it. The source is looked up at
/// `<project_root>/<original_file_path>` and, for installed-package nodes,
/// `<project_root>/dbt_packages/<package>/<original_file_path>`; a source
/// that isn't on disk (a manifest taken without its project checkout) yields
/// `false` — undeterminable is not stale.
pub fn is_stale(
    compiled_file: &Path,
    project_root: &Path,
    package: &str,
    original_file_path: &str,
) -> bool {
    let Ok(compiled_mtime) = fs::metadata(compiled_file).and_then(|m| m.modified()) else {
        return false;
    };
    let ofp = Path::new(original_file_path);
    [
        project_root.join(ofp),
        project_root.join("dbt_packages").join(package).join(ofp),
    ]
    .iter()
    .any(|src| {
        fs::metadata(src)
            .and_then(|m| m.modified())
            .is_ok_and(|src_mtime| src_mtime > compiled_mtime)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn target_dir_resolves_manifest_sibling_and_project_dir() {
        let m = Path::new("/proj/target/manifest.json");
        assert_eq!(
            default_target_dir(Some(m), None).as_deref(),
            Some(Path::new("/proj/target"))
        );
        // The manifest path wins when both are given (the loaders never do,
        // but the rule is documented here).
        assert_eq!(
            default_target_dir(Some(m), Some(Path::new("/other"))).as_deref(),
            Some(Path::new("/proj/target"))
        );
        assert_eq!(
            default_target_dir(None, Some(Path::new("/proj"))).as_deref(),
            Some(Path::new("/proj/target"))
        );
        assert_eq!(default_target_dir(None, None), None);
        assert_eq!(
            default_compiled_dir(None, Some(Path::new("/proj"))).as_deref(),
            Some(Path::new("/proj/target/compiled"))
        );
    }

    #[test]
    fn compiled_file_path_joins_package_and_original_path() {
        let dir = Path::new("/p/target/compiled");
        assert_eq!(
            compiled_file_path(dir, "pkg", "models/m.sql").as_deref(),
            Some(Path::new("/p/target/compiled/pkg/models/m.sql"))
        );
        // Traversal is rejected on BOTH coordinates — an untrusted manifest
        // must not steer the read outside target/compiled/.
        assert_eq!(compiled_file_path(dir, "..", "models/m.sql"), None);
        assert_eq!(compiled_file_path(dir, "a/b", "models/m.sql"), None);
        assert_eq!(compiled_file_path(dir, "pkg", "../evil.sql"), None);
        assert_eq!(compiled_file_path(dir, "pkg", "/abs/m.sql"), None);
    }

    #[test]
    fn staleness_compares_source_and_compiled_mtimes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let compiled_dir = root.join("target").join("compiled").join("pkg");
        fs::create_dir_all(&compiled_dir).unwrap();
        fs::create_dir_all(root.join("models")).unwrap();
        let compiled = compiled_dir.join("models").join("m.sql");
        fs::create_dir_all(compiled.parent().unwrap()).unwrap();
        fs::write(&compiled, "select 1").unwrap();
        let source = root.join("models").join("m.sql");
        fs::write(&source, "select {{ ref('x') }}").unwrap();

        let t0 = SystemTime::now() - Duration::from_secs(3600);
        let t1 = SystemTime::now() - Duration::from_secs(60);
        let set = |p: &Path, t: SystemTime| {
            fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };

        // Source older than the expansion: fresh.
        set(&source, t0);
        set(&compiled, t1);
        assert!(!is_stale(&compiled, root, "pkg", "models/m.sql"));

        // Source newer than the expansion: stale.
        set(&compiled, t0);
        set(&source, t1);
        assert!(is_stale(&compiled, root, "pkg", "models/m.sql"));

        // A missing source cannot be judged stale.
        assert!(!is_stale(&compiled, root, "pkg", "models/absent.sql"));

        // dbt_packages fallback: an installed-package node's source lives
        // under dbt_packages/<pkg>/, not the project root.
        fs::remove_file(&source).unwrap();
        let pkg_src = root.join("dbt_packages").join("pkg").join("models");
        fs::create_dir_all(&pkg_src).unwrap();
        let pkg_source = pkg_src.join("m.sql");
        fs::write(&pkg_source, "select 2").unwrap();
        set(&pkg_source, t1); // compiled still at t0
        assert!(
            is_stale(&compiled, root, "pkg", "models/m.sql"),
            "dbt_packages source newer than expansion is stale"
        );
    }
}
