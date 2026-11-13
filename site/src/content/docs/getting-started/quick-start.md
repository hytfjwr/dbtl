---
title: Quick start
description: Run dbtl at the root of a dbt project, choose between a compiled manifest and project source, and pick the data source with flags.
---

Run `dbtl` at the root of any dbt project.

> [!TIP]
> Not installed yet? The [playground](/dbtl/playground/) runs the same engine in your browser on bundled sample projects — a way to try the workflow below before installing.

```sh
dbtl
```

The model list opens on the left, grouped by layer directory. Picking a model draws its lineage — what it reads from on the left and what depends on it on the right. Press `?` for the key list, or `q` to quit.

## Choose a data source

`dbtl` reads lineage from one of two sources.

| Mode | Reads | Flag |
|---|---|---|
| Manifest | A compiled `target/manifest.json` | `--manifest` |
| Source | `dbt_project.yml`, model `.sql` files, `schema.yml` | `--source` |

With no flag, `dbtl` auto-detects. If `target/manifest.json` exists, it uses the manifest. Otherwise it parses the project source directly.

```sh
# Auto-detect: ./target/manifest.json if present, otherwise parse ./ as source
dbtl

# Auto-detect under another project directory
dbtl --project /path/to/dbt-project

# Force a mode
dbtl --manifest /path/to/target/manifest.json
dbtl --source /path/to/dbt-project
```

### Source mode

Source mode infers lineage from the raw SQL without rendering Jinja, so a compiled manifest is always more accurate.

If `dbt` is on your `PATH`, press `P` inside the app to run `dbt parse` and switch to the generated manifest in place.

### Unsupported resource types

A manifest, including one written by dbt Fusion, can carry resource types that `dbtl` does not handle. The load still succeeds.

- Unsupported or unknown `resource_type` values become generic lineage nodes.
- A non-empty `functions` map is reported.
- Each kind produces one warning, shown on stderr at startup and as a toast after an `r` reload, so nothing is dropped silently. See [notifications](../tui/interface.md#notifications).

## Start on a model

`--select` opens a model at startup: `dbtl` resolves the selector and opens its first matching model in a tab, skipping non-model matches like exposures. `state:` selectors work when `--diff` provides a baseline.

```sh
dbtl --select stg_orders
dbtl --select 'tag:nightly'
dbtl --select 'meta:domain=sales'
```

Add `--watch` to reload automatically when the data source changes on disk.

```sh
dbtl --select stg_orders --watch
```

The selector grammar is covered in [selector syntax](../cli/selectors.md).

## Package scope

By default the graph contains the root project only; `--include-packages` widens it to installed-package nodes while every metric still counts the root project. See [Package scope](../cli/overview.md#package-scope).

## Other launch options

| Flag | Effect |
|---|---|
| `--diff <baseline>` | Compare against a baseline. See [Diff](../tui/diff.md). |
| `--run-results <path>` | Load a specific `run_results.json`. See [Run results and catalog](../tui/run-results-and-catalog.md). |
| `--catalog <path>` | Load a specific `catalog.json`. See [Run results and catalog](../tui/run-results-and-catalog.md). |
| `--theme <name>` | Pick a color theme. See [Themes](../tui/themes.md). |
| `--ascii` / `--unicode` | Force a glyph mode instead of probing the terminal. See [Troubleshooting](../configuration/troubleshooting.md#borders-look-doubled-or-misaligned). |

Global data flags work on either side of a subcommand. The full flag list is in the [CLI reference](../reference/cli.md).

## Next steps

- Learn the screen layout in [Interface](../tui/interface.md).
- Move around the graph in [Lineage](../tui/lineage.md).
- Press `?` inside the app for the full key list, or read the [keybindings reference](../reference/keybindings.md).
