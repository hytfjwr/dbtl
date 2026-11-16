---
title: dbtl columns
description: Trace column-level lineage from the command line, with no compile and no Python.
---

`dbtl columns` resolves column-level lineage: which upstream columns feed a model's output columns, and which downstream columns an input feeds. It works in both manifest and source mode, with no compile and no Python.

`dbtl` parses each model's SQL with a SQL parser and resolves column references through the real FROM and JOIN scope. That covers table aliases, CTEs, derived and correlated subqueries, wildcards (`*` and `t.*`), and set operations.

Compiled SQL is preferred, and flagged `[stale]` when the source file is newer. Without compiled SQL, `dbtl` renders the raw model SQL first: `{{ ref() }}`, `{{ source() }}`, `{% %}`, and `{# #}` are rendered, and then it parses the result.

## Usage

Name the column as `<model>.<column>`. The model segment is a node name or a `unique_id`. The column segment matches case-insensitively.

```sh
dbtl columns stg_orders.id                # downstream closure (the blast radius)
dbtl columns cll_demo.order_id --upstream # provenance
dbtl columns stg_orders.id --format json  # machine-readable report
dbtl --sql-dialect snowflake columns m.id # pin the parser dialect
```

A versioned model's name carries its version, as in `fct_orders.v1`. Write `dbtl columns fct_orders.v1.id` for one version. The bare `fct_orders.id` is rejected as ambiguous when the model has several versions, and the error lists the unique IDs to choose from.

| Flag | Effect |
| ---- | ------ |
| `--downstream` | List the columns the target feeds. This is the default. The flag exists so scripts can state the direction. |
| `--upstream` | List the columns that feed into the target. |
| `--format markdown\|json` | Output format. Default: `markdown`. |
| `--sql-dialect <DIALECT>` | Override the SQL dialect. Default: the manifest's `adapter_type`, or `generic` in source mode or when unset. |

`dbtl` splits the target on the last `.`. A column whose own name contains a `.`, such as a quoted `"a.b"` identifier, cannot be expressed here today.

The global flags `columns` accepts are listed in the [CLI reference](../reference/cli.md#dbtl-columns).

## Output

The default output is a deterministic Markdown table with the columns `column`, `via`, `kind`, and `confidence`. It suits a PR comment. `--format json` emits the same report as structured data.

Each edge has a `kind`:

| Kind | Meaning |
| ---- | ------- |
| `passthrough` | Same name, same value. |
| `rename` | The column is renamed. |
| `transform` | A computed expression. |
| `filter_or_join` | A row-shaping reference, such as a join key or a `WHERE`, `GROUP BY`, or `QUALIFY` column, attached to every output. |

The `confidence` field records how the edge was resolved:

| Confidence | Meaning |
| ---------- | ------- |
| `exact` | The SQL parser ran. |
| `stale` | The compiled SQL is older than the source file. |
| `name_match` | The model fell back to the name-matching approximation. |

## Fallback to name matching

The fallback is per model. A model whose SQL is missing or unparsable resolves by matching column names, so `o.id` can bind to every parent's `id`. `dbtl columns` reports the fallback count on stderr. The TUI's structure view tags such models `[name-match]`.

## Known approximations

- `select *` and `t.*` expand over the parent's known columns, taken from the catalog first, then documented columns, then the parent's own resolved outputs. A parent with entirely unknown columns contributes nothing. `dbtl` does not invent names.
- An unqualified column that matches several bindings keeps all candidates. This over-connection is documented behavior for genuine ambiguity. A binding with an unknown column list cannot disprove a reference, so it joins the candidates too.
- `{% if %}` and `{% for %}` bodies all resolve, so every branch's refs count. A `{{ macro(...) }}` call renders as `NULL` where a value is expected, and as a generated name where SQL needs a name, such as after `FROM`. Either way, macro-emitted SQL stays opaque.

## In the TUI

The structure view (`Enter`) shows `↑u ↓d` one-hop edge counts beside every column: direct parents in, direct children out. These are not the transitive closure. The keys that move between columns and trace one are in [Structure view](../tui/structure-view.md#column-rows).
