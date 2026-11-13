---
title: pre-commit
description: Run dbtl lint as a pre-commit hook, with a hook that builds dbtl or one that uses the binary on your PATH.
---

The dbtl repository ships `.pre-commit-hooks.yaml` with two hooks. Both run [`dbtl lint`](../cli/lint.md) before a commit.

| Hook | Behavior |
| ---- | -------- |
| `dbtl-lint` | Builds dbtl from the dbtl repository. |
| `dbtl-lint-system` | Uses the `dbtl` already on your `PATH`. |

Add one to your `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/hytfjwr/dbtl
  rev: @DBTL_TAG@
  hooks:
    - id: dbtl-lint-system
```

Both hooks run once per commit and only when dbt project files changed. They set `pass_filenames: false`, because lint reads the whole project.

## Which hook to pick

- `dbtl-lint` builds the pinned revision of `dbtl` from source with `cargo install`. The first run on a machine takes minutes; every committer then lints with the version `rev:` names.
- `dbtl-lint-system` starts instantly but uses whatever `dbtl` version is on `PATH`, so the version can drift between machines.

Either way, keep a CI gate as the source of truth — a hook only shortens the feedback loop. See [GitHub Action](github-action.md) and [other CI systems](other-ci.md).

## What the hook runs

- The entry is `dbtl lint` with no flags. It resolves the data source at the repository root — `target/manifest.json` when one exists, the project source otherwise. See [choose the data source](../cli/overview.md#choose-the-data-source).
- The `files` pattern limits runs to commits touching `.sql`, `.py`, `.yml`, `.yaml`, `.csv`, or `.md` files.
- The hook blocks the commit when `dbtl lint` exits 1 — a finding at or above `fail_on`. Severities and the threshold come from `.dbtl.yml`; `fail_on: never` makes the hook report-only. See [severities and fail_on](../cli/lint.md#severities-and-fail_on).
- On a project that already violates rules, freeze the current findings so only new ones block a commit: `dbtl lint --write-baseline` and a committed `.dbtl-baseline.json`. See [adopt lint on an existing project](../guides/adopt-lint.md).

## A project in a subdirectory

`dbtl lint` auto-detects at the repository root. When the dbt project sits deeper, pass the project flag through the hook's `args`:

```yaml
- repo: https://github.com/hytfjwr/dbtl
  rev: @DBTL_TAG@
  hooks:
    - id: dbtl-lint-system
      args: [--project, path/to/dbt-project]
```

Without it, a root that holds no `dbt_project.yml` fails with a "no data source" input error. See [troubleshooting](../configuration/troubleshooting.md#dbtl-reports-no-data-source).
