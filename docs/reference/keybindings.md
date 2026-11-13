<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->

# Keybindings

The TUI's key bindings, grouped by interaction mode — the same rows the in-app `?` overlay shows, generated from the keymap table that also drives dispatch and the command palette.

## Selection

The main three-pane screen.

| Keys | Action |
| ---- | ------ |
| `j/Down` | down (list / lineage cursor) |
| `k/Up` | up (list / lineage cursor) |
| `h/Left` | lineage cursor left (upstream) |
| `l/Right` | lineage cursor right (downstream) |
| `g` | jump to first model (list) |
| `G` | jump to last model (list) |
| `Tab` | switch list / lineage focus |
| `Cmd-b/Ctrl-b` | show / hide the model list pane |
| `Enter` | open structure / re-root to cursor / toggle +more |
| `/` | search (list filter / lineage jump) |
| `?` | toggle this help |
| `o` | open the model's file in $VISUAL or $EDITOR |
| `y` | yank menu |
| `Y` | copy model name |
| `m` | copy lineage as Mermaid |
| `x` | copy lineage as Graphviz DOT |
| `r` | reload data source |
| `z` | cursor home + re-center lineage |
| `u` | toggle upstream side |
| `d` | toggle downstream side |
| `[` | limit lineage depth (-) |
| `]` | widen lineage depth (+) |
| `0` | reset lineage view |
| `b` | jump back (re-root history) |
| `f` | jump forward (re-root history) |
| `q/Ctrl-c` | quit |
| `s` | preview SQL |
| `S` | stats dashboard |
| `t` | cycle lineage lens |
| `Space` | toggle bookmark |
| `'` | cycle bookmarks |
| `.` | cycle list sort |
| `M` | toggle minimap |
| `v` | toggle compact lineage |
| `Ctrl-p/Cmd-p` | command palette |
| `c` | copy lineage as text diagram |
| `C` | copy raw SQL |
| `i` | copy impact report (Markdown) |
| `e` | export lineage to <name>_lineage.txt |
| `PgDn/Ctrl-d` | page down (list) |
| `PgUp/Ctrl-u` | page up (list) |
| `n` | next untested model (Lint lens: next lint finding) |
| `N` | previous untested model (Lint lens: previous lint finding) |
| `"` | cycle bookmarks backward |
| `H` | lineage cursor to first column |
| `L` | lineage cursor to last column |
| `T` | filter list: untested only |
| `*` | filter list: bookmarked only |
| `F` | filter list: lint findings only |
| `-` | fold/unfold model group |
| `P` | run dbt parse (build manifest) |
| `Ctrl-t` | cycle color theme |
| `D` | diff vs baseline (--diff) |
| `!` | copy dbt --select command |
| `w` | toggle whole-graph overview |
| `p` | anchor/clear two-node path |
| `K` | column lineage modal (traced column) |
| `a` | node actions menu |
| `Esc` | exit column focus |
| `<` | narrow pane 4 cols |
| `>` | widen pane 4 cols |
| `=` | reset pane widths |
| `Ctrl-n` | notification center |
| `O` | open node in new tab |
| `X/Ctrl-w` | close tab |
| `{` | previous tab |
| `}` | next tab |
| `1` | go to tab 1 |
| `2` | go to tab 2 |
| `3` | go to tab 3 |
| `4` | go to tab 4 |
| `5` | go to tab 5 |
| `6` | go to tab 6 |
| `7` | go to tab 7 |
| `8` | go to tab 8 |
| `9` | go to tab 9 |
| `I` | inspector pane auto / on / off |
| `~` | go to home tab |
| `click` | list row selects · node moves cursor · tab / chip / hint / title / minimap act |
| `double-click` | node commits (re-root / structure / expand) · list row: structure · border: reset |
| `right click` | node actions menu (list row / lineage node) |
| `middle click` | list row / node opens in a new tab · tab closes |
| `drag` | pan lineage · resize a pane border · scrub the minimap |
| `hover` | accent node + direct edges · rest 0.5s for a tooltip |
| `wheel` | list selects · lineage pans (Shift: sideways) · inspector scrolls · strip scrolls |

## Yank

The `y` yank menu.

| Keys | Action |
| ---- | ------ |
| `y` | copy unique_id |
| `Y` | copy model name |
| `m` | copy lineage as Mermaid |
| `x` | copy lineage as Graphviz DOT |
| `c` | copy lineage as text diagram |
| `C` | copy raw SQL |
| `s` | copy compiled SQL |
| `i` | copy impact report (Markdown) |
| `l` | copy column lineage (Markdown) |
| `!` | copy dbt --select command |
| `Esc/q` | cancel |
| `?` | help |
| `mouse` | row click picks · double-click yanks · outside closes |

## Search

The `/` list search.

| Keys | Action |
| ---- | ------ |
| `Esc` | cancel search |
| `Enter` | confirm |
| `Bksp` | delete char |
| `Up` | previous match |
| `Down` | next match |
| `a-z..` | type to filter / search |

