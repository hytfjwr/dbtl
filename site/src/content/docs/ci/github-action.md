---
title: GitHub Action
description: Run dbtl check in GitHub Actions with annotations, a step summary, SARIF upload, and sticky pull request comments.
---

The dbtl repository ships a composite action at its root. One `uses:` step installs a release binary and runs `dbtl check` with GitHub annotations, a Markdown report, and a step summary. On pull requests it also diffs against the base commit.

Everything `dbtl` checks runs without a TTY, so the action needs no terminal setup.

## Add the action

```yaml
name: dbtl

on:
  pull_request:

permissions: {}

jobs:
  check:
    name: dbtl check
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: read
    steps:
      - name: Checkout
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          fetch-depth: 0

      - name: dbtl check
        uses: hytfjwr/dbtl@<commit-sha> # @DBTL_TAG@
        with:
          version: @DBTL_TAG@
          project-dir: .
```

Pin the action to a commit SHA. The `# @DBTL_TAG@` comment keeps the tag readable. Set `version:` to the release tag too, because a SHA ref carries no version for the installer to resolve.

`fetch-depth: 0` makes the base commit resolvable for `--diff git:`. The action also fetches a missing base sha itself at depth 1.

## Inputs

| Input | Default | Meaning |
| ----- | ------- | ------- |
| `version` | `''` | Release tag to install (`vX.Y.Z`). Empty resolves to the tag the action was invoked at. `local` uses the `dbtl` already on `PATH`. |
| `project-dir` | `.` | dbt project directory, relative to the repository root. |
| `base-ref` | `''` | Revision `--diff` compares against. Empty uses the PR's base sha, or a push's `before`. A missing commit is fetched at depth 1. |
| `lint` | `true` | Also run the lint gate set. |
| `fail-on` | `''` | `error`, `warn`, or `never`. Empty follows `.dbtl.yml`. |
| `args` | `''` | Extra arguments appended to `dbtl check`, split on whitespace. |
| `step-summary` | `true` | Append the Markdown report to the job summary. |
| `sarif` | `false` | Upload a SARIF report to code scanning. Needs `security-events: write`. |
| `comment` | `false` | Post or update a sticky `<!-- dbtl:report -->` PR comment. Needs `pull-requests: write`. |
| `github-token` | `${{ github.token }}` | Token for the PR comment. SARIF upload always uses the job's `GITHUB_TOKEN`. |
| `dry-run` | `false` | Print the SARIF upload and PR comment calls instead of making them. Useful for action development. |

## Outputs

| Output | Value |
| ------ | ----- |
| `exit-code` | The `dbtl check` verdict. The last action step re-raises it. |
| `report` | Path to the Markdown report. |
| `sarif` | Path to the SARIF file. |

The exit code travels through an output so the comment and SARIF steps run even when a gate fails.

## SARIF and PR comments

Set `sarif: 'true'` to upload findings to code scanning, and `comment: 'true'` to post the report on the pull request. Grant the job the permissions each one needs.

```yaml
    permissions:
      contents: read
      security-events: write   # sarif upload
      pull-requests: write     # sticky comment
    steps:
      # ... checkout ...
      - uses: hytfjwr/dbtl@<commit-sha> # @DBTL_TAG@
        with:
          version: @DBTL_TAG@
          sarif: 'true'
          comment: 'true'
```

The comment is sticky. The action updates the first `<!-- dbtl:report -->` comment instead of stacking new ones.

> [!NOTE]
> On forked pull requests the token is read-only. The action skips the write-back steps with a warning. The check itself still runs and gates.

## What the action runs

The action runs [`dbtl check`](../cli/check-and-impact.md) with the lint gates enabled by default. Gate severities and parameters come from your project's [`.dbtl.yml`](../configuration/project-config.md). The `fail-on` and `args` inputs override or extend them.

To run the same steps yourself, see [Other CI systems](other-ci.md). To run lint locally before you push, see [pre-commit](pre-commit.md).
