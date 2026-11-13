<!-- GENERATED FILE — do not edit. Regenerate with: cargo xtask gen-docs -->

# Configuration reference

dbtl reads two YAML config files:

- **`.dbtl.yml`** — project-local policy (lint rules, check gates, docs options), discovered beside the data source.
- **`~/.config/dbtl/config.yml`** — user-level defaults for the interactive app (precedence: config < saved state < explicit CLI flags).

Both ship a generated JSON Schema under `schema/` — point yaml-language-server at them for completion and validation:

```yaml
# .dbtl.yml
# yaml-language-server: $schema=https://raw.githubusercontent.com/hytfjwr/dbtl/main/schema/dbtl.schema.json

# ~/.config/dbtl/config.yml
# yaml-language-server: $schema=https://raw.githubusercontent.com/hytfjwr/dbtl/main/schema/config.schema.json
```
(A relative path like `./schema/dbtl.schema.json` works when the repository is checked out.)

## `.dbtl.yml`

Looked up beside the data source: the project directory in source mode; in manifest mode the project root first, then `target/` as a fallback. Unknown **top-level** keys are ignored with a warning; unknown keys inside `lint:`/`check:`/`docs:` — or inside a rule or gate entry — are hard errors.

### Top-level keys

| Key | Type | Default | Description |
| --- | ---- | ------- | ----------- |
| `fail_on` | `error` \| `warn` \| `never` | `error` | `error` \| `warn` \| `never` — the minimum severity at which findings fail a run. `--fail-on` overrides it. |
| `lint` | object | — | `lint:` — the rule-set preset, per-category severities, per-rule enables, severities, parameters, excludes, naming prefixes, and the baseline path. |
| `check` | object | — | `check:` — gate severities/parameters and whether the lint gates fold into `dbtl check`. |
| `docs` | object | — | `docs:` — generated-docs options. |
| `owner_meta_key` | string | `owner` | The `meta:` key the affected-owners report reads (default `owner`; wins over `config.yml`'s `owner_meta_key`). |

Any other top-level key is allowed but ignored (it produces a warning).

### `lint:`

| Key | Type | Default | Description |
| --- | ---- | ------- | ----------- |
| `model_fanout_threshold` | integer ≥ 1 | `5` | Compat flat keys — the same value `rules.<id>.threshold` sets; both spellings together are an error naming both keys. |
| `source_fanout_threshold` | integer ≥ 1 | `2` | Compat flat key — the same value `rules.source_fanout.threshold` sets; both spellings together are an error naming both keys. |
| `naming_prefixes` | object: layer → list of strings | `intermediate: [int_]` `marts: [fct_, dim_, rpt_]` `staging: [stg_]` | layer dir → allowed prefixes; merges per-layer over the defaults. |
| `baseline` | path | `.dbtl-baseline.json` | The frozen-violations file — resolved against this file's directory in `ProjectConfig::load`. |
| `preset` | `minimal` \| `standard` \| `strict` | `standard` | base lint rule set: `minimal` (error-default rules only), `standard` (the shipped default), `strict` (every rule, opt-in included). `lint.categories`, `lint.rules.<id>`, and `--rules` sit above it. Unknown names are an error. |
| `categories` | object: category → `error` \| `warn` \| `off` | — | category → `error` \| `warn` \| `off` — one severity (and enable/disable) for a whole category at once. Above `lint.preset`, below `lint.rules.<id>`. Unknown category names are an error. |
| `rules` | object | — | rule id → `false` (off) \| `true` (the registry default) \| a settings object (`severity`, `exclude`, and the rule's declared parameters). Unknown rule names are an error. |

#### `lint.rules.<rule-id>`

`false` disables the rule, `true` enables it (at the `lint.categories` severity when one is set, else the registry default), or a settings object sets severity/excludes/parameters. Per-rule entries resolve above `lint.categories`, which resolves above `lint.preset`.

| Key | Type | Description |
| --- | ---- | ----------- |
| `severity` | `error` \| `warn` \| `off` | The severity findings report at; `off` disables the rule. |
| `exclude` | list | dbt selector specs — findings whose node matches the union are suppressed. Each item is a bare selector string or `{select: <selector>, reason: <why>}`; the reason reports under `--show-suppressed`. |
| _rule parameters_ | per-rule | Declared per rule — see the parameters tables in the [lint rules reference](lint-rules.md); an undeclared name is an error. |

### `check:`

| Key | Type | Default | Description |
| --- | ---- | ------- | ----------- |
| `lint` | boolean | `false` | `check.lint` — include the lint gates in `check` runs without `--lint`. |
| `gates` | object | — | gate id → bare severity (`error` \| `warn` \| `off`) or `{severity, <params>}`. Unknown ids and params are errors. |

#### `check.gates.<gate-id>`

A gate accepts a bare severity (`error` \| `warn` \| `off`) or an object `{severity, <params>}`. An entry that sets only parameters resolves to `error` when the gate is off by default.

| Gate | Default | Needs `--diff` | Parameters | Summary |
| ---- | ------- | -------------- | ---------- | ------- |
| `coverage` | `error` | no | `min` (integer 0–100) | Root-project models/snapshots/seeds with a test below `min` percent |
| `column_coverage` | `off` | no | `min` (integer 0–100) | Declared columns covered by the schema-test graph below `min` percent |
| `doc_coverage` | `off` | no | `min` (integer 0–100) | Models/snapshots/seeds with a description below `min` percent |
| `new_layer_violations` | `error` | yes | — | Layer-violating edges the diff introduces (needs `--diff`) |
| `untested_changes` | `error` | yes | — | Models added or modified by the diff that have no tests (needs `--diff`) |
| `breaking_columns` | `off` | yes | — | Columns the diff removes or renames that dependents still reference (needs `--diff`) |
| `changed_hubs` | `warn` | yes | `min_downstream` (integer ≥ 1) | Diff-changed models with `min_downstream` or more dependents (needs `--diff`) |
| `affected_exposures` | `warn` | yes | — | Exposures whose upstream intersects the diff's blast radius (needs `--diff`) |
| `blast_radius` | `off` | yes | `max` (integer ≥ 0) | The diff's blast radius — affected models over `max` (needs `--diff`) |
| `docs_drift` | `error` | no | `dir` (path) | The generated docs under `dir` differ from the project's current output |

### `docs:`

| Key | Type | Default | Description |
| --- | ---- | ------- | ----------- |
| `catalog` | boolean | `false` | `docs.catalog` — opt in to the sibling `catalog.json` (same path rule as the CLI's auto-detect, but only when the file asks for it). |
| `run_results` | boolean | `false` | `docs.run_results` — opt in to the sibling `run_results.json`. |
| `lineage_depth` | integer ≥ 1 or `all` | `2` | Mermaid ancestry depth per node page: an integer ≥ 1, or `all` for the whole closure (default 2). |

## `config.yml` (`~/.config/dbtl/config.yml`)

User-level defaults for the interactive app — every key optional. Unknown keys are ignored.

| Key | Type | Description |
| --- | ---- | ----------- |
| `theme` | string | Default colour theme: a preset name or a theme under `~/.config/dbtl/themes/<name>.yml`. Lower precedence than `--theme`/`DBTL_THEME`. |
| `lens` | string | Default lineage lens name. |
| `density` | string | Default lineage density name. |
| `sort` | string | Default model-list sort name. |
| `group_by_meta` | string | Group the model list by this `meta` key's VALUE instead of the layer dirs (`group_by_meta: owner` → `owner=alice` groups, `(no owner)` last; unset = the default layer grouping). The `.` sort cycle still orders within each group. |
| `owner_meta_key` | string | The `meta` key the PR-impact pack collects as "affected owners" (default `owner`) — read by the `D` modal, its `e` export, and `dbtl impact`/`check`. |
| `lineage_collapse` | integer | The sibling-collapse threshold: a lineage column stacking more than this many same-parent siblings contracts the tail into one `+M more` marker (default 8; `0` = never collapse). Config-only — it is a display policy, not a per-project pref, so the state file ignores it. |
| `inspector` | string | Default inspector visibility pref (`auto`/`on`/`off`) — the `I` cycle. Same Option-layering as the other pref names. |
