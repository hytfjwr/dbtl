---
title: Review a pull request
description: See what a dbt branch changes — the blast radius, breaking column changes, and affected exposures and owners — in the TUI, in the CLI, or in CI.
---

`dbtl` answers "what does this branch change, and who does it break?" with one diff payload. The TUI's `D` report, `dbtl impact`, `dbtl check`, and the GitHub Action all render the same comparison, so a CI comment cannot disagree with the screen.

## What you need

- `dbtl` installed — see [Installation](../getting-started/installation.md).
- A baseline to compare against: a git revision such as `git:origin/main`, another checkout, or a saved `manifest.json`. A `git:` baseline needs the revision resolvable in your clone — a shallow checkout may need a fetch first.

## Steps

1. **Make the base resolvable.** Fetch the branch the pull request targets:

   ```sh
   git fetch origin main
   ```

2. **Open the diff.** In the TUI, start `dbtl` with the baseline:

   ```sh
   dbtl --diff git:origin/main
   ```

   Added nodes show green and modified ones amber under the diff lens. Press `D` to open the PR impact report.

   Without the TUI, `dbtl impact` prints the same report as Markdown — or as JSON with `--format json`:

   ```sh
   dbtl impact --diff git:origin/main
   ```

3. **Read what the change reaches.** The report lists the changed nodes with their reasons, the downstream blast radius, column changes classified as breaking, partial-breaking, or non-breaking, the affected exposures and owners, and a ready-to-run `dbt build --select` suggestion. See [how the diff compares](../tui/diff.md#how-the-diff-compares).

4. **Gate on it.** `dbtl check` turns the findings into an exit code:

   ```sh
   dbtl check --diff git:origin/main --no-breaking
   dbtl check --diff git:origin/main --lint
   ```

   `--no-breaking` fails on breaking column changes only — columns the diff removes or renames that downstream models still reference. Type and expression changes report as partial-breaking and never trip it. With `--lint`, lint gates count only findings the change adds. See [the gate set](../cli/check-and-impact.md#the-gate-set).

5. **Put it in CI.** The [GitHub Action](../ci/github-action.md) runs `dbtl check` on each pull request, annotates the findings, and appends the report to the step summary; a sticky PR comment is opt-in via the `comment` input. On other systems, run the same `dbtl check --diff` yourself — see [other CI systems](../ci/other-ci.md).

## Check it works

- The report's `changed:` line counts added, modified, and removed nodes; `affected models:` sizes the blast radius.
- `dbtl check --diff` prints `PASS`, `WARN`, `FAIL`, or `SKIP` rows for the six diff gates — `new_layer_violations`, `untested_changes`, `breaking_columns`, `changed_hubs`, `affected_exposures`, and `blast_radius`.
- A `git:` baseline parses the committed source, so uncommitted changes in your working tree count as the current side. See [git diffs and baselines](../cli/check-and-impact.md#git-diffs-and-baselines).

## Where next

- [Diff](../tui/diff.md) — the diff lens, the `D` report, and how columns are classified.
- [Check and impact](../cli/check-and-impact.md) — every gate, flag, and report format.
- [GitHub Action](../ci/github-action.md) — annotations, SARIF, and the sticky PR comment.
