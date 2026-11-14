//! The dependency bookkeeping [`RefScope`](crate::RefScope) records so
//! `unresolved_references` can judge a `ref()`/`source()` against the same
//! name universe a real `dbt parse` would see.
//!
//! Two inputs, both cheap reads — package SQL is NEVER parsed (matching the
//! loader's root-only stance, and keeping the scan fast on real installs):
//!
//! - `dbt_packages/*/`: each installed dir's OWN `dbt_project.yml` names the
//!   package and locates its resource roots; the `.sql`/`.py`/`.csv` stems
//!   under them — plus `{% snapshot %}`/`snapshots:` yml names — become
//!   refable names, and `sources:` yml declarations become sourceable pairs.
//! - `dependencies.yml` / `packages.yml` at the project root: the
//!   `projects:` entries are the cross-project (dbt-mesh) names a two-arg
//!   `ref('proj', 'x')` may legitimately name, and the `packages:` count
//!   compares against the installed dirs — fewer installed means a missing
//!   name may simply live in a package that was never installed, so the
//!   rule degrades those hits to a summary warning instead of findings.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// The `projects:`/`packages:` entries `dependencies.yml` and `packages.yml`
/// declare — dbt's two dependency manifests (a project may carry either or
/// both).
#[derive(Debug, Default)]
pub(super) struct DeclaredDeps {
    /// `projects:` names — cross-project refs a `ref('proj', 'x')` may name.
    /// dbt writes `{name: …}` maps; a bare string counts too (lenient read —
    /// either file is hand-edited and its worst outcome is a skipped
    /// suppression, never a load error).
    pub(super) projects: BTreeSet<String>,
    /// Total `packages:` entries across both files — the count the installed
    /// dir tally compares against.
    pub(super) declared: usize,
}

impl DeclaredDeps {
    /// Read both manifests from the project root. Either may be absent or
    /// unparsable — a bad dependency file already fails `dbt parse`
    /// upstream, so a lenient read here only narrows the suppression.
    pub(super) fn load(dir: &Path, warnings: &mut Vec<String>) -> Self {
        let mut deps = DeclaredDeps::default();
        for file in ["dependencies.yml", "packages.yml"] {
            let text = read(&dir.join(file));
            if text.is_empty() {
                continue;
            }
            let Ok(v) = yaml_to_json(&text, warnings) else {
                continue;
            };
            let Some(map) = v.as_object() else {
                continue;
            };
            if let Some(serde_json::Value::Array(pkgs)) = map.get("packages") {
                deps.declared += pkgs.len();
            }
            if let Some(serde_json::Value::Array(projs)) = map.get("projects") {
                for p in projs {
                    if let Some(name) = p
                        .as_str()
                        .or_else(|| p.get("name").and_then(|n| n.as_str()))
                    {
                        deps.projects.insert(name.to_string());
                    }
                }
            }
        }
        deps
    }
}

/// The names installed `dbt_packages/` dirs contribute: `name -> packages`
/// for `ref()`, `(source, table)` pairs for `source()`.
#[derive(Debug, Default)]
pub(super) struct InstalledPackages {
    pub(super) refs: HashMap<String, BTreeSet<String>>,
    pub(super) sources: BTreeSet<(String, String)>,
    /// Directories directly under `dbt_packages/` — the tally
    /// [`DeclaredDeps::declared`] compares against.
    pub(super) installed: usize,
}

