---
title: Search
description: Filter the model list with fuzzy, full-text, or dbt selector search, jump around the lineage, and run any command from the palette.
---

`dbtl` has three ways to find things: the `/` search, a `!` or `?` prefix that changes what `/` matches, and the command palette. Press `/` to start a search.

## List search

With the list focused, `/` filters the model list as you type. The query is a fuzzy match on model names, and the list narrows to the matches.

| Key | Action |
|---|---|
| `Enter` | Confirm the search |
| `Esc` | Cancel the search |
| `Up` `Down` | Move to the previous or next match |
| `Bksp` | Delete a character |

Press `/` on the [home tab](interface.md#home-tab) to search models from there.

### Full-text search

Start the query with `?` to search descriptions, column names, and raw SQL instead of model names.

```text
?customer_id
```

### Selector search

Start the query with `!` to use a dbt selector. The list narrows to the models the selector picks.

```text
!tag:nightly
!+fct_orders
!meta:owner=alice
!state:modified+
```

`state:` selectors need a `--diff` baseline. `meta:key` matches when the key exists, and `meta:key=value` matches the stringified value.

The same grammar drives `dbtl ls`, `--select`, and `--exclude`. See [selector syntax](../cli/selectors.md) for the full list of atoms, hops, globs, unions, and intersections.

## Lineage search

With the lineage pane focused, `/` searches the nodes in the graph instead of the list. Confirming a match jumps to it. A match inside a folded `+N more` group expands the group. See [Lineage](lineage.md#fold-large-groups).

## Command palette

Press `Ctrl-p` or `Cmd-p` to open the command palette. It lists the same actions as the keymap, so every command is reachable without remembering its key.

| Key | Action |
|---|---|
| Typing | Filter commands |
| `Up` `Down` | Move through the matches |
| `Enter` | Run the selected command |
| `Esc` | Close the palette |

Start the query with `@` to search models instead of commands. `Enter` on a model re-roots the lineage there.

```text
@fct_orders
```

Recently run palette commands are saved with your view state. See [Saved view state](interface.md#saved-view-state).

## Help filter

`?` opens the help overlay. Press `/` inside it, or start typing, to filter the help lines.

## Start on a model

`dbtl --select <name-or-selector>` opens a model at startup. See the [quick start](../getting-started/quick-start.md#start-on-a-model).
