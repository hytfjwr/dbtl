//! The CLI surface: the `clap` argument model, the non-interactive
//! subcommands, and the shared data-source resolution (`--manifest` /
//! `--source` / `--project` / auto-detect) both the TUI and the `docs`
//! subcommand go through — so precedence can never diverge.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use dbtl::app::App;
use dbtl::{load_dag_from_source_with_warnings, load_dag_with_warnings, ColumnResolver};

/// The `--sql-dialect` value parser: clap-friendly `Err(String)` wrapper over
/// [`dbtl::SqlDialect::parse_name`], so a typo fails at parse time with the
/// accepted names spelled out.
fn parse_sql_dialect(s: &str) -> std::result::Result<dbtl::SqlDialect, String> {
    dbtl::SqlDialect::parse_name(s).ok_or_else(|| {
        format!(
            "unknown SQL dialect '{s}' (accepted: {})",
            dbtl::SqlDialect::NAMES.join(", ")
        )
    })
}

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
    /// Deprecated no-op kept for backwards compatibility: the default scope
    /// is ALREADY the root project only (installed-package nodes are pruned
    /// at load). Passing it prints a deprecation warning and changes nothing —
    /// use `--include-packages` to WIDEN the scope instead. Will be removed in
    /// a future release.
    /// `conflicts_with_all`: `--include-packages` is the opposite scope and
    /// `--list-themes` is a query-only flag that never loads a project —
    /// warning about a scope that will never apply is noise.
    #[arg(long, hide = true, conflicts_with_all = ["include_packages", "list_themes"])]
    pub project_only: bool,
    /// Include nodes installed from dbt packages (`dbt deps` / `packages.yml`)
    /// in the graph: manifest mode's compiled `nodes` map mixes them in. They
    /// appear in the model list, lineage, docs pages, and `--diff` baselines as
    /// dependency context — but every aggregate metric (coverage / orphans /
    /// stats / docs summary / `dbtl check` / the PR-impact risk flags) still
    /// counts the root project only. Source mode never parses `dbt_packages`,
    /// so the flag is a no-op there (with a warning). `global` so it works
    /// before OR after a subcommand (`dbtl --include-packages check` ≡
    /// `dbtl check --include-packages`).
    #[arg(long, global = true)]
    pub include_packages: bool,
    /// SQL dialect for parser-based column lineage: `auto` reads the manifest's
    /// adapter_type (source mode has none → generic); any other value pins the
    /// dialect for every model (`generic`, `postgres`, `snowflake`, `bigquery`,
    /// `mysql`, `sqlite`, `duckdb`, `spark`, …). `global` so `dbtl columns`
    /// accepts it too.
    #[arg(long, global = true, value_name = "DIALECT", value_parser = parse_sql_dialect)]
    pub sql_dialect: Option<dbtl::SqlDialect>,
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
        /// untested-change checks (same resolution as the TUI's --diff). With
        /// --lint it also scopes lint gating to NEW violations only.
        #[arg(long)]
        diff: Option<String>,
        /// Fail when test coverage (model/seed/snapshot nodes with ≥1 test)
        /// falls below this percent. 0 disables the coverage check.
        #[arg(long, default_value = "0", value_parser = clap::value_parser!(u8))]
        coverage_min: u8,
        /// Run the static structure-lint rules (the `dbt_project_evaluator`-
        /// style checks — no warehouse or `dbt compile` needed). Rule
        /// enable/disable, thresholds, and naming prefixes come from the
        /// project-local `.dbtl.yml`. With --diff, only violations that did
        /// not exist in the baseline gate.
        #[arg(long)]
        lint: bool,
        /// Fail when the diff carries BREAKING column changes that
        /// downstream columns still reference — removed or renamed columns
        /// whose baseline consumers did not migrate (detected through
        /// column-level lineage). Needs `--diff`. Partial-breaking changes
        /// (data-type / expression moves) report in the impact pack but
        /// never trip this gate. Without `--diff` the gate is skipped and a
        /// note is printed — under `--format json` the note lands in `info`
        /// (not `checks`, since no check ran).
        #[arg(long)]
        no_breaking: bool,
        /// Restrict linting to these rules (comma-separated rule names;
        /// implies --lint). Overrides `.dbtl.yml`'s per-rule enables — the
        /// flag is the authoritative rule selector; the file still supplies
        /// thresholds and naming prefixes.
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        rules: Vec<String>,
        /// Output format: `markdown` (the line report, default) or `json`
        /// (the structured check payload for automation).
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
/// came from (a manifest path or a project dir, for docs provenance), whether
/// it was parsed from source (vs a compiled manifest), and every warning the
/// load produced (loader + package scope — each already printed to stderr
/// exactly as before).
pub struct ResolvedSource {
    pub dag: dbtl::Dag,
    pub label: String,
    pub is_source: bool,
    /// The collected warnings (loader first, then scope). `build_app` feeds
    /// them to the App's notification log as unread WARNING entries (Issue
    /// 08) so they stay visible after the TUI opens; non-TUI callers ignore
    /// them — stderr output is unchanged.
    pub warnings: Vec<String>,
}

