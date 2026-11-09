//! The CLI surface: the `clap` argument model, the non-interactive
//! subcommands, and the shared data-source resolution (`--manifest` /
//! `--source` / `--project` / auto-detect) both the TUI and the `docs`
//! subcommand go through — so precedence can never diverge.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use dbtl::app::App;
use dbtl::{load_dag, load_dag_from_source_with_warnings};

/// dbtl: browse dbt models and their lineage — from a compiled manifest.json
/// or straight from project source (no `dbt compile`).
#[derive(Parser, Debug)]
#[command(
    name = "dbtl",
    version,
    about = "Browse dbt model lineage from a manifest or project source"
)]
pub struct Cli {
    /// Print version information and exit (lowercase alias of `-V`/`--version`).
    #[arg(short = 'v', action = clap::ArgAction::Version)]
    pub version_short: Option<bool>,
    /// Path to a compiled dbt manifest.json (forces manifest mode).
    #[arg(long, conflicts_with = "source")]
    pub manifest: Option<String>,
    /// Path to a dbt project dir, parsed from source with no compile (forces source mode).
    #[arg(long, conflicts_with = "manifest")]
    pub source: Option<String>,
    /// dbt project dir for auto-detect: prefer <dir>/target/manifest.json, else parse source.
    #[arg(long, default_value = ".")]
    pub project: String,
    /// Force pure-ASCII glyphs (boxes/borders/markers). Default: probe the terminal.
    #[arg(long, conflicts_with = "unicode")]
    pub ascii: bool,
    /// Force Unicode box-drawing glyphs, skipping the terminal probe.
    #[arg(long, conflicts_with = "ascii")]
    pub unicode: bool,
    /// Start with this model selected (by name, e.g. `stg_orders`).
    #[arg(long)]
    pub select: Option<String>,
    /// Auto-reload when the data source changes on disk (the manifest file, or
    /// any .sql/.yml/.csv under the project dir in source mode).
    #[arg(long)]
    pub watch: bool,
    /// Color theme: a preset name (see --list-themes), a theme under
    /// ~/.config/dbtl/themes/<name>.yml, or a path to a theme YAML file.
    #[arg(long, env = "DBTL_THEME")]
    pub theme: Option<String>,
    /// List the available color themes (presets + user theme files) and exit.
    #[arg(long)]
    pub list_themes: bool,
    /// Baseline to diff against: a manifest.json file, or a dbt project dir
    /// (its target/manifest.json if compiled, else parsed from source). Adds
    /// the Diff lens (added/modified tints), the `D` summary modal, and a
    /// status diff chip.
    #[arg(long)]
    pub diff: Option<String>,
    /// A non-interactive subcommand. Omitted = launch the TUI (the default,
    /// unchanged behaviour).
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Non-interactive subcommands. Absent → the TUI; present → run and exit
/// without ever touching the terminal (like `--list-themes`).
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Generate static Markdown docs (tbls-style) for the project and exit,
    /// without launching the TUI. Writes one Markdown page per node plus an
    /// index README.md under <DIR>. The data source is resolved the same way
    /// the TUI resolves it (--manifest / --source / --project / auto-detect).
    Docs {
        /// Output directory for the generated docs. Created if missing; existing
        /// node pages / README.md are overwritten (no files are deleted).
        #[arg(long)]
        out: String,
        /// Path to a compiled dbt manifest.json (forces manifest mode).
        #[arg(long, conflicts_with = "source")]
        manifest: Option<String>,
        /// Path to a dbt project dir, parsed from source with no compile.
        #[arg(long, conflicts_with = "manifest")]
        source: Option<String>,
        /// dbt project dir for auto-detect: prefer <dir>/target/manifest.json,
        /// else parse source.
        #[arg(long, default_value = ".")]
        project: String,
        /// Suppress the success summary on stdout (errors still go to stderr).
        /// For quiet CI runs where only the exit code matters.
        #[arg(long)]
        quiet: bool,
    },
}

/// A resolved data source: the loaded `Dag`, a human-readable label of where it
/// came from (a manifest path or a project dir, for docs provenance), and
/// whether it was parsed from source (vs a compiled manifest).
pub struct ResolvedSource {
    pub dag: dbtl::Dag,
    pub label: String,
    pub is_source: bool,
}

/// The shared data-source resolution, identical for the TUI and the `docs`
/// subcommand. Explicit `manifest` / `source` win; otherwise auto-detect under
/// `project`: a compiled `target/manifest.json` if present (full fidelity), else
/// parse the project from source. The TUI ([`build_app`]) and docs both go
/// through here so the precedence can never diverge.
pub fn resolve_source(
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
) -> Result<ResolvedSource> {
    if let Some(path) = manifest {
        let dag = load_dag(path)?;
        return Ok(ResolvedSource {
            dag,
            label: path.clone(),
            is_source: false,
        });
    }
    if let Some(dir) = source {
        let (dag, warnings) = load_dag_from_source_with_warnings(dir)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        return Ok(ResolvedSource {
            dag,
            label: dir.clone(),
            is_source: true,
        });
    }
    let project = PathBuf::from(project);
    let manifest = project.join("target").join("manifest.json");
    if manifest.is_file() {
        let dag = load_dag(&manifest)?;
        Ok(ResolvedSource {
            dag,
            label: manifest.display().to_string(),
            is_source: false,
        })
    } else if project.join("dbt_project.yml").is_file() {
        let (dag, warnings) = load_dag_from_source_with_warnings(&project)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        Ok(ResolvedSource {
            dag,
            label: project.display().to_string(),
            is_source: true,
        })
    } else {
        anyhow::bail!(
            "no data source under {}: neither target/manifest.json nor dbt_project.yml found \
             (pass --manifest <file> or --source <dir>)",
            project.display()
        )
    }
}

