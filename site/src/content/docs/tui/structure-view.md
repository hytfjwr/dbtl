---
title: Structure view
description: Open the structure view on a node to see its columns, tests, description, and governance fields, and to start a column trace.
---

`Enter` on the selected model opens the structure view. It shows the model's columns, tests, and description. It also shows these fields when the model declares them:

- Governance: `access`, `group`, `contract`, and `version`.
- Build settings: `language` (when it is not `sql`), `incremental_strategy`, `deprecation_date`, and `constraints`.
- The `meta` dict, sorted, with nested values as compact JSON.

When the node has lint findings, a lint section lists them below the tests. They are the same findings the inspector pane shows.

## Column rows

Each column row carries test badges next to its one-hop column-edge counts.

| Badge | Test |
|---|---|
| `NN` | `not_null` |
| `U` | `unique` |
| `R` | `relationships` |
| `AV` | `accepted_values` |
| `AR` | `accepted_range` |
| `*` | any other test |

| Key | Action |
|---|---|
| `h` `l` | Move the cursor across column rows |
| `Tab` | Move the cursor between the column rows and the tests list |
| `Enter` or `c` | Trace the cursored column in the lineage pane. See [Lineage](lineage.md#column-trace). |
| `K` | Open the column-lineage modal for the cursored column |
| `y` then `l` | Copy the cursored column's downstream lineage as Markdown |

On the tests side, the cursor lands only on `relationships` rows. The resolved `to:` target shows as `-> model.column`, and `Enter` there re-roots the lineage to the target.

## Warehouse data

With a `catalog.json` loaded, the structure view also shows warehouse column types, row counts, and drift. See [Run results and catalog](run-results-and-catalog.md).
