---
title: Overview
description: dbtl shows your dbt project's model lineage in the terminal, from a compiled manifest or straight from project source, with no compile step and no browser.
---

`dbtl` shows a dbt project's model lineage in the terminal. Pick a model in the list and the graph of what it reads from and what depends on it appears beside it — no `dbt compile`, no browser.

```text
╭Seed────────────────────╮   ╭View───────────────────╮   ╭View────────────────────╮
│ source_datetime_policy │──▶│ stg_payment__shoppers │──▶│ int_shoppers__combined │
╰────────────────tests:4─╯  │╰───────────────tests:5─╯   ╰────────────────────────╯
                            │
╭Source────╮                │
│ shoppers │────────────────╯
╰──────────╯
```

It builds that graph from `dbt_project.yml`, model `.sql` files (`ref()` and `source()`), and `schema.yml`, or from a compiled `manifest.json` when one exists — see [Choose the data source](../cli/overview.md#choose-the-data-source). `dbtl` is a single self-contained binary (the Linux musl builds are fully static) and runs entirely in the terminal, including over SSH.

To install `dbtl` and open your first project, start with [Installation](installation.md) and the [quick start](quick-start.md). To try it first, the [playground](/dbtl/playground/) runs the same engine in the browser on bundled sample projects — nothing is uploaded.

## Feature tour

### In the terminal

#### Navigate the graph

Move a cursor between lineage nodes and re-root on any model. Filter by direction and depth, step back and forward through your re-root history, and fold large sibling groups. See [Lineage](../tui/lineage.md).

#### Trace a path

The path from the root to the cursor lights up and everything else dims. Compact mode shrinks nodes to one row, and overview mode shows the whole DAG. See [Lineage](../tui/lineage.md).

#### Coverage and impact lenses

Lenses tint the lineage by test coverage, downstream blast radius, layer, lint findings, `--diff` changes, last-run status, and more. See [Lenses](../tui/lenses.md).

#### Exposures and the semantic layer

Exposures appear as end nodes at the right edge of the lineage. A compiled manifest adds semantic models, metrics, and saved queries. See [Exposures](../tui/exposures.md).

#### The last run and the warehouse

`dbtl` auto-detects `run_results.json` and `catalog.json` next to the data source. They add run-status and runtime lenses, real column types, row counts, and a drift check. See [Run results and catalog](../tui/run-results-and-catalog.md).

#### Review a branch with `--diff`

Compare against another manifest, a checkout, or a git revision. The `D` report lists changed nodes, blast radius, affected exposures and owners, and a ready-to-run `dbt build --select`. See [Diff](../tui/diff.md).

#### Search and select like dbt

The `/` list search takes a `!`-prefixed dbt selector or a `?`-prefixed full-text query, and `Ctrl-p` opens a command palette. See [Search](../tui/search.md).

#### Tabs, a home page, and an inspector

Work on several models at once in tabs, start from a home tab that shows project health, and keep an inspector pane open on the node under the cursor. See [Interface](../tui/interface.md).

#### Mouse support

Click to select, double-click to commit, right-click for an actions menu, and drag to pan or resize. See [Interface](../tui/interface.md#mouse).

#### Never lose a warning

Loader and config warnings show as toasts and also land in a notification log. See [Interface](../tui/interface.md#notifications).

#### Color themes

Five built-in presets, live switching with `Ctrl-t`, and your own YAML theme files. See [Themes](../tui/themes.md).

### In CI and scripts

#### Gates, lint, and CI

`dbtl check` gates coverage, layer violations, untested and breaking changes, and docs drift. `dbtl lint` runs a static best-practice rule set over the project graph. See [Lint](../cli/lint.md), [Check and impact](../cli/check-and-impact.md), and the [GitHub Action](../ci/github-action.md).

#### Docs that match the screen

`dbtl docs` writes a committable Markdown tree with a page per node. Regeneration is byte-deterministic, so CI can gate on it. See [dbtl docs](../cli/docs.md).

#### One selector grammar everywhere

The dbt selector language (`tag:`, `path:`, `config.materialized:`, `meta:`, `exposure:`, `state:`, hops, globs, unions, intersections) drives `dbtl ls`, the TUI's `!` search, and `--select` on `check`, `lint`, `impact`, and `docs` — `docs` and `ls` also take `--exclude`. A spec means the same set in every context. See [Selector syntax](../cli/selectors.md).
