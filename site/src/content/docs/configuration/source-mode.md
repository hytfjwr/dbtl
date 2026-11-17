---
title: Source mode
description: What dbtl reads and approximates when it builds the graph straight from dbt project files, with no dbt compile and no manifest.json.
---

Source mode builds the lineage graph from the project files themselves. It never runs `dbt`, so it needs no `target/manifest.json`. `dbtl` uses it when there is no manifest to auto-detect, or when you pass `--source <DIR>`. See [choose a data source](../cli/overview.md#choose-the-data-source).

Source mode aims to produce the same graph as a real `dbt parse` for the common project shape. The repository's test suite compares both loaders on one fixture project. It is still an approximation, and this page lists where it stops.

## What source mode reads

| Input | What `dbtl` takes from it |
| ----- | ------------------------- |
| `dbt_project.yml` | The project name, the resource paths (`model-paths`, `seed-paths`, `snapshot-paths`, `macro-paths`, `docs-paths`), and the `models:`, `seeds:`, `snapshots:`, and `sources:` config trees. |
| Model `.sql` files | Literal `ref()` and `source()` calls, and `config()` arguments. |
| Model `.py` files | Literal `dbt.ref()` and `dbt.source()` calls, and `dbt.config()` arguments. |
| `schema.yml` files | Sources, models, columns, descriptions, tests, `config:` blocks, exposures, and unit tests. |
| Seed `.csv` files | One seed per file, with the header row as its columns. |
| Snapshots | `{% snapshot %}` blocks in `.sql` files, and YAML-defined snapshots. |
| Project macros | The `ref()` and `source()` calls inside macros that your models invoke. |
| `.md` files | `{% docs %}` blocks, so `{{ doc('name') }}` in a description expands the way dbt renders it. |
| `target/compiled/` | Compiled SQL for the SQL preview and the SQL rules, when you have run `dbt compile`. It never changes the edges. |

Two details about the resource paths:

- `data-paths` and `source-paths` are accepted as the older names of `seed-paths` and `model-paths`. `data-paths` alone still emits dbt's rename-to-`seed-paths` deprecation warning. If a file declares both `seed-paths` and `data-paths`, `seed-paths` wins with a warning.
- YAML property files are read only under the configured model, seed, and snapshot paths. `target/`, `dbt_packages/`, and stray YAML elsewhere are never read.

## How configs resolve

A setting can come from several places, and the most specific one wins. For `materialized` the order is:

1. `config()` in the model file.
2. A `config:` block in `schema.yml`.
3. The `dbt_project.yml` config tree, where the deepest matching path wins.
4. dbt's default, `view`.

Governance settings follow the same order: `access`, `group`, `contract`, `incremental_strategy`, and `language`. `constraints` and `deprecation_date` come from `schema.yml` only. A model with no `access` reads as `protected`, like dbt's default.

A Python model carries one quirk: with no `materialized` anywhere, it materializes `table` — dbt's Python default — and that default outranks the `dbt_project.yml` tree.

`config()` takes either keyword arguments — optionally followed by one trailing `**{...}` spread — or a single positional dict. `config(materialized='table')`, `config({'materialized': 'table'})`, and `config(**{'materialized': 'table'})` mean the same. A `*`/`**` over a non-literal operand — `config(**var('cfg'))` — is evaluated by dbt at render, so source mode applies the call's explicit keyword arguments and warns that the rest could not be read. Any other argument shape (a dict mixed with kwargs, a repeated kwarg, `*{...}`) is a dbt error, so source mode applies nothing from the call and warns rather than guessing. `meta` merges per top-level key, with deeper levels winning.

A `config()` call only counts inside Jinja tags — `{{ config(…) }}`, `{% do config(…) %}`, `{% set _ = config(…) %}`, and `{% if`/`{% elif` conditions — never in plain SQL text. Calls apply in order; calls inside a Jinja conditional arm are scoped to that arm. An arm whose condition is provably false (`{% if false %}`, `{% if execute %}`, a resolvable `var()`) contributes nothing — `execute` is parse-time false, the same as dbt's parser context. An arm whose condition cannot be evaluated — `target.name`, a macro call, a `{% for %}` header, or a name bound by `{% set %}`/`{% for %}`/macro signatures — may still contribute: for `materialized`, `meta`, `tags`, and other non-`enabled` keys both arms' calls apply in source order (the conservative read), while `enabled` follows the stricter rule in the next section. Calls inside deferred definition bodies — `{% macro %}`, `{% test %}`, `{% docs %}`, `{% materialization %}`, `{% verification %}` — never configure the model; they run (at most) at an invocation the parse can't prove.

Node `tags` union from three channels, in order: the `+tags` rungs of `dbt_project.yml` (accumulated outermost level first), the file's own `config(tags=…)` / `dbt.config(tags=…)` calls, and the schema entry's `config.tags:`. A schema entry's bare `tags:` property is parsed by dbt but never applied, so source mode ignores it too. Scalar tags wrap into one-element lists and non-string scalars stringify, the way dbt coerces them.

Source tables resolve `meta`, `freshness`, `loaded_at_field`, and `tags` through the table, source, and `sources:` tree levels. A table's top-level `loaded_at_field: null` clears the inherited value; the same `null` under `config:` clears nothing — it simply declines to override. Only a table's `config: enabled:` disables it.

## How edges are found

Edges come from literal `ref()` and `source()` calls found by scanning the SQL. Jinja is not executed to find them. Some details follow dbt:

- A `{# … #}` comment hides a `ref()`, but a `--` or `/* … */` SQL comment does not. dbt renders Jinja before SQL comments exist, so a commented-out `ref()` is still a dependency.
- A `ref()` that names nothing the project defines is dropped. The [`unresolved_references`](../reference/lint-rules.md#unresolved_references) lint rule reports it.
- A `ref()`/`source()` in either arm of a `{% if %}` produces an edge — a possible edge is better than a missing one — unless the arm is provably dead (a statically-false condition drops the call, same as dbt's own extractor). Calls inside deferred definition bodies — `{% macro %}`, `{% test %}`, `{% docs %}`, `{% materialization %}`, `{% verification %}` — produce no edges; invoking a model-local definition marks the node unprovable rather than fabricating the edge.
- A project macro that wraps `ref()` or `source()` produces edges. The call's literal arguments bind to the macro's parameters, and nested project macros are followed up to a depth limit. A macro's `{% if %}` branches are not evaluated, so the calls in every branch become edges. A possible edge is better than a missing one.
- A call that cannot be resolved adds no edge. That covers a macro from an installed package, a `var()` or computed argument, and a call past the depth limit. Lint rules that judge a node's parents or children treat such a node as unprovable and stay quiet.
- `.py` model files contribute edges the way dbt's own Python scan does: literal `dbt.ref()`/`dbt.source()` calls resolve wherever they appear — helper functions, module scope, and f-string interpolations included — and `dbt.config()` sets the node's config instead of making an edge. A call whose arguments are not literal, such as a variable or a computed value, adds no edge and marks the node unprovable. `dbt.config()` follows the same shape rule as `config()` in SQL — kwargs or one positional dict — except `*`/`**` spreads, which dbt's Python extractor cannot evaluate and so never apply.

## Enabled expressions and `--vars`

`enabled` can be a literal or a Jinja expression, in `config()`, in a `schema.yml` `enabled:` or `config: enabled:`, or in a `+enabled` key of `dbt_project.yml`. Source mode evaluates this subset:

- Literals, and `var('name')` or `var('name', default)`.
- Comparisons such as `==`, `!=`, and `<=`.
- `and`, `or`, and `not`.
- `in` over a literal list, and parentheses.
- The `| as_bool` filter.
- The Jinja `is` tests — `defined`, `undefined`, `none`, `boolean`, `number`, `string`, `sequence`, `iterable`, `mapping`, `even`, `odd`, and `sameas` on a literal — with `is not` negations.
- A project macro call — `my_macro(…)` or `<project>.my_macro(…)` — whose body is a simple value shape: straight-line `{% set %}` assignments, `{% if %}` blocks that only raise or log, and one final `{% do return(…) %}`/`{{ return(…) }}`. Arguments bind positionally or by keyword against the signature's defaults, nested project macros follow, and evaluation caps out at a fixed depth. Package macros, `adapter.dispatch`, and bodies with `{% for %}` or value-affecting branches stay unprovable.

A resource is dropped only when its expression evaluates to a proven `false` on a `config()` call that provably executes. An expression outside the subset, such as `target.name` or a macro call outside the value shape above, or a `var()` with no value, keeps the resource enabled and warns once — warnings fold by expression, so the same call in many files reports as one line. The kept resource is marked uncertain: lint rules drop every finding that names it, and degree rules treat its edges as unprovable, so nothing reports a claim whose premise may not exist.

The same rule applies when the call sits inside a `{% if %}`/`{% elif %}`/`{% else %}` arm: a statically-true arm's `enabled=false` drops the node, a statically-false arm's never applies, and when the arm's condition cannot be evaluated the node stays enabled, warns once naming the condition, and is marked uncertain. `execute` reads as parse-time `false`, names bound by `{% set %}`/`{% for %}`/`{% macro %}`/`{% import %}`/`{% with %}`/`{% call %}` are unprovable, and so are the dbt parse-context names (`target`, `this`, `model`, `ref`, `source`, `config`, `env_var`, `is_incremental`, and the rest). A `{% for %}` arm is always unprovable from source.

Variables resolve in these scopes:

- `+enabled` keys in `dbt_project.yml` see only the top level of `vars.yml`. dbt renders that file before it loads its own `vars:`.
- Every other `enabled` expression sees `vars.yml` when the project has one, and `dbt_project.yml`'s `vars:` otherwise. A project-scoped key shadows a top-level key.
- `--vars '<YAML|JSON>'` outranks both everywhere, in the graph, a source-parsed `--diff` baseline, and TUI reloads. Manifest mode ignores `--vars` with a warning, because the manifest already carries dbt's rendered values.

## Tests, docs, and other resources

- Generic tests in `schema.yml` become test records on their model, column, or source. They are never graph nodes, the same as in manifest mode.
- Unit tests under `unit_tests:` attach to their `model:`. A `versions:` filter expands one `unit_test…_v<v>` record per selected member, matching the version spelling verbatim like `ref(name, v=…)` does.
- `versions:`-declared models synthesize the same nodes a manifest carries. Each `v:` binds its implementation file — `defined_in:` first, then the `<name>_v<v>` convention, with the latest also allowed to live in `<name>.sql` — into a `model.<project>.<name>.v<v>` node, so the file stem never becomes a separate refable model. A bare `ref('name')` resolves to `latest_version:` (or the pairwise max of the declared versions); `v=`/`version=` pins name the exact member (`v='v3'` answers `v: 'v3'`, not `v: 3`). Enabled folds in-file `config()`, the version's `config.enabled:`, the model's, then the project tree; a disabled member drops and a pin on it reports as a disabled version. Version-level `description`, `deprecation_date`, `config:`, and `meta:` layer over the model-level declarations; `columns:` `include:`/`exclude:` markers slice the inherited set while a same-named def replaces its column (its tests union onto the inherited ones), and a version's own `tests:`/`data_tests:` replaces the model-level list outright. Shapes dbt rejects outright (a repeated `v:`, an undeclared `latest_version`, a missing file, `<name>.sql` beside the family) warn and degrade instead of failing the load.
- Exposures are read from YAML, including their `depends_on:` edges, `tags`, and `group`. An exposure's `tags:` unions with the `exposures:` tree's `+tags`; declaring `config.tags` on the entry replaces the tree's `+tags` outright — even `config: {tags: []}` does. `config.group` is the only group channel (a top-level `group:` is a dbt parse error — source mode warns and ignores it); the tree's `+group` fills in when it is absent. A `semantic_models:` entry only marks its `model:` as consumed, so rules such as `unused_models` see the consumer.
- Doc blocks resolve from the project's own `.md` files. A `doc()` call that cannot resolve keeps its raw text and warns, where dbt would fail.

## What source mode approximates or skips

- **Dynamic version pins.** A `v=` argument that is not a literal — `v=var('x')`, an unbound macro parameter — resolves to the family's latest member, the closest a static scan can get to dbt's run-time binding.
- **Jinja in SQL.** SQL rules and column lineage render the Jinja in a model approximately, only to parse it. `{% set %}` and `{% macro %}` bodies are blanked, mutually exclusive `{% if %}` arms render as one arm, and an expression `dbtl` cannot evaluate becomes a generated name. A separator a `{% for %}` body leaves dangling — `loop.last` is not evaluated, so the `,`/`or`/`union all` inside `{% if not loop.last %} SEP {% endif %}` stands with no right operand — is blanked on a retry; a string literal, quoted identifier, or dollar-/`q'[..]'`-quoted body counts as an operand, so a `,` inside `('1', '2')` or before a quoted relation is never blanked. The graph never depends on that render.
- **`{{ }}` expressions render by position.** A `{{ name(…) }}` that resolves to a root-project macro expands first when its whole call is provable — the body is only literal text, `{{ }}` interpolations, `ref()`/`source()` calls, `{% set %}`/`{% if %}`/`{% elif %}`/`{% else %}`/`{% for %}` over values the arguments decide (strings, integers, floats, lists, tuples, dicts, `range()`, and the `loop.*`/`| join`/`| length`/membership/`is`-test vocabulary around them — a `for … if` filter settles before `loop.last`/`loop.length`, and an empty `for` takes its `{% else %}`), and other expandable project macros; every argument binds by dbt's own rules (positional, keyword, or a literal default), and `{{- -}}`/`{%- -%}` whitespace control trims exactly the way Jinja does. `{{ join_on('c', 'a.id') }}` then parses as the `left join` the macro writes, `{{ status_codes() }}` parses as the tuple it builds, and a model whose statement IS such a call parses instead of skipping the SQL rules — a relation name in the body reads the CTE it spells, while a `ref()`/`source()` it emits still stands for the relation rather than an in-file `WITH`. Any part that can't prove — a `{% %}` outside that subset, a condition or `{% set %}` value the arguments don't decide (a `ref()` relation used as a value, a `var()`, an unknown name), a rejected argument, a recursive call, a package or builtin callee — leaves the tag to the positional shape below. An expression `dbtl` cannot evaluate picks a shape that parses where it stands: on an `IN`/`ANY`/`SOME`/`ALL` right-hand side it becomes `(NULL)` — a list that parses under every dialect whether the macro returns a tuple or a subquery — and directly after a complete expression it drops to whitespace, because only a whole clause (a `JOIN` arm, an `AND` condition, a `LIMIT`) can stand there. "Complete" means a `)`, `]`, closed literal, number, or non-keyword identifier — the strict rule never closes on a keyword, so a `{{ }}` after `AT TIME ZONE` keeps its value shape. If all four renders fail to parse, the four retry once more under a looser boundary that also counts `null`/`true`/`false` and column-shaped non-reserved words (`id`, `name`, `status`, `value`) as completing the expression, so `where id is not null {{ group_by() }}` parses instead of going unparsed; a word that still awaits an operand (`interval`, `to`, `date`, `any`, `first`) keeps the value shape in the retry too. A model whose render dropped a clause that way is marked: it still parses and the other rules still judge it. Each absence-proving finding is withheld only when a replaced tag could actually invalidate it, judged per tag and per rule: `cartesian_join` yields to a clause-boundary tag in the same SELECT positioned after the joined relation (it could emit the `ON`/`WHERE`), `unused_cte` only when a tag — clause boundary or `(NULL)` right-hand side — could emit a read of that CTE's name, so an unrelated `{{ codes() }}` no longer silences the CTEs it can't reach, `order_by_without_limit` only when a clause-boundary tag sits after the outer `ORDER BY` where it could emit the bound, and `unknown_column_reference` still yields to any clause-boundary tag model-wide. A root-project macro's body (and the root macros it calls) is scanned for the names it can emit; a package macro is judged by a rule of thumb — it can't name the caller's CTEs unless an argument's literal carries them, so only a matching string literal withholds a CTE for it (an approximation: a package macro coincidentally reading a same-named table slips through, since over-withholding is the worse direction); a callee that resolves nowhere — an unknown bare name or `adapter.dispatch(…)` — or a value-shaped tag (`var()`, a `{% set %}`-bound name) could emit anything and withholds them all. `--fix` still plans no edit on a marked model, and column lineage resolves it by name match like an unparsable model. The (model, rule) pairs a replaced tag muted are named on the lint summary — the `N models had SQL findings withheld next to {{…}} tags` line in text, `summary.withheld` in JSON — informational only, so the muted judgment is never invisible.
- **Installed packages.** Only the root project is parsed. `dbt_packages/` is never read for nodes and `--include-packages` does nothing but print a warning. Package names are still read so `ref()` resolves the way dbt does: a two-argument `ref('pkg', 'name')` scopes to that package outright — it resolves to the package's model, never to a same-named root model — and a bare `ref('name')` the root doesn't define can still resolve to a package's member. Either way the call makes no edge (the package node isn't in the graph); the caller is recorded as possibly having an unshown parent, so rules like `root_models` stay quiet. A `ref` naming a package that isn't installed resolves to nothing and `unresolved_references` reports it — the package names are also how that rule tells a typo from a missing install.
- **Other resource kinds.** Singular tests, analyses, metrics, and saved queries are not read.
- **Adapter and profile data.** Nothing from `profiles.yml` or the warehouse is available. The `database` and `schema` of non-source nodes are unknown, and `target.*` expressions are not evaluated. The SQL dialect is `generic` unless you pass `--sql-dialect`.

## Get manifest precision

Use a compiled manifest when exact dbt output matters. Keep `target/manifest.json` current with `dbt parse` or `dbt compile`. In the TUI, press `P` to run `dbt parse` and switch to the generated manifest in place. With `--watch`, the TUI reloads when the project files change.
