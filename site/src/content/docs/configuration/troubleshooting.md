---
title: Troubleshooting
description: Fix startup, data-source, rendering, and exit-code problems — no TTY, a missing manifest, source and manifest differences, doubled borders, and narrow terminals.
---

## `dbtl` exits with "failed to initialize terminal (no TTY?)"

The bare `dbtl` command starts the interactive TUI, which needs a real terminal. In a pipe, a CI step, an editor task, or `ssh` without `-t`, it exits with code 3 and that message instead of running headless.

- Every subcommand is non-interactive and needs no TTY: `lint`, `check`, `impact`, `ls`, `docs`, `columns`, `completions`.
- Over SSH, allocate a terminal with `ssh -t`.

## `dbtl` reports "no data source"

Auto-detect looks for `target/manifest.json` under the project directory, then falls back to parsing the project source. When it finds neither, the run exits 3:

```text
error: no data source under .: neither target/manifest.json nor dbt_project.yml found (pass --manifest <file> or --source <dir>)
```

- From another directory, point at the project: `dbtl --project path/to/dbt-project`.
- Or force the mode: `--manifest <PATH>` or `--source <DIR>`. See [choose the data source](../cli/overview.md#choose-the-data-source).

## Source mode and manifest mode disagree

A compiled manifest is always more accurate. Source mode infers lineage from raw SQL without rendering Jinja: a `ref()` it cannot resolve drops out of the graph, `enabled` expressions evaluate only the dbt-compatible subset (`var()`, literals, comparisons, `and`/`or`/`not`, `in`/lists, `| as_bool` — `--vars` applies too), and macros never run — so an edge a macro would produce is absent. See [source mode](../getting-started/quick-start.md#source-mode).

- Keep `target/manifest.json` up to date (`dbt compile` or `dbt parse` after changing models) when precision matters.
- In the TUI, press `P` to run `dbt parse` and switch to the generated manifest in place.

## `check` or `lint` exits 2 or 3 instead of 1

Code 1 is a gate failure only. The other codes mean the run never evaluated the project:

- **2 — usage or configuration error.** A flag the subcommand does not take, a malformed selector, an unknown rule or gate name, a broken `.dbtl.yml`. The message names the offending flag or key.
- **3 — input error.** A manifest, project, baseline, catalog, or git revision that cannot be found or read.

The full table is in [exit codes](exit-codes.md).

## `--diff git:<rev>` cannot resolve the revision

A `git:` baseline reads the project as committed at that revision. The resolve fails when the commit is missing — commonly a shallow CI checkout. Give the checkout the history it needs with `fetch-depth: 0`, or fetch the one base commit:

```sh
git fetch --depth=1 origin <sha>
```

See [git diffs and baselines](../cli/check-and-impact.md#git-diffs-and-baselines).

## Warnings at startup or after a reload

The loader reports what it cannot use — resource types the graph cannot draw, a malformed `config.yml`, `--include-packages` under source mode — once each, on stderr at startup and as a toast after an `r` reload. Nothing is dropped silently: every warning lands in the notification log, counted by the `!N` status chip and paged through with `Ctrl-n`. See [notifications](../tui/interface.md#notifications).

## Borders look doubled or misaligned

Unicode box-drawing characters have East Asian Ambiguous width. Some terminal configurations draw them 2 cells wide, which doubles or ghosts the borders.

`dbtl` probes the terminal at startup and falls back to pure ASCII (`+ - | >`) when it detects this. You can also force a mode:

```sh
dbtl --ascii     # pure ASCII rendering
dbtl --unicode   # skip the probe, keep Unicode box drawing
```

## The terminal is too small or panes disappeared

`dbtl` needs at least 50 columns by 12 rows. Below that, it draws only a "terminal too small" notice. `q` and `Ctrl-c` still work.

The layout adapts to the width:

| Width | Behavior |
| ----- | -------- |
| Under 50 columns | The "terminal too small" notice. |
| 50 to 79 columns | The model list collapses into a drawer. `Ctrl-b` toggles it and `Tab` cycles focus. |
| Under 160 columns | The inspector does not appear by itself. `I` toggles it manually. |

In a narrow tmux pane, resize the pane or zoom it with `prefix z` instead of disabling panes.
