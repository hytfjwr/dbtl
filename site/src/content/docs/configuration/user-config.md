---
title: User config and state
description: Where dbtl saves your view per project, and how config.yml sets defaults for the TUI.
---

`dbtl` keeps two kinds of per-user data: a saved view for each project, and an optional `config.yml` with defaults. Policy that a CI gate depends on lives in the project's [`.dbtl.yml`](project-config.md) instead.

## Saved state

`dbtl` remembers your view per project and restores it on the next run. It saves the following when you quit:

- bookmarks (`Space`)
- the open tabs (each root and view filter) and the active tab
- recently rooted models
- folded list groups
- the active lens, box density, and list sort
- the list and inspector pane widths
- the visibility of the list pane, minimap, and inspector (`Ctrl-b`, `M`, `I`)
- the theme
- `/` search history and recently run palette commands
- the last selected model

The files live under `$XDG_STATE_HOME/dbtl/`, or `~/.local/state/dbtl/` when that variable is unset. There is one JSON file per project. Deleting a file resets that project's view.

Session-only state is deliberately not saved: a tab's lineage cursor, re-root history, and scroll positions.

A state file can reference models that no longer exist. `dbtl` drops those bookmarks.

## config.yml

Defaults for a fresh project come from `~/.config/dbtl/config.yml`, or `$XDG_CONFIG_HOME/dbtl/config.yml`. Every key is optional.

```yaml
# ~/.config/dbtl/config.yml
theme: dusk            # same names as --theme (CLI/env still wins)
lens: coverage         # off|coverage|degree-heat|layer|layer-violation|lint|diff|run-status|run-time|column-impact|primary-key
density: compact       # comfortable|compact
sort: downstream       # layer|downstream|tests
inspector: auto        # auto|on|off, the inspector pane's default visibility (I cycles it live)
group_by_meta: owner   # group the model list by this meta key's value instead of layer dirs
owner_meta_key: owner  # the meta key the impact report counts as "affected owners" (default owner)
lineage_collapse: 8    # fold a column's same-parent sibling tail into a +N more box past N (0 = never)
```

| Key | Meaning |
| --- | ------- |
| `theme` | A preset name, or a theme under `~/.config/dbtl/themes/<name>.yml`. See [Themes](../tui/themes.md). |
| `lens` | The default lineage lens. |
| `density` | The default lineage density. |
| `sort` | The default model list sort. |
| `inspector` | The default inspector visibility. `I` cycles it live. |
| `group_by_meta` | Group the model list by this `meta` key's value instead of by layer directory. |
| `owner_meta_key` | The `meta` key that `dbtl impact` and the `D` modal read for "Affected owners". Default: `owner`. |
| `lineage_collapse` | The sibling-collapse threshold. A lineage column that stacks more than this many same-parent siblings folds the tail into one `+N more` box. Default: 8. `0` never collapses. |

`lineage_collapse` is config-only. It is a display policy, not a per-project preference, so the state file ignores it.

### Group the list by meta

`group_by_meta` replaces the left pane's layer grouping with `key=value` groups. Groups sort alphabetically. Models that lack the key land in a `(no <key>)` group at the end, for example `(no owner)` when you group by `owner`. The `.` sort cycle still orders models within each group. Leave the key unset for the default layer grouping.

### Owner meta key

`owner_meta_key` changes which `meta` key `dbtl impact` and the `D` modal read for their "Affected owners" list. `dbtl check --diff` prints the owners as an informational `affected owners:` line. It is never a gate. An `owner_meta_key` in the project's `.dbtl.yml` wins over this one.

## Precedence

For a TUI preference, the order from lowest to highest is:

1. `config.yml`
2. saved project state
3. explicit command-line flags

So `dbtl --theme x` always wins over a configured theme. A project you already visited restores its saved lens instead of the configured one.

A malformed `config.yml` prints a warning and `dbtl` ignores it. Unknown keys are ignored.

## Editor completion

A generated JSON Schema covers `config.yml`:

```yaml
# ~/.config/dbtl/config.yml
# yaml-language-server: $schema=https://raw.githubusercontent.com/hytfjwr/dbtl/main/schema/config.schema.json
```

For every key, see the [config reference](../reference/config.md).
