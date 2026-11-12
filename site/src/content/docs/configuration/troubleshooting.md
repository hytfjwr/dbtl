---
title: Troubleshooting
description: Fix doubled terminal borders and missing panes when dbtl runs in a narrow or unusual terminal.
---

## Borders look doubled or misaligned

Unicode box-drawing characters have East Asian Ambiguous width. Some terminal configurations draw them 2 cells wide, which doubles or ghosts the borders.

`dbtl` probes the terminal at startup and falls back to pure ASCII (`+ - | >`) when it detects this. You can also force a mode:

```sh
dbtl --ascii     # pure ASCII rendering
dbtl --unicode   # skip the probe, keep Unicode box drawing
```

## The terminal is too small or panes disappeared

`dbtl` needs at least 50 columns by 12 rows. Below that, it draws only a "terminal too small" notice. `q` and `Ctrl-c` still work.

The layout adapts to the width:

| Width | Behavior |
| ----- | -------- |
| Under 50 columns | The "terminal too small" notice. |
| 50 to 79 columns | The model list collapses into a drawer. `Ctrl-b` toggles it and `Tab` cycles focus. |
| Under 160 columns | The inspector does not appear by itself. `I` toggles it manually. |

In a narrow tmux pane, resize the pane or zoom it with `prefix z` instead of disabling panes.
