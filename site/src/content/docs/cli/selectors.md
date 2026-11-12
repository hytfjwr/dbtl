---
title: Selectors
description: The dbt selector grammar that dbtl uses in the TUI search, --select, --exclude, dbtl ls, and lint rule excludes.
---

`dbtl` uses one selector grammar everywhere. A spec means the same set of nodes in the TUI search box, in `--select` and `--exclude`, in `dbtl ls`, and in `.dbtl.yml` rule excludes.

## Grammar

| Syntax | Meaning |
| ------ | ------- |
| `a b` | Whitespace unions the terms. |
| `a,b` | A comma intersects the factors. |
| `+name`, `name+`, `+name+` | Add all upstream nodes, all downstream nodes, or both. |
| `N+name` | A number before `+` limits the expansion to that many hops, for example `2+fct_orders`. |
| `tag:x` | Nodes with the tag. |
| `path:x` | Nodes whose path, as dbt records it in the manifest, contains `x`. A model's path is relative to its model directory, for example `marts/fct_orders.sql`. It is a substring match, so `path:marts` also matches `stg_marts.sql`. |
| `config.materialized:x` | Nodes with the materialization. |
| `exposure:x` | The named exposure and every node upstream of it. |
| `meta:key` | Nodes where the `meta` key exists. |
| `meta:key=value` | Nodes where the stringified `meta` value equals `value`. |
| `state:modified`, `state:new` | Nodes the `--diff` baseline marks as modified or new. |
| `name`, `fct_*` | A bare node name, or a glob. |

Examples:

```text
tag:nightly                      # every node tagged nightly
+fct_orders                      # fct_orders and everything upstream of it
2+fct_orders                     # fct_orders and two hops upstream
path:marts+                      # nodes whose path contains marts, and their downstream
config.materialized:table,path:marts   # tables whose path contains marts
meta:owner=alice                 # nodes whose owner meta is alice
state:modified+                  # changed nodes and their downstream
fct_*                            # a glob
```

> [!NOTE]
> `state:` needs a `--diff` baseline. `dbtl` resolves it with its own comparison of SQL, config, column, and macro changes. The set can differ from what dbt's `state:modified` selects.

## Where selectors work

| Surface | How you use it |
| ------- | -------------- |
| TUI list search | Press `/` and start the query with `!`, for example `!tag:nightly`, `!+fct_orders`, `!meta:owner=alice`, or `!state:modified+` when a `--diff` baseline is loaded. |
| TUI full-text search | Start the query with `?` to search descriptions, column names, and raw SQL instead. |
| `dbtl --select <spec>` | Start the TUI with a model selected. Accepts the same selectors except `state:`; the spec's first match opens in a tab, and it must be a model. |
| `dbtl ls <SELECTOR>...` | List the matching nodes. See [dbtl ls](ls.md). |
| `--select` on `check`, `lint`, `impact`, `docs` | Scope the command to the matching nodes. Repeatable. Specs union. |
| `--exclude` on `ls` and `docs` | Subtract matches after the union. Repeatable. |
| `.dbtl.yml` `exclude:` | Drop lint findings on matching nodes. See [Project config](../configuration/project-config.md). |

The `--select` flag on subcommands and the `--select` flag on the TUI take different input. On `check`, `lint`, `impact`, and `docs` it is a full selector spec and can be repeated. On the bare `dbtl` command it is a model name or a spec whose first match — which must be a model — opens in a model tab. `state:` is unavailable there because the selection resolves before `--diff` loads.

## Exclude

`--exclude <SELECTOR>` subtracts the nodes a spec matches. It applies after every inclusion term. Each occurrence takes exactly one spec, so a following positional argument is never swallowed.

```sh
dbtl ls '2+fct_orders' --exclude path:staging
dbtl docs --out ./dbt-docs --exclude 'tag:legacy'
```

## Errors

A malformed spec is a usage error and exits with code 2. A valid spec that matches nothing is not an error. `dbtl ls` prints nothing (or `[]` with `--output json`) and exits 0. See [Exit codes](../configuration/exit-codes.md).
