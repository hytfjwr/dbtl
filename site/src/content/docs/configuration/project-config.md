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

Every key is optional, and a rule you do not mention stays enabled at its default. The blocks below cover the sections one at a time. Each is a valid `.dbtl.yml` on its own. Put blocks that share the `lint:` key together under one `lint:`.

Top-level keys and the rule set:

```yaml
fail_on: warn            # error | warn | never
owner_meta_key: owner    # meta key for the affected-owners report

lint:
  preset: minimal        # minimal | standard (default) | strict
  categories:            # one severity per rule category
    sql: error
    documentation: off
  # baseline: .dbtl-baseline.json   # the freeze file (this is the default)
```

Per-rule settings. A rule entry is `false`, `true`, or an object with `severity`, `exclude`, and the rule's own parameters:

```yaml
lint:
  rules:
    root_models: false          # off
    model_fanout:
      severity: warn            # error | warn | off
      threshold: 5              # a declared parameter
      exclude:                  # suppress findings on matching nodes
        - "stg_legacy_*"
        - "path:models/legacy"
        - {select: "tag:wip", reason: "WIP models ship knowingly"}
    select_star:
      allow_final_star_over_single_cte: true
    rejoining_upstream:
      ignore_shared_parents: ["dim_*"]
    required_meta_keys:
      keys: [owner, domain]
      resource_types: [model, source]   # default: [model]
    layer_materializations:
      allowed:                          # layer -> permitted materializations
        staging: [view, ephemeral]
        marts: [table, incremental]
    models_without_unit_tests:
      layers: [marts]                   # same layer names as `allowed`
    exposures_dependent_on_private_models:
      allowed_access: [public, protected]
  naming_prefixes:                      # per-layer merge over the defaults
    staging: [stg_, base_]
    marts: []
    other: [rpt_]
```

Gates and docs options:

```yaml
check:
  lint: true                    # fold the lint gates into `dbtl check`
  gates:
    coverage: {severity: error, min: 80}
    changed_hubs: {min_downstream: 10}
    docs_drift: {dir: docs/lineage}

docs:
  lineage_depth: 2              # an integer >= 1, or all
  catalog: false                # opt in to the sibling catalog.json
  run_results: false            # opt in to the sibling run_results.json
```

## lint

The `lint:` section configures the [lint rules](../cli/lint.md).

| Key | Meaning |
| --- | ------- |
| `preset` | `minimal`, `standard` (the default), or `strict`. The base rule set a run starts from. See [Presets](../cli/lint.md#presets). Unknown names are an error. |
| `categories` | Maps a rule category to `error`, `warn`, or `off`. The valid categories are `modeling`, `testing`, `documentation`, `structure`, `performance`, `governance`, `sql`, and `warehouse`. Unknown names are an error. |
| `rules` | Maps a rule id to `false` (off), `true` (on), or a settings object. Unknown rule names are an error. |
| `baseline` | The frozen-violations file. Default: `.dbtl-baseline.json`. It resolves against the directory of `.dbtl.yml`. |
| `naming_prefixes` | Maps a layer directory to its allowed prefixes. It merges per layer over the defaults: `intermediate: [int_]`, `marts: [fct_, dim_, rpt_]`, `staging: [stg_]`. The layers you name here also become valid layer names for `layer_materializations.allowed` and `models_without_unit_tests.layers`, next to `staging`, `intermediate`, `marts`, and `other`. |
| `model_fanout_threshold`, `source_fanout_threshold` | Flat keys for the fanout thresholds. Defaults: 5 and 2. They set the same values as `rules.<id>.threshold`. Using both spellings together is an error that names both keys. |

A rule entry can set:

| Key | Meaning |
| --- | ------- |
| `severity` | `error`, `warn`, or `off`. `off` disables the rule. |
| `exclude` | A list of [selectors](../cli/selectors.md). Each item is a selector string, or an object `{select: <selector>, reason: <why>}`. Findings on matching nodes are suppressed, and the reason shows under `--show-suppressed`. |
| rule parameters | Declared per rule. An undeclared name is an error. |

Parameters are listed per rule in the [lint rules reference](../reference/lint-rules.md).

### Rule-set resolution order

A rule's enablement and severity resolve in this order — highest first:

1. An explicit `rules.<id>` entry.
2. Its category's `categories.<category>` setting.
3. The `preset` set (or `--preset`).
4. The rule's registry default.

In this order, `categories.<category>: off` disables every rule in the category, and `error` or `warn` enables them at that severity. A rule set to `true` runs at its category's severity when one is set, and at its registry default otherwise.

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

A node can suppress rules through `meta.dbtl.ignore`. dbt's `+meta` inheritance from `dbt_project.yml` applies as usual. The value takes one of three forms:

| Form | Effect |
| ---- | ------ |
| A list of rule ids | Suppresses those rules on the node. |
| The string `"all"` | Suppresses every rule on the node. |
| A map of `<rule id or all>: <reason>` | Suppresses the named rules, each with its own reason. A `~` value has no reason of its own. |

```yaml
models:
  - name: stg_legacy_orders
    meta:
      dbtl:
        ignore: [root_models, naming_conventions]
        reason: "legacy import, tracked in DEC-482"

  - name: stg_legacy_payments
    meta:
      dbtl:
        ignore:
          root_models: "no upstream by design"
          naming_conventions: ~       # uses meta.dbtl.reason below
        reason: "legacy import, tracked in DEC-482"
```

`meta.dbtl.reason` is the fallback reason for the list form, for `"all"`, and for map entries whose value is `~`. In a map, a rule's own key wins over `all`. Reasons appear under `--show-suppressed`. See [suppressed findings](../cli/lint.md#suppressed-findings).

The opt-in [`suppressions_without_reason`](../reference/lint-rules.md#suppressions_without_reason) rule enforces this convention — one finding per node whose suppression records no reason. It audits the suppression machinery itself, so neither `meta.dbtl.ignore` (naming it or via `all`) nor an `exclude:` entry can suppress it.

A value that suppresses nothing is reported once per node as a load warning:

- `ignore` is none of the three forms, including an empty map or a map value that is not a string.
- `ignore` names a rule id that does not exist.
- `reason` is a list or a map instead of a string, number, or boolean.

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
