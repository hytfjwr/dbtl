---
title: Project config
description: Set lint rules, check gates, docs options, and owner metadata for a dbt project in the project-local .dbtl.yml file.
---

`.dbtl.yml` holds project policy: lint rules, check gates, and docs options. It lives in the dbt project, so CI runs and developer machines apply the same policy. User-level TUI preferences live in a different file. See [User config](user-config.md).

## Where dbtl reads it

`dbtl` looks for `.dbtl.yml` beside the data source:

- In source mode, it reads the project directory.
- In manifest mode, it reads the project root first and falls back to `target/`.

The TUI re-reads the file on `r` reload.

Unknown top-level keys are ignored with a warning. Unknown keys inside `lint:`, `check:`, or `docs:`, or inside a rule or gate entry, are errors. A broken `.dbtl.yml` exits with code 2.

## Editor completion

Both config files have a generated JSON Schema. Point yaml-language-server at it for completion and validation:

```yaml
# .dbtl.yml
# yaml-language-server: $schema=https://raw.githubusercontent.com/hytfjwr/dbtl/main/schema/dbtl.schema.json
```

## Top-level keys

| Key | Meaning |
| --- | ------- |
| `fail_on` | `error` (default), `warn`, or `never`. The minimum severity at which findings fail a run. `--fail-on` overrides it. |
| `lint` | The rule-set preset, per-category severities, per-rule enables/severities/parameters, excludes, naming prefixes, and the baseline path. |
| `check` | Gate severities and parameters, and whether the lint gates fold into `dbtl check`. |
| `docs` | Options for [`dbtl docs`](../cli/docs.md). |
| `owner_meta_key` | The `meta` key the affected-owners report reads. Default: `owner`. It wins over the `owner_meta_key` in `config.yml`. |

## Example

```yaml
# .dbtl.yml. Every key is optional; absent rules stay enabled at their default.
fail_on: warn                # error | warn | never
owner_meta_key: owner        # meta key for the affected-owners report

lint:
  preset: minimal             # minimal | standard (default) | strict
  categories:                 # one severity per rule category, above the preset
    sql: error                #   and below per-rule `rules:` entries
    documentation: off
  baseline: .dbtl-baseline.json   # the freeze file (default shown)
  rules:
    root_models: false            # bool: off, or on at default severity
    model_fanout:
      severity: warn              # error | warn | off
      threshold: 5                # declared params only; unknown keys error
      exclude:                    # dbt selector specs: skip findings on matches
        - "stg_legacy_*"
        - "path:legacy"
        - "tag:wip"
    select_star:
      allow_final_star_over_single_cte: true   # `select *` closing one
                                             #   all-explicit-columns CTE is fine
    rejoining_upstream:
      ignore_shared_parents: ["dim_*"]   # selector list: exempt shared
                                         #   parents matching it
  model_fanout_threshold: 5       # compat flat keys; an error if combined
  source_fanout_threshold: 2      #   with the nested param form
  naming_prefixes:                # per-layer merge over the defaults
    staging: [stg_, base_]
    marts: []
    other: [rpt_]

check:
  lint: true                      # fold the lint gates into `dbtl check`
  gates:
    coverage: {severity: error, min: 80}
    changed_hubs: {min_downstream: 10}
    docs_drift: {dir: docs/lineage}

docs:
  lineage_depth: 2                # an integer >= 1, or all
  catalog: false                  # opt in to the sibling catalog.json
  run_results: false              # opt in to the sibling run_results.json
```

## lint

The `lint:` section configures the [lint rules](../cli/lint.md).

