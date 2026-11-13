<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->

# Themes

The TUI's colour themes — the built-in presets below plus user files. `--theme <name>` (or `DBTL_THEME`) names a preset or a user file, then falls back to a literal path to a theme YAML file. User files are `*.yml`/`*.yaml` under `$XDG_CONFIG_HOME/dbtl/themes` (else `~/.config/dbtl/themes`); a file's stem is its theme name, and a user file shadows a same-named preset. `dbtl --list-themes` prints every resolvable name.

## Presets

| Name | For |
| ---- | --- |
| `default` | xterm-256 palette for dark backgrounds; the fallback when no theme is named |
| `dark` | truecolor, for near-black neutral-cool backgrounds |
| `dusk` | truecolor, for lighter blue mid-dark backgrounds |
| `light` | truecolor, for light backgrounds — the only light preset |
| `warm` | truecolor, for warm umber backgrounds |

## Custom theme files

A theme file starts from a `base` preset (default `default`) and overrides roles under `colors:`:

```yaml
base: dark
colors:
  accent: "#87afff"
  class_table: 114
```

A colour value is a `"#rrggbb"` string or an xterm-256 index (`0`–`255`). Unknown `base` names, unknown role names, and unparsable colours are errors naming the accepted alternatives. `dbtl` warns — but still launches — when a theme's lens tints collide with a class colour, because a tint that shares a class's colour is invisible on those nodes.

## Roles

Every `colors:` key the parser accepts — the same list the `unknown colour role` error names.

### Core chrome

| Role | Paints |
| ---- | ------ |
| `accent` | The primary accent: focused-pane borders/titles, modal frames, carets, the selection bar (via REVERSED), the sort chip. |
| `border_idle` | Unfocused pane borders — recede into the background. |
| `text_bright` | Bright body text (the selected-name echo in the status bar, modal titles). |
| `text_dim` | Secondary text: titles of unfocused panes, status-bar core, key labels. |
| `text_faint` | Tertiary text: dependency badges, minimap. |
| `lineage_dim` | Off-path lineage nodes under the focus dim (cursor off-root). Deliberately brighter than `text_faint`: the dim must recede from the path band yet stay readable — the off-path nodes are context, not noise. Distinct from `text_dim`/`sb_thumb`/`class_other` so no surface aliases by accident. |
| `surface` | The status-bar background band. |
| `surface_hi` | A raised surface: the root↔cursor path band, gauge tracks. |
| `sb_thumb` | The right-border scrollbar thumb. |

### Semantic states

| Role | Paints |
| ---- | ------ |
| `ok` | Good / passing: the coverage-gauge high grade, the focus chip. |
| `warn` | Caution: coverage chip + mid gauge grade, lineage scroll markers, the top-hub stats bar. |
| `danger` | Problem: impact chip, low gauge grade, coverage-gap names, and the `Warn`/`Violation` lens tints (vivid — no class colour may equal it, so the tint can never disappear into one). |
| `orphan` | A leaf model with no downstream (a quieter red than `danger`). |
| `gold` | Search/palette match-char highlight, the bookmark star + chip. |

### Materialization classes

| Role | Paints |
| ---- | ------ |
| `class_table` | A `table`-materialized model — the healthy resting state (aliases `ok` in the default). |
| `class_view` | A `view`-materialized model. |
| `class_incremental` | An `incremental`-materialized model. |
| `class_ephemeral` | An `ephemeral` model. |
| `class_source` | A dbt source — an external input. |
| `class_seed` | A dbt seed. |
| `class_snapshot` | A dbt snapshot. |
| `class_exposure` | A declared downstream consumer (exposure) — the impact terminator. |
| `class_other` | A model with an unknown or missing materialization. |

### Layers

| Role | Paints |
| ---- | ------ |
| `layer_staging` | The `staging` layer's colour. |
| `layer_intermediate` | The `intermediate` layer's colour. |
| `layer_marts` | The `marts` layer's colour. |
| `layer_utilities` | The `utilities` layer's colour. |
| `layer_other` | The colour for nodes in no known layer. |

### Degree-heat ramp

| Role | Paints |
| ---- | ------ |
| `heat_low` | Low downstream-degree tint. |
| `heat_mid` | Mid downstream-degree tint. |
| `heat_high` | High downstream-degree tint. |

### Diff lens

| Role | Paints |
| ---- | ------ |
| `diff_add` | Nodes added vs the `--diff` baseline. |
| `diff_mod` | Nodes modified vs the `--diff` baseline. |

### Status-bar chips

| Role | Paints |
| ---- | ------ |
| `chip_view` | The `[view]` direction/depth chip (a chip role, deliberately NOT `class_view`: it names the lineage view filter, not a materialization). |

### Overlays

| Role | Paints |
| ---- | ------ |
| `section` | Overlay section headers (`columns:` / `tests:` / stats sections). |
| `sql_keyword` | SQL keyword highlight in the `s` preview. |
| `sql_string` | SQL string literals in the `s` preview. |
| `sql_comment` | SQL comments (`--`, `/* */`, and Jinja `{# #}`) in the `s` preview. |
| `sql_jinja` | Jinja expressions/statements (`{{ ref(..) }}` / `{% if %}`) in the `s` preview. |

### Stats mini-bars

| Role | Paints |
| ---- | ------ |
| `bar_resource` | The resource-type bar's fill. |
| `bar_materialization` | The materialization bar's fill. |
| `bar_degree` | The degree bar's fill. |
| `bar_transitive` | The transitive-hub bar's fill. |

