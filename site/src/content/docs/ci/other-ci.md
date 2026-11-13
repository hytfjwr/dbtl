---
title: Other CI systems
description: Run dbtl check without the GitHub Action, wire it into GitLab or any other CI, and gate on generated docs drift.
---

Nothing in `dbtl` is specific to GitHub. Every checking subcommand returns before the terminal initializes, so any CI runner can call it. This page covers a hand-written GitHub workflow, GitLab, and docs drift.

## Without the action

The action's pipeline by hand: install a release binary (or run `cargo install`), then run `dbtl check` directly.

```yaml
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

      - name: Install dbtl
        env:
          TAG: @DBTL_TAG@
        run: |
          curl -fsSLO "https://github.com/hytfjwr/dbtl/releases/download/${TAG}/checksums.txt"
          curl -fsSLO "https://github.com/hytfjwr/dbtl/releases/download/${TAG}/dbtl-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
          sha256sum --check --ignore-missing checksums.txt
          tar -xzf "dbtl-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
          echo "$PWD/dbtl-${TAG}-x86_64-unknown-linux-gnu" >> "$GITHUB_PATH"

      - name: dbtl check
        env:
          BASE: ${{ github.event.pull_request.base.sha }}
        run: |
          dbtl check --diff "git:${BASE}" --lint --format github \
            --report "markdown:${RUNNER_TEMP}/dbtl-report.md" \
            --report "sarif:${RUNNER_TEMP}/dbtl.sarif" \
            --step-summary
```

The report flags in that step:

- `--format github` emits `::error` and `::warning` annotations per finding, in severity order. GitHub caps how many annotations a step shows.
- `--report <fmt>:<path>` writes any format to a file.
- `--step-summary` appends the Markdown report to `GITHUB_STEP_SUMMARY`.
- `--path-prefix` rewrites paths in `github`, `sarif`, and `codeclimate` output, for a project in a repository subdirectory.

All eight formats are listed under [Report formats](../cli/check-and-impact.md#report-formats). To compare against a git revision, the checkout needs history. See [Git diffs and baselines](../cli/check-and-impact.md#git-diffs-and-baselines).

## GitLab

For GitLab Code Quality, collect a `codeclimate` report as an artifact. For test-report UIs, collect `junit`.

```yaml
dbtl check:
  script:
    - dbtl check --lint --report "codeclimate:dbtl-codeclimate.json" --report "junit:dbtl-junit.xml"
  artifacts:
    when: always
    reports:
      codequality: dbtl-codeclimate.json
      junit: dbtl-junit.xml
```

`artifacts: when: always` keeps the reports when a gate fails and the job exits 1.

## Docs drift

`dbtl docs --check` compares a committed docs tree with what the generator would write now. It exits 1 on any difference. See [Housekeeping: check and clean](../cli/docs.md#housekeeping-check-and-clean).

The `docs_drift` gate folds the same comparison into `dbtl check`. Enable it with `--docs-dir <DIR>` or with `check.gates.docs_drift.dir` in `.dbtl.yml`.

```sh
dbtl check --docs-dir docs/lineage
```

## Exit codes

CI steps fail on any non-zero exit. Code 1 means a gate failed. Codes 2 and 3 mean the invocation or an input was wrong. See [Exit codes](../configuration/exit-codes.md).
