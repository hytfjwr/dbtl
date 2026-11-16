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

A closing tally summarizes the run. Each line appears only when it applies:

| Line | Appears when |
| ---- | ------------ |
| `Found N errors and M warnings in K files` | Always. It ends with `K baselined` or `K pre-existing, not counted` in parentheses when a baseline or `--diff` held findings back. |
| `N rules reported findings; M skipped (need --catalog)` | Always. The `skipped` part appears when rules lacked an input. |
| `N findings suppressed (K meta.dbtl.ignore, M exclude; J without a reason)` | [Suppression](#suppressed-findings) dropped at least one finding. |
| `N findings auto-fixed in M files` | A fix pass changed files. A dry run reads `auto-fixable … (dry run — no files written)`. |
| `N fix edits refused:` | A fix pass refused edits — one `file: rule — reason` line follows per refusal. |
| `N models skipped SQL rules (unparsable SQL): <name (reason), …>` | A model carried SQL the parser could not read. Each name carries why — `parse error at L:C: …`, `depth limit exceeded`, or `no query statement`. It lists up to ten names, then `+K more`. |
| `N exclude entries in .dbtl.yml without a reason` | `suppressions_without_reason` ran and at least one `exclude:` entry carried no reason. |

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

Five rules ship opt-in. `standard` skips them, and `strict` or a `rules:` entry turns them on:

| Rule | Flags | Main parameter |
| ---- | ----- | -------------- |
| `undocumented_exposures` | An exposure without a description. | none |
| `model_too_long` | A SQL model whose counted lines pass a budget. Blank lines and Jinja-comment-only lines never count. | `max_lines` |
| `layer_materializations` | A model materialized in a way its layer's `allowed` map does not list. | `allowed` |
| `models_without_unit_tests` | A model in a checked layer with no unit test. Python models and materialized views cannot carry one and never flag. | `layers` (default `marts`) |
| `suppressions_without_reason` | A node's `meta.dbtl` suppression records no reason. It can never be suppressed itself, and while it runs the summary counts `exclude:` entries without a reason. | none |

`unresolved_references` flags a literal `ref()` or `source()` call that names nothing the project or its installed packages provide, such as a typo'd model name or an undeclared source table. It pins the finding to the call itself. See the [rule reference](../reference/lint-rules.md#unresolved_references) for what it skips.

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
dbtl lint --list-rules            # id, category, severity, enabled, per-preset state, fix, summary
dbtl lint --list-rules --preset minimal   # effective state under another preset
dbtl lint --explain model_fanout  # rationale, effective severity, params, docs link
```

## Auto-fix

Some findings have exactly one correct repair. `dbtl lint --fix` applies those repairs to the model files themselves, then re-runs lint on the fixed state and reports what remains — the post-fix findings are what the exit code gates on:

```sh
dbtl lint --fix                 # apply every safe fix, then re-report
dbtl lint --fix-dry-run         # print a unified diff per file, write nothing
```

Two rules are auto-fixable today. `trailing_semicolon` removes the statement's final `;`, and `unused_cte` removes the unread CTE definition plus its separating comma — comments around the deleted definition (`--`, `/* */`, `{# #}`) stay, since they document the surviving neighbours. The `FIX` column of `--list-rules` and the `auto-fix:` line of `--explain <ID>` mark the fixable rules.

A fix is only ever applied when it is provably safe — the edit is refused, and the finding simply stays, when:

- the finding's span does not map to literal source bytes (a `;` inside an `{% if %}` arm or a CTE definition the Jinja template produces),
- the model's render dropped a `{{ }}` at a clause boundary, or stood `(NULL)` in on an `IN`/`ANY`/`SOME`/`ALL` right-hand side — the parse can't see the clause or subquery the tag emits, so no edit computed on it is safe,
- deleting the range would split a Jinja tag or a paired `{% … %}`/`{% end… %}` block,
- the result would not re-parse, or would introduce a finding under any other rule (a deleted CTE holding the last `ref()` to a model is left in place — removing it would orphan that model),
- the file on disk no longer matches the graph's raw text (a stale `manifest.json` or a mid-run edit), or the planned edits overlap.

How a fix run behaves:

- `--fix` repeats until nothing more changes, so a delete that makes another CTE unread is cleaned up in the same run. A second `--fix` is always a no-op.
- Every write goes through a temporary sibling file and an atomic rename. It preserves the file's LF or CRLF style and whether it ends with a newline.
- `--fix-dry-run` previews the same patch as a unified diff per file, then prints the post-fix report `--fix` would produce. The diff goes to stdout for the human formats and to stderr for the machine ones, so `--format json` stays parseable.
- Every refused edit is listed — `N fix edits refused:` followed by one `file: rule — reason` line per refusal in the text summary, and the same rows under `summary.fix_refused` in JSON — on `--fix` and `--fix-dry-run` alike.

Limits:

- `--fix` and `--fix-dry-run` narrow with `--rules` and `--select`. Neither flag combines with `--write-baseline`, `--list-rules`, `--explain`, or each other. `--fix-dry-run` also excludes `--quiet`.
- Fixing from a manifest needs the project root. A manifest under `<project>/target/` resolves it. A manifest anywhere else is a usage error, so pass `--source` instead.

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
- In manifest mode, a `ref()` or `source()` inside a Jinja branch dbt never rendered — a `target`-gated arm, an `{% if execute %}` body — is missing from `depends_on`, so it adds no graph edge. Rules that judge whether a node is read or is a proven leaf (`unused_sources`, `unused_models`, `model_fanout`, `rejoining_upstream`, `chained_views`) still count the call as a possible read, matching what source mode records as real edges. One limit: the default root-project scope prunes package nodes, so a root source read only from an installed package's unrendered branch still reports as unused.
- `catalog.json` is detected next to the data source. Use `--catalog <path>` to point elsewhere.
- `--sql-dialect` pins the parser dialect. Some results depend on it: `duplicate_output_columns` flags repeats of an un-aliased expression such as `sum(id), sum(id)` only under Postgres and Redshift. Source mode has no adapter to read, so pass `--sql-dialect postgres` to get that check.
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

Two mechanisms suppress a finding without disabling its rule:

- `lint.rules.<id>.exclude` selectors in `.dbtl.yml`.
- `meta.dbtl.ignore` on the node itself.

Both can record why. An `exclude` entry may be a `{select, reason}` object. `meta.dbtl.ignore` takes per-rule reasons in map form, with `meta.dbtl.reason` as the fallback. See [Suppress rules on one node](../configuration/project-config.md#suppress-rules-on-one-node).

The opt-in `suppressions_without_reason` rule audits those reasons: one finding per node whose `meta.dbtl` suppression records none, pointing at the metadata declaration. Because it judges the suppression machinery itself, neither `meta.dbtl.ignore` (including `ignore: all`) nor an `exclude:` spec can suppress it. `exclude:` entries are config declarations, never findings — while the rule runs, the summary counts the ones carrying no reason instead.

A suppressed finding never gates. It drops out of the `--fail-on` count, the baseline, and the exit code. It is also left out of the `sarif`, `junit`, `codeclimate`, `github`, and `markdown` outputs. The `github` and `markdown` summaries still count it, as in `2 suppressed`. The text and JSON summaries count it too, and `--show-suppressed` lists the findings themselves:

```sh
dbtl lint --show-suppressed
dbtl check --lint --show-suppressed    # same listing inside the check report
```

Under the flag, `text` and `concise` mark each finding `suppressed` with the mechanism and reason (`= suppressed: meta.dbtl.ignore — pk enforced upstream`). `json` adds `suppressed: true` and a `suppression: {by, reason}` object to the finding.

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

## Reports

`dbtl lint` renders the same `dbtl.report/v1` document as `dbtl check`. It accepts `--format`, `--color`, `--report <FMT>:<PATH>`, `--step-summary`, `--path-prefix`, and `--quiet`. See [report formats](check-and-impact.md#report-formats).

### JSON summary keys

The `json` report's `summary` always carries `errors`, `warnings`, `baselined`, and `skipped`. These keys appear only when they apply:

| Key | Appears when |
| --- | ------------ |
| `suppressed` | A finding was suppressed. It holds the total, the `exclude`/`meta` split, and how many recorded no reason. |
| `unparsed_models` | Models carried SQL the parser could not read. It is `{count, names, reasons}` — `reasons` maps each name to why its parse failed. |
| `exclude_entries_without_reason` | `suppressions_without_reason` ran — the count of `.dbtl.yml` `exclude:` entries carrying no reason. |
| `fixed` | The run used `--fix` or `--fix-dry-run` and at least one file changed (or would have). |
| `fix_refused` | A fix pass refused at least one edit — `[{file, rule, reason}]` per refusal, in file order. |

## Exit codes

`dbtl lint` exits 1 when a finding lands at or above `fail_on`, 2 on usage errors such as an unknown rule name or a malformed `.dbtl.yml`, and 3 on input errors such as a missing data source or an unreadable baseline. See [Exit codes](../configuration/exit-codes.md).