impl InstalledPackages {
    /// Scan every dir under `<dir>/dbt_packages/`, sorted so the walk is
    /// deterministic. A missing `dbt_packages/` is the common case, not an
    /// error.
    pub(super) fn load(dir: &Path, pat: &Patterns, warnings: &mut Vec<String>) -> Self {
        let mut out = InstalledPackages::default();
        let mut dirs: Vec<PathBuf> = fs::read_dir(dir.join("dbt_packages"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        out.installed = dirs.len();
        for dir in dirs {
            out.scan_package(&dir, pat, warnings);
        }
        out
    }

    /// One installed package: its `dbt_project.yml` names it and locates the
    /// resource roots, then name collection is file-stem-and-yml only —
    /// nothing parses the package's SQL.
    fn scan_package(&mut self, pkg_dir: &Path, pat: &Patterns, warnings: &mut Vec<String>) {
        let yml = pkg_dir.join("dbt_project.yml");
        let project = yaml_to_json(&read(&yml), warnings)
            .ok()
            .and_then(|v| serde_json::from_value::<DbtProject>(v).ok());
        // A package without a parseable `dbt_project.yml` keeps the dbt
        // defaults: the dir name answers `ref('pkg', …)` and `models/` etc.
        // still name its resources.
        let fallback_name = || {
            pkg_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        };
        let name = match &project {
            Some(p) if !p.name.is_empty() => p.name.clone(),
            _ => fallback_name(),
        };
        // `source-paths` is the legacy alias for `model-paths`; `data-paths`
        // for `seed-paths` — same aliasing the root project's load applies.
        let or_default = |pick: &dyn Fn(&DbtProject) -> Option<Vec<String>>, default: &str| {
            contained_resource_paths(
                project
                    .as_ref()
                    .and_then(pick)
                    .unwrap_or_else(|| vec![default.to_string()]),
            )
        };
        let model_paths = or_default(
            &|p| p.model_paths.clone().or_else(|| p.source_paths.clone()),
            "models",
        );
        let seed_paths = or_default(
            &|p| p.seed_paths.clone().or_else(|| p.data_paths.clone()),
            "seeds",
        );
        let snapshot_paths = or_default(&|p| p.snapshot_paths.clone(), "snapshots");

        let mut yml_roots: Vec<String> = Vec::new();
        for sub in &model_paths {
            yml_roots.push(sub.clone());
            for file in collect_with_root(pkg_dir, std::slice::from_ref(sub), "sql")
                .into_iter()
                .map(|(f, _)| f)
                .chain(
                    collect_with_root(pkg_dir, std::slice::from_ref(sub), "py")
                        .into_iter()
                        .map(|(f, _)| f),
                )
            {
                self.refs
                    .entry(file_stem(&file))
                    .or_default()
                    .insert(name.clone());
            }
        }
        for sub in &seed_paths {
            yml_roots.push(sub.clone());
            for (file, _) in collect_with_root(pkg_dir, std::slice::from_ref(sub), "csv") {
                self.refs
                    .entry(file_stem(&file))
                    .or_default()
                    .insert(name.clone());
            }
        }
        for sub in &snapshot_paths {
            yml_roots.push(sub.clone());
            for (file, _) in collect_with_root(pkg_dir, std::slice::from_ref(sub), "sql") {
                // `{% snapshot x %}` names the refable resource; the file
                // stem counts too — dbt accepts both spellings for a
                // name collision it would have rejected at parse.
                let raw = read(&file);
                let masked = mask_jinja_comments(&raw);
                self.refs
                    .entry(file_stem(&file))
                    .or_default()
                    .insert(name.clone());
                for snap in pat.snapshots(&masked) {
                    self.refs.entry(snap).or_default().insert(name.clone());
                }
            }
        }
        // yml under every resource root: `sources:` entries declare the
        // `source('s', 't')` pairs; `snapshots:` entries name resources
        // without a `.sql` file.
        let mut yml_files: Vec<PathBuf> = Vec::new();
        for sub in &yml_roots {
            let mut files = Vec::new();
            let root = pkg_dir.join(sub);
            collect_files(&root, "yml", &mut files);
            collect_files(&root, "yaml", &mut files);
            yml_files.extend(files);
        }
        yml_files.sort();
        yml_files.dedup();
        for file in yml_files {
            let Ok(v) = yaml_to_json(&read(&file), warnings) else {
                continue;
            };
            let Ok(schema) = serde_json::from_value::<SchemaFile>(v) else {
                continue;
            };
            for src in &schema.sources {
                for tbl in &src.tables {
                    self.sources.insert((src.name.clone(), tbl.name.clone()));
                }
            }
            for snap in &schema.snapshots {
                self.refs
                    .entry(snap.name.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }
    }
}
