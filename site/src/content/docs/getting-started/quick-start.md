---
title: Quick start
description: Run dbtl at the root of a dbt project, choose between a compiled manifest and project source, and pick the data source with flags.
---

Run `dbtl` at the root of any dbt project.

```sh
dbtl
```

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

A manifest, including one written by dbt Fusion, can carry resource types that `dbtl` does not handle. It still loads. Unsupported or unknown `resource_type` values are kept as generic lineage nodes, and a non-empty `functions` map is reported. Each kind produces one warning, shown on stderr at startup and as a toast after an `r` reload, so nothing is dropped silently. See [notifications](../tui/interface.md#notifications).

## Start on a model

`--select` opens a model at startup. Pass a model name or a dbt selector spec. Its first match opens in a tab, and it must be a model. `state:` selectors are not available here because the selector resolves before `--diff` loads.

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

By default, the graph contains the root project only. A compiled manifest mixes in nodes from installed dbt packages, and `dbtl` prunes them at load, so manifest mode and source mode agree on the node set.

Pass `--include-packages` to widen the graph to the packages' nodes. They appear in the model list and lineage as dependency context, but every metric (coverage, orphans, stats, `dbtl check`) still counts the root project only.

```sh
dbtl --include-packages
```

The status line shows `scope:root` or `scope:+pkgs`. In source mode `--include-packages` has no effect and prints a warning, because installed packages are never parsed.

## Other launch options

| Flag | Effect |
|---|---|
| `--diff <baseline>` | Compare against a baseline. See [Diff](../tui/diff.md). |
| `--run-results <path>` | Load a specific `run_results.json`. See [Run results and catalog](../tui/run-results-and-catalog.md). |
| `--catalog <path>` | Load a specific `catalog.json`. See [Run results and catalog](../tui/run-results-and-catalog.md). |
| `--theme <name>` | Pick a color theme. See [Themes](../tui/themes.md). |
| `--ascii` / `--unicode` | Force a glyph mode instead of probing the terminal. |

Global data flags work on either side of a subcommand. The full flag list is in the [CLI reference](../reference/cli.md).

## Next steps

- Learn the screen layout in [Interface](../tui/interface.md).
- Move around the graph in [Lineage](../tui/lineage.md).
- Press `?` inside the app for the full key list, or read the [keybindings reference](../reference/keybindings.md).
