---
title: Interface
description: The dbtl screen layout, including the model list, lineage pane, inspector, tabs, home tab, status line, notifications, and mouse controls.
---

The `dbtl` screen has three panes, a tab strip, and a status line. This page covers each part and the controls that apply to the screen as a whole. Press `?` for the in-app help, which lists every key for the current mode. The same table is in the [keybindings reference](../reference/keybindings.md).

## Panes

| Pane | Position | Shows |
|---|---|---|
| Model list | Left | Every model, grouped by layer directory |
| Lineage | Center | The graph around the selected model. See [Lineage](lineage.md). |
| Inspector | Right | A summary of the node under the lineage cursor |

| Key | Action |
|---|---|
| `Tab` | Cycle focus: list, lineage, inspector |
| `Ctrl-b` or `Cmd-b` | Show or hide the model list pane |
| `I` | Cycle the inspector: auto, on, off |
| `<` `>` | Narrow or widen a pane by 4 columns. The inspector resizes while it has focus, the list otherwise. |
| `=` | Reset both pane widths to the default |

`Cmd`-modified keys reach `dbtl` only through terminals that speak the kitty keyboard protocol — `dbtl` probes for it at startup and the `Ctrl` twins always work.

### Terminal size

`dbtl` needs at least 50 columns by 12 rows. Below that it draws only a "terminal too small" notice, and `q` and `Ctrl-c` still work.

- From 50 to 79 columns, the model list collapses into a drawer. `Ctrl-b` toggles it and `Tab` cycles focus.
- At 160 columns or more, the inspector appears by itself. Below that, press `I` to show it.

In a narrow tmux pane, resize or zoom the pane (`prefix z`) instead of disabling panes.

## Model list

The list groups models by layer directory. Selecting a row re-roots the lineage on that model.

| Key | Action |
|---|---|
| `j` `k` | Move the selection |
| `g` `G` | Jump to the first or last model |
| `PgUp` `PgDn` | Page the list. `Ctrl-u` and `Ctrl-d` also work. |
| `.` | Cycle the sort: layer, downstream, tests |
| `-` | Fold or unfold the selected model's group. Clicking the group's `▾` or `▸` header does the same. |
| `T` | Filter to untested models |
| `*` | Filter to bookmarked models |
| `F` | Filter to models with lint findings |
| `Space` | Bookmark or unbookmark the model |
| `'` `"` | Cycle bookmarks forward or backward |
| `n` `N` | Jump to the next or previous untested model. Under the lint lens, these jump between lint findings. |

To group the list by a `meta` key instead of by layer, set `group_by_meta` in your [user config](../configuration/user-config.md). To search the list, see [Search](search.md).

## Tabs

Each tab has its own root model, lineage view, cursor, and re-root history. The tab strip is restored on the next run.

| Key | Action |
|---|---|
| `O` | Open the cursor's node in a new tab. An already-open model activates its existing tab. |
| `{` `}` | Previous or next tab |
| `1` to `9` | Jump to that tab |
| `X` or `Ctrl-w` | Close the active tab |
| `~` | Jump to the home tab |

In the strip, the `×` on a tab closes it and `+` opens a model picker for a new tab. The wheel over the strip scrolls it.

## Home tab

The fixed `⌂` tab is the project's front page. It shows:

- resume, recent models, and bookmarks
- health: coverage, lint, layer violations, and orphans
- the last run
- the models to look at next

On the home tab, `j` and `k` move between items and `Enter` opens the item in a tab. `/` searches models.

## Inspector

The inspector summarizes the node under the lineage cursor. It shows:

- materialization and path
- owner, tags, and `meta`
- description
- tests
- columns, with their one-hop column-lineage counts
- blast radius
- the last run
- lint findings
- diff status

It shows the same data as the structure view, always on screen. `I` cycles auto, on, and off. In auto mode the inspector appears at 160 columns or more. Set the default with `inspector` in your [user config](../configuration/user-config.md).

## Structure view

`Enter` on the selected model opens the structure view: columns, tests, and description. It also shows governance fields (access, group, contract, version) and the `meta` dict when the model declares them. The dict is sorted, and nested values appear as compact JSON.

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

With a `catalog.json` loaded, the structure view also shows warehouse column types, row counts, and drift. See [Run results and catalog](run-results-and-catalog.md).

## SQL preview and stats

`s` opens a syntax-highlighted SQL preview. Inside the preview, `c` toggles between raw and compiled SQL. The title shows `[raw]`, `[compiled]`, or `[compiled, stale]` when the source file is newer than the compiled version. Compiled SQL comes from the manifest's `compiled_code` or from `target/compiled/`. Run `dbt compile` or `dbt build` to produce it.

`o` opens the model's file in `$VISUAL`, or `$EDITOR` when `$VISUAL` is unset.

`S` opens the project stats dashboard. It includes a runtime-weighted critical path when run results are loaded, and a lint worklist.

