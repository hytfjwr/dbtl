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

/// The `--gate` value parser: `<gate-id>=<error|warn|off>` — a severity
/// override for one `check` gate, validated eagerly so an unknown gate id or
/// severity is a usage error before any loading (the same accepted ids
/// `.dbtl.yml`'s `check.gates` takes).
fn parse_gate_override(
    s: &str,
) -> std::result::Result<(dbtl::CheckGate, Option<dbtl::Severity>), String> {
    let (id, severity) = s
        .split_once('=')
        .ok_or_else(|| format!("expected GATE=SEVERITY, got '{s}'"))?;
    let gate = dbtl::CheckGate::from_name(id).ok_or_else(|| {
        format!(
            "unknown gate '{id}' (valid gates: {})",
            dbtl::CheckGate::valid_ids()
        )
    })?;
    let severity = match severity {
        "error" => Some(dbtl::Severity::Error),
        "warn" => Some(dbtl::Severity::Warn),
        "off" => None,
        _ => return Err(format!("unknown severity '{severity}' (error|warn|off)")),
    };
    Ok((gate, severity))
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
    /// Start the TUI on a model's tab: a model name (e.g. `stg_orders`), or
    /// a dbt selector spec — its first match opens in a tab, and it
    /// must be a model. `state:` atoms can't match: the `--diff` baseline
    /// loads after this flag resolves.
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
    /// Baseline to diff against: a manifest.json file, a dbt project dir
    /// (its target/manifest.json if compiled, else parsed from source), or
    /// `git:<rev>` — the project's committed files at that git revision,
    /// parsed from source (the current side then parses source too, so
    /// loader differences never read as changes). Adds the Diff lens
    /// (added/modified tints), the `D` summary modal, and a status diff
    /// chip; `impact` requires it and `check` uses it for its diff gates.
    /// `global` so it works before OR after a subcommand.
    #[arg(long, global = true)]
    pub diff: Option<String>,
    /// Path to a dbt run_results.json (the last run's per-node status +
    /// execution_time). Adds the RunStatus/RunTime lenses, a `run n/m @date`
    /// status chip, and the runtime-weighted critical path in the `S` stats
    /// modal. Default: auto-detect target/run_results.json next to the data
    /// source — a missing file just means no run data, never an error.
    /// `docs` also accepts it (the `last run` page bullet — an explicit
    /// path there is REQUIRED); every other subcommand rejects it.
    /// `global` so it parses before OR after a subcommand.
    #[arg(long, global = true)]
    pub run_results: Option<String>,
    /// Path to a dbt catalog.json (the `dbt docs generate` side-map: real
    /// column types, row counts, byte sizes — and the top column-list tier
    /// for column-level lineage). Default: auto-detect
    /// target/catalog.json next to the data source — a missing file just
    /// means no catalog data, never an error. `docs` accepts it too — as an
    /// explicit REQUIRED path: docs never auto-detects it (the `.dbtl.yml`
    /// `docs.catalog` opt-in reads the default sibling instead). `global` so
    /// it works before OR after a subcommand.
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
    /// and `dbtl docs` accept it too.
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
    /// Generate static Markdown docs for the project and exit,
    /// without launching the TUI. Writes one Markdown page per node plus an
    /// index README.md under <DIR>. The data source is resolved the same way
    /// the TUI resolves it (--manifest / --source / --project / auto-detect).
    Docs {
        /// Output directory for the generated docs. Created if missing; existing
        /// node pages / README.md are overwritten (no files are deleted).
        #[arg(long)]
        out: String,
        /// Suppress the success summary on stdout — only that one line.
        /// Errors still go to stderr, and a failing --check still prints
        /// its per-file difference list (the actionable output). For CI
        /// runs where only the exit code matters.
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
        /// Per-node lineage-diagram depth: a hop count (`2`, `1`, …) or `all`
        /// for the node's whole closure. Overrides `.dbtl.yml`'s
        /// `docs.lineage_depth`; the shipped default is `2`.
        #[arg(long, value_name = "N|all")]
        lineage_depth: Option<String>,
        /// Generate pages only for nodes matching a dbt selector — the
        /// grammar of `dbtl ls`/`check --select` (`tag:x`, `path:y`,
        /// `+name+`, `state:modified`, …), NOT the TUI's `--select`, which
        /// opens one model's tab. `state:` atoms need `--diff`; `--diff`
        /// on docs exists ONLY for them. Repeatable — specs union. The
        /// README notes the selector and project-wide summary counts stay
        /// unchanged; links to omitted pages render as plain names.
        #[arg(long, value_name = "SELECTOR", conflicts_with_all = ["clean", "check"])]
        select: Vec<String>,
        /// Subtract nodes matching a spec from the generated set — applied
        /// after `--select` (with none, the whole graph minus exclusions
        /// generates). Same selector grammar; repeatable.
        #[arg(long, value_name = "SELECTOR", conflicts_with_all = ["clean", "check"])]
        exclude: Vec<String>,
    },
    /// Print the PR impact pack for a `--diff` baseline comparison to stdout
    /// and exit — Markdown by default (paste into a PR description), `--format
    /// json` for automation. The same `dbtl_core::diff_report` payload the TUI's
    /// `D` modal snapshots, so a CI comment matches the screen byte-for-byte.
    Impact {
        /// Output format: `markdown` (default) or `json`.
        #[arg(long, value_enum, default_value = "markdown")]
        format: ImpactFormat,
        /// Also write the report to a file: `--report <FMT>:<PATH>`
        /// (repeatable, `FMT` limited to `markdown` / `json`). Contents
        /// match the bytes that format prints on stdout; parent directories
        /// are created and existing files overwritten.
        #[arg(long, value_name = "FMT:PATH")]
        report: Vec<String>,
        /// Restrict the report to changed nodes matching a dbt selector —
        /// the grammar of `dbtl ls` (`tag:x`, `path:y`, `+name+`,
        /// `state:modified`, …), NOT the TUI's `--select`, which opens
        /// one model's tab. `state:` atoms compare against the `--diff`
        /// baseline. Repeatable — specs union.
        #[arg(long, value_name = "SELECTOR")]
        select: Vec<String>,
    },
    /// Run CI checks over the resolved data source and exit 1 when any fails.
    /// The gate set comes from `.dbtl.yml`'s `check:` section plus the flags:
    /// test coverage below `--coverage-min`, and — only with `--diff` —
    /// newly introduced layer violations, changed buildable nodes without
    /// tests, changed hubs, affected exposures, and (with `--no-breaking` or
    /// `--gate breaking_columns=…`) breaking column changes still referenced
    /// downstream. The report is plain lines on stdout, one per gate.
    Check {
        /// Fail when test coverage (model/seed/snapshot nodes with ≥1 test)
        /// falls below this percent. Overrides `.dbtl.yml`'s
        /// `check.gates.coverage.min`; when that gate is `off`, setting this
        /// enables it at error severity.
        #[arg(long, value_name = "PCT", value_parser = clap::value_parser!(u8))]
        coverage_min: Option<u8>,
        /// Override one check gate's severity: `--gate coverage=warn`,
        /// `--gate breaking_columns=error`, `--gate changed_hubs=off`
        /// (repeatable). Overrides `.dbtl.yml`'s `check.gates.<id>.severity`.
        #[arg(long = "gate", value_name = "ID=error|warn|off", value_parser = parse_gate_override)]
        gate: Vec<(dbtl::CheckGate, Option<dbtl::Severity>)>,
        /// Fail when declared columns covered by a schema test fall below
        /// this percent. Overrides `.dbtl.yml`'s
        /// `check.gates.column_coverage.min`; when that gate is `off`,
        /// setting this enables it at error severity.
        #[arg(long, value_name = "PCT", value_parser = clap::value_parser!(u8))]
        column_coverage_min: Option<u8>,
        /// Fail when models carrying a description fall below
        /// this percent. Overrides `.dbtl.yml`'s `check.gates.doc_coverage.min`;
        /// when that gate is `off`, setting this enables it at error
        /// severity.
        #[arg(long, value_name = "PCT", value_parser = clap::value_parser!(u8))]
        doc_coverage_min: Option<u8>,
        /// Fail when the changed set's blast radius exceeds this many
        /// affected models. Overrides `.dbtl.yml`'s
        /// `check.gates.blast_radius.max`; when that gate is `off`, setting
        /// this enables it at error severity. Needs `--diff`.
        #[arg(long, value_name = "N")]
        max_blast_radius: Option<usize>,
        /// Run the static structure-lint rules (best-practice checks over
        /// the project graph — no warehouse or `dbt compile` needed). Rule
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
        /// never trip this gate. Without `--diff` the gate reports `skip`
        /// (asked for but not evaluable) — visible, never counted.
        #[arg(long)]
        no_breaking: bool,
        /// Restrict linting to these rules (comma-separated rule names;
        /// implies --lint). Overrides `.dbtl.yml`'s per-rule enables — the
        /// flag is the authoritative rule selector; the file still supplies
        /// thresholds and naming prefixes.
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        rules: Vec<String>,
        /// Minimum finding severity that fails a check: `error` (default)
        /// fails on errors only, `warn` also fails on warnings, `never`
        /// only reports (a tripped gate prints WARN and never counts).
        /// Overrides `.dbtl.yml`'s top-level `fail_on`; each gate's
        /// severity comes from `--gate`, its `check.gates` entry, or the
        /// shipped default.
        #[arg(long, value_enum)]
        fail_on: Option<FailOnArg>,
        /// Report format on stdout: `text` (the annotated report, default),
        /// `concise` (one finding per line — editor/grep-friendly),
        /// `markdown` (a document suited to a PR comment), `json` (the
        /// `dbtl.report/v1` payload), `github` (Actions workflow
        /// annotations), `sarif` (code-scanning upload), `junit` (test
        /// report XML), or `codeclimate` (Code Quality issues).
        #[arg(long, value_enum, default_value = "text")]
        format: ReportFormat,
        /// When the stdout `text`/`concise` report uses ANSI color: `auto`
        /// (default) colors only when the terminal and environment allow,
        /// `always` colors even when stdout is piped, `never` disables it.
        /// Every other format, every `--report` file, and the step summary
        /// are always unstyled.
        #[arg(long, value_enum, default_value = "auto")]
        color: ColorArg,
        /// Also write the report to a file: `--report <FMT>:<PATH>`
        /// (repeatable; `FMT` is any `--format` value). Contents match the
        /// bytes that format prints on stdout apart from color — files are
        /// always unstyled; parent directories are created and existing
        /// files overwritten.
        #[arg(long, value_name = "FMT:PATH")]
        report: Vec<String>,
        /// Append the markdown report to the file the GITHUB_STEP_SUMMARY
        /// environment variable names (a no-op when the variable is unset
        /// or empty — plain CI runs only).
        #[arg(long)]
        step_summary: bool,
        /// Prefix prepended to the file paths the `github`, `sarif`, and
        /// `codeclimate` formats emit — for a project living in a
        /// repository subdirectory. A usage error when none of those
        /// formats is selected via `--format` or `--report`.
        #[arg(long, value_name = "PREFIX")]
        path_prefix: Option<String>,
        /// Gate on generated-docs freshness: compare this directory against
        /// what `dbtl docs` would generate right now — the exact comparison
        /// `dbtl docs --check` runs — and fail the run when they differ.
        #[arg(long, value_name = "DIR")]
        docs_dir: Option<String>,
        /// Restrict every gate to nodes matching a dbt selector — coverage
        /// counts, lint findings, and the `--diff` gates all intersect the
        /// selection (`docs_drift` never does; it compares whole doc sets).
        /// The grammar of `dbtl ls` (`tag:x`, `path:y`, `+name+`,
        /// `state:modified`, …), NOT the TUI's `--select`, which opens one
        /// model's tab; `state:` atoms need `--diff`. Repeatable — specs
        /// union.
        #[arg(long, value_name = "SELECTOR")]
        select: Vec<String>,
        /// Suppress the per-check report on stdout; only the exit code matters.
        #[arg(long)]
        quiet: bool,
    },
    /// Run the static structure-lint rules over the resolved data source and
    /// exit — the same `run_rules` findings `check --lint` gates on and the
    /// TUI's Lint lens quotes. `.dbtl.yml` supplies rule enables, severities,
    /// thresholds and naming prefixes; `--rules` overrides the selection;
    /// the baseline file freezes known violations; `--diff` narrows the gate
    /// to violations the baseline doesn't already carry.
    Lint {
        /// Restrict linting to these rules (comma-separated rule names).
        /// The flag is the authoritative rule selector — the file still
        /// supplies thresholds and naming prefixes.
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        rules: Vec<String>,
        /// Minimum finding severity that fails the run: `error` (default)
        /// fails on errors only, `warn` also fails on warnings, `never`
        /// only reports. Overrides `.dbtl.yml`'s top-level `fail_on`.
        #[arg(long, value_enum)]
        fail_on: Option<FailOnArg>,
        /// Report format on stdout: `text` (the annotated report, default),
        /// `concise` (one finding per line — editor/grep-friendly),
        /// `markdown` (a document suited to a PR comment), `json` (the
        /// `dbtl.report/v1` payload), `github` (Actions workflow
        /// annotations), `sarif` (code-scanning upload), `junit` (test
        /// report XML), or `codeclimate` (Code Quality issues).
        #[arg(long, value_enum, default_value = "text")]
        format: ReportFormat,
        /// When the stdout `text`/`concise` report uses ANSI color: `auto`
        /// (default) colors only when the terminal and environment allow,
        /// `always` colors even when stdout is piped, `never` disables it.
        /// Every other format, every `--report` file, and the step summary
        /// are always unstyled.
        #[arg(long, value_enum, default_value = "auto")]
        color: ColorArg,
        /// Also write the report to a file: `--report <FMT>:<PATH>`
        /// (repeatable; `FMT` is any `--format` value). Contents match the
        /// bytes that format prints on stdout apart from color — files are
        /// always unstyled; parent directories are created and existing
        /// files overwritten.
        #[arg(long, value_name = "FMT:PATH")]
        report: Vec<String>,
        /// Append the markdown report to the file the GITHUB_STEP_SUMMARY
        /// environment variable names (a no-op when the variable is unset
        /// or empty — plain CI runs only).
        #[arg(long)]
        step_summary: bool,
        /// Prefix prepended to the file paths the `github`, `sarif`, and
        /// `codeclimate` formats emit — for a project living in a
        /// repository subdirectory. A usage error when none of those
        /// formats is selected via `--format` or `--report`.
        #[arg(long, value_name = "PREFIX")]
        path_prefix: Option<String>,
        /// Freeze every current finding into a lint baseline file and exit
        /// (PATH defaults to `lint.baseline`, else `.dbtl-baseline.json` at
        /// the project root). Writes always exit 0 and cannot combine with
        /// `--diff`.
        #[arg(long, num_args = 0..=1, value_name = "PATH",
               conflicts_with_all = ["list_rules", "explain", "fail_on", "format", "color", "no_baseline", "report", "step_summary", "path_prefix"])]
        write_baseline: Option<Option<String>>,
        /// Do not read the lint baseline file — every finding counts again.
        #[arg(long)]
        no_baseline: bool,
        /// List every lint rule — id, category, effective severity, summary —
        /// and exit. Reads `.dbtl.yml` when found; never loads a data source.
        #[arg(long,
               conflicts_with_all = ["explain", "write_baseline", "quiet", "format", "color", "fail_on", "no_baseline", "report", "step_summary", "path_prefix"])]
        list_rules: bool,
        /// Show one rule's full registry entry (rationale, example, params,
        /// fix guidance, docs link) and exit — no data load.
        #[arg(long, value_name = "ID",
               conflicts_with_all = ["list_rules", "write_baseline", "quiet", "format", "color", "fail_on", "no_baseline", "report", "step_summary", "path_prefix"])]
        explain: Option<String>,
        /// Restrict findings to nodes matching a dbt selector — the grammar
        /// of `dbtl ls` (`tag:x`, `path:y`, `+name+`, `state:modified`, …),
        /// NOT the TUI's `--select`, which opens one model's tab.
        /// `state:` atoms need `--diff`. Repeatable — specs union. Cannot
        /// combine with `--write-baseline` (a partial finding set must
        /// never overwrite the frozen file).
        #[arg(long, value_name = "SELECTOR",
               conflicts_with_all = ["write_baseline", "list_rules", "explain"])]
        select: Vec<String>,
        /// Suppress the report on stdout; only the exit code matters.
        #[arg(long)]
        quiet: bool,
    },
    /// List the nodes a dbt selector matches, one per line — the
    /// non-interactive answer to "what does this spec select?" (`dbtl ls
    /// 'tag:nightly'`, `dbtl ls '2+fct_orders' --exclude path:staging`).
    /// `state:` selectors need `--diff`; note they are resolved by dbtl's
    /// own comparison (the SQL, config, column, and macro changes dbtl can
    /// read) — the set may not match dbt's `state:modified`.
    Ls {
        /// The selector spec(s): whitespace-separated terms union,
        /// comma-separated factors intersect, `+`/numeric hops expand, and
        /// atoms are `tag:`, `path:`, `config.materialized:`, `exposure:`,
        /// `state:modified|new` (needs `--diff`), `meta:key[=value]`, bare
        /// names and globs.
        #[arg(required = true, value_name = "SELECTOR")]
        selector: Vec<String>,
        /// Subtract the nodes a spec matches — applied after every
        /// inclusion term. Repeatable — each occurrence takes exactly one
        /// spec so a following positional can never be swallowed.
        #[arg(long, value_name = "SELECTOR")]
        exclude: Vec<String>,
        /// Output shape: `names` (node names, one per line), `uids`
        /// (unique_ids), `selector` (one space-separated line of buildable
        /// model names for `dbt build --select`), `json` (one object per
        /// node).
        #[arg(long, value_enum, default_value = "names")]
        output: LsFormat,
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
    /// Print a shell completion script to stdout and exit — reads no data
    /// source and accepts no global flags, so it runs anywhere (the release
    /// packaging generates the shipped `completions/` files with it).
    Completions {
        /// The shell to generate the completion script for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the man page (roff) to stdout and exit. Hidden from `--help`:
    /// it feeds release packaging (`man/dbtl.1`), not interactive
    /// discovery.
    #[command(hide = true)]
    Man,
    /// Print the generated CLI reference (Markdown) to stdout and exit.
    /// Hidden from `--help`: it feeds `docs/reference/cli.md`, not
    /// interactive discovery.
    #[command(hide = true)]
    CliReference,
}

/// The `dbtl ls` output shape: plain listings for humans (`names`/`uids`),
/// a paste-ready `--select` line for dbt (`selector`), and a structured
/// array for automation (`json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LsFormat {
    /// Node names, one per line — sorted by (name, unique_id) so same-name
    /// nodes (a model and a source) both appear, deterministically.
    Names,
    /// `unique_id`s, one per line, sorted.
    Uids,
    /// One space-separated line of deduplicated model/seed/snapshot names —
    /// paste-ready for `dbt build --select` (non-buildable resources drop).
    Selector,
    /// A pretty JSON array — one `{unique_id, name, resource_type,
    /// materialized, path}` object per node in unique_id order; `[]` when
    /// empty so `jq` pipelines still parse.
    Json,
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

/// The `--fail-on` argument: the minimum finding severity that fails a gate —
/// maps onto [`dbtl::FailOn`] (kept separate because `dbtl-core` has no clap
/// dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FailOnArg {
    /// Only error findings fail (the default).
    Error,
    /// Warn findings fail too.
    Warn,
    /// Report-only — findings print but nothing fails.
    Never,
}