## Palette

The `Ctrl-p` command palette.

| Keys | Action |
| ---- | ------ |
| `Esc` | close palette |
| `Enter` | run command |
| `Bksp` | delete char |
| `Up` | prev command |
| `Down` | next command |
| `a-z..` | type to filter commands |
| `@name` | search models / re-root |
| `mouse` | row click picks · double-click runs · wheel scrolls · outside closes |

## Detail

The `Enter` node-detail overlay.

| Keys | Action |
| ---- | ------ |
| `Esc/q` | close |
| `?` | help |
| `Enter/c` | trace column / jump to test target |
| `Tab/S-Tab` | columns / tests section |
| `K` | column lineage modal (this column) |
| `h/H` | previous column / test |
| `l/L` | next column / test |
| `y` | yank menu |
| `a` | node actions menu |
| `j/Down` | scroll down |
| `k/Up` | scroll up |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `mouse` | wheel scrolls · outside click closes |

## Sql

The `s` SQL-preview overlay.

| Keys | Action |
| ---- | ------ |
| `Esc/Enter/q` | close |
| `?` | help |
| `c` | toggle raw / compiled SQL |
| `a` | node actions menu |
| `j/Down` | scroll down |
| `k/Up` | scroll up |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `mouse` | wheel scrolls · outside click closes |

## Stats

The `S` project-stats dashboard.

| Keys | Action |
| ---- | ------ |
| `Esc/Enter/q` | close |
| `?` | help |
| `j/Down` | scroll down |
| `k/Up` | scroll up |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `mouse` | wheel scrolls · outside click closes |

## Diff

The `D` baseline-diff summary (needs `--diff`).

| Keys | Action |
| ---- | ------ |
| `Esc/Enter/q` | close |
| `?` | help |
| `e` | export PR impact report (Markdown) |
| `y` | copy dbt build --select (column-narrowed) |
| `Y` | copy dbt build --select (wide) |
| `j/Down` | scroll down |
| `k/Up` | scroll up |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `mouse` | wheel scrolls · outside click closes |

## Columns

The column-lineage modal.

| Keys | Action |
| ---- | ------ |
| `Esc/q` | close |
| `?` | help |
| `Enter` | jump to this model |
| `j/Down` | next column row |
| `k/Up` | previous column row |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `mouse` | row click picks · double-click jumps · wheel scrolls · outside closes |

## Notifications

The `Ctrl-n` notification centre.

| Keys | Action |
| ---- | ------ |
| `Esc/Enter/q/Ctrl-n` | close |
| `j/Down` | scroll down |
| `k/Up` | scroll up |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `mouse` | wheel scrolls · outside click closes |

## Menu

The node context menu (`a` / right-click).

| Keys | Action |
| ---- | ------ |
| `j/Down` | next item |
| `k/Up` | previous item |
| `Enter` | run the selected action |
| `Esc/q` | close menu |
| `item key` | an item's own key runs it directly |
| `click item` | run the action (outside click closes) |

## Help

The `?` help overlay itself.

| Keys | Action |
| ---- | ------ |
| `Esc/?/q` | close help |
| `j/Down` | scroll down |
| `k/Up` | scroll up |
| `d/PgDn/Ctrl-d` | page down |
| `u/PgUp/Ctrl-u` | page up |
| `g` | jump to top |
| `G` | jump to bottom |
| `/` | filter help lines |
| `a-z..` | type to filter help lines |
| `mouse` | wheel scrolls · outside click closes |

## Home

The home tab.

| Keys | Action |
| ---- | ------ |
| `j/Down` | next item |
| `k/Up` | previous item |
| `Enter` | open item in a tab |
| `/` | search models |
| `1` | go to tab 1 |
| `2` | go to tab 2 |
| `3` | go to tab 3 |
| `4` | go to tab 4 |
| `5` | go to tab 5 |
| `6` | go to tab 6 |
| `7` | go to tab 7 |
| `8` | go to tab 8 |
| `9` | go to tab 9 |
| `{` | previous tab |
| `}` | next tab |
| `~` | go to home tab |
| `X/Ctrl-w` | close tab |
| `w` | toggle whole-graph overview |
| `S` | stats dashboard |
| `D` | diff vs baseline (--diff) |
| `P` | run dbt parse (build manifest) |
| `Ctrl-n` | notification center |
| `?` | toggle this help |
| `q/Ctrl-c` | quit |
| `click / wheel` | open the row / move the item cursor |

## Outside the app

Files the TUI writes land in the current working directory: `e` exports the lineage view as `<model>_lineage.txt` (`<project>_overview.txt` from the whole-graph overview), and the `D` modal's `e` exports the PR impact report as `<project>_pr_impact.md`. `o` opens the model's file with `$VISUAL`, falling back to `$EDITOR` when it is unset.

`Cmd`-modified bindings are delivered only by terminals that speak the kitty keyboard protocol — dbtl probes for it at startup; everywhere else their `Ctrl` alternatives cover the same actions.
