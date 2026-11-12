---
title: dbtl ls
description: List the nodes a dbt selector matches, as names, unique IDs, a dbt build selector line, or JSON.
---

`dbtl ls <SELECTOR>...` answers "what does this spec pick?" without starting the TUI. It evaluates the same [selector grammar](selectors.md) as the TUI's `!` search and the `--select` and `--exclude` flags on other subcommands.

```sh
dbtl ls 'tag:nightly'                              # names, one per line
```

```text
stg_orders
stg_customers
```

```sh
dbtl ls 'config.materialized:table,path:marts' --output selector
```

```text
fct_orders fct_sessions
```

The `selector` output is ready to paste into `dbt build --select`.

```sh
dbtl ls 'state:modified+' --diff git:main          # a branch's blast radius
dbtl ls 'fct_*' --output json                      # [{unique_id, name, ...}]
dbtl ls '2+fct_orders' --exclude path:staging      # subtract after the union
```

## Selectors

Whitespace-separated terms union. Comma-separated factors intersect. `+` and `N+` expand up and downstream. Atoms are `tag:`, `path:`, `config.materialized:`, `exposure:`, `state:modified|new`, `meta:key[=value]`, bare names, and globs. See [Selectors](selectors.md) for the full grammar.

`--exclude <SELECTOR>` subtracts a spec's matches after the union. It is repeatable, and each occurrence takes one spec.

`state:` specs need `--diff`. `dbtl` resolves them with its own comparison of the SQL, config, column, and macro changes it can read. The set may not match dbt's `state:modified`.

## Output shapes

`--output` picks the shape:

| Value | Output |
| ----- | ------ |
| `names` | Node names, one per line. This is the default. |
| `uids` | `unique_id`s, one per line. |
| `selector` | One space-separated line of buildable model, seed, and snapshot names, for `dbt build --select`. |
| `json` | One object per node. |

## Exit codes

An empty match prints nothing (`[]` under `json`) and exits 0. A malformed spec is a usage error and exits 2. See [Exit codes](../configuration/exit-codes.md).

`ls` accepts the global flags `--manifest`, `--source`, `--project`, `--include-packages`, and `--diff`.
