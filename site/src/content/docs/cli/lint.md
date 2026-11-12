---
title: dbtl lint
description: Run static best-practice rules over your dbt project graph, with severities, baselines, and no warehouse connection.
---

`dbtl lint` runs a set of static best-practice rules over the project graph. These are the checks that would otherwise run as models in your warehouse. `dbtl` evaluates them locally against the same graph every other surface reads. There is no warehouse connection and no `dbt run`. In source mode it does not even need `dbt parse` or `dbt compile`. A CI run takes seconds, and the results are deterministic.

```sh
dbtl lint                                   # auto-detect the data source
dbtl lint --manifest target/manifest.json
dbtl lint --select 'path:marts+'            # restrict findings to a selector
dbtl lint --rules root_models,model_fanout  # a named subset
dbtl lint --fail-on warn                    # fail on warnings too
```

Text output is one annotated block per finding: an `error[rule]`/`warning[rule]` header, a `--> file[:line[:column]]` position, the source excerpt with the span underlined when it resolves, and `= help:`/`= docs:` footers. Findings in a properties `.yml` (descriptions, tests, freshness, contract columns) point at the `- name:` entry's line and column; a `.sql` finding points at the offending syntax (the wildcard, relation name, joined table, column reference, or regex match) when the rule can pin it exactly. A closing tally reads `Found N errors and M warnings in K files` — with `K baselined`/`K pre-existing, not counted` in parentheses when a baseline or `--diff` suppresses findings — plus `N rules reported findings; M skipped (need --catalog)` and, when models carried SQL the parser could not read, `N models skipped SQL rules (unparsable SQL)`.

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

Found 10 errors and 36 warnings in 21 files
16 rules reported findings; 3 skipped (need --catalog)
```

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

The rules span:

- graph structure (`model_fanout`, `root_models`, `layer_violation`, and more)
- governance (`missing_primary_key_tests`, `naming_conventions`, `public_models_without_contract`, `references_deprecated_model`, and more)
- documentation and metadata (`undocumented_models`, `undocumented_columns`, `required_meta_keys`, and more)
- source and performance checks (`unused_sources`, `unused_models`, `incremental_without_unique_key`, and more)
- SQL-content checks (`select_star`, `hard_coded_references`, `cartesian_join`, and more)
- warehouse drift checks against `catalog.json` (`warehouse_type_drift`, `columns_missing_in_warehouse`, `undeclared_warehouse_columns`)

The [lint rules reference](../reference/lint-rules.md) lists every rule with its category, default severity, inputs, rationale, fix guidance, and parameters.

### List and explain rules

`--list-rules` and `--explain <ID>` are query modes. They read `.dbtl.yml` for severity and parameter overrides but never load a data source.

```sh
dbtl lint --list-rules            # id, category, effective severity, summary
dbtl lint --explain model_fanout  # rationale, example, fix guidance, params, docs link
```

## Severities and fail_on

Every finding carries the severity its rule resolves to. Only the rules that flag a broken guarantee or a build-time failure ship at `error` — `missing_primary_key_tests`, `public_models_without_contract`, and `unknown_column_reference`. Every other rule ships at `warn` so it reports without gating; opt a rule into `error` with `severity: error` in `.dbtl.yml` when it should fail your builds. The `fail_on` threshold decides what fails the run:

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
  rules:
    model_fanout: false   # disable a rule, or {severity: warn}, or {exclude: [<selectors>]}
```

The [project config](../configuration/project-config.md) page covers every `lint:` key: per-rule severities, parameters, `exclude` selectors, naming prefixes, and the `meta.dbtl.ignore` opt-out on a single node.

## Freeze existing violations

Adopting lint on a project that already breaks rules should not force a flag day. Two mechanisms work together.

### The baseline file

`dbtl lint --write-baseline` writes every current finding to a file. The path is the one `lint.baseline` names in `.dbtl.yml`, or `.dbtl-baseline.json` at the project root. `--write-baseline <PATH>` picks the path explicitly. The write always exits 0.

Commit the file. Baselined findings still report but never fail, and the tally counts them separately.

- `--no-baseline` ignores the file for one run.
- `--write-baseline` cannot combine with `--diff` or `--select`. A partial set must never overwrite the frozen one.
- A missing path that you configured explicitly produces a warning.
- An unreadable or malformed baseline is an input error (exit 3). A frozen set that cannot be read must never silently unfreeze.

### The diff baseline

`--diff <BASELINE>` narrows the gate to findings the diff baseline does not already carry. It compares finding keys, the same "new only" idea the layer-violation gate uses.

The two compose. Keep `main` green on a committed `.dbtl-baseline.json`, and pull requests fail only on the violations they add:

```sh
dbtl lint --write-baseline                    # writes .dbtl-baseline.json; commit it
dbtl check --lint --diff git:origin/main      # PRs fail only on NEW findings
```

## Reports and exit codes

`dbtl lint` renders the same `dbtl.report/v1` document as `dbtl check`. It accepts `--format`, `--color`, `--report <FMT>:<PATH>`, `--step-summary`, `--path-prefix`, and `--quiet`. See [report formats](check-and-impact.md#report-formats).

| Code | Meaning |
| ---- | ------- |
| `0` | Pass. |
| `1` | A finding at or above `fail_on`. |
| `2` | Usage error: an unknown rule name, conflicting flags, or a malformed `.dbtl.yml`. |
| `3` | Input error: a missing data source or an unreadable baseline. |

The [exit codes](../configuration/exit-codes.md) page has the full table.
