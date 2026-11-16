//! The `*-paths` resolution of `dbt_project.yml` — one place the key
//! defaults, the legacy aliases, and the project-containment filter are
//! applied, so the source loader and outside callers asking "which
//! directories does `dbt parse` read" can never disagree.

use super::*;

/// The resolved resource-path lists of a `dbt_project.yml`: every key's
/// dbt default filled, the pre-1.0 `source-paths`/`data-paths` aliases
/// honored, and every entry run through [`contained_resource_paths`] so
/// an absolute or `..` path can never point a scan outside the project.
#[derive(Debug, Default)]
pub(super) struct ResourcePaths {
    /// `model-paths` (or the pre-0.17 `source-paths` alias) — `.sql`/`.py`
    /// models.
    pub(super) model_paths: Vec<String>,
    /// `seed-paths` (or the pre-1.0 `data-paths` alias) — `.csv` seeds.
    pub(super) seed_paths: Vec<String>,
    /// `snapshot-paths` — `{% snapshot %}` blocks and YAML snapshots.
    pub(super) snapshot_paths: Vec<String>,
    /// `macro-paths` — the project `{% macro %}` table's `.sql` files.
    pub(super) macro_paths: Vec<String>,
    /// `analysis-paths`.
    pub(super) analysis_paths: Vec<String>,
    /// `test-paths`.
    pub(super) test_paths: Vec<String>,
    /// The docs-block scan roots — `docs-paths` when declared, else dbt's
    /// default: the union of every other resource path (a `{% docs %}`
    /// block under `macros/` resolves with no `docs-paths` set).
    pub(super) doc_paths: Vec<String>,
}

/// One `model-paths`-style key: the declared list wins, then the legacy
/// `alias` list, then dbt's `default` — always contained to the project.
fn pick_paths(
    declared: &Option<Vec<String>>,
    alias: &Option<Vec<String>>,
    default: &str,
) -> Vec<String> {
    contained_resource_paths(
        declared
            .clone()
            .or_else(|| alias.clone())
            .unwrap_or_else(|| vec![default.to_string()]),
    )
}

/// Resolve one parsed `dbt_project.yml` into its [`ResourcePaths`]. The
/// `data-paths` rename notice and the both-aliases-declared notice are
/// emitted here (appended to `warnings` like every recoverable oddity)
/// so every caller resolves — and reports — the paths identically;
/// callers that only need the directories pass a sink they discard.
pub(super) fn resolve_resource_paths(
    project: &DbtProject,
    warnings: &mut Vec<String>,
) -> ResourcePaths {
    let model_paths = pick_paths(&project.model_paths, &project.source_paths, "models");
    // `data-paths` alone still emits dbt's deprecation warning — the
    // message dbt itself prints (verified on 1.12.6), recorded here so
    // source mode's warning surface reads the same.
    if project.data_paths.is_some() {
        warnings.push(
            "The data-paths config has been renamed to seed-paths. Please update your dbt_project.yml configuration to reflect this change."
                .to_string(),
        );
    }
    // Declaring both spellings is a dbt error — degraded to a warning
    // here like every other recoverable oddity, `seed-paths` winning.
    if project.seed_paths.is_some() && project.data_paths.is_some() {
        warnings.push(
            "dbt_project.yml declares both 'seed-paths' and 'data-paths' (a dbt error); 'seed-paths' wins"
                .to_string(),
        );
    }
    let seed_paths = pick_paths(&project.seed_paths, &project.data_paths, "seeds");
    let snapshot_paths = pick_paths(&project.snapshot_paths, &None, "snapshots");
    let macro_paths = pick_paths(&project.macro_paths, &None, "macros");
    let analysis_paths = pick_paths(&project.analysis_paths, &None, "analyses");
    let test_paths = pick_paths(&project.test_paths, &None, "tests");
    let doc_paths = match &project.docs_paths {
        Some(paths) => contained_resource_paths(paths.clone()),
        None => [
            model_paths.clone(),
            seed_paths.clone(),
            snapshot_paths.clone(),
            macro_paths.clone(),
            analysis_paths.clone(),
            test_paths.clone(),
        ]
        .concat(),
    };
    ResourcePaths {
        model_paths,
        seed_paths,
        snapshot_paths,
        macro_paths,
        analysis_paths,
        test_paths,
        doc_paths,
    }
}

/// The directories a dbt project reads its resources from — the union of
/// every resolved `*-paths` list — as normalized absolute paths under
/// `dir`, sorted and deduplicated. Parsed through the same YAML loader and
/// `DbtProject` shape the source-mode build uses, so "which directories
/// are manifest inputs" is answered identically in both places. `None`
/// when `dbt_project.yml` is missing, unreadable, or unparsable — the
/// caller then falls back to a broader scan rather than guessing paths.
pub fn project_resource_dirs(dir: &Path) -> Option<Vec<PathBuf>> {
    let text = fs::read_to_string(dir.join("dbt_project.yml")).ok()?;
    let mut ignored = Vec::new();
    let project: DbtProject = yaml_to_json(&text, &mut ignored)
        .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        .ok()?;
    let paths = resolve_resource_paths(&project, &mut ignored);
    let mut dirs: Vec<PathBuf> = [
        paths.model_paths,
        paths.seed_paths,
        paths.snapshot_paths,
        paths.macro_paths,
        paths.analysis_paths,
        paths.test_paths,
        paths.doc_paths,
    ]
    .concat()
    .iter()
    .map(|rel| normalize_joined(dir, rel))
    .collect();
    dirs.sort();
    dirs.dedup();
    Some(dirs)
}

/// `dir.join(rel)` with `rel`'s `.` components folded out — the spellings
/// `"models"`, `"./models"`, and `"."` of one resource root dedupe to a
/// single entry. `rel` is already project-contained (no `..`, not
/// absolute), so nothing outside `dir` can be produced.
fn normalize_joined(dir: &Path, rel: &str) -> PathBuf {
    let mut clean = PathBuf::new();
    for c in Path::new(rel).components() {
        if let Component::Normal(seg) = c {
            clean.push(seg);
        }
    }
    dir.join(clean)
}
