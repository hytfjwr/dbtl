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
    /// `global` so it works before OR after a subcommand.
    #[arg(long, global = true, conflicts_with = "source")]
    pub manifest: Option<String>,
    /// Path to a dbt project dir, parsed from source with no compile (forces source mode).
    #[arg(long, global = true, conflicts_with = "manifest")]
    pub source: Option<String>,
    /// dbt project dir for auto-detect: prefer <dir>/target/manifest.json, else parse source.
    #[arg(long, global = true, default_value = ".")]
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
    /// status diff chip; `impact` requires it and `check` uses it for its
    /// diff gates. `global` so it works before OR after a subcommand.
    #[arg(long, global = true)]
    pub diff: Option<String>,
    /// Path to a dbt run_results.json (the last run's per-node status +
    /// execution_time). Adds the RunStatus/RunTime lenses, a `run n/m @date`
    /// status chip, and the runtime-weighted critical path in the `S` stats
    /// modal. Default: auto-detect target/run_results.json next to the data
    /// source — a missing file just means no run data, never an error.
    /// TUI-only: `global` so it parses next to a subcommand too, where the
    /// ignored-flag check rejects it.
    #[arg(long, global = true)]
    pub run_results: Option<String>,
    /// Path to a dbt catalog.json (the `dbt docs generate` side-map: real
    /// column types, row counts, byte sizes — and the top column-list tier
    /// for column-level lineage). Default: auto-detect
    /// target/catalog.json next to the data source — a missing file just
    /// means no catalog data, never an error. `global` so it works before
    /// OR after a subcommand.
    #[arg(long, global = true)]
    pub catalog: Option<String>,
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
        /// Output format: `markdown` (default) or `json`.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ImpactFormat,
    },
    /// Run CI checks over the resolved data source and exit 1 when any fails:
    /// test coverage below `--coverage-min`, newly introduced layer violations,
    /// or changed buildable nodes without tests (the last two need `--diff`).
    /// The report is plain lines on stdout, one per check.
    Check {
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

mod baseline;
mod columns;
mod docs;
mod exit;
mod flags;
mod gates;
mod resolve;
mod resolvers;
#[cfg(test)]
mod tests;

pub use baseline::load_baseline;
pub use columns::run_columns;
pub use docs::run_docs;
pub use exit::{exit_code, Classify, CliError};
pub use flags::check_flag_usage;
pub use gates::{run_check, run_impact};
use resolve::*;
pub use resolve::{build_app, DataSourceArgs, ResolvedSource};
use resolvers::*;