/// Resolve the CLI into a loaded [`App`]. Explicit `--manifest` / `--source` win;
/// otherwise auto-detect under `--project`: a compiled `target/manifest.json` if
/// present (full fidelity), else parse the project from source.
pub fn build_app(cli: &Cli) -> Result<App> {
    let resolved = resolve_source(&cli.manifest, &cli.source, &cli.project)?;
    let ResolvedSource {
        dag,
        label,
        is_source,
    } = resolved;
    if is_source {
        Ok(App::from_source(dag, PathBuf::from(label)))
    } else {
        Ok(App::new(dag, PathBuf::from(label)))
    }
}

/// Run the `docs` subcommand: resolve the data source (shared with the TUI),
/// generate the Markdown doc set in memory (the pure [`dbtl::generate_docs`]),
/// then write each file under `out` — creating the directory tree as needed and
/// overwriting existing pages, never deleting. Prints a one-line summary to
/// stdout on success. Every failure (load error, IO error) returns `Err`, which
/// `main` reports as plain text + a non-zero exit — no panic, no terminal init.
pub fn run_docs(
    out: &str,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    quiet: bool,
) -> Result<()> {
    let resolved = resolve_source(manifest, source, project)?;
    let provenance = if resolved.is_source {
        format!("project source ({})", resolved.label)
    } else {
        format!("manifest ({})", resolved.label)
    };
    let project_name = docs_project_name(&resolved.dag, &resolved.label);
    let files = dbtl::generate_docs(&resolved.dag, &project_name, &provenance);

    let out_dir = PathBuf::from(out);
    for (rel_path, contents) in &files {
        let path = out_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    // The success summary is the only stdout `docs` writes; `--quiet` drops it
    // for CI. Errors are returned as `Err` and printed to stderr by `main`
    // either way, so silencing stdout never hides a failure.
    if !quiet {
        println!("wrote {} files to {}", files.len(), out_dir.display());
    }
    Ok(())
}

/// A project title for the docs index. Prefers the dbt project name read from
/// the loaded graph itself ([`dbtl::Dag::project_name`], the `unique_id` project
/// segment) — authoritative and identical in manifest and source mode. Falls
/// back to a label-derived heuristic (the dir name of a `<root>/target/
/// manifest.json`, else the path's file name) only when the graph has no node
/// with a project segment, and finally to a generic title so the heading is
/// never blank.
fn docs_project_name(dag: &dbtl::Dag, label: &str) -> String {
    if let Some(name) = dag.project_name().filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    let path = std::path::Path::new(label);
    // Manifest path `<root>/target/manifest.json` → `<root>` dir name.
    if path.file_name().and_then(|f| f.to_str()) == Some("manifest.json") {
        if let Some(root) = path.parent().and_then(|p| p.parent()) {
            if let Some(name) = root.file_name().and_then(|f| f.to_str()) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    path.file_name()
        .and_then(|f| f.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("dbt project")
        .to_string()
}

/// Resolve the `--diff` baseline into a loaded `Dag` + a display label: a
/// manifest.json FILE directly, or a dbt project DIR through the same
/// auto-detect `build_app` uses (compiled `target/manifest.json` first, else
/// source parse) — so "diff against that other worktree" just works.
pub fn load_baseline(path: &str) -> Result<(dbtl::Dag, String)> {
    let p = PathBuf::from(path);
    if p.is_file() {
        let dag = load_dag(&p).with_context(|| format!("--diff: failed to load {path}"))?;
        return Ok((dag, path.to_string()));
    }
    if p.is_dir() {
        let manifest = p.join("target").join("manifest.json");
        if manifest.is_file() {
            let dag = load_dag(&manifest)
                .with_context(|| format!("--diff: failed to load {}", manifest.display()))?;
            return Ok((dag, manifest.display().to_string()));
        }
        if p.join("dbt_project.yml").is_file() {
            let (dag, warnings) = load_dag_from_source_with_warnings(&p)
                .with_context(|| format!("--diff: failed to parse project {path}"))?;
            for w in &warnings {
                eprintln!("warning (--diff baseline): {w}");
            }
            return Ok((dag, path.to_string()));
        }
    }
    anyhow::bail!(
        "--diff: {path} is neither a manifest.json file nor a dbt project dir \
         (no target/manifest.json or dbt_project.yml found)"
    )
}
