---
title: Explore a large project
description: Find models fast in a big dbt project — selector search, direction and depth filters, folded groups, tabs, bookmarks, and lenses.
---

On a project with hundreds of models, scrolling the list is the slow path. `dbtl` folds wide columns, filters by selector, and keeps several models open at once.

## What you need

- `dbtl` installed and a project open — see the [quick start](../getting-started/quick-start.md).
- The steps below are TUI keys. Press `?` inside the app for the same list.

## Steps

1. **Start where the work is.** `--select` opens on a model or on a selector's first match:

   ```sh
   dbtl --select fct_orders
   dbtl --select 'fct_*'
   ```

2. **Search instead of scrolling.** With the list focused, `/` filters by name. A `?` prefix searches descriptions, columns, and raw SQL; a `!` prefix filters by dbt selector:

   ```text
   !tag:nightly
   !+fct_orders
   !meta:owner=alice
   ```

   `Ctrl-p` opens the command palette over every action, and `@name` inside it jumps straight to a model. See [Search](../tui/search.md).

3. **Shrink the graph.** Wide sibling groups fold into `+N more` boxes — `Enter` on the box, or a `/` lineage search that lands inside it, expands the group. `u` and `d` toggle the upstream and downstream sides, `[` and `]` bound the depth, `v` draws one-row compact nodes, `w` shows the whole DAG with the minimap, and `0` resets the view. See [Lineage](../tui/lineage.md).

4. **Keep several threads open.** `O` opens the cursor's node in a new tab and `{` `}` switch tabs. `Space` bookmarks the selected model and `'` `"` cycle the bookmarks. `.` cycles the sort and `T`, `*`, `F` filter the list to untested, bookmarked, or lint-finding models. See [Interface](../tui/interface.md).

5. **Read one property across the graph.** `t` cycles the lenses — test coverage, downstream blast radius, layer, lint findings, `--diff` changes. Under the lint lens, `n` and `N` jump between findings instead of untested models. See [Lenses](../tui/lenses.md).

## Check it works

- The lineage title shows the active `[view]` and `[lens:…]` tags; the status line shows the `scope:` and sort chips.
- `dbtl ls '<selector>'` prints the set a selector matches, so you can check a spec before using it in the TUI — the grammar is identical. See [selector syntax](../cli/selectors.md).

## Where next

- [Search](../tui/search.md) — fuzzy, full-text, selector search, and the palette.
- [Lineage](../tui/lineage.md) — the cursor, re-rooting, filters, folding, and the minimap.
- [Interface](../tui/interface.md) — panes, tabs, the home tab, bookmarks, and the mouse.
