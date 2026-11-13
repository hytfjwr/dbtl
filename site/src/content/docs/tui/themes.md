---
title: Themes
description: Choose a built-in color theme, switch themes live, set a default, or define your own theme in a YAML file.
---

`dbtl` ships five color themes and lets you define your own. Pick one at launch, switch live while the app runs, or set a default.

## Built-in themes

| Theme | Notes |
|---|---|
| `default` | xterm-256 colors, works everywhere |
| `dark` | Truecolor |
| `dusk` | Truecolor |
| `light` | Truecolor |
| `warm` | Truecolor |

## Choose a theme

Pass `--theme <name>` or set the `DBTL_THEME` environment variable. Press `Ctrl-t` inside the app to cycle themes live. List every available theme, including your own, with `--list-themes`.

```sh
dbtl --theme dusk
dbtl --list-themes
DBTL_THEME=dusk dbtl
```

`--theme` also accepts a path to a theme YAML file.

## Set a default

Set `theme` in your [user config](../configuration/user-config.md):

```yaml
# ~/.config/dbtl/config.yml
theme: dusk
```

The precedence is `config.yml`, then the theme saved with your project's view state, then `--theme` or `DBTL_THEME`. The command-line flag and the environment variable always win over a configured theme.

## Define your own theme

Put a YAML file under `~/.config/dbtl/themes/` or `$XDG_CONFIG_HOME/dbtl/themes/`. The file name becomes the theme name.

```yaml
# ~/.config/dbtl/themes/mine.yml
base: dusk              # optional preset to start from (default: "default")
colors:                 # override any role; the rest comes from the base
  accent: "#ffcc66"     # "#rrggbb" truecolor...
  class_table: 114      # ...or an xterm-256 index
```

Then run it by name:

```sh
dbtl --theme mine
```

| Key | Meaning |
|---|---|
| `base` | The preset to start from. Defaults to `default`. |
| `colors` | A map of role name to color. Each color is `#rrggbb` or an xterm-256 index. Roles you leave out come from the base. |

Every role name and what it paints is in the [themes reference](../reference/themes.md). A user file named like a preset (`default.yml`, `dusk.yml`, …) overrides that preset for you.

### Errors and warnings

An unknown role or an invalid color stops startup with an error that lists the valid values.

If a palette makes a lens invisible, for example a lens tint that equals a node color, `dbtl` still starts but prints a warning. See [Lenses](lenses.md).
