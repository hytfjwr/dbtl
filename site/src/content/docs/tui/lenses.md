---
title: Lenses
description: Tint the lineage by test coverage, blast radius, layer, lint findings, diff changes, last-run status, runtime, column impact, or primary-key tests.
---

A lens tints the lineage nodes by one property, so a question such as "which of these models have no tests?" becomes visible at a glance. Only one lens is active at a time.

## Cycle lenses

Press `t` to move to the next lens. The cycle runs in this order, then wraps back to off:

1. off
2. coverage
3. blast radius
4. layer
5. layer violation
6. lint
7. diff
8. run status
9. runtime
10. column impact
11. primary key

Lenses with no data are skipped. The column-impact lens is reachable only while a column is traced. The active lens shows as a `[lens:…]` tag in the lineage title.

## Available lenses

One lens appears under three names: the name this page uses, the value the `lens` key in your [user config](../configuration/user-config.md) takes, and the `[lens:…]` tag the lineage title shows.

| Lens | Config value | Title tag | Tints nodes by | Needs |
|---|---|---|---|---|
| Coverage | `coverage` | `[lens:coverage]` | Test coverage | Nothing |
| Blast radius | `degree-heat` | `[lens:heat]` | Size of the downstream impact, on a low, mid, high ramp | Nothing |
| Layer | `layer` | `[lens:layer]` | The layer the node belongs to | Nothing |
| Layer violation | `layer-violation` | `[lens:violation]` | Layer violations | Nothing |
| Lint | `lint` | `[lens:lint]` | Static lint findings | Nothing |
| Diff | `diff` | `[lens:diff]` | Changes against a `--diff` baseline | `--diff` |
| Run status | `run-status` | `[lens:run]` | Status in the last run | `run_results.json` |
| Runtime | `run-time` | `[lens:time]` | Execution time in the last run | `run_results.json` |
| Column impact | `column-impact` | `[lens:col]` | The downstream closure of a traced column | A traced column |
| Primary key | `primary-key` | `[lens:pk]` | Whether the model has a primary-key test | Nothing |

### Coverage

Tints nodes by test coverage. Press `T` to filter the model list to untested models. Press `n` or `N` to jump to the next or previous untested model.

### Blast radius

Tints nodes by how much sits downstream of them. The ramp uses a low, mid, and high tint.

### Layer and layer violation

The layer lens tints nodes by their layer, such as staging, intermediate, and marts. The layer-violation lens highlights backward edges between logical layers. An example is a staging model that reads from a mart.

The same check runs as the `layer_violation` lint rule and as the `new_layer_violations` gate in [`dbtl check`](../cli/check-and-impact.md).

### Lint

Tints nodes by static lint findings. While this lens is active, `n` and `N` jump between lint findings instead of untested models. The `F` key filters the model list to models with findings. The inspector lists the findings for the node under the cursor. See [Lint](../cli/lint.md).

### Diff

Shows added nodes in green and modified nodes in amber. Available only when you start `dbtl` with `--diff`. See [Diff](diff.md).

### Run status

Needs `run_results.json`. Error and fail show red, warn shows yellow, and skipped shows muted. Successful nodes keep their normal color. See [Run results and catalog](run-results-and-catalog.md).

### Runtime

Needs `run_results.json`. Tints nodes by execution time on the same low, mid, high ramp as the blast-radius lens.

### Column impact

Available while you trace a column. Gold marks the downstream closure of the traced column. See [Column trace](lineage.md#column-trace).

### Primary key

Tints models that lack a primary-key test. A model passes with either of these:

- a `unique` and a `not_null` test on the same column
- a `unique_combination_of_columns` test

This is the same guarantee the `missing_primary_key_tests` lint rule evaluates. See the [lint rules reference](../reference/lint-rules.md).

## Stats dashboard

Press `S` to open the project stats dashboard. With run results loaded, it includes a runtime-weighted critical path. It also includes a worklist of lint findings.

## Export from the TUI

The lineage can be copied or exported as Mermaid (`m`), Graphviz DOT (`x`), an ASCII diagram (`c`), or a Markdown impact report (`i`). See [Copy and export](copy-and-export.md).

## Set a default lens

Set `lens` in your [user config](../configuration/user-config.md) to start with a lens. The accepted values are the config values in the table above, plus `off`. A project you already visited restores its saved lens instead.
