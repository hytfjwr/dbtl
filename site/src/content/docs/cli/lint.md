---
title: dbtl lint
description: Run static best-practice rules over your dbt project graph, with severities, baselines, and no warehouse connection.
---

`dbtl lint` runs a set of static best-practice rules over the project graph. These are the checks that would otherwise run as models in your warehouse. `dbtl` evaluates them locally against the same graph every other surface reads. There is no warehouse connection and no `dbt run`. In source mode it does not even need `dbt parse` or `dbt compile`. A CI run takes seconds, and the results are deterministic.

```sh
dbtl lint                                   # auto-detect the data source
dbtl lint --manifest target/manifest.json
dbtl lint --select 'path:models/marts+'            # restrict findings to a selector
dbtl lint --rules root_models,model_fanout  # a named subset
dbtl lint --preset minimal                  # only the error-default rules
dbtl lint --fail-on warn                    # fail on warnings too
dbtl lint --fix                             # delete what the fixable rules flag
dbtl lint --fix-dry-run                     # preview the edits as unified diffs
```

Text output is one annotated block per finding. The header names the severity and rule (`error[rule]` or `warning[rule]`), followed by a `--> file[:line[:column]]` position, the source excerpt with the span underlined when it resolves, and `= help:`/`= docs:` footers.

Findings in a properties `.yml` (descriptions, tests, freshness, contract columns) point at the `- name:` entry's line and column. A `.sql` finding points at the offending syntax — the wildcard, relation name, joined table, column reference, regex match, trailing semicolon, duplicated output column, unread CTE, or unbounded `ORDER BY` — when the rule can pin it exactly.

