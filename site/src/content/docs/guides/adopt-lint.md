---
title: Adopt lint on an existing project
description: Bring dbtl lint into a dbt project that already violates rules — tune the policy in .dbtl.yml, freeze the current findings, and let CI gate only new ones.
---

Linting a project that already breaks rules should not require a flag day. The path below freezes today's findings so that only the violations a change adds can fail a build.

## What you need

- `dbtl` installed — see [Installation](../getting-started/installation.md).
- Run the commands from your dbt project root, or point at a compiled manifest with `--manifest target/manifest.json`. No compile step and no warehouse connection are needed.

## Steps

1. **Run every rule once** to see where the project stands:

   ```sh
   dbtl lint
   ```

   The closing tally reports errors, warnings, and the rules that skipped for want of a `catalog.json`. To survey the findings without a failing exit code, add `--fail-on never`.

2. **Tune the policy to the project.** `--list-rules` shows every rule with its effective severity; `--explain <id>` shows what one rule checks and how to fix it:

   ```sh
   dbtl lint --list-rules
   dbtl lint --explain model_fanout
   ```

   Then set severities, parameters, and selector excludes in `.dbtl.yml` — the project-local file that CI and every developer share:

   ```yaml
   # .dbtl.yml
   lint:
     rules:
       model_fanout: {severity: warn, threshold: 5}
       select_star:
         exclude: ["path:models/legacy"]
   ```

   A single node opts out through `meta.dbtl.ignore`. The full key list is in [project config](../configuration/project-config.md).

3. **Freeze today's findings.** `--write-baseline` records every current finding in `.dbtl-baseline.json`. Commit the file:

   ```sh
   dbtl lint --write-baseline
   git add .dbtl-baseline.json && git commit -m "Freeze existing lint findings"
   ```

   Baselined findings still report — the tally counts them separately — but they never fail a run. See [the baseline file](../cli/lint.md#the-baseline-file).

4. **Gate only new findings in CI.** With `--diff`, lint compares finding keys against the baseline commit, so a pull request fails only on the violations it adds:

   ```sh
   dbtl check --lint --diff git:origin/main
   ```

   The [GitHub Action](../ci/github-action.md) runs the same check with annotations and a step summary. On other CI systems the same command works directly — see [other CI systems](../ci/other-ci.md).

## Check it works

- `dbtl lint` prints `No findings (N baselined)` and exits 0 on a fully frozen project.
- `dbtl check --lint` prints a `WARN`, `FAIL`, or `SKIP` row for each rule that reported or could not run, and folds the clean rules into one `PASS lint: N rules clean` line. With `--diff`, a `FAIL` row appears only when the change adds findings.
- Exit codes tell a gate failure (1) from a wrong invocation or a missing input (2 and 3). See [exit codes](../configuration/exit-codes.md).

## Where next

- [dbtl lint](../cli/lint.md) — rule inputs, severities, and how the baseline and `--diff` narrowing compose.
- [Check and impact](../cli/check-and-impact.md) — the full gate set.
- [pre-commit](../ci/pre-commit.md) — catch findings locally before they reach CI.
