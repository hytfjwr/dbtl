---
title: Lineage
description: Move the lineage cursor, re-root on any model, filter by direction and depth, trace paths and columns, and change how the graph is drawn.
---

The lineage pane draws the selected model, its upstream nodes on the left, and its downstream nodes on the right. Each node is a box with a type tag in the top border and its test count in the bottom border.

```text
╭Seed────────────────────╮   ╭View───────────────────╮   ╭View────────────────────╮
│ source_datetime_policy │──▶│ stg_payment__shoppers │──▶│ int_shoppers__combined │
╰────────────────tests:4─╯  │╰───────────────tests:5─╯   ╰────────────────────────╯
                            │
╭Source────╮                │
│ shoppers │────────────────╯
╰──────────╯
```

Give the pane focus with `Tab`. Then the movement keys move the lineage cursor instead of the list selection.

## Move the cursor

| Key | Action |
|---|---|
| `h` or `Left` | Move the cursor left, upstream |
| `l` or `Right` | Move the cursor right, downstream |
| `j` `k` or `Down` `Up` | Move within the cursor's column |
| `H` `L` | Send the cursor to the first or last column |
| `z` | Send the cursor home and re-center the view |

The inspector follows the cursor. See [Interface](interface.md#inspector).

## Re-root on a model

Re-rooting makes another model the center of the graph.

- Press `Enter` on a cursor that sits on a lineage model.
- Click a node.
- Pick a model in the list.
- Jump with a `/` search while the lineage pane has focus. See [Search](search.md).

Double-clicking a node commits it like `Enter`: it re-roots a model, opens the structure view for other nodes, and expands a `+N more` box.

### History

Each tab records its re-root history.

| Key | Action |
|---|---|
| `b` | Jump back |
| `f` | Jump forward |

## Filter the view

Limit what the graph shows around the root.

| Key | Action |
|---|---|
| `u` | Toggle the upstream side |
| `d` | Toggle the downstream side |
| `[` | Limit the depth |
| `]` | Widen the depth |
| `0` | Reset the view |

The pane title shows the active view as a `[view]` tag.

## Fold large groups

When one column stacks more than `lineage_collapse` siblings of the same parent, the tail folds into a single `+N more` box. The default threshold is 8. Set it in your [user config](../configuration/user-config.md); `0` never collapses.

- `Enter`, or a click, on the `+N more` box expands the group into all its members behind a `-N less` box.
- `Enter` on the `-N less` box collapses the group again.
- A `/` lineage search that lands on a hidden member expands its group automatically.
- Expansions reset when you re-root.

The nodes that stay visible are deterministic. The cursor, the path from the root to the cursor, and bookmarks always show. The rest fill in `unique_id` order.

Folding only affects the screen. The Mermaid (`m`, `y` `m`) and DOT (`x`, `y` `x`) exports always include every real node. The ASCII copy (`c`, `y` `c`) and the `e` diagram export copy exactly what the pane draws, including the markers. Expand a group first if you want its members in the text.

## Trace a path

The path from the root to the cursor lights up, including the connectors between nodes. Everything off the path dims.

Press `p` to anchor the path's start on the selected model. The highlight then runs from the anchor to the cursor, so you can ask why one node reaches another. It answers the same question as dbt's `A+,+B` selector. Press `p` again to clear the anchor.

## Column trace

Trace one column through the graph from the [structure view](interface.md#structure-view).

1. Open the structure view with `Enter`.
2. Move to a column with `h` and `l`.
3. Press `Enter` or `c`.

The lineage pane then filters to the models related to that column.

- Each box lists its related columns. `>` marks the traced column, and the other marks name the edge kind.
- A `column:model.col` chip replaces the `[view]` label in the status line, and the pane title gains `[col:name]`.
- `h` and `l` step the traced column upstream and downstream.
- `K` opens the column-lineage modal. In it, `j` and `k` move the row cursor and `Enter` re-roots to a row's model.
- `t` reaches the column-impact lens. See [Lenses](lenses.md).
- The view filters `u`, `d`, `[`, `]`, and `0` are disabled while tracing.
- `Esc` exits the trace.

The same data is available without the TUI. See [dbtl columns](../cli/columns.md).

## Change how the graph is drawn

| Key | Action |
|---|---|
| `v` | Toggle compact mode. Nodes become one-row boxes, which fits about twice as much graph on screen. |
| `w` | Toggle overview mode. The whole DAG shows in the lineage pane in compact form, with the minimap on. |
| `M` | Toggle the minimap |
| `t` | Cycle the lens. See [Lenses](lenses.md). |

Drag the lineage to pan it, or use the wheel. Drag the minimap, or click it, to move the viewport. See [Mouse](interface.md#mouse).