In any scrollable modal, `j` and `k` scroll, `d` `u` `Ctrl-d` `Ctrl-u` `PgDn` `PgUp` page, `g` and `G` jump to the top or bottom, and `Esc` or `q` closes it. Two modals give `Enter` a job instead: in the structure view it traces the cursored column (or jumps to a `relationships` test's target), and in the column-lineage modal it jumps to that column's model — close those with `Esc` or `q`.

## Node actions menu

`a` opens the actions menu for the current node. Right-click opens the same menu on a list row or a lineage node.

- `j` and `k` move through items.
- `Enter` runs the selected item.
- An item's own key runs it directly.
- `Esc` or `q` closes the menu.

Items act on the node the menu opened on. Over the structure view or the SQL preview, that is the modal's node.

## Copy and export

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

The yank menu has one key per target: `y` copies the `unique_id`, `Y` the name, `m` Mermaid, `x` DOT, `c` ASCII, `C` raw SQL, `s` compiled SQL, `i` the impact report, `!` the selector, and `l` the column lineage of the structure view's selected column. The single keys above still work. Pressing `y` over a modal aims every yank at the modal's node, not the list selection.

## Reload and parse

| Key | Action |
|---|---|
| `r` | Reload the data source. Also re-reads `run_results.json`. |
| `P` | Run `dbt parse` and switch to the generated manifest. Needs `dbt` on your `PATH`. |

Start with `--watch` to reload automatically when the data source changes — a change counts once the poller confirms a newer stamp, scanning at most every two seconds. In manifest mode it watches the manifest file's mtime. In source mode it watches the `.sql`, `.py`, `.yml`, `.yaml`, and `.csv` files under the project, skipping dot-directories and `target/` except `target/compiled/` (so a `dbt compile` refreshes the compiled-SQL view).

## Status line

The status line shows key hints on the left and status chips on the right. Click a hint or a chip to run what it names. Chips include:

| Chip | Meaning |
|---|---|
| `scope:root` or `scope:+pkgs` | The package scope of the graph |
| `!N` | `N` unread warnings. See [Notifications](#notifications). |
| `run 12/34 @2024-05-01` | How many nodes the loaded run results cover, and the run date |
| A diff chip | Present when a `--diff` baseline is loaded |
| `column:model.col` | The traced column, with its one-hop counts |

## Notifications

Warnings from the loader and from config files appear as short-lived toasts. Every warning also lands in a bounded notification log.

The `!N` status chip counts the unread entries. Press `Ctrl-n` to open the notification center and page through them. In the center, `j` and `k` scroll, `d` and `u` page, and `Esc`, `Enter`, `q`, or `Ctrl-n` closes it.

## Mouse

A click confirms on release, so a slip never navigates. A second click on the same cell within about 400 ms is a double-click.

| Gesture | Target | Effect |
|---|---|---|
| Left click | List row | Select it. The active tab re-roots. |
| Left click | Group header | Fold or unfold it |
| Left click | Lineage node | Move the cursor there |
| Left click | Inspector column row | Move its cursor |
| Left click | Tab | Activate it. `×` closes it and `+` opens a model picker. |
| Left click | Status hint or chip | Run what it names |
| Left click | Lineage title | The breadcrumb, `[view]`, and `[lens:…]` parts respond |
| Left click | Minimap | Center the viewport |
| Double-click | Lineage node | Commit it like `Enter`: re-root a model, open the structure view elsewhere, or expand a `+N more` box |
| Double-click | List row | Open the structure view |
| Double-click | Pane border | Reset its width |
| Right-click | List row or lineage node | Open the actions menu |
| Middle-click | List row or lineage node | Open it in a new tab |
| Middle-click | Tab | Close it |
| Drag | Lineage | Pan it |
| Drag | Shared pane border | Resize it |
| Drag | Minimap | Scrub the viewport |

The wheel over the list moves the selection. Over the lineage it pans vertically, and `Shift` with the wheel or a horizontal notch pans sideways. Over the inspector it scrolls the inspector, and over the tab strip it scrolls the tabs.

Hover accents a node's box and its direct edges. Rest the pointer for about half a second to see a tooltip. Pane borders, status hints, chips, and tab controls light up too.

While a modal or menu is open, the rest of the screen dims. The wheel scrolls its content, a click on a row selects it, a double-click runs the row, and a click outside closes it like `Esc`. A menu row runs on a single click.

## Saved view state

`dbtl` saves your view for each project when you quit and restores it on the next run. The saved state includes bookmarks, open tabs, the active lens, density, list sort, pane widths and visibility, the theme, search history, and the last selected model. A tab's lineage cursor, re-root history, and scroll position are not saved. See [User config](../configuration/user-config.md) for the file location and precedence.

## Borders look wrong

Some terminals draw Unicode box characters two cells wide. `dbtl` probes the terminal at startup and falls back to ASCII automatically. Force a mode with `--ascii` or `--unicode`. See [Troubleshooting](../configuration/troubleshooting.md).