/// The shared data-source resolution, identical for the TUI and the `docs`
/// subcommand. Explicit `manifest` / `source` win; otherwise auto-detect under
/// `project`: a compiled `target/manifest.json` if present (full fidelity), else
/// parse the project from source. The TUI ([`build_app`]) and docs both go
/// through here so the precedence can never diverge.
///
/// `scope` is applied to the loaded graph on the way out (the default `Root`
/// scope prunes installed-package nodes — manifest mode's `nodes` map mixes
/// them in), so every caller sees the same scoped `Dag` and the package
/// boundary lives in one place.
pub fn resolve_source(
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    scope: dbtl::PackageScope,
) -> Result<ResolvedSource> {
    if let Some(path) = manifest {
        let (dag, mut warnings) = load_dag_with_warnings(path)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, false);
        warnings.extend(scope_w);
        return Ok(ResolvedSource {
            dag,
            label: path.clone(),
            is_source: false,
            warnings,
        });
    }
    if let Some(dir) = source {
        let (dag, mut warnings) = load_dag_from_source_with_warnings(dir)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, true);
        warnings.extend(scope_w);
        return Ok(ResolvedSource {
            dag,
            label: dir.clone(),
            is_source: true,
            warnings,
        });
    }
    let project = PathBuf::from(project);
    let manifest = project.join("target").join("manifest.json");
    if manifest.is_file() {
        let (dag, mut warnings) = load_dag_with_warnings(&manifest)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, false);
        warnings.extend(scope_w);
        Ok(ResolvedSource {
            dag,
            label: manifest.display().to_string(),
            is_source: false,
            warnings,
        })
    } else if project.join("dbt_project.yml").is_file() {
        let (dag, mut warnings) = load_dag_from_source_with_warnings(&project)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let (dag, scope_w) = apply_package_scope(dag, scope, true);
        warnings.extend(scope_w);
        Ok(ResolvedSource {
            dag,
            label: project.display().to_string(),
            is_source: true,
            warnings,
        })
    } else {
        anyhow::bail!(
            "no data source under {}: neither target/manifest.json nor dbt_project.yml found \
             (pass --manifest <file> or --source <dir>)",
            project.display()
        )
    }
}

/// The package scope the CLI asks for (`--include-packages` ⇒ `All`, else the
/// default `Root`). The flag is `global`, so clap lands the value here whether
/// it was typed before or after a subcommand — the TUI and the subcommand
/// dispatchers all read this one field.
impl Cli {
    pub fn package_scope(&self) -> dbtl::PackageScope {
        if self.include_packages {
            dbtl::PackageScope::All
        } else {
            dbtl::PackageScope::Root
        }
    }
}

/// Apply the loaded graph's package scope: `All` keeps the manifest's full
/// contents (installed-package nodes as dependency context); the default
/// `Root` prunes to the root project — what `--project-only` used to ask for.
/// The scope decision itself (prune predicate, early-out, and the warnings —
/// an emptied graph, or an ambiguous root guess when `metadata.project_name`
/// is absent) lives in [`dbtl::PackageScope::apply_with_warnings`], so the
/// TUI's `reload` re-running the same scope reports the same outcomes. This
/// wrapper prints them to stderr exactly as before AND returns them (the
/// `(scope)` tag mirroring the stderr prefix) so `resolve_source` can hand
/// them to the notification log; the one CLI-side warning left is the
/// `--include-packages`-in-source-mode no-op, which is about the flag, not
/// the graph.
fn apply_package_scope(
    dag: dbtl::Dag,
    scope: dbtl::PackageScope,
    is_source: bool,
) -> (dbtl::Dag, Vec<String>) {
    let mut collected = Vec::new();
    if scope.includes_packages() && is_source {
        // Source mode synthesizes the manifest from the local project
        // only — there are never package nodes to include.
        let w = "--include-packages has no effect in source mode \
                 (installed packages are never parsed)"
            .to_string();
        eprintln!("warning: {w}");
        collected.push(w);
    }
    let (dag, warnings) = scope.apply_with_warnings(dag);
    for w in &warnings {
        eprintln!("warning (scope): {w}");
        collected.push(format!("(scope) {w}"));
    }
    (dag, collected)
}

