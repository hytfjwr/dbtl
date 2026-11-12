---
title: Exit codes
description: The exit codes of dbtl subcommands, so CI can tell a failed gate from a wrong invocation.
---

Every non-interactive subcommand maps its outcome onto a small set of codes. CI can then tell "the project failed a gate" from "the invocation was wrong".

| Code | Meaning | Examples |
| ---- | ------- | -------- |
| `0` | Success. | Every gate passed. Output was written. Docs are up to date. A selector matched, even an empty set. |
| `1` | Gate failure. | A `check` gate failed. A `lint` or `check` finding is at or above `fail_on`. `docs --check` found drift. |
| `2` | Usage or configuration error. | Misused flags, including a global flag the subcommand does not take. A malformed selector. An unknown rule or gate name. Conflicting flags. A broken `.dbtl.yml`. |
| `3` | Input error. | A manifest, project, baseline, catalog, diff source, or git revision that cannot be found or read. An unknown `columns` node. Unclassified errors also report as `3`. |
| `101` | Internal panic. | A bug. Please [report it](https://github.com/hytfjwr/dbtl/issues/new/choose). |

Code `1` means a gate failure only.

## Notes by command

- **`dbtl check` and `dbtl lint`.** The `fail_on` threshold decides which findings produce code 1. With `--fail-on never`, the run only reports. See [Severities and fail_on](../cli/lint.md#severities-and-fail_on).
- **`dbtl lint --write-baseline`.** The write always exits 0.
- **`dbtl lint` baseline.** An unreadable or malformed baseline file is an input error (code 3).
- **`dbtl docs --check`.** Any difference between the committed tree and the generated set exits 1.
- **`dbtl ls`.** An empty match prints nothing and exits 0. A malformed spec exits 2.
- **`dbtl columns`.** An unknown node exits 3.

## Use the codes in CI

A CI step fails on any non-zero exit. To treat a failed gate differently from a bad invocation, branch on the code:

```sh
dbtl check --diff git:origin/main --lint
case $? in
  0) echo "all gates passed" ;;
  1) echo "a gate failed" ;;
  *) echo "bad invocation or input" ;;
esac
```

The [GitHub Action](../ci/github-action.md) exposes the `dbtl check` verdict as the `exit-code` output and re-raises it in its last step.