impl From<FailOnArg> for dbtl::FailOn {
    fn from(v: FailOnArg) -> Self {
        match v {
            FailOnArg::Error => dbtl::FailOn::Error,
            FailOnArg::Warn => dbtl::FailOn::Warn,
            FailOnArg::Never => dbtl::FailOn::Never,
        }
    }
}

/// The `--color` argument `check`/`lint` accept: when the stdout `text`/
/// `concise` report carries ANSI styling. Resolved to `anstream`'s choice
/// (the detection itself stays with anstream — tty probing, `NO_COLOR`,
/// `CLICOLOR[_FORCE]`, `TERM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorArg {
    /// Color when the terminal/environment allows it (the default).
    Auto,
    /// Always emit ANSI color on stdout, even piped.
    Always,
    /// Never emit ANSI color.
    Never,
}

impl From<ColorArg> for anstream::ColorChoice {
    fn from(v: ColorArg) -> Self {
        match v {
            ColorArg::Auto => anstream::ColorChoice::Auto,
            ColorArg::Always => anstream::ColorChoice::Always,
            ColorArg::Never => anstream::ColorChoice::Never,
        }
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

mod baseline;
mod columns;
mod completions;
mod docs;
mod exit;
mod flags;
mod gates;
mod git_baseline;
mod lint;
mod ls;
mod reference;
mod report;
mod resolve;
mod resolvers;
#[cfg(test)]
mod tests;

pub use baseline::load_baseline;
pub use columns::run_columns;
pub use completions::{run_completions, run_man};
use docs::*;
pub use docs::{run_docs, DocsArgs};
pub use exit::{exit_code, Classify, CliError};
pub use flags::check_flag_usage;
pub use gates::{run_check, run_impact, CheckArgs};
use lint::*;
pub use lint::{run_lint, LintArgs};
pub use ls::run_ls;
pub use reference::run_cli_reference;
use report::*;
use resolve::*;
pub use resolve::{build_app, DataSourceArgs, ResolvedSource};
use resolvers::*;