/// Resolve the CLI into a loaded [`App`]. Explicit `--manifest` / `--source` win;
/// otherwise auto-detect under `--project`: a compiled `target/manifest.json` if
/// present (full fidelity), else parse the project from source. The package
/// scope (`--include-packages`) is applied inside [`resolve_source`] and
/// recorded on the App so `reload` re-scopes the same way.
pub fn build_app(cli: &Cli) -> Result<App> {
    let scope = cli.package_scope();
    let resolved = resolve_source(&cli.manifest, &cli.source, &cli.project, scope)?;
    let ResolvedSource {
        dag,
        label,
        is_source,
        warnings,
    } = resolved;
    let mut app = if is_source {
        App::from_source(dag, PathBuf::from(label))
    } else {
        App::new(dag, PathBuf::from(label))
    };
    app.set_scope(scope);
    // Issue 08: the load/scope warnings that already went to stderr also join
    // the notification log as unread WARNING entries — opening `Ctrl-n`
    // replays what scrolled past before the TUI came up. One stamp per batch
    // (now_hhmmss spawns `date`; per-line stamping would fork per warning).
    app.record_startup_warnings(&warnings, &crate::event_loop::now_hhmmss());
    // An explicit --sql-dialect pins the column-lineage parser dialect;
    // otherwise the resolver auto-selects from the manifest's adapter_type.
    // An adapter_type we don't know lands on the generic dialect — say so
    // as the startup toast (the override suppresses it), at WARNING level so
    // it also counts toward `!N`.
    if let Some(dialect) = cli.sql_dialect {
        app.set_sql_dialect(dialect);
    } else if let Some(a) = dbtl::SqlDialect::unrecognized_adapter(&app.dag) {
        app.set_notice_level(
            dbtl::NoticeLevel::Warning,
            format!("column lineage: unknown adapter '{a}' — using the generic SQL dialect"),
        );
    }
    // An explicit --run-results overrides the auto-detected sibling; its load
    // error IS user-facing (unlike the silent auto-detect).
    if let Some(rr) = &cli.run_results {
        app.set_run_results(PathBuf::from(rr))?;
    }
    Ok(app)
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
#[allow(clippy::too_many_arguments)] // the subcommand's flat flag set
pub fn run_docs(
    out: &str,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    quiet: bool,
    clean: bool,
    check: bool,
    include_packages: bool,
) -> Result<()> {
    let scope = if include_packages {
        dbtl::PackageScope::All
    } else {
        dbtl::PackageScope::Root
    };
    let resolved = resolve_source(manifest, source, project, scope)?;
    let provenance = if resolved.is_source {
        format!("project source ({})", resolved.label)
    } else {
        format!("manifest ({})", resolved.label)
    };
    let project_name = docs_project_name(&resolved.dag, &resolved.label);
    // The README's Lint section + per-node violation lists quote the SAME
    // `run_rules` vector `check --lint` and the TUI read — `.dbtl.yml` is
    // auto-detected beside the source, a malformed file fatal (a committed
    // doc set must never describe the wrong rule set).
    let lint_cfg = load_lint_config(&resolved, &[])?;
    let lint_findings = dbtl::run_rules(&resolved.dag, &lint_cfg);
    let files = dbtl::generate_docs_with_findings(
        &resolved.dag,
        &project_name,
        &provenance,
        scope,
        Some(&lint_findings),
    );

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

/// The `meta` key the affected-owners set reads, from
/// `~/.config/dbtl/config.yml`'s `owner_meta_key` (default `owner`) — the
/// same config the TUI reads, so a `dbtl impact` comment and the `D` modal
/// never disagree on who to page.
fn owner_meta_key() -> String {
    match crate::state::load_config() {
        Ok(c) => c.owner_meta_key.unwrap_or_else(|| "owner".to_string()),
        // Same policy as the TUI's startup: a malformed config warns and
        // falls back to defaults — it never blocks the subcommand.
        Err(e) => {
            eprintln!("warning: ignoring dbtl config.yml: {e}");
            "owner".to_string()
        }
    }
}

/// The CLL resolver for a resolved CURRENT source, built exactly like the
/// app's: the auto-detected `target/catalog.json` (when present) tops the
/// column/type priority chain; `--sql-dialect` pins the parse dialect, else
/// it auto-selects from the manifest's adapter (an unknown adapter lands on
/// generic with a stderr notice — the override suppresses it). Shared by
/// `columns`, `impact`, and `check` so every surface resolves identically.
fn cll_resolver(
    resolved: &ResolvedSource,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> dbtl::SqlParseResolver {
    let catalog_path = if resolved.is_source {
        dbtl::default_catalog_path(None, Some(std::path::Path::new(&resolved.label)))
    } else {
        dbtl::default_catalog_path(Some(std::path::Path::new(&resolved.label)), None)
    };
    let catalog = catalog_path
        .filter(|p| p.is_file())
        .and_then(|p| dbtl::Catalog::load(&p).ok())
        .map(std::rc::Rc::new);
    let dialect = sql_dialect.unwrap_or_else(|| dbtl::SqlDialect::from_dag(&resolved.dag));
    if sql_dialect.is_none() {
        if let Some(a) = dbtl::SqlDialect::unrecognized_adapter(&resolved.dag) {
            eprintln!(
                "warning: column lineage: unknown adapter '{a}' — using the generic SQL dialect"
            );
        }
    }
    dbtl::SqlParseResolver::new(catalog, dialect)
}

/// The baseline side's diff resolver: no catalog (the baseline's warehouse
/// record isn't loaded — its declared + CLL-inferred columns still resolve
/// through the same priority chain); `--sql-dialect` pins BOTH sides'
/// dialects, else [`dbtl::SqlDialect::for_diff_baseline`] shares the current
/// side's dialect — one parse dialect per diff, or normalized expressions
/// can't compare.
fn baseline_resolver(
    current: &dbtl::Dag,
    base: &dbtl::Dag,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> dbtl::SqlParseResolver {
    let dialect = sql_dialect.unwrap_or_else(|| dbtl::SqlDialect::for_diff_baseline(current, base));
    dbtl::SqlParseResolver::new(None, dialect)
}

/// Run the `impact` subcommand: resolve the current source + the `--diff`
/// baseline, compute the shared [`dbtl::diff_report`] payload, and print it as
/// Markdown (the same bytes the TUI's `D`→`e` export writes) or pretty JSON —
/// to stdout, no TTY needed, so CI can post it as a PR comment.
#[allow(clippy::too_many_arguments)]
pub fn run_impact(
    diff_path: &str,
    format: ImpactFormat,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    include_packages: bool,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> Result<()> {
    let scope = if include_packages {
        dbtl::PackageScope::All
    } else {
        dbtl::PackageScope::Root
    };
    let resolved = resolve_source(manifest, source, project, scope)?;
    let (base, label) = load_baseline(diff_path, scope)?;
    // The same resolver pair the TUI diffs through (catalog-backed current,
    // catalog-less baseline) so the report's column analysis matches the
    // `D` modal byte-for-byte.
    let cur_res = cll_resolver(&resolved, sql_dialect);
    let base_res = baseline_resolver(&resolved.dag, &base, sql_dialect);
    let diff = dbtl::compute_diff_with(&base, &resolved.dag, &base_res, &cur_res);
    let report = dbtl::diff_report(
        &resolved.dag,
        Some(&base),
        &diff,
        &label,
        scope,
        &owner_meta_key(),
    );
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

/// One gate check in the `--format json` report: its name, pass/fail, the same
/// detail string the line report carries, and — for lint checks — the gating
/// [`dbtl::Finding`] list automation consumes (new violations only under
/// `--diff`).
#[derive(serde::Serialize)]
struct CheckJsonItem {
    check: String,
    status: &'static str, // "pass" | "fail"
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<dbtl::Finding>,
}

/// The `check --format json` payload: the scope line, the never-gating context
/// lines (`baseline:`, `affected owners:`), then every check in report order.
/// `failures` lets a consumer read the verdict without re-counting statuses.
#[derive(serde::Serialize)]
struct CheckReportJson {
    scope: String,
    info: Vec<String>,
    checks: Vec<CheckJsonItem>,
    failures: usize,
}

/// Run the `check` subcommand: evaluate the CI gates over the resolved source
/// and return `Err` (exit 1 via `main`) when any fails. Coverage below
/// `--coverage-min` (when >0), and — only with `--diff` — newly introduced
/// layer violations, changed buildable nodes without tests, and (with
/// `--no-breaking`) breaking column changes still referenced downstream, all
/// from the single [`dbtl::pr_impact`] / [`dbtl::compute_diff_with`] analyses.
/// `--lint` adds one PASS/FAIL line per static-lint rule (the shared
/// [`dbtl::run_rules`] findings — the SAME vector the TUI lens/worklist and
/// `dbtl docs` quote); under `--diff` the lint gate narrows to findings whose
/// [`dbtl::Finding::key`] is absent from the baseline, so pre-existing
/// violations never fail the gate. Prints one PASS/FAIL line per check to
/// stdout (or the whole report as one JSON object under `--format json`),
/// plus informational lines that never gate (`baseline:` and, when the blast
/// radius has any, `affected owners:`); `--quiet` prints nothing (exit code
/// only).
#[allow(clippy::too_many_arguments)] // the subcommand's flat flag set
pub fn run_check(
    diff_path: &Option<String>,
    coverage_min: u8,
    lint: bool,
    rules: &[String],
    format: ImpactFormat,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    quiet: bool,
    include_packages: bool,
    no_breaking: bool,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> Result<()> {
    let scope = if include_packages {
        dbtl::PackageScope::All
    } else {
        dbtl::PackageScope::Root
    };
    let resolved = resolve_source(manifest, source, project, scope)?;
    let dag = &resolved.dag;
    // `--rules` implies linting (a rule list with nothing to run is a no-op
    // flag, so honour the intent instead).
    let lint_active = lint || !rules.is_empty();

    let mut lines: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    // The JSON mirror of the report, populated alongside each line so both
    // renderings always agree.
    let mut json_info: Vec<String> = Vec::new();
    let mut json_checks: Vec<CheckJsonItem> = Vec::new();

    // The scope line always leads the report: the checked numbers describe
    // the ROOT project under either scope (installed-package nodes, when
    // loaded via --include-packages, are dependency context only), and the
    // line says so verbatim so a pasted CI log can never be misread.
    let scope_desc = scope.describe().to_string();
    lines.push(format!("scope: {scope_desc}"));

    // Coverage — always measured (the shared `coverage_counts` base, the same
    // numbers the TUI's `cov` segment quotes); the gate only engages above 0.
    let (tested, total) = dbtl::coverage_counts(dag);
    // No buildable nodes → nothing untested → 100%.
    let pct = (tested * 100).checked_div(total).unwrap_or(100);
    if coverage_min == 0 || pct >= usize::from(coverage_min) {
        let msg = format!("{tested}/{total} ({pct}%)");
        lines.push(format!("PASS coverage: {msg}"));
        json_checks.push(CheckJsonItem {
            check: "coverage".to_string(),
            status: "pass",
            message: msg,
            findings: Vec::new(),
        });
    } else {
        let msg =
            format!("coverage {pct}% below --coverage-min {coverage_min}% ({tested}/{total})");
        lines.push(format!("FAIL {msg}"));
        failures.push(msg.clone());
        json_checks.push(CheckJsonItem {
            check: "coverage".to_string(),
            status: "fail",
            message: msg,
            findings: Vec::new(),
        });
    }

    // The baseline loads once and feeds BOTH diff gates (layer violations +
    // untested changes) and the lint new-violation comparison.
    let base_ctx = match diff_path {
        Some(d) => Some(load_baseline(d, scope)?),
        None => None,
    };
    if let Some((base, label)) = &base_ctx {
        // Resolver-aware diff — the same pair `run_impact` and the TUI's `D`
        // modal use — so `--no-breaking` gates on the identical column
        // analysis the reviewer reads.
        let cur_res = cll_resolver(&resolved, sql_dialect);
        let base_res = baseline_resolver(dag, base, sql_dialect);
        let diff = dbtl::compute_diff_with(base, dag, &base_res, &cur_res);
        let pr = dbtl::pr_impact(dag, Some(base), &diff, &owner_meta_key());
        lines.push(format!("baseline: {label}"));
        json_info.push(format!("baseline: {label}"));
        // Informational listing (never a gate): who to page for the blast
        // radius — the same `affected_owners` set the impact report lists.
        if !pr.affected_owners.is_empty() {
            let info = format!("affected owners: {}", pr.affected_owners.join(", "));
            lines.push(info.clone());
            json_info.push(info);
        }
        if pr.new_layer_violations.is_empty() {
            let msg = "none".to_string();
            lines.push(format!("PASS new layer violations: {msg}"));
            json_checks.push(CheckJsonItem {
                check: "new_layer_violations".to_string(),
                status: "pass",
                message: msg,
                findings: Vec::new(),
            });
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
            failures.push(msg.clone());
            json_checks.push(CheckJsonItem {
                check: "new_layer_violations".to_string(),
                status: "fail",
                message: msg,
                findings: Vec::new(),
            });
        }
        if pr.untested_changes.is_empty() {
            let msg = "none".to_string();
            lines.push(format!("PASS untested changes: {msg}"));
            json_checks.push(CheckJsonItem {
                check: "untested_changes".to_string(),
                status: "pass",
                message: msg,
                findings: Vec::new(),
            });
        } else {
            let msg = format!(
                "{} changed node(s) without tests: {}",
                pr.untested_changes.len(),
                pr.untested_changes.join(", ")
            );
            lines.push(format!("FAIL {msg}"));
            failures.push(msg.clone());
            json_checks.push(CheckJsonItem {
                check: "untested_changes".to_string(),
                status: "fail",
                message: msg,
                findings: Vec::new(),
            });
        }
        // `--no-breaking`: fail only on BREAKING column changes whose
        // downstream still references the old column (removed/renamed with
        // unmigrated consumers). Partial moves (type/expr) report in the
        // impact pack, never here. Renames print `old -> new` so the log
        // shows the exact migration to ask for.
        if no_breaking {
            let breaking = diff.breaking_referenced();
            if breaking.is_empty() {
                let msg = "none".to_string();
                lines.push(format!("PASS breaking column changes: {msg}"));
                json_checks.push(CheckJsonItem {
                    check: "breaking_columns".to_string(),
                    status: "pass",
                    message: msg,
                    findings: Vec::new(),
                });
            } else {
                let names = breaking
                    .iter()
                    .map(|ch| match &ch.renamed_from {
                        Some(old) => format!("{}.{} -> {}", ch.node_name, old, ch.column),
                        None => format!("{}.{}", ch.node_name, ch.column),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = format!(
                    "{} breaking column change(s) referenced downstream: {names}",
                    breaking.len()
                );
                lines.push(format!("FAIL {msg}"));
                failures.push(msg.clone());
                json_checks.push(CheckJsonItem {
                    check: "breaking_columns".to_string(),
                    status: "fail",
                    message: msg,
                    findings: Vec::new(),
                });
            }
        }
    }
    // `--no-breaking` is a diff gate — without a baseline there is nothing
    // to classify. A silent no-op gate is the worst CI pattern, so say so.
    // The note goes to `info`, not `checks`: no check ran, and a
    // skipped-gate entry would look like a result to a CI parser.
    if no_breaking && base_ctx.is_none() {
        let note = "note: --no-breaking needs --diff; skipped".to_string();
        lines.push(note.clone());
        json_info.push(note);
    }

    // Static lint — the same `run_rules` findings the TUI's Lint lens/stats
    // worklist and `dbtl docs` quote. The project-local `.dbtl.yml` supplies
    // enables/thresholds/prefixes; a malformed file is FATAL here (a CI gate
    // must never silently lint the wrong rule set — the TUI's warn+fallback
    // policy is deliberately different). `--rules` overrides the file's
    // enables: it selects exactly which rules run, the file still supplies
    // the thresholds/prefixes.
    if lint_active {
        let cfg = load_lint_config(&resolved, rules)?;
        let findings = dbtl::run_rules(dag, &cfg);
        // Under --diff the gate narrows to NEW violations: a finding whose
        // `key()` already exists in the baseline is pre-existing noise — same
        // "new only" idea as the layer-violation gate.
        let base_keys: std::collections::BTreeSet<(dbtl::LintRule, String, Vec<String>)> =
            match &base_ctx {
                Some((base, _)) => dbtl::run_rules(base, &cfg)
                    .iter()
                    .map(|f| {
                        let (r, n, rel) = f.key();
                        (r, n.to_string(), rel.to_vec())
                    })
                    .collect(),
                None => std::collections::BTreeSet::new(),
            };
        let gating: Vec<&dbtl::Finding> = findings
            .iter()
            .filter(|f| {
                base_ctx.is_none() || {
                    let (r, n, rel) = f.key();
                    !base_keys.contains(&(r, n.to_string(), rel.to_vec()))
                }
            })
            .collect();
        let name = |uid: &str| -> String {
            dag.get(uid)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| uid.to_string())
        };
        for rule in dbtl::LintRule::ALL {
            if !cfg.enabled(rule) {
                continue;
            }
            let rule_findings: Vec<&dbtl::Finding> =
                gating.iter().copied().filter(|f| f.rule == rule).collect();
            let total = findings.iter().filter(|f| f.rule == rule).count();
            let check_name = format!("lint:{}", rule.name());
            if rule_findings.is_empty() {
                // Under --diff a rule whose violations are all pre-existing
                // still passes — the message says so explicitly.
                let msg = if base_ctx.is_some() && total > 0 {
                    format!("no new violations ({total} pre-existing)")
                } else {
                    "none".to_string()
                };
                lines.push(format!("PASS lint {}: {msg}", rule.name()));
                json_checks.push(CheckJsonItem {
                    check: check_name,
                    status: "pass",
                    message: msg,
                    findings: Vec::new(),
                });
            } else {
                let names = rule_findings
                    .iter()
                    .map(|f| name(&f.node_uid))
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = if base_ctx.is_some() {
                    format!(
                        "lint {}: {} new violation(s): {}",
                        rule.name(),
                        rule_findings.len(),
                        names
                    )
                } else {
                    format!(
                        "lint {}: {} violation(s): {}",
                        rule.name(),
                        rule_findings.len(),
                        names
                    )
                };
                lines.push(format!("FAIL {msg}"));
                failures.push(msg);
                json_checks.push(CheckJsonItem {
                    check: check_name,
                    status: "fail",
                    message: format!("{} violation(s)", rule_findings.len()),
                    findings: rule_findings.into_iter().cloned().collect(),
                });
            }
        }
    }

    if !quiet {
        match format {
            ImpactFormat::Markdown => {
                for line in &lines {
                    println!("{line}");
                }
            }
            ImpactFormat::Json => {
                let report = CheckReportJson {
                    scope: scope_desc,
                    info: json_info,
                    checks: json_checks,
                    failures: failures.len(),
                };
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} check(s) failed", failures.len())
    }
}

/// The lint config for `check --lint`: `.dbtl.yml` beside the resolved data
/// source (same auto-detect as the TUI/docs), a malformed file fatal, and
/// `--rules` overlaid as the authoritative rule selector (exactly the named
/// rules run — the file still supplies thresholds and naming prefixes).
fn load_lint_config(resolved: &ResolvedSource, rules: &[String]) -> Result<dbtl::LintConfig> {
    let (manifest_arg, project_arg) = if resolved.is_source {
        (None, Some(std::path::Path::new(&resolved.label)))
    } else {
        (Some(std::path::Path::new(&resolved.label)), None)
    };
    let path = dbtl::lint_config_path(manifest_arg, project_arg);
    let mut cfg = match &path {
        Some(p) => dbtl::LintConfig::load(p)
            .with_context(|| format!("check --lint: failed to read {}", p.display()))?,
        None => dbtl::LintConfig::default(),
    };
    if !rules.is_empty() {
        for name in rules {
            anyhow::ensure!(
                dbtl::LintRule::from_name(name).is_some(),
                "unknown lint rule '{name}' (valid rules: {})",
                dbtl::LintRule::ALL
                    .iter()
                    .map(|r| r.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for r in dbtl::LintRule::ALL {
            cfg.rules
                .insert(r.name().to_string(), rules.iter().any(|n| n == r.name()));
        }
    }
    Ok(cfg)
}

/// Run the `columns` subcommand: resolve the data source (shared with the
/// TUI), split `<model>.<column>`, and print the column's upstream or
/// downstream [`dbtl::ColumnReport`] — Markdown by default (the SAME bytes
/// the `yl` yank writes), `--format json` for automation. Everything is
/// `Err`-shaped like the other subcommands: a bad model or column name
/// reports as plain text with a non-zero exit, never a panic or a TTY probe.
#[allow(clippy::too_many_arguments)]
pub fn run_columns(
    target: &str,
    upstream: bool,
    format: ImpactFormat,
    manifest: &Option<String>,
    source: &Option<String>,
    project: &str,
    include_packages: bool,
    sql_dialect: Option<dbtl::SqlDialect>,
) -> Result<()> {
    let (model, column) = target
        .rsplit_once('.')
        .with_context(|| format!("columns: expected <model>.<column>, got '{target}'"))?;
    anyhow::ensure!(
        !model.is_empty() && !column.is_empty(),
        "columns: expected <model>.<column>, got '{target}'"
    );
    let scope = if include_packages {
        dbtl::PackageScope::All
    } else {
        dbtl::PackageScope::Root
    };
    let resolved = resolve_source(manifest, source, project, scope)?;
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
    // `target/catalog.json` (when present) is the top column-list tier. The
    // parser-backed resolver resolves per model and falls back to the
    // name-match approximation whenever that model's SQL is missing or
    // unparseable — the count is reported on stderr below.
    let resolver = cll_resolver(&resolved, sql_dialect);

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
    // Parser coverage transparency: how many models ended up on the Issue 07
    // name-match approximation (missing/unparseable SQL). Status goes to
    // stderr so stdout stays machine-clean for both formats.
    let fallbacks = resolver.fallback_count();
    if fallbacks > 0 {
        eprintln!(
            "columns: {fallbacks} model{} resolved by name-match fallback (missing/unparseable SQL)",
            if fallbacks == 1 { "" } else { "s" }
        );
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
/// source parse) — so "diff against that other worktree" just works. The same
/// package `scope` as the current graph is applied, so both sides of the diff
/// agree on the node set (a baseline manifest carrying installed packages is
/// pruned the same way).
pub fn load_baseline(path: &str, scope: dbtl::PackageScope) -> Result<(dbtl::Dag, String)> {
    let p = PathBuf::from(path);
    if p.is_file() {
        let (dag, warnings) =
            load_dag_with_warnings(&p).with_context(|| format!("--diff: failed to load {path}"))?;
        for w in &warnings {
            eprintln!("warning (--diff baseline): {w}");
        }
        return Ok((apply_package_scope(dag, scope, false).0, path.to_string()));
    }
    if p.is_dir() {
        let manifest = p.join("target").join("manifest.json");
        if manifest.is_file() {
            let (dag, warnings) = load_dag_with_warnings(&manifest)
                .with_context(|| format!("--diff: failed to load {}", manifest.display()))?;
            for w in &warnings {
                eprintln!("warning (--diff baseline): {w}");
            }
            return Ok((
                apply_package_scope(dag, scope, false).0,
                manifest.display().to_string(),
            ));
        }
        if p.join("dbt_project.yml").is_file() {
            let (dag, warnings) = load_dag_from_source_with_warnings(&p)
                .with_context(|| format!("--diff: failed to parse project {path}"))?;
            for w in &warnings {
                eprintln!("warning (--diff baseline): {w}");
            }
            return Ok((apply_package_scope(dag, scope, true).0, path.to_string()));
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
                ..Default::default()
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
    fn root_scope_prunes_foreign_package_nodes() {
        // A deps-only project: the recorded root `zzz_proj` contributes NO
        // nodes — every kept node belongs to an installed package. The strict
        // `Root` scope keeps none of them (it warns on stderr instead of
        // re-scoping to a foreign package, which would show the exact opposite
        // of the scope's intent).
        let dag = dag_with(Some("zzz_proj"), &["model.aaa_pkg.a", "model.aaa_pkg.b"]);
        let (scoped, _) = apply_package_scope(dag, dbtl::PackageScope::Root, false);
        assert!(
            scoped.is_empty(),
            "the scope stays strict — no foreign nodes"
        );
    }

    #[test]
    fn root_scope_keeps_root_nodes() {
        // The normal case: the root project's nodes survive, foreign ones are
        // dropped, no warning.
        let dag = dag_with(
            Some("zzz_proj"),
            &["model.zzz_proj.root_model", "model.aaa_pkg.a"],
        );
        let (scoped, _) = apply_package_scope(dag, dbtl::PackageScope::Root, false);
        assert_eq!(scoped.len(), 1, "only the root project's model survives");
        assert!(scoped.contains("model.zzz_proj.root_model"));
    }

    #[test]
    fn all_scope_keeps_every_package_node() {
        // `--include-packages`: the manifest's full contents stay — installed
        // package nodes alongside the root project's own.
        let dag = dag_with(
            Some("zzz_proj"),
            &["model.zzz_proj.root_model", "model.aaa_pkg.a"],
        );
        let (scoped, _) = apply_package_scope(dag, dbtl::PackageScope::All, false);
        assert_eq!(scoped.len(), 2, "package nodes stay in scope");
        assert!(scoped.contains("model.aaa_pkg.a"));
    }

    /// Issue 08: the load warnings the fusion fixture produces (unsupported
    /// resource types) already printed to stderr must ALSO land in the
    /// App's notification log as unread WARNING rows — `Ctrl-n` replays them
    /// after the TUI opens, and `!N` counts them.
    #[test]
    fn build_app_routes_startup_warnings_into_the_notification_log() {
        let fusion = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fusion_manifest.json"
        );
        let cli = Cli::parse_from(["dbtl", "--manifest", fusion]);
        let app = build_app(&cli).expect("build_app over the fusion fixture");
        assert!(
            app.unread_warnings() > 0,
            "startup load warnings count toward !N"
        );
        let view = app.notifications_snapshot();
        assert_eq!(
            view.rows.len(),
            app.unread_warnings(),
            "every recorded row is an unread warning here"
        );
        assert!(
            view.rows
                .iter()
                .all(|r| r.level == dbtl::NoticeLevel::Warning
                    && r.stamp.len() == 8
                    && r.stamp.contains(':')),
            "each row is a stamped warning: {:?}",
            view.rows
        );
    }
}