A closing tally reads `Found N errors and M warnings in K files`, with `K baselined` or `K pre-existing, not counted` in parentheses when a baseline or `--diff` suppresses findings. It then reports `N rules reported findings; M skipped (need --catalog)`, `N findings suppressed (K meta.dbtl.ignore, M exclude; J without a reason)` when [suppression](#suppressed-findings) dropped any, `N findings auto-fixed in M files` (or `auto-fixable … (dry run — no files written)`) when a fix pass changed files, and, when models carried SQL the parser could not read, `N models skipped SQL rules (unparsable SQL): <names>` — up to ten model names, then `+K more`.

```text
error[public_models_without_contract]: fct_public_api is access: public without an enforced contract
  --> models/marts/governance.yml:15:11
   |
15 |   - name: fct_public_api
   |           ^^^^^^^^^^^^^^ declared here
   |
   = help: set contract: { enforced: true } on the model
   = docs: https://hytfjwr.github.io/dbtl/reference/lint-rules/#public_models_without_contract

warning[select_star]: fct_daily_orders's final SELECT uses select * / t.*
 --> models/marts/fct_daily_orders.sql:4:8
  |
4 | select * from {{ ref('stg_orders') }}
  |        ^ wildcard
  |
  = help: list the columns the model produces
  = docs: https://hytfjwr.github.io/dbtl/reference/lint-rules/#select_star

…

Found 10 errors and 38 warnings in 21 files
17 rules reported findings; 3 skipped (need --catalog)
```

The tally above is `dbtl lint --source tests/fixtures/sample_project` on the repository's sample fixture — yours will differ.

`--format concise` prints the same findings as `file[:line[:column]]: severity[rule] message` one-liners for editors and grep.

```text
models/marts/cll_syntax.sql:24:5: warning[select_star] cll_syntax's final SELECT uses select * / t.*
models/marts/fct_daily_orders.sql:4:8: warning[select_star] fct_daily_orders's final SELECT uses select * / t.*
models/marts/governance.yml:15:11: error[public_models_without_contract] fct_public_api is access: public without an enforced contract
models/marts/schema.yml:4:11: error[missing_primary_key_tests] fct_orders has no unique+not_null test pair (or unique_combination_of_columns)
…
```

`--color auto|always|never` controls ANSI styling of the human formats on stdout (`auto` is the default; report files and machine formats never carry color).

`dbtl check --lint` folds the same evaluation into the [check gate set](check-and-impact.md). The two can never disagree. Setting `check.lint: true` in `.dbtl.yml` makes it the default for `check` runs.

One findings list feeds every surface, so the numbers cannot drift apart:

- `dbtl check --lint` gates on it.
- `dbtl lint` reports it.
- [`dbtl docs`](docs.md) puts a summary in the index README and a "Lint violations" section on each page.
- The TUI reads it through the Lint lens, the `F` filter, `n` and `N` cycling, inspector findings, the `S` worklist, and the home tab.

## The rule set

The [lint rules reference](../reference/lint-rules.md) lists every rule with its category, default severity, inputs, rationale, fix guidance, and parameters. Rules in the `warehouse` category run against `catalog.json` and are skipped without one.

Three rules ship opt-in — `undocumented_exposures` (an exposure without a description), `model_too_long` (a SQL model's counted lines past the `max_lines` budget — blank and Jinja-comment-only lines never count), and `layer_materializations` (a model materialized in a way its layer's `allowed` map doesn't list). `standard` skips them; `strict` or a `rules:` entry turns them on.

### Presets

A preset picks the base rule set a run starts from. `.dbtl.yml`'s `lint.preset` sets it per project and `--preset` overrides it per run:

| Preset | Rules |
| ------ | ----- |
| `minimal` | Only the rules whose default severity is `error`. |
| `standard` (default) | Every rule the registry enables by default. |
| `strict` | Every rule, opt-in included. |

Above the preset, `lint.categories` pins a whole category to `error`/`warn`/`off`, and `lint.rules.<id>` still wins for a single rule — a category or preset can disable a rule and a `rules:` entry enables it back. `--rules` stays the authoritative selector on top of everything: unnamed rules go off and a named rule runs even when a `rules:`, `categories:`, or `preset` layer turned it off. The [project config](../configuration/project-config.md#rule-set-resolution-order) page has the full resolution order.

### List and explain rules

`--list-rules` and `--explain <ID>` are query modes. They read `.dbtl.yml` for severity and parameter overrides but never load a data source.

```sh
dbtl lint --list-rules            # id, category, default + effective severity, enabled
dbtl lint --list-rules --preset minimal   # effective state under another preset
dbtl lint --explain model_fanout  # rationale, effective severity, params, docs link
```

The `FIX` column marks the rules `dbtl lint --fix` can repair (`auto`) — today `trailing_semicolon` and `unused_cte`. `--explain <ID>` reports the same thing on its `auto-fix:` line.

## Auto-fix

Some findings have exactly one correct repair. `dbtl lint --fix` applies those repairs to the model files themselves, then re-runs lint on the fixed state and reports what remains — the post-fix findings are what the exit code gates on:

```sh
dbtl lint --fix                 # apply every safe fix, then re-report
dbtl lint --fix-dry-run         # print a unified diff per file, write nothing
```

Today two rules are auto-fixable: `trailing_semicolon` removes the statement's final `;`, and `unused_cte` removes the unread CTE definition plus its separating comma. The `--list-rules` `FIX` column and `--explain`'s `auto-fix:` line mark them.

A fix is only ever applied when it is provably safe — the edit is refused, and the finding simply stays, when:

- the finding's span does not map to literal source bytes (a `;` inside an `{% if %}` arm or a CTE definition the Jinja template produces),
- deleting the range would split a Jinja tag or a paired `{% … %}`/`{% end… %}` block,
- the result would not re-parse, or would introduce a finding under any other rule (a deleted CTE holding the last `ref()` to a model is left in place — removing it would orphan that model),
- the file on disk no longer matches the graph's raw text (a stale `manifest.json` or a mid-run edit), or the planned edits overlap.

`--fix` repeats to a fixpoint, so a delete that makes another CTE unread is cleaned up in the same run, and a second `--fix` is always a no-op. Every write goes through a temporary sibling file and an atomic rename, and preserves the file's LF/CRLF style and whether it ends with a newline. `--fix-dry-run` previews the same patch as a unified diff per file — on stdout for the human formats, stderr for the machine ones, so `--format json` stays parseable — followed by the same post-fix report `--fix` would produce.

Fixes narrow with `--rules` and `--select` like the findings do. Neither flag combines with `--write-baseline`, `--list-rules`, `--explain`, or each other; `--fix-dry-run` also excludes `--quiet`. Fixing from a manifest needs the project root — a manifest under `<project>/target/` resolves it; a manifest anywhere else is a usage error, so pass `--source` for those.

## Severities and fail_on

Every finding carries the severity its rule resolves to; each rule's default severity is listed in the [lint rules reference](../reference/lint-rules.md). Only the rules that flag a broken guarantee or a build-time failure ship at `error`. Every other rule ships at `warn` so it reports without gating; opt a rule into `error` with `severity: error` in `.dbtl.yml` when it should fail your builds. The `fail_on` threshold decides what fails the run:

| `fail_on` | Result |
| --------- | ------ |
| `error` (default) | Any error finding fails the run (exit 1). Warnings are reported only. |
| `warn` | Warnings fail the run too. |
| `never` | Nothing fails. The run only reports. |

The precedence is `--fail-on`, then the top-level `fail_on` in `.dbtl.yml`, then `error`.

## Rule inputs and SKIP

Each rule declares what it needs: the graph alone, parsed model SQL, or the warehouse `catalog.json` record. A rule whose input is absent never runs — `dbtl check` prints a `SKIP lint <rule>: requires …` row and `dbtl lint` counts it on the summary line (`M skipped (need --catalog)`). That is informational and never a failure.

- `dbtl` parses SQL lazily per model and reuses the result across rules.
- `catalog.json` is detected next to the data source. Use `--catalog <path>` to point elsewhere.
- `--sql-dialect` pins the parser dialect.
- The TUI runs the graph and catalog rules only. Parsing every model up front would stall startup.

## Configure rules

Rule policy lives in the project-local `.dbtl.yml`. It is deliberately not the user-level `config.yml`, so a CI gate cannot vary with the developer's machine.

```yaml
# .dbtl.yml. Every key is optional; absent rules stay enabled.
fail_on: warn             # error (default) | warn | never
lint:
  preset: minimal         # the base rule set; --preset overrides it
  categories:
    sql: error            # one severity for a whole category
  rules:
    model_fanout: false   # disable a rule, or {severity: warn}, or {exclude: [<selectors>]}
```

The [project config](../configuration/project-config.md) page covers every `lint:` key: presets, category severities, per-rule severities, parameters, `exclude` selectors, naming prefixes, and the `meta.dbtl.ignore` opt-out on a single node.

## Suppressed findings

Two mechanisms suppress a finding without disabling its rule: `lint.rules.<id>.exclude` selectors in `.dbtl.yml` and `meta.dbtl.ignore` on the node itself. Both can record *why* — an `exclude` entry may carry `reason:` as a `{select, reason}` object, and `meta.dbtl.ignore` takes per-rule reasons in map form with `meta.dbtl.reason` as the fallback. See [Suppress rules on one node](../configuration/project-config.md#suppress-rules-on-one-node).

A suppressed finding never gates: it drops out of the `--fail-on` count, the baseline, the exit code, and the `sarif`/`junit`/`codeclimate`/`github` outputs entirely. It is not invisible, though — the text and JSON summaries always count it (`summary.suppressed` carries the total, the `exclude`/`meta` split, and how many recorded no reason), and `--show-suppressed` lists the findings themselves:

```sh
dbtl lint --show-suppressed
dbtl check --lint --show-suppressed    # same listing inside the check report
```

Under the flag, `text`/`concise` mark each finding `suppressed` with the mechanism and reason (`= suppressed: meta.dbtl.ignore — pk enforced upstream`), and `json` adds `suppressed: true` and a `suppression: {by, reason}` object to the finding. The `json` summary also names the models the SQL rules could not parse under `summary.unparsed_models` (`{count, names}`).

## Freeze existing violations

Adopting lint on a project that already breaks rules should not force a flag day. Two mechanisms work together.

### The baseline file

`dbtl lint --write-baseline` writes every current finding to a file. The path is the one `lint.baseline` names in `.dbtl.yml`, or `.dbtl-baseline.json` at the project root. `--write-baseline <PATH>` picks the path explicitly. The write exits 0.

Commit the file. Baselined findings still report but never fail, and the tally counts them separately.

- `--no-baseline` ignores the file for one run.
- `--write-baseline` cannot combine with `--diff` or `--select`. A partial set must never overwrite the frozen one.
- `--write-baseline` combined with `--rules` merges instead of replacing: only rows for rules that actually ran are rewritten — rows for filtered-out or skipped rules keep their frozen entries, and the write reports how many were kept.
- A missing path that you configured explicitly produces a warning.
- An unreadable or malformed baseline is an input error (exit 3) — even for `--write-baseline`, which refuses to clobber a frozen set it cannot read. A frozen set that cannot be read must never silently unfreeze.

### The diff baseline

`--diff <BASELINE>` narrows the gate to findings the diff baseline does not already carry. It compares finding keys, the same "new only" idea the layer-violation gate uses.

The two compose. Keep `main` green on a committed `.dbtl-baseline.json`, and pull requests fail only on the violations they add:

```sh
dbtl lint --write-baseline                    # writes .dbtl-baseline.json; commit it
dbtl check --lint --diff git:origin/main      # PRs fail only on NEW findings
```

## Reports and exit codes

`dbtl lint` renders the same `dbtl.report/v1` document as `dbtl check`. It accepts `--format`, `--color`, `--report <FMT>:<PATH>`, `--step-summary`, `--path-prefix`, and `--quiet`. See [report formats](check-and-impact.md#report-formats).

`dbtl lint` exits 1 when a finding lands at or above `fail_on`, 2 on usage errors such as an unknown rule name or a malformed `.dbtl.yml`, and 3 on input errors such as a missing data source or an unreadable baseline. See [Exit codes](../configuration/exit-codes.md).
