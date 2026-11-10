//! The CLI surface: the `clap` argument model, the non-interactive
//! subcommands, and the shared data-source resolution (`--manifest` /
//! `--source` / `--project` / auto-detect) both the TUI and the `docs`
//! subcommand go through — so precedence can never diverge.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use dbtl::app::App;
use dbtl::{load_dag_from_source_with_warnings, load_dag_with_warnings, ColumnResolver};

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
    /// Path to a dbt run_results.json (the last run's per-node status +
    /// execution_time). Adds the RunStatus/RunTime lenses, a `run n/m @date`
    /// status chip, and the runtime-weighted critical path in the `S` stats
    /// modal. Default: auto-detect target/run_results.json next to the data
    /// source — a missing file just means no run data, never an error.
    #[arg(long)]
    pub run_results: Option<String>,
    /// Hide models installed from dbt packages (`dbt_utils`, …): keep only the
    /// project's own nodes. Manifest mode only — a compiled manifest mixes
    /// `deps`-installed packages into `nodes`; source mode is local-only by
    /// construction.
    #[arg(long)]
    pub project_only: bool,
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
        /// After writing, delete stale generated pages: marked dbtl files under
        /// <DIR> that this run did not regenerate (e.g. pages of deleted or
        /// renamed models). Only files carrying the generated marker are
        /// removed — hand-written pages sharing the dir are never touched.
        #[arg(long)]
        clean: bool,
        /// Write nothing: compare the generated set against <DIR> on disk and
        /// exit 1 if anything differs — a missing page, a changed page, or a
        /// stale marked file. Prints one line per difference for the CI log.
        #[arg(long, conflicts_with = "clean")]
        check: bool,
    },
    /// Print the PR impact pack for a `--diff` baseline comparison to stdout
    /// and exit — Markdown by default (paste into a PR description), `--format
    /// json` for automation. The same `dbtl_core::diff_report` payload the TUI's
    /// `D` modal snapshots, so a CI comment matches the screen byte-for-byte.
    Impact {
        /// Baseline to diff against: a manifest.json file, or a dbt project dir
        /// (same resolution as the TUI's --diff).
        #[arg(long)]
        diff: String,
        /// Output format: `markdown` (default) or `json`.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ImpactFormat,
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
    },
    /// Run CI checks over the resolved data source and exit 1 when any fails:
    /// test coverage below `--coverage-min`, newly introduced layer violations,
    /// or changed buildable nodes without tests (the last two need `--diff`).
    /// The report is plain lines on stdout, one per check.
    Check {
        /// Baseline to diff against: enables the new-layer-violation and
        /// untested-change checks (same resolution as the TUI's --diff).
        #[arg(long)]
        diff: Option<String>,
        /// Fail when test coverage (model/seed/snapshot nodes with ≥1 test)
        /// falls below this percent. 0 disables the coverage check.
        #[arg(long, default_value = "0", value_parser = clap::value_parser!(u8))]
        coverage_min: u8,
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
        /// Suppress the per-check report on stdout; only the exit code matters.
        #[arg(long)]
        quiet: bool,
    },
    /// Print a column's column-level lineage to stdout and exit — the same
    /// `ColumnReport` the TUI's `yl` yank copies (Markdown by default,
    /// `--format json` for automation). `TARGET` is `<model>.<column>`; the
    /// model resolves by name (or exact unique_id), the column by the
    /// resolver's normalized name. Downstream is the default direction.
    /// Non-interactive: works in a non-TTY / CI environment.
    Columns {
        /// The column to trace, as `<model>.<column>` — e.g.
        /// `cll_demo.order_id`. The model segment is a node name or unique_id;
        /// the column segment matches case-insensitively (unquoted-identifier
        /// normalization, same as the resolver). LIMIT: the split is on the
        /// LAST `.`, so a column whose name itself contains one (a quoted
        /// `"a.b"` identifier) cannot be expressed here today.
        target: String,
        /// List the columns feeding INTO the target (provenance). Default is
        /// the downstream blast radius.
        #[arg(long, conflicts_with = "downstream")]
        upstream: bool,
        /// List the columns the target feeds (the default). Explicit so the
        /// direction is self-documenting in scripts.
        #[arg(long)]
        downstream: bool,
        /// Output format: `markdown` (default) or `json`.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ImpactFormat,
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
    },
}