| Key | Meaning |
| --- | ------- |
| `preset` | `minimal`, `standard` (the default), or `strict`. The base rule set a run starts from: `minimal` enables only the rules whose default severity is `error`, `standard` enables every rule the registry enables by default, and `strict` enables every rule, opt-in included. Unknown names are an error. |
| `categories` | Maps a rule category to `error`, `warn`, or `off` — one severity (and enable/disable) for a whole category at once. `off` disables the category; `error`/`warn` enable its rules at that severity. It sits above `preset` and below `rules.<id>`. Unknown category names are an error. |
| `rules` | Maps a rule id to `false` (off), `true` (on — at the `categories` severity when one is set, else the registry default), or a settings object. Unknown rule names are an error. |
| `baseline` | The frozen-violations file. Default: `.dbtl-baseline.json`. It resolves against the directory of `.dbtl.yml`. |
| `naming_prefixes` | Maps a layer directory to its allowed prefixes. It merges per layer over the defaults: `intermediate: [int_]`, `marts: [fct_, dim_, rpt_]`, `staging: [stg_]`. |
| `model_fanout_threshold`, `source_fanout_threshold` | Flat keys for the fanout thresholds. Defaults: 5 and 2. They set the same values as `rules.<id>.threshold`. Using both spellings together is an error that names both keys. |

A rule entry can set:

| Key | Meaning |
| --- | ------- |
| `severity` | `error`, `warn`, or `off`. `off` disables the rule. |
| `exclude` | A list of [selectors](../cli/selectors.md). Findings on matching nodes are dropped. |
| rule parameters | Declared per rule. An undeclared name is an error. |

Parameters are listed per rule in the [lint rules reference](../reference/lint-rules.md).

### Rule-set resolution order

A rule's enablement and severity resolve in this order — highest first:

1. An explicit `rules.<id>` entry.
2. Its category's `categories.<category>` setting.
3. The `preset` set (or `--preset`).
4. The rule's registry default.

`--rules` is different: it is an authoritative selector above all of these — every rule not named goes off, and a named rule runs even when a `rules.<id>`, `categories`, or `preset` entry turned it off.

### Shared dimensions

A shared dimension feeds both a fact and its downstream, so two rules flag the same shape on purpose. Exempt the dimension — which nodes each key matches differs: `exclude` drops findings on matching nodes (the flagged side), while `ignore_shared_parents` drops matching shared parents (the dim itself).

```yaml
lint:
  rules:
    rejoining_upstream:
      ignore_shared_parents: ["dim_*"]   # a dim joining at both hops is fine
    model_fanout:
      exclude: ["dim_*"]                 # a dim legitimately feeds many marts
```

### Suppress rules on one node

A node can suppress rules through its `meta`. dbt's `+meta` inheritance from `dbt_project.yml` applies as usual.

```yaml
models:
  - name: stg_legacy_orders
    meta:
      dbtl:
        ignore: [root_models, naming_conventions]   # or the string "all"
        reason: "legacy import — tracked in DEC-482"      # optional; for humans
```

`dbtl` reads `dbtl.ignore` only — sibling keys like `reason:` are a convention for recording why the suppression exists; the linter ignores them.

## check

The `check:` section configures [`dbtl check`](../cli/check-and-impact.md).

| Key | Meaning |
| --- | ------- |
| `lint` | When `true`, include the lint gates without `--lint`. Default: `false`. |
| `gates` | Maps a gate id to a bare severity (`error`, `warn`, `off`) or to `{severity, <params>}`. Unknown ids and params are errors. |

An entry that sets only parameters resolves to `error` when the gate is off by default. The [config reference](../reference/config.md#checkgatesgate-id) lists every gate with its default, parameters, and whether it needs `--diff`.

## docs

The `docs:` section configures [`dbtl docs`](../cli/docs.md).

| Key | Meaning |
| --- | ------- |
| `catalog` | When `true`, opt in to the sibling `catalog.json`. Default: `false`. |
| `run_results` | When `true`, opt in to the sibling `run_results.json`. Default: `false`. |
| `lineage_depth` | Mermaid depth per node page: an integer of 1 or more, or `all`. Default: `2`. `--lineage-depth` overrides it. |

## Precedence

Command-line flags win over `.dbtl.yml`. For the failure threshold the order is `--fail-on`, then `fail_on` in `.dbtl.yml`, then `error`. For the lint rule set, `--preset` overrides `lint.preset`, and `--rules` selects the final rule list above every `lint` layer. For gates, `--gate` and the parameter flags such as `--coverage-min` override `check.gates`.

## Full reference

Every key, type, and default is in the [config reference](../reference/config.md).
