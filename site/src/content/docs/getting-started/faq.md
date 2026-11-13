---
title: FAQ
description: Quick answers about dbtl — trying it without installing, dbt version support, Fusion manifests, warehouse access, TTY and exit codes, and where it writes files.
---

Short answers to the questions that come up most. Each answer links to the page that covers the topic in depth.

## Can I try dbtl without installing it?

Yes. The [playground](/dbtl/playground/) runs the real engine — the same Rust code compiled to WebAssembly — in your browser on bundled sample projects. You can match selectors, read the lineage, run lint, trace columns, or drop in your own `manifest.json`. Nothing is uploaded.

## Does dbtl need dbt installed?

No. Source mode parses the project files itself, and manifest mode reads an existing `target/manifest.json`. `dbt` on your `PATH` is needed only for the `P` key, which runs `dbt parse` and switches to the generated manifest in place. See [source mode](quick-start.md#source-mode).

## Which dbt versions does dbtl work with?

`dbtl` parses the manifest format tolerantly — fields it does not model are ignored — so manifests from a range of releases load. These are verified by the test suite and CI:

| dbt | Verified | How it is verified |
| --- | -------- | ------------------ |
| dbt Core | the latest release, plus 1.12.6 and 1.9.1 | A weekly job regenerates the fixture projects' manifests at the newest `dbt-core` release and at each project's recorded version, then runs the manifest↔source consistency tests. Every committed fixture loads in the test suite; they carry `dbt_version` 1.12.6 and 1.9.1 on manifest schema v12. |
| dbt Fusion | 2.0.0 | A committed Fusion-generated manifest (schema v12) loads in the test suite. Resource types `dbtl` cannot draw are kept as generic nodes or reported as warnings — nothing is dropped silently. See [unsupported resource types](quick-start.md#unsupported-resource-types). |
| No dbt at all | always | Source mode reads `dbt_project.yml`, model SQL, and `schema.yml` directly and never calls `dbt`. |

## Does dbtl need a warehouse connection or credentials?

No. Everything is read from local files. A `catalog.json` next to the data source is optional and adds warehouse column types, row counts, and the catalog-only lint rules — never a connection. See [run results and catalog](../tui/run-results-and-catalog.md).

## Should I use source mode or manifest mode?

Use the manifest when you have one — it is always more accurate, because source mode infers lineage from raw SQL without rendering Jinja. With neither flag, `dbtl` auto-detects `target/manifest.json` and parses the source otherwise. See [choose a data source](quick-start.md#choose-a-data-source).

## Why does `dbtl` exit immediately in CI or over a pipe?

The bare `dbtl` command is the interactive TUI, and it needs a real terminal — it exits with code 3 rather than running headless. Every subcommand is non-interactive and safe in CI. See [CLI overview](../cli/overview.md) and [troubleshooting](../configuration/troubleshooting.md).

## What do the exit codes mean?

`0` success, `1` a gate or lint finding failed the run, `2` the invocation or configuration was wrong, `3` an input could not be found or read. The full table is in [exit codes](../configuration/exit-codes.md).

## How do I adopt lint without fixing everything first?

Freeze the current findings with `dbtl lint --write-baseline`, commit `.dbtl-baseline.json`, then gate only new findings. The walkthrough is in [adopt lint on an existing project](../guides/adopt-lint.md).

## How do I see what a pull request changes?

Start `dbtl` with `--diff <baseline>` — a manifest, another checkout, or a `git:` revision — and press `D` for the PR impact report. The same report prints with `dbtl impact`, and `dbtl check` gates on it. The walkthrough is in [review a pull request](../guides/review-a-pr.md).

## What does dbtl write to my project?

Nothing by default. Two opt-in commands write files: `dbtl lint --write-baseline` writes `.dbtl-baseline.json`, and `dbtl docs --out <DIR>` writes the generated Markdown tree. Your per-project view (bookmarks, tabs, the active lens) is saved outside the project under `~/.local/state/dbtl/`. See [saved state](../configuration/user-config.md#saved-state).

## How do I upgrade dbtl?

With Homebrew, `brew upgrade dbtl`. Otherwise download the new archive from the [releases page](https://github.com/hytfjwr/dbtl/releases), or run `cargo install --path .` in a fresh clone. `dbtl` follows semantic versioning — the [changelog](/dbtl/changelog/) calls out breaking changes before you upgrade across a version line.

## Where do I report a bug?

Open an issue on [GitHub](https://github.com/hytfjwr/dbtl/issues/new/choose) — the templates ask for the `dbtl` version, platform, invocation, and what you expected versus what happened.