/// The `dbtl impact` / `dbtl columns` output format — Markdown for pasting
/// into a review, JSON for automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ImpactFormat {
    /// The reviewer-shaped Markdown report (same bytes as the `D`→`e` export
    /// for `impact`, the `yl` yank for `columns`).
    Markdown,
    /// The payload as pretty JSON, for automation.
    Json,
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
        let (dag, warnings) = load_dag_with_warnings(path)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
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
        let (dag, warnings) = load_dag_with_warnings(&manifest)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
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
    // --project-only: drop installed-package nodes (uid's package segment ≠
    // the graph's own package). Source mode resolves to a no-op naturally —
    // every synthesized uid already names the project.
    let dag = if cli.project_only {
        let (dag, warning) = project_only_filter(dag);
        if let Some(w) = warning {
            eprintln!("warning (--project-only): {w}");
        }
        dag
    } else {
        dag
    };
    let mut app = if is_source {
        App::from_source(dag, PathBuf::from(label))
    } else {
        App::new(dag, PathBuf::from(label))
    };
    // An explicit --run-results overrides the auto-detected sibling; its load
    // error IS user-facing (unlike the silent auto-detect).
    if let Some(rr) = &cli.run_results {
        app.set_run_results(PathBuf::from(rr))?;
    }
    Ok(app)
}

/// Apply `--project-only`: keep only nodes whose `unique_id` package segment
/// is the owning package ([`Dag::primary_package`]). Returns the filtered Dag
/// plus a warning to surface when the filter emptied the graph — a manifest
/// whose recorded root project contributes NO kept nodes (a deps-only
/// project, or every local node disabled) is a valid if surprising outcome:
/// the empty graph is honest, whereas silently re-scoping to a foreign
/// package would show the exact opposite of the flag's intent.
fn project_only_filter(dag: dbtl::Dag) -> (dbtl::Dag, Option<String>) {
    let Some(pkg) = dag.primary_package() else {
        return (dag, None);
    };
    let filtered = dag.filtered(|n| n.unique_id.split('.').nth(1) == Some(pkg.as_str()));
    let warning = filtered
        .is_empty()
        .then(|| format!("no nodes belong to the project '{pkg}' — the graph is empty"));
    (filtered, warning)
}

