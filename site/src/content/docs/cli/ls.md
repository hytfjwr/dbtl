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
dbtl ls 'config.materialized:table,path:models/marts' --output selector
```

```text
fct_orders fct_sessions
```

The `selector` output is ready to paste into `dbt build --select`.

```sh
dbtl ls 'state:modified+' --diff git:main          # a branch's blast radius
dbtl ls 'fct_*' --output json                      # [{unique_id, name, ...}]
dbtl ls '2+fct_orders' --exclude path:models/staging      # subtract after the union
```

## Selectors

The selector grammar — unions, intersections, upstream and downstream hops, `tag:`/`path:`/`config.materialized:`/`exposure:`/`state:`/`meta:` atoms, and globs — is documented under [Selectors](selectors.md). `state:` specs need `--diff`.

`--exclude <SELECTOR>` subtracts a spec's matches after the union. It is repeatable, and each occurrence takes one spec.

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

The global flags `ls` accepts are listed in the [CLI reference](../reference/cli.md#dbtl-ls).
