---
title: Diff
description: Compare your project against another manifest, checkout, or git revision to see changed nodes, column-level breaking changes, and blast radius.
---

Start `dbtl` with `--diff` to answer "what does my branch change?". The TUI marks added and modified models and opens a PR impact report on `D`.

## Choose a baseline

`--diff` takes one of three baselines:

| Baseline | Example |
|---|---|
| A `manifest.json` file | `--diff target/manifest.prod.json` |
| A dbt project directory, such as another worktree | `--diff /path/to/main-checkout` |
| A git revision | `--diff git:main` |

A project directory uses its `target/manifest.json` if it is compiled, and is parsed from source otherwise.

```sh
dbtl --diff /path/to/main-checkout
dbtl --diff target/manifest.prod.json
dbtl --diff git:main
```

## What the TUI shows

With a baseline loaded, `dbtl` adds three things.

- **Diff lens.** Added models show green and modified ones amber. Press `t` to reach it. See [Lenses](lenses.md#diff).
- **Status chip.** A diff chip appears in the status line.
- **PR impact report.** Press `D` to open it.

The report lists:

- changed nodes, with the reasons they changed
- the downstream blast radius
- affected marts
- exposures, with their type and owner
- affected owners, read from a `meta` key (default `owner`)
- a ready-to-run `dbt build --select …`
- risk flags

In the report, `e` exports it as Markdown and `y` copies the suggested `dbt build --select` command, narrowed by the changed columns. `Y` copies the wide version. `j` and `k` scroll, and `Esc`, `Enter`, or `q` closes it.

To read a different `meta` key for owners, set `owner_meta_key` in your [user config](../configuration/user-config.md) or in the project's `.dbtl.yml`.

The same report is available without the TUI. See [Check and impact](../cli/check-and-impact.md).

## How the diff compares

The diff identifies nodes by `unique_id`. A rename shows up as one node removed and another added, which is also how every downstream `ref()` sees it.

It compares the surfaces that a dbt change moves:

- materialization
- direct dependencies
- columns
- tests
- model SQL

### Macros

When both sides are compiled manifests, a change inside a shared macro is caught too. The diff reads each manifest's `macros` and every node's `depends_on.macros`, then marks callers of a changed macro with a `macro:<name>` reason. Transitive macro-to-macro calls count. Raw SQL cannot show this change.

### Column-level breaking changes

Every modified node also gets a column diff. It uses the same column-level lineage engine as [`dbtl columns`](../cli/columns.md), which resolves columns from the catalog, then declared columns, then SQL-derived outputs.

Each changed column gets one of these classifications:

| Change | Meaning |
|---|---|
| Removed | A baseline output name that no current column carries |
| Renamed | A dropped name and a new name draw from the same upstream column, for example `status as order_status` |
| Type changed | `data_type` differs. The catalog is read first, then declared types. |
| Expression changed | The normalized SELECT expression differs. Formatting, comments, and keyword casing never count. |
| Added | A new output column |

The downstream picture sets the severity:

| Severity | Applies to |
|---|---|
| Breaking | A removed or renamed column that still has readers: consumers whose reference now resolves to nothing, or that remain linked to the changed node |
| Partial-breaking | Type and expression changes |
| Non-breaking | Added columns and removals nobody references |

Each finding lists the downstream columns, models, marts, and exposures it reaches. The `D` modal, the Markdown export, and the JSON output all carry the same `column_changes` data. A column-clean change keeps its `+` fan-out in the suggested `dbt build --select` only while the analysis cannot prove otherwise.

## Baseline from a git revision

`--diff git:<rev>` compares against the project as committed on another revision. `<rev>` is anything `git rev-parse` resolves to a commit: a branch (`git:main`), a tag (`git:v1.4.0`), a sha, or an expression (`git:HEAD~3`).

`dbtl` lists the project's files at that revision with `git ls-tree`, then parses them through the source loader into a scratch directory. It reads source files only: models, schema YAML, seeds, docs, and `dbt_project.yml`. `target/` and `dbt_packages/` stay out.

Nothing touches the repository. There is no checkout, no worktree, and no index write. The current side stays your live working tree, uncommitted changes included.

The baseline is parsed from source, so the current side parses from source too, even when `target/manifest.json` exists. Otherwise loader differences would read as changes. An explicit `--manifest` still wins, with a warning that the two loaders may disagree on the margins.

> [!TIP]
> In a pull request workflow, pair `dbtl check --diff git:origin/main` with `actions/checkout` and `fetch-depth: 0`, or fetch the base ref, so the revision resolves. See [GitHub Action](../ci/github-action.md).

## Package scope

Both sides of a diff load under the same package scope. A compiled-manifest baseline and a source-parsed current side compare cleanly by default. The scope shows in the `D` modal header. See the [quick start](../getting-started/quick-start.md#package-scope).