/// Run the `docs` subcommand: resolve the data source (shared with the TUI),
/// generate the Markdown doc set in memory (the pure [`dbtl::generate_docs`]),
/// then write each file under `out` — creating the directory tree as needed and
/// overwriting existing pages. `--check` writes nothing and exits 1 when the
/// on-disk set differs; `--clean` additionally deletes marked generated files
/// the run did not regenerate (stale pages of removed models). Prints a
/// one-line summary to stdout on success. Every failure (load error, IO error)
/// returns `Err`, which `main` reports as plain text + a non-zero exit — no
/// panic, no terminal init.
pub fn run_docs(
    out: &str,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    quiet: bool,
    clean: bool,
    check: bool,
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
    if check {
        return check_docs(&out_dir, &files, quiet);
    }

    for (rel_path, contents) in &files {
        let path = out_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if clean {
        let removed = remove_stale_docs(&out_dir, &files)?;
        if !quiet && !removed.is_empty() {
            println!("removed {} stale file(s)", removed.len());
        }
    }
    // The success summary is the only stdout `docs` writes; `--quiet` drops it
    // for CI. Errors are returned as `Err` and printed to stderr by `main`
    // either way, so silencing stdout never hides a failure.
    if !quiet {
        println!("wrote {} files to {}", files.len(), out_dir.display());
    }
    Ok(())
}

/// The dbtl-owned Markdown files under `out_dir`: every `*.md` file whose
/// contents start with [`dbtl::GENERATED_MARKER`], returned as sorted relative
/// paths (`/`-separated, matching the generated rel paths). Unmarked files —
/// hand-written docs sharing the dir — are invisible here, so `--clean` and
/// `--check` can never touch them.
fn marked_docs_on_disk(out_dir: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, base, found);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !contents.starts_with(dbtl::GENERATED_MARKER) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(base) {
                found.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut found = Vec::new();
    walk(out_dir, out_dir, &mut found);
    found.sort();
    found
}

/// `docs --check`: compare the in-memory generated set against what's on disk
/// without writing anything. Prints one sorted line per difference — a
/// `missing:` page, a `changed:` page, or a `stale:` marked file — then returns
/// `Err` so `main` exits 1. An exact match exits 0.
fn check_docs(out_dir: &std::path::Path, files: &[(String, String)], quiet: bool) -> Result<()> {
    let expected: std::collections::HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut problems: Vec<String> = Vec::new();

    for (rel, want) in files {
        match std::fs::read_to_string(out_dir.join(rel)) {
            Err(_) => problems.push(format!("missing: {rel}")),
            Ok(have) if have != *want => problems.push(format!("changed: {rel}")),
            Ok(_) => {}
        }
    }
    for rel in marked_docs_on_disk(out_dir) {
        if !expected.contains(rel.as_str()) {
            problems.push(format!("stale: {rel}"));
        }
    }

    if problems.is_empty() {
        if !quiet {
            println!("docs up to date ({} files)", files.len());
        }
        return Ok(());
    }
    problems.sort();
    for p in &problems {
        println!("{p}");
    }
    anyhow::bail!("docs are out of date: {} difference(s)", problems.len())
}

/// `docs --clean`: delete marked generated files under `out_dir` that this run
/// did NOT regenerate (the stale pages of removed/renamed models), then prune
/// directories the deletions left empty. Returns the removed relative paths
/// (sorted). Only files carrying the generated marker are ever considered.
fn remove_stale_docs(out_dir: &std::path::Path, files: &[(String, String)]) -> Result<Vec<String>> {
    let expected: std::collections::HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut removed = Vec::new();
    for rel in marked_docs_on_disk(out_dir) {
        if expected.contains(rel.as_str()) {
            continue;
        }
        let path = out_dir.join(&rel);
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove stale {}", path.display()))?;
        // Prune the (possibly nested) parent dirs the deletion emptied, bottom
        // up, stopping at `out_dir` — `remove_dir` is a no-op error on a dir
        // that still holds files, so a half-emptied tree stops cleanly.
        let mut dir = path.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d == out_dir || std::fs::remove_dir(&d).is_err() {
                break;
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        removed.push(rel);
    }
    Ok(removed)
}

/// Run the `impact` subcommand: resolve the current source + the `--diff`
/// baseline, compute the shared [`dbtl::diff_report`] payload, and print it as
/// Markdown (the same bytes the TUI's `D`→`e` export writes) or pretty JSON —
/// to stdout, no TTY needed, so CI can post it as a PR comment.
pub fn run_impact(
    diff_path: &str,
    format: ImpactFormat,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
) -> Result<()> {
    let resolved = resolve_source(manifest, source, project)?;
    let (base, label) = load_baseline(diff_path)?;
    let diff = dbtl::compute_diff(&base, &resolved.dag);
    let report = dbtl::diff_report(&resolved.dag, Some(&base), &diff, &label);
    match format {
        ImpactFormat::Markdown => {
            let project = resolved.dag.project_name().unwrap_or("dbt");
            print!("{}", dbtl::pr_impact_markdown(project, &report));
        }
        ImpactFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

/// Run the `check` subcommand: evaluate the CI gates over the resolved source
/// and return `Err` (exit 1 via `main`) when any fails. Coverage below
/// `--coverage-min` (when >0), and — only with `--diff` — newly introduced
/// layer violations and changed buildable nodes without tests, all from the
/// single [`dbtl::pr_impact`] analysis. Prints one PASS/FAIL line per check to
/// stdout; `--quiet` prints nothing (exit code only).
pub fn run_check(
    diff_path: &Option<String>,
    coverage_min: u8,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    quiet: bool,
) -> Result<()> {
    let resolved = resolve_source(manifest, source, project)?;
    let dag = &resolved.dag;

    let mut lines: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    // Coverage — always measured (the shared `coverage_counts` base, the same
    // numbers the TUI's `cov` segment quotes); the gate only engages above 0.
    let (tested, total) = dbtl::coverage_counts(dag);
    // No buildable nodes → nothing untested → 100%.
    let pct = (tested * 100).checked_div(total).unwrap_or(100);
    if coverage_min == 0 || pct >= usize::from(coverage_min) {
        lines.push(format!("PASS coverage: {tested}/{total} ({pct}%)"));
    } else {
        let msg =
            format!("coverage {pct}% below --coverage-min {coverage_min}% ({tested}/{total})");
        lines.push(format!("FAIL {msg}"));
        failures.push(msg);
    }

    if let Some(diff_path) = diff_path {
        let (base, label) = load_baseline(diff_path)?;
        let diff = dbtl::compute_diff(&base, dag);
        let pr = dbtl::pr_impact(dag, Some(&base), &diff);
        lines.push(format!("baseline: {label}"));
        if pr.new_layer_violations.is_empty() {
            lines.push("PASS new layer violations: none".to_string());
        } else {
            let msg = format!(
                "{} new layer violation(s): {}",
                pr.new_layer_violations.len(),
                pr.new_layer_violations
                    .iter()
                    .map(|(p, c)| format!("{p} -> {c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            lines.push(format!("FAIL {msg}"));
            failures.push(msg);
        }
        if pr.untested_changes.is_empty() {
            lines.push("PASS untested changes: none".to_string());
        } else {
            let msg = format!(
                "{} changed node(s) without tests: {}",
                pr.untested_changes.len(),
                pr.untested_changes.join(", ")
            );
            lines.push(format!("FAIL {msg}"));
            failures.push(msg);
        }
    }

    if !quiet {
        for line in &lines {
            println!("{line}");
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} check(s) failed", failures.len())
    }
}

/// Run the `columns` subcommand: resolve the data source (shared with the
/// TUI), split `<model>.<column>`, and print the column's upstream or
/// downstream [`dbtl::ColumnReport`] — Markdown by default (the SAME bytes
/// the `yl` yank writes), `--format json` for automation. Everything is
/// `Err`-shaped like the other subcommands: a bad model or column name
/// reports as plain text with a non-zero exit, never a panic or a TTY probe.
pub fn run_columns(
    target: &str,
    upstream: bool,
    format: ImpactFormat,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
) -> Result<()> {
    let (model, column) = target
        .rsplit_once('.')
        .with_context(|| format!("columns: expected <model>.<column>, got '{target}'"))?;
    anyhow::ensure!(
        !model.is_empty() && !column.is_empty(),
        "columns: expected <model>.<column>, got '{target}'"
    );
    let resolved = resolve_source(manifest, source, project)?;
    let dag = &resolved.dag;

    // The model segment: an exact unique_id first, else the unique node NAME
    // (dbt names are project-unique; two matches → ambiguous, so bail and
    // tell the caller to pass the uid).
    let uid = if dag.contains(model) {
        model.to_string()
    } else {
        let mut hits: Vec<&str> = dag
            .nodes()
            .values()
            .filter(|n| n.name == model)
            .map(|n| n.unique_id.as_str())
            .collect();
        hits.sort();
        match hits.as_slice() {
            [one] => (*one).to_string(),
            [] => anyhow::bail!("columns: no node named '{model}'"),
            _ => anyhow::bail!(
                "columns: '{model}' is ambiguous ({}); pass the unique_id",
                hits.join(", ")
            ),
        }
    };

    // The CLL resolver, built exactly like the app's: the auto-detected
    // `target/catalog.json` (when present) is the top column-list tier.
    let catalog_path = if resolved.is_source {
        dbtl::default_catalog_path(None, Some(std::path::Path::new(&resolved.label)))
    } else {
        dbtl::default_catalog_path(Some(std::path::Path::new(&resolved.label)), None)
    };
    let catalog = catalog_path
        .filter(|p| p.is_file())
        .and_then(|p| dbtl::Catalog::load(&p).ok())
        .map(std::rc::Rc::new);
    let resolver = dbtl::NameMatchResolver::new(catalog);

    let col = dbtl::ColumnRef::new(&uid, column);
    let known = resolver.columns(dag, &uid);
    anyhow::ensure!(
        known.contains(&col.column),
        "columns: '{model}' has no column '{column}' (known: {})",
        if known.is_empty() {
            "(none — the node has no resolvable columns)".to_string()
        } else {
            known.join(", ")
        }
    );

    let direction = if upstream {
        dbtl::ClosureDirection::Upstream
    } else {
        dbtl::ClosureDirection::Downstream
    };
    let report = dbtl::closure_report(dag, &resolver, &col, direction);
    match format {
        ImpactFormat::Markdown => print!("{report}"),
        ImpactFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    Ok(())
}

/// A project title for the docs index. Prefers the dbt project name read from
/// the loaded graph itself ([`dbtl::Dag::project_name`] — the manifest's
/// `metadata.project_name`, else a `unique_id`-segment heuristic) —
/// authoritative and identical in manifest and source mode. Falls back to a
/// label-derived heuristic (the dir name of a `<root>/target/manifest.json`,
/// else the path's file name) only when the graph has no name at all, and
/// finally to a generic title so the heading is never blank.
fn docs_project_name(dag: &dbtl::Dag, label: &str) -> String {
    if let Some(name) = dag.project_name().filter(|s| !s.trim().is_empty()) {
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
        let (dag, warnings) =
            load_dag_with_warnings(&p).with_context(|| format!("--diff: failed to load {path}"))?;
        for w in &warnings {
            eprintln!("warning (--diff baseline): {w}");
        }
        return Ok((dag, path.to_string()));
    }
    if p.is_dir() {
        let manifest = p.join("target").join("manifest.json");
        if manifest.is_file() {
            let (dag, warnings) = load_dag_with_warnings(&manifest)
                .with_context(|| format!("--diff: failed to load {}", manifest.display()))?;
            for w in &warnings {
                eprintln!("warning (--diff baseline): {w}");
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use dbtl::{Dag, RawManifest, RawMetadata, RawNode};
    use std::collections::HashMap;

    /// A `Dag` whose recorded root project is `project_name`, built from
    /// model `unique_id`s (each `model.<pkg>.<name>` — the package segment is
    /// what `--project-only` scopes by).
    fn dag_with(project_name: Option<&str>, uids: &[&str]) -> Dag {
        let nodes = uids
            .iter()
            .map(|uid| {
                (
                    uid.to_string(),
                    RawNode {
                        name: uid.rsplit('.').next().unwrap_or(uid).into(),
                        resource_type: "model".into(),
                        ..Default::default()
                    },
                )
            })
            .collect();
        Dag::build(&RawManifest {
            metadata: RawMetadata {
                project_name: project_name.map(str::to_string),
            },
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            macros: HashMap::new(),
            semantic_models: HashMap::new(),
            metrics: HashMap::new(),
            saved_queries: HashMap::new(),
            unit_tests: HashMap::new(),
            functions: HashMap::new(),
            parent_map: HashMap::new(),
            child_map: HashMap::new(),
        })
    }

    #[test]
    fn project_only_filter_warns_when_the_graph_goes_empty() {
        // A deps-only project: the recorded root `zzz_proj` contributes NO
        // nodes — every kept node belongs to an installed package. Strict
        // scope keeps none of them, so the graph is empty AND a warning
        // explains it (re-scoping to `aaa_pkg` would show the exact opposite
        // of the flag's intent).
        let dag = dag_with(Some("zzz_proj"), &["model.aaa_pkg.a", "model.aaa_pkg.b"]);
        let (dag, warning) = project_only_filter(dag);
        assert!(dag.is_empty(), "the filter stays strict — no foreign nodes");
        let w = warning.expect("an empty result must carry a warning");
        assert!(
            w.contains("zzz_proj"),
            "the warning names the missing project, got: {w}"
        );
    }

    #[test]
    fn project_only_filter_keeps_root_nodes_silently() {
        // The normal case: the root project's nodes survive, foreign ones are
        // dropped, no warning.
        let dag = dag_with(
            Some("zzz_proj"),
            &["model.zzz_proj.root_model", "model.aaa_pkg.a"],
        );
        let (dag, warning) = project_only_filter(dag);
        assert_eq!(dag.len(), 1, "only the root project's model survives");
        assert!(dag.contains("model.zzz_proj.root_model"));
        assert!(warning.is_none(), "no warning when the scope is non-empty");
    }
}
