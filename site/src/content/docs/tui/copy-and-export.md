---
title: Copy and export
description: Copy the lineage, SQL, and reports to the clipboard, or write the diagram to a file — all from inside the TUI.
---

The lineage, SQL, and reports copy to the clipboard with single keys. The `e` key writes the diagram to a file instead.

## Copy keys

| Key | Action |
|---|---|
| `Y` | Copy the model name |
| `C` | Copy the raw SQL |
| `m` | Copy the lineage as Mermaid |
| `x` | Copy the lineage as Graphviz DOT |
| `c` | Copy the lineage as an ASCII diagram |
| `i` | Copy a Markdown impact report |
| `e` | Write the diagram to a file in the current directory: `<model>_lineage.txt` (`<project>_overview.txt` from the whole-graph overview) |
| `!` | Copy the view as a runnable `dbt build --select` command |
| `y` | Open the yank menu |

## The yank menu

The yank menu has one key per target: `y` copies the `unique_id`, `Y` the name, `m` Mermaid, `x` DOT, `c` ASCII, `C` raw SQL, `s` compiled SQL, `i` the impact report, `!` the selector, and `l` the column lineage of the [structure view](structure-view.md)'s selected column. The single keys above still work. Pressing `y` over a modal aims every yank at the modal's node, not the list selection.
