<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->

# `dbtl` command-line reference

Browse dbt model lineage from a manifest or project source

Running `dbtl` with no subcommand launches the interactive TUI — the top-level flags below apply to it. Every subcommand resolves its data source the same way (`--manifest` / `--source` / `--project` / auto-detect) and prints the same `error: …` line shape on stderr.

## Global flags

Global flags parse on either side of a subcommand — but a subcommand only honors the ones it consumes; typing another explicitly is a usage error (exit 2). The per-command accepted sets are the same table the ignored-flag check reads.

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `--manifest` | `<MANIFEST>` | — | — | Path to a compiled dbt manifest.json (forces manifest mode). `global` so it works before OR after a subcommand |
| `--source` | `<SOURCE>` | — | — | Path to a dbt project dir, parsed from source with no compile (forces source mode) |
| `--project` | `<PROJECT>` | — | `.` | dbt project dir for auto-detect: prefer <dir>/target/manifest.json, else parse source |
| `--diff` | `<DIFF>` | — | — | Baseline to diff against: a manifest.json file, a dbt project dir (its target/manifest.json if compiled, else parsed from source), or `git:<rev>` — the project's committed files at that git revision, parsed from source (the current side then parses source too, so loader differences never read as changes). Adds the Diff lens (added/modified tints), the `D` summary modal, and a status diff chip; `impact` requires it and `check` uses it for its diff gates. `global` so it works before OR after a subcommand |
| `--run-results` | `<RUN_RESULTS>` | — | — | Path to a dbt run_results.json (the last run's per-node status + execution_time). Adds the RunStatus/RunTime lenses, a `run n/m @date` status chip, and the runtime-weighted critical path in the `S` stats modal. Default: auto-detect target/run_results.json next to the data source — a missing file just means no run data, never an error. `docs` also accepts it (the `last run` page bullet — an explicit path there is REQUIRED); every other subcommand rejects it. `global` so it parses before OR after a subcommand |
| `--catalog` | `<CATALOG>` | — | — | Path to a dbt catalog.json (the `dbt docs generate` side-map: real column types, row counts, byte sizes — and the top column-list tier for column-level lineage). Default: auto-detect target/catalog.json next to the data source — a missing file just means no catalog data, never an error. `docs` accepts it too — as an explicit REQUIRED path: docs never auto-detects it (the `.dbtl.yml` `docs.catalog` opt-in reads the default sibling instead). `global` so it works before OR after a subcommand |
| `--include-packages` | — | — | `false` | Include nodes installed from dbt packages (`dbt deps` / `packages.yml`) in the graph: manifest mode's compiled `nodes` map mixes them in. They appear in the model list, lineage, docs pages, and `--diff` baselines as dependency context — but every aggregate metric (coverage / orphans / stats / docs summary / `dbtl check` / the PR-impact risk flags) still counts the root project only. Source mode never parses `dbt_packages`, so the flag is a no-op there (with a warning). `global` so it works before OR after a subcommand (`dbtl --include-packages check` ≡ `dbtl check --include-packages`) |
| `--sql-dialect` | `<DIALECT>` | — | — | SQL dialect for parser-based column lineage: `auto` reads the manifest's adapter_type (source mode has none → generic); any other value pins the dialect for every model (`generic`, `postgres`, `snowflake`, `bigquery`, `mysql`, `sqlite`, `duckdb`, `spark`, …). `global` so `dbtl columns` and `dbtl docs` accept it too |

## TUI flags

Top-level flags that configure the interactive app. An explicit occurrence next to a subcommand is a usage error (exit 2).

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `-v` | — | — | — | Print version information and exit (lowercase alias of `-V`/`--version`) |
| `--ascii` | — | — | `false` | Force pure-ASCII glyphs (boxes/borders/markers). Default: probe the terminal |
| `--unicode` | — | — | `false` | Force Unicode box-drawing glyphs, skipping the terminal probe |
| `--select` | `<SELECT>` | — | — | Start the TUI on a model's tab: a model name (e.g. `stg_orders`), or a dbt selector spec — the spec's first match opens in a tab, and it must be a model. `state:` atoms can't match: the `--diff` baseline loads after this flag resolves |
| `--watch` | — | — | `false` | Auto-reload when the data source changes on disk (the manifest file, or any .sql/.yml/.csv under the project dir in source mode) |
| `--theme` | `<THEME>` | `DBTL_THEME` | — | Color theme: a preset name (see --list-themes), a theme under ~/.config/dbtl/themes/<name>.yml, or a path to a theme YAML file |
| `--list-themes` | — | — | `false` | List the available color themes (presets + user theme files) and exit |
| `-h` `--help` | — | — | — | Print help |
| `-V` `--version` | — | — | — | Print version |

## Subcommands


### `dbtl docs`

Generate static Markdown docs for the project and exit, without launching the TUI. Writes one Markdown page per node plus an index README.md under <DIR>. The data source is resolved the same way the TUI resolves it (--manifest / --source / --project / auto-detect)

```text
Usage: dbtl docs [OPTIONS] --out <OUT>
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `--out` | `<OUT>` | — | — | Output directory for the generated docs. Created if missing; existing node pages / README.md are overwritten (no files are deleted) |
| `--quiet` | — | — | `false` | Suppress the success summary on stdout — only that one line. Errors still go to stderr, and a failing --check still prints its per-file difference list (the actionable output). For CI runs where only the exit code matters |
| `--clean` | — | — | `false` | After writing, delete stale generated pages: marked dbtl files under <DIR> that this run did not regenerate (e.g. pages of deleted or renamed models). Only files carrying the generated marker are removed — hand-written pages sharing the dir are never touched |
| `--check` | — | — | `false` | Write nothing: compare the generated set against <DIR> on disk and exit 1 if anything differs — a missing page, a changed page, or a stale marked file. Prints one line per difference for the CI log |
| `--lineage-depth` | `<N\|all>` | — | — | Per-node lineage-diagram depth: a hop count (`2`, `1`, …) or `all` for the node's whole closure. Overrides `.dbtl.yml`'s `docs.lineage_depth`; the shipped default is `2` |
| `--select` | `<SELECTOR>` (repeatable) | — | — | Generate pages only for nodes matching a dbt selector — the grammar of `dbtl ls`/`check --select` (`tag:x`, `path:y`, `+name+`, `state:modified`, …), NOT the TUI's `--select`, which opens one model's tab. `state:` atoms need `--diff`; `--diff` on docs exists ONLY for them. Repeatable — specs union. The README notes the selector and project-wide summary counts stay unchanged; links to omitted pages render as plain names |
| `--exclude` | `<SELECTOR>` (repeatable) | — | — | Subtract nodes matching a spec from the generated set — applied after `--select` (with none, the whole graph minus exclusions generates). Same selector grammar; repeatable |
| `-h` `--help` | — | — | — | Print help |

Accepts global flags: `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`, `--run-results`, `--catalog`, `--sql-dialect`.

### `dbtl impact`

Print the PR impact pack for a `--diff` baseline comparison to stdout and exit — Markdown by default (paste into a PR description), `--format json` for automation. The same `dbtl_core::diff_report` payload the TUI's `D` modal snapshots, so a CI comment matches the screen byte-for-byte

```text
Usage: dbtl impact [OPTIONS]
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `--format` | `markdown` \| `json` | — | `markdown` | Output format: `markdown` (default) or `json` |
| `--report` | `<FMT:PATH>` (repeatable) | — | — | Also write the report to a file: `--report <FMT>:<PATH>` (repeatable, `FMT` limited to `markdown` / `json`). Contents match the bytes that format prints on stdout; parent directories are created and existing files overwritten |
| `--select` | `<SELECTOR>` (repeatable) | — | — | Restrict the report to changed nodes matching a dbt selector — the grammar of `dbtl ls` (`tag:x`, `path:y`, `+name+`, `state:modified`, …), NOT the TUI's `--select`, which opens one model's tab. `state:` atoms compare against the `--diff` baseline. Repeatable — specs union |
| `-h` `--help` | — | — | — | Print help (see a summary with '-h') |

Accepts global flags: `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`, `--catalog`, `--sql-dialect`.

### `dbtl check`

Run CI checks over the resolved data source and exit 1 when any fails. The gate set comes from `.dbtl.yml`'s `check:` section plus the flags: test coverage below `--coverage-min`, and — only with `--diff` — newly introduced layer violations, changed buildable nodes without tests, changed hubs, affected exposures, and (with `--no-breaking` or `--gate breaking_columns=…`) breaking column changes still referenced downstream. The report is plain lines on stdout, one per gate

```text
Usage: dbtl check [OPTIONS]
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `--coverage-min` | `<PCT>` | — | — | Fail when test coverage (model/seed/snapshot nodes with ≥1 test) falls below this percent. Overrides `.dbtl.yml`'s `check.gates.coverage.min`; when that gate is `off`, setting this enables it at error severity |
| `--gate` | `<ID=error\|warn\|off>` (repeatable) | — | — | Override one check gate's severity: `--gate coverage=warn`, `--gate breaking_columns=error`, `--gate changed_hubs=off` (repeatable). Overrides `.dbtl.yml`'s `check.gates.<id>.severity` |
| `--column-coverage-min` | `<PCT>` | — | — | Fail when declared columns covered by a schema test fall below this percent. Overrides `.dbtl.yml`'s `check.gates.column_coverage.min`; when that gate is `off`, setting this enables it at error severity |
| `--doc-coverage-min` | `<PCT>` | — | — | Fail when models carrying a description fall below this percent. Overrides `.dbtl.yml`'s `check.gates.doc_coverage.min`; when that gate is `off`, setting this enables it at error severity |
| `--max-blast-radius` | `<N>` | — | — | Fail when the changed set's blast radius exceeds this many affected models. Overrides `.dbtl.yml`'s `check.gates.blast_radius.max`; when that gate is `off`, setting this enables it at error severity. Needs `--diff` |
| `--lint` | — | — | `false` | Run the static structure-lint rules (best-practice checks over the project graph — no warehouse or `dbt compile` needed). Rule enable/disable, thresholds, and naming prefixes come from the project-local `.dbtl.yml`. With --diff, only violations that did not exist in the baseline gate |
| `--no-breaking` | — | — | `false` | Fail when the diff carries BREAKING column changes that downstream columns still reference — removed or renamed columns whose baseline consumers did not migrate (detected through column-level lineage). Needs `--diff`. Partial-breaking changes (data-type / expression moves) report in the impact pack but never trip this gate. Without `--diff` the gate reports `skip` (asked for but not evaluable) — visible, never counted |
| `--rules` | `<RULES>` (repeatable) | — | — | Restrict linting to these rules (comma-separated rule names; implies --lint). Overrides `.dbtl.yml`'s per-rule enables — the flag is the authoritative rule selector; the file still supplies thresholds and naming prefixes |
| `--fail-on` | `error` \| `warn` \| `never` | — | — | Minimum finding severity that fails a check: `error` (default) fails on errors only, `warn` also fails on warnings, `never` only reports (a tripped gate prints WARN and never counts). Overrides `.dbtl.yml`'s top-level `fail_on`; each gate's severity comes from `--gate`, its `check.gates` entry, or the shipped default |
| `--format` | `text` \| `concise` \| `markdown` \| `json` \| `github` \| `sarif` \| `junit` \| `codeclimate` | — | `text` | Report format on stdout: `text` (the annotated report, default), `concise` (one finding per line — editor/grep-friendly), `markdown` (a document suited to a PR comment), `json` (the `dbtl.report/v1` payload), `github` (Actions workflow annotations), `sarif` (code-scanning upload), `junit` (test report XML), or `codeclimate` (Code Quality issues) |
| `--color` | `auto` \| `always` \| `never` | — | `auto` | When the stdout `text`/`concise` report uses ANSI color: `auto` (default) colors only when the terminal and environment allow, `always` colors even when stdout is piped, `never` disables it. Every other format, every `--report` file, and the step summary are always unstyled |
| `--report` | `<FMT:PATH>` (repeatable) | — | — | Also write the report to a file: `--report <FMT>:<PATH>` (repeatable; `FMT` is any `--format` value). Contents match the bytes that format prints on stdout apart from color — files are always unstyled; parent directories are created and existing files overwritten |
| `--step-summary` | — | — | `false` | Append the markdown report to the file the GITHUB_STEP_SUMMARY environment variable names (a no-op when the variable is unset or empty — plain CI runs only) |
| `--path-prefix` | `<PREFIX>` | — | — | Prefix prepended to the file paths the `github`, `sarif`, and `codeclimate` formats emit — for a project living in a repository subdirectory. A usage error when none of those formats is selected via `--format` or `--report` |
| `--docs-dir` | `<DIR>` | — | — | Gate on generated-docs freshness: compare this directory against what `dbtl docs` would generate right now — the exact comparison `dbtl docs --check` runs — and fail the run when they differ |
| `--select` | `<SELECTOR>` (repeatable) | — | — | Restrict every gate to nodes matching a dbt selector — coverage counts, lint findings, and the `--diff` gates all intersect the selection (`docs_drift` never does; it compares whole doc sets). The grammar of `dbtl ls` (`tag:x`, `path:y`, `+name+`, `state:modified`, …), NOT the TUI's `--select`, which opens one model's tab; `state:` atoms need `--diff`. Repeatable — specs union |
| `--quiet` | — | — | `false` | Suppress the per-check report on stdout; only the exit code matters |
| `-h` `--help` | — | — | — | Print help (see a summary with '-h') |

Accepts global flags: `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`, `--catalog`, `--sql-dialect`.

### `dbtl lint`

Run the static structure-lint rules over the resolved data source and exit — the same `run_rules` findings `check --lint` gates on and the TUI's Lint lens quotes. `.dbtl.yml` supplies rule enables, severities, thresholds and naming prefixes; `--rules` overrides the selection; the baseline file freezes known violations; `--diff` narrows the gate to violations the baseline doesn't already carry

```text
Usage: dbtl lint [OPTIONS]
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `--rules` | `<RULES>` (repeatable) | — | — | Restrict linting to these rules (comma-separated rule names). The flag is the authoritative rule selector — the file still supplies thresholds and naming prefixes |
| `--fail-on` | `error` \| `warn` \| `never` | — | — | Minimum finding severity that fails the run: `error` (default) fails on errors only, `warn` also fails on warnings, `never` only reports. Overrides `.dbtl.yml`'s top-level `fail_on` |
| `--format` | `text` \| `concise` \| `markdown` \| `json` \| `github` \| `sarif` \| `junit` \| `codeclimate` | — | `text` | Report format on stdout: `text` (the annotated report, default), `concise` (one finding per line — editor/grep-friendly), `markdown` (a document suited to a PR comment), `json` (the `dbtl.report/v1` payload), `github` (Actions workflow annotations), `sarif` (code-scanning upload), `junit` (test report XML), or `codeclimate` (Code Quality issues) |
| `--color` | `auto` \| `always` \| `never` | — | `auto` | When the stdout `text`/`concise` report uses ANSI color: `auto` (default) colors only when the terminal and environment allow, `always` colors even when stdout is piped, `never` disables it. Every other format, every `--report` file, and the step summary are always unstyled |
| `--report` | `<FMT:PATH>` (repeatable) | — | — | Also write the report to a file: `--report <FMT>:<PATH>` (repeatable; `FMT` is any `--format` value). Contents match the bytes that format prints on stdout apart from color — files are always unstyled; parent directories are created and existing files overwritten |
| `--step-summary` | — | — | `false` | Append the markdown report to the file the GITHUB_STEP_SUMMARY environment variable names (a no-op when the variable is unset or empty — plain CI runs only) |
| `--path-prefix` | `<PREFIX>` | — | — | Prefix prepended to the file paths the `github`, `sarif`, and `codeclimate` formats emit — for a project living in a repository subdirectory. A usage error when none of those formats is selected via `--format` or `--report` |
| `--write-baseline` | `[<PATH>]` | — | — | Freeze every current finding into a lint baseline file and exit (PATH defaults to `lint.baseline`, else `.dbtl-baseline.json` at the project root). Writes always exit 0 and cannot combine with `--diff` |
| `--no-baseline` | — | — | `false` | Do not read the lint baseline file — every finding counts again |
| `--list-rules` | — | — | `false` | List every lint rule — id, category, effective severity, summary — and exit. Reads `.dbtl.yml` when found; never loads a data source |
| `--explain` | `<ID>` | — | — | Show one rule's full registry entry (rationale, example, params, fix guidance, docs link) and exit — no data load |
| `--select` | `<SELECTOR>` (repeatable) | — | — | Restrict findings to nodes matching a dbt selector — the grammar of `dbtl ls` (`tag:x`, `path:y`, `+name+`, `state:modified`, …), NOT the TUI's `--select`, which opens one model's tab. `state:` atoms need `--diff`. Repeatable — specs union. Cannot combine with `--write-baseline` (a partial finding set must never overwrite the frozen file) |
| `--quiet` | — | — | `false` | Suppress the report on stdout; only the exit code matters |
| `-h` `--help` | — | — | — | Print help (see a summary with '-h') |

Accepts global flags: `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`, `--catalog`, `--sql-dialect`.

### `dbtl ls`

List the nodes a dbt selector matches, one per line — the non-interactive answer to "what does this spec select?" (`dbtl ls 'tag:nightly'`, `dbtl ls '2+fct_orders' --exclude path:staging`). `state:` selectors need `--diff`; note they are resolved by dbtl's own comparison (the SQL, config, column, and macro changes dbtl can read) — the set may not match dbt's `state:modified`

```text
Usage: dbtl ls [OPTIONS] <SELECTOR>...
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `<SELECTOR>` | `<SELECTOR>` (repeatable) | — | — | The selector spec(s): whitespace-separated terms union, comma-separated factors intersect, `+`/numeric hops expand, and atoms are `tag:`, `path:`, `config.materialized:`, `exposure:`, `state:modified\|new` (needs `--diff`), `meta:key[=value]`, bare names and globs |
| `--exclude` | `<SELECTOR>` (repeatable) | — | — | Subtract the nodes a spec matches — applied after every inclusion term. Repeatable — each occurrence takes exactly one spec so a following positional can never be swallowed |
| `--output` | `names` \| `uids` \| `selector` \| `json` | — | `names` | Output shape: `names` (node names, one per line), `uids` (unique_ids), `selector` (one space-separated line of buildable model names for `dbt build --select`), `json` (one object per node) |
| `-h` `--help` | — | — | — | Print help (see a summary with '-h') |

Accepts global flags: `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`.

### `dbtl columns`

Print a column's column-level lineage to stdout and exit — the same `ColumnReport` the TUI's `yl` yank copies (Markdown by default, `--format json` for automation). `TARGET` is `<model>.<column>`; the model resolves by name (or exact unique_id), the column by the resolver's normalized name. Downstream is the default direction. Non-interactive: works in a non-TTY / CI environment

```text
Usage: dbtl columns [OPTIONS] <TARGET>
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `<TARGET>` | `<TARGET>` | — | — | The column to trace, as `<model>.<column>` — e.g. `cll_demo.order_id`. The model segment is a node name or unique_id; the column segment matches case-insensitively (unquoted-identifier normalization, same as the resolver). LIMIT: the split is on the LAST `.`, so a column whose name itself contains one (a quoted `"a.b"` identifier) cannot be expressed here today |
| `--upstream` | — | — | `false` | List the columns feeding INTO the target (provenance). Default is the downstream blast radius |
| `--downstream` | — | — | `false` | List the columns the target feeds (the default). Explicit so the direction is self-documenting in scripts |
| `--format` | `markdown` \| `json` | — | `markdown` | Output format: `markdown` (default) or `json` |
| `-h` `--help` | — | — | — | Print help (see a summary with '-h') |

Accepts global flags: `--manifest`, `--source`, `--project`, `--include-packages`, `--catalog`, `--sql-dialect`.

### `dbtl completions`

Print a shell completion script to stdout and exit — reads no data source and accepts no global flags, so it runs anywhere (the release packaging generates the shipped `completions/` files with it)

```text
Usage: dbtl completions [OPTIONS] <SHELL>
```

| Flag | Value | Env | Default | Description |
| ---- | ----- | --- | ------- | ----------- |
| `<SHELL>` | `bash` \| `elvish` \| `fish` \| `powershell` \| `zsh` | — | — | The shell to generate the completion script for |
| `-h` `--help` | — | — | — | Print help |

Accepts no global flags.

### `dbtl help`

Print this message or the help of the given subcommand(s)

```text
Usage: dbtl help [COMMAND]
```

Accepts no global flags.

### Hidden subcommands

Real commands kept out of `--help` because they feed tooling, not interactive discovery.

| Command | Description |
| ------- | ----------- |
| `dbtl man` | Print the man page (roff) to stdout and exit. Hidden from `--help`: it feeds release packaging (`man/dbtl.1`), not interactive discovery |
| `dbtl cli-reference` | Print the generated CLI reference (Markdown) to stdout and exit. Hidden from `--help`: it feeds `docs/reference/cli.md`, not interactive discovery |

## Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Success — every gate passed, output written. |
| `1` | Gate failure — a check tripped (a failing gate, a lint finding at or above `--fail-on`, or a docs `--check` diff). |
| `2` | Usage / configuration error — clap misuse, unknown rule or gate names, a malformed `.dbtl.yml`, an unresolvable selector, or a global flag the subcommand does not consume. |
| `3` | Input error — a named input (manifest, project, baseline, catalog, diff source, git rev) could not be found or read. Unclassified errors report as this code. |
| `101` | Panic. |
