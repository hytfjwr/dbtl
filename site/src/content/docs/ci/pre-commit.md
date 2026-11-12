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
