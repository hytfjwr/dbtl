---
title: Source mode
description: What dbtl reads and approximates when it builds the graph straight from dbt project files, with no dbt compile and no manifest.json.
---

Source mode builds the lineage graph from the project files themselves. It never runs `dbt`, so it needs no `target/manifest.json`. `dbtl` uses it when there is no manifest to auto-detect, or when you pass `--source <DIR>`. See [choose a data source](../cli/overview.md#choose-the-data-source).

Source mode aims to produce the same graph as a real `dbt parse` for the common project shape. The repository's test suite compares both loaders on one fixture project. It is still an approximation, and this page lists where it stops.

## What source mode reads

| Input | What `dbtl` takes from it |
| ----- | ------------------------- |
| `dbt_project.yml` | The project name, the resource paths (`model-paths`, `seed-paths`, `snapshot-paths`, `macro-paths`, `docs-paths`), and the `models:`, `seeds:`, `snapshots:`, and `sources:` config trees. |
| Model `.sql` files | Literal `ref()` and `source()` calls, and `config()` arguments. |
| `schema.yml` files | Sources, models, columns, descriptions, tests, `config:` blocks, exposures, and unit tests. |
| Seed `.csv` files | One seed per file, with the header row as its columns. |
| Snapshots | `{% snapshot %}` blocks in `.sql` files, and YAML-defined snapshots. |
| Project macros | The `ref()` and `source()` calls inside macros that your models invoke. |
| `.md` files | `{% docs %}` blocks, so `{{ doc('name') }}` in a description expands the way dbt renders it. |
| `target/compiled/` | Compiled SQL for the SQL preview and the SQL rules, when you have run `dbt compile`. It never changes the edges. |

Two details about the resource paths:

- `data-paths` and `source-paths` are accepted as the older names of `seed-paths` and `model-paths`. If a file declares both `seed-paths` and `data-paths`, `seed-paths` wins with a warning.
- YAML property files are read only under the configured model, seed, and snapshot paths. `target/`, `dbt_packages/`, and stray YAML elsewhere are never read.

## How configs resolve

A setting can come from several places, and the most specific one wins. For `materialized` the order is:

1. `config()` in the model file.
2. A `config:` block in `schema.yml`.
3. The `dbt_project.yml` config tree, where the deepest matching path wins.
4. dbt's default, `view`.

Governance settings follow the same order: `access`, `group`, `contract`, `incremental_strategy`, and `language`. `constraints` and `deprecation_date` come from `schema.yml` only. A model with no `access` reads as `protected`, like dbt's default.

`config()` takes either keyword arguments or a single positional dict. `config(materialized='table')` and `config({'materialized': 'table'})` mean the same. `meta` merges per top-level key, with deeper levels winning.

Source tables resolve `meta`, `freshness`, `loaded_at_field`, and `tags` through the table, source, and `sources:` tree levels. Only a table's `config: enabled:` disables it.

## How edges are found

Edges come from literal `ref()` and `source()` calls found by scanning the SQL. Jinja is not executed to find them. Some details follow dbt:

- A `{# … #}` comment hides a `ref()`, but a `--` or `/* … */` SQL comment does not. dbt renders Jinja before SQL comments exist, so a commented-out `ref()` is still a dependency.
- A `ref()` that names nothing the project defines is dropped. The [`unresolved_references`](../reference/lint-rules.md#unresolved_references) lint rule reports it.
- A project macro that wraps `ref()` or `source()` produces edges. The call's literal arguments bind to the macro's parameters, and nested project macros are followed up to a depth limit. A macro's `{% if %}` branches are not evaluated, so the calls in every branch become edges. A possible edge is better than a missing one.
- A call that cannot be resolved adds no edge. That covers a macro from an installed package, a `var()` or computed argument, and a call past the depth limit. Lint rules that judge a node's parents or children treat such a node as unprovable and stay quiet.

## Enabled expressions and `--vars`

`enabled` can be a literal or a Jinja expression, in `config()`, in a `schema.yml` `enabled:` or `config: enabled:`, or in a `+enabled` key of `dbt_project.yml`. Source mode evaluates this subset:

- Literals, and `var('name')` or `var('name', default)`.
- Comparisons such as `==`, `!=`, and `<=`.
- `and`, `or`, and `not`.
- `in` over a literal list, and parentheses.
- The `| as_bool` filter.

A resource is dropped only when its expression evaluates to a proven `false`. An expression outside the subset, such as `target.name` or another macro call, or a `var()` with no value, keeps the resource enabled and warns once.

Variables resolve in these scopes:

- `+enabled` keys in `dbt_project.yml` see only the top level of `vars.yml`. dbt renders that file before it loads its own `vars:`.
- Every other `enabled` expression sees `vars.yml` when the project has one, and `dbt_project.yml`'s `vars:` otherwise. A project-scoped key shadows a top-level key.
- `--vars '<YAML|JSON>'` outranks both everywhere, in the graph, a source-parsed `--diff` baseline, and TUI reloads. Manifest mode ignores `--vars` with a warning, because the manifest already carries dbt's rendered values.

## Tests, docs, and other resources

- Generic tests in `schema.yml` become test records on their model, column, or source. They are never graph nodes, the same as in manifest mode.
- Unit tests under `unit_tests:` attach to their `model:`. A `versions:` filter expands to the `<model>_v<version>` files that dbt's naming convention requires.
- Exposures are read from YAML. A `semantic_models:` entry only marks its `model:` as consumed, so rules such as `unused_models` see the consumer.
- Doc blocks resolve from the project's own `.md` files. A `doc()` call that cannot resolve keeps its raw text and warns, where dbt would fail.

## What source mode approximates or skips

- **Versioned models.** A `ref('x', v=2)` is recorded against the model `x`. The `.vN` nodes that a `versions:` block defines are not synthesized, so a versioned manifest shows one node per version where source mode shows the files. The version names are still read, so `unresolved_references` knows them.
- **Jinja in SQL.** SQL rules and column lineage render the Jinja in a model approximately, only to parse it. `{% set %}` and `{% macro %}` bodies are blanked, mutually exclusive `{% if %}` arms render as one arm, and an expression `dbtl` cannot evaluate becomes a generated name. The graph never depends on that render.
- **Installed packages.** Only the root project is parsed. `dbt_packages/` is never read for nodes, a `ref` into another package drops out, and `--include-packages` does nothing but print a warning. Package names are read only so `unresolved_references` can tell a typo from a package that is not installed.
- **Other resource kinds.** Python models, singular tests, analyses, metrics, and saved queries are not read.
- **Adapter and profile data.** Nothing from `profiles.yml` or the warehouse is available. The `database` and `schema` of non-source nodes are unknown, and `target.*` expressions are not evaluated. The SQL dialect is `generic` unless you pass `--sql-dialect`.

## Get manifest precision

Use a compiled manifest when exact dbt output matters. Keep `target/manifest.json` current with `dbt parse` or `dbt compile`. In the TUI, press `P` to run `dbt parse` and switch to the generated manifest in place. With `--watch`, the TUI reloads when the project files change.
