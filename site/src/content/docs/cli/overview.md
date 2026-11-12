---
title: CLI overview
description: Every dbtl subcommand in one table, and how the data-source flags behave across them.
---

`dbtl` with no subcommand starts the interactive TUI. Every subcommand is non-interactive: it prints to stdout, sets an [exit code](../configuration/exit-codes.md), and needs no TTY, so you can run it in CI.

## Subcommands

| Command | Purpose | Page |
| ------- | ------- | ---- |
| `dbtl` | Start the interactive TUI. | [Getting started](../getting-started/quick-start.md) |
| `dbtl docs` | Write a Markdown documentation tree, one page per node. | [dbtl docs](docs.md) |
| `dbtl impact` | Print the PR impact pack for a `--diff` baseline. | [Check and impact](check-and-impact.md) |
| `dbtl check` | Run CI gates and exit 1 when one fails. | [Check and impact](check-and-impact.md) |
| `dbtl lint` | Run the static lint rules on their own. | [dbtl lint](lint.md) |
| `dbtl ls` | List the nodes a dbt selector matches. | [dbtl ls](ls.md) |
| `dbtl columns` | Print column-level lineage for one column. | [dbtl columns](columns.md) |
| `dbtl completions` | Print a shell completion script. | [Completions](completions.md) |
| `dbtl help` | Print help for `dbtl` or a subcommand. | [CLI reference](../reference/cli.md) |

Two more commands are hidden from `--help` because they feed tooling: `dbtl man` prints the man page, and `dbtl cli-reference` prints the generated CLI reference. See [Completions and man page](completions.md) for `dbtl man`.

The full flag list for every command is in the [CLI reference](../reference/cli.md).

## Choose the data source

Every subcommand that reads a project resolves its data source the same way:

| Flag | Effect |
| ---- | ------ |
| `--manifest <PATH>` | Read a compiled `manifest.json`. Forces manifest mode. |
| `--source <DIR>` | Parse a dbt project directory from source, with no compile. Forces source mode. |
| `--project <DIR>` | Auto-detect inside this directory. Default: `.`. |

With neither `--manifest` nor `--source`, `dbtl` uses `<project>/target/manifest.json` if it exists and parses the project source otherwise.

```sh
dbtl lint                                   # auto-detect in the current directory
dbtl lint --project /path/to/dbt-project    # auto-detect in another directory
dbtl lint --manifest target/manifest.json   # force manifest mode
dbtl lint --source /path/to/dbt-project     # force source mode
```

## Global flags

These flags work on either side of the subcommand. `dbtl --manifest m.json check` and `dbtl check --manifest m.json` are the same command.

| Flag | Meaning |
| ---- | ------- |
| `--manifest`, `--source`, `--project` | Choose the data source (above). |
| `--diff <BASELINE>` | Compare against a baseline: a `manifest.json`, a dbt project directory, or `git:<rev>`. |
| `--run-results <PATH>` | Use this `run_results.json` instead of the one found next to the data source. |
| `--catalog <PATH>` | Use this `catalog.json` instead of the one found next to the data source. |
| `--include-packages` | Include nodes installed from dbt packages in the graph. |
| `--sql-dialect <DIALECT>` | Pin the SQL dialect the parser uses. `auto` reads the manifest's adapter type. |

A subcommand honors only the global flags it consumes. If you type another one explicitly, the command stops with a usage error (exit code 2) instead of ignoring the flag. The same applies to TUI-only flags such as `--theme` or `--watch` placed next to a subcommand.

| Command | Global flags it accepts |
| ------- | ----------------------- |
| `docs` | `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`, `--run-results`, `--catalog`, `--sql-dialect` |
| `impact`, `check`, `lint` | `--manifest`, `--source`, `--project`, `--include-packages`, `--diff`, `--catalog`, `--sql-dialect` |
| `ls` | `--manifest`, `--source`, `--project`, `--include-packages`, `--diff` |
| `columns` | `--manifest`, `--source`, `--project`, `--include-packages`, `--catalog`, `--sql-dialect` |
| `completions` | none |

## Package scope

By default the graph holds the root project only. A compiled manifest mixes installed-package nodes (`dbt_utils`, for example) into `nodes`, and `dbtl` prunes them at load. Manifest mode and source mode then agree on the node set.

`--include-packages` widens the graph to the manifest's full contents. Package nodes appear in the model list, lineage, docs pages, and `--diff` baselines as dependency context. Every aggregate metric still counts the root project only: coverage, orphans, stats, the docs summary, `dbtl check`, and the PR impact risk flags.

In source mode `--include-packages` does nothing and prints a warning, because installed packages are never parsed. The flag is global, so `dbtl --include-packages check` and `dbtl check --include-packages` are equivalent.

The scope shows wherever numbers are reported. The status bar shows `scope:root` or `scope:+pkgs`, the generated docs README leads its summary with a `- scope:` bullet, `dbtl check` prints a `scope:` line, and the diff modal and the `dbtl impact` report carry it in the header.

## Side artifacts

`dbtl` looks for `run_results.json` and `catalog.json` next to the data source. That is the manifest's directory in manifest mode and `<project>/target/` in source mode. A missing file means less data, never an error. `--run-results` and `--catalog` point at a different file.

`dbtl docs` is the exception. It never auto-detects these files. See [Optional artifacts](docs.md#optional-artifacts-catalog-and-run_results).

## Selectors

`ls`, `check`, `lint`, `impact`, and `docs` take `--select` with the full dbt selector grammar. `ls` and `docs` also take `--exclude`. See [Selectors](selectors.md).
