# Changelog

All notable changes to `dbtl` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The docs site is easier to drive with a keyboard and assistive tech:
  single-key shortcuts can be switched off from the status line, code blocks
  are only tab stops while they actually scroll, and search results rank
  titles and headings above body text.

### Fixed

- Source mode now resolves `var('name'[, default])` (optionally
  `| as_bool`) in `+enabled` config-tree keys and in-file
  `config(enabled=…)` calls, through the same var tables dbt renders
  each in — `vars.yml` for `dbt_project.yml`'s own `+enabled` strings,
  the var table a resource's `config()` sees (`vars:`, or `vars.yml`
  when declared, with project-scoped keys shadowing the top level). A
  `var()`-gated `enabled` no longer lists models dbt parks disabled; a
  value dbt's boolean validation would reject keeps the node enabled
  with a warning naming the file or tree level.
- Subcommand `--help` no longer advertises global flags the subcommand
  does not accept — `dbtl ls --help`, for example, omits `--catalog` and
  `--run-results`. Passing one explicitly still fails with the usage
  error naming the unsupported flag.
- Documentation drift corrected across the site and README: the lint
  example tallies match the sample project (10 errors / 38 warnings /
  17 rules / 3 skipped), the report-format count is eight, `o` opens the
  model's file in `$VISUAL` (falling back to `$EDITOR`), `r` reloads the
  data source, the model list drawer covers widths 50–79, scrolling
  modals describe their real close keys, and lint rules are grouped by
  the registry's own categories.
- Closing the stdout pipe early (`dbtl ls … | head -1`, `dbtl columns … |
  head`, `dbtl docs … | head`) now exits cleanly instead of panicking on a
  broken pipe — every CLI stdout write goes through one writer that treats
  `EPIPE` as a normal end of output.

### Added

- `SECURITY.md` with private vulnerability-reporting instructions.
- A generated `docs/reference/themes.md` page lists every theme role a
  custom theme file may set, sourced from the same table the parser
  accepts, and the CLI reference now lists the accepted `--sql-dialect`
  spellings. Undocumented features covered: `e` export file names, the
  `Cmd-` key terminal requirement, bare-`dbtl` non-TTY behaviour, and
  the `--watch` poll interval and watched file set.
- `tests/doc_links.rs` now scans `site/src/content/docs/**` too — `dbtl`
  invocations in fenced blocks and inline code are checked against the
  flags each subcommand actually accepts.

## [0.6.3] - 2026-11-13

### Added

- The docs site gained a changelog page at
  https://hytfjwr.github.io/dbtl/changelog/, rendered from this file at
  each release and linked from the footer and the docs menu.
- The playground now runs the real `dbtl` engine — the Rust code compiled
  to WebAssembly — inside a Web Worker in the browser. Selector matching,
  lineage, `lint` output (positions, excerpts, help/docs links, exit
  codes), and column-level lineage are the engine's own output, with a
  server-rendered snapshot of the default run shipped in the initial
  HTML. Model SQL and `.dbtl.yml` can be edited and re-linted in place,
  and a local `manifest.json` can be dropped onto the page — nothing is
  uploaded anywhere.

### Changed

- The docs site now ships a sitemap and Open Graph/Twitter card
  metadata (including a share image) so its pages preview correctly
  when linked, page titles name their nav group, and muted text and
  control borders meet contrast minimums in both themes.

### Fixed

- The docs site on phones and narrow windows: the menu drawer scrolls to
  the last link instead of stopping at the first screenful, and the page
  behind it no longer scrolls or stays reachable to assistive technology
  while it is open. Inline code wraps on small screens, the playground's
  selector field fits and no longer triggers focus zoom on iOS, heading
  anchors show on touch screens, the search dialog has a close button,
  and below 1180px a collapsible "On this page" list replaces the hidden
  outline.

## [0.6.2] - 2026-11-13

### Breaking changes

- **The `text` report format was rebuilt.** `dbtl lint` and
  `dbtl check --lint` now render each counted finding as an annotated
  block — `error[rule]: message` over a `--> file:line:column` position,
  the source excerpt with the span underlined, and `= help:`/`= docs:`
  footers — followed by `Found N errors and M warnings in K files` and
  `N rules reported findings; M skipped (…)` summary lines. The previous
  per-rule `PASS`/`WARN`/`FAIL` lines with indented findings are gone:
  clean lint rules fold into a single `PASS lint: N rules clean` row,
  `dbtl check` keeps a `STATUS lint <rule>: N [new] finding(s)` row for
  each tripped rule only, and `dbtl lint` reports skipped rules on the
  summary line instead of `SKIP lint <rule>:` rows. Scripts parsing the
  old layout should switch to `--format concise` or a machine format.
- **The lint rule `undocumented_public_models` was removed.** It flagged
  the same missing model and column descriptions `undocumented_models` and
  `undocumented_columns` already report, so every public model's gap was
  listed twice. The id stays reserved: naming it in `.dbtl.yml`, `--rules`,
  or `--explain` is a usage error that points at the two replacements, and
  baseline entries for it are ignored with a single stderr warning
  (`warning: baseline: ignoring N entries for removed rule
  undocumented_public_models`). Point any configured entry at
  `undocumented_models`/`undocumented_columns` instead — and regenerate the
  baseline (`dbtl lint --write-baseline`) to drop its stale rows.
- **The default lint severities were re-tiered.** Only rules that flag a
  broken guarantee or a build-time failure default to `error` now:
  `missing_primary_key_tests`, `public_models_without_contract`, and
  `unknown_column_reference` (raised from `warn` — a column the upstream
  model doesn't emit fails in the warehouse at run time). Every other rule
  ships at `warn` so it reports without gating: the ten former `error`
  rules `direct_join_to_source`, `multiple_sources_joined`, `source_fanout`,
  `model_fanout`, `root_models`, `rejoining_upstream`,
  `marts_depend_on_source`, `undocumented_models`, `naming_conventions`,
  and `layer_violation` now warn by default. The findings themselves are
  unchanged — the same projects report the same rows, fewer of them gate.
  Opt any rule back into `error` explicitly where it should keep failing
  builds:

  ```yaml
  # .dbtl.yml
  lint:
    rules:
      root_models: {severity: error}
      naming_conventions: {severity: error}
  ```

### Added

- Lint findings carry positions: `.yml` attribute findings underline their
  declaring `- name:` value, and the SQL rules (`select_star`,
  `hard_coded_references`, `cartesian_join`, `unknown_column_reference`,
  `forbidden_patterns`) underline the offending syntax in the model's
  `.sql` file — additional occurrences ride as secondary labels. A
  position a rule can't pin exactly is left file-scoped rather than
  guessed.
- Graph-rule findings point at the offending `ref()`/`source()`/`config()`
  call site in the model file rather than only naming the file, related
  nodes are named (`source.name.table`, `name.vN`, plain names), and a
  help suggestion appears when a staging replacement is uniquely
  identifiable.
- Source mode loads the same governance and config fields as manifest
  mode — `access` (models default to `protected`), `group`,
  `contract.enforced`, `constraints`, `incremental_strategy`,
  `deprecation_date`, and `language` — through the same
  in-file → schema.yml → project-tree precedence chain as `materialized`.
- `--format concise` on `check`/`lint`: one finding per line as
  `file[:line[:column]]: severity[rule] message` for editors and grep.
- `--color auto|always|never` on `check`/`lint`: ANSI styling for the
  stdout human formats, honoring the terminal and `NO_COLOR`/
  `CLICOLOR_FORCE` under `auto`. Report files, machine formats, and the
  step summary are always unstyled, and a closed stdout pipe
  (`dbtl lint | head`) ends the report without panicking.
- Three lint rules. `unused_models` (default: staging + intermediate
  layers, retargetable via `layers`) flags a model nothing reads — any
  downstream counts, including exposures, semantic models, and consumers
  hidden by package pruning; `references_deprecated_model` flags a node
  whose direct parent carries a `deprecation_date`, pointing at the
  offending `ref()`/`source()` call; `undeclared_warehouse_columns`
  reports warehouse columns the node's declared schema never mentions
  (models, seeds, and snapshots; `--catalog` required). Source mode now
  reads `semantic_models:` entries — including `enabled: false` under
  either spelling — so a model consumed only by the semantic layer
  answers "used" exactly like manifest mode.
- Four lint rules (36 → 40). `trailing_semicolon` (error, graph-only —
  it reads `raw_code`, so it runs even where no SQL parser is available)
  flags a model whose SQL ends with a `;` after trailing whitespace,
  comments, and non-outputting Jinja tags are discounted, and points at
  the semicolon itself; `duplicate_output_columns` (error) flags a final
  SELECT emitting one output name twice — unquoted names compare
  case-insensitively, quoted names case-sensitively, and an expandable
  wildcard's carried columns count — pointing the primary position at
  the repeated name's second occurrence; `unused_cte` (warn) flags a
  WITH clause nothing reads, from the body, a sibling CTE, or a nested
  query — a recursive CTE's self-reference never counts; and
  `order_by_without_limit` (warn) flags a final `ORDER BY` with no
  `LIMIT`/`FETCH`/`TOP` (an `OFFSET` alone is not a bound), pointing at
  the sort expressions. Python models are out of scope, and any position
  the raw source can't confirm is left file-scoped rather than guessed.

### Changed

- Lint findings now carry precise positions and guidance. `.yml` findings
  (descriptions, tests, freshness, contract columns) point at the
  declaring `- name:` entry's line, column and end column in text,
  markdown, junit, json, github, sarif and codeclimate output, and the
  finding anchor prints `file:line:column` — findings that only name a
  file no longer print a guessed `:1` (SARIF/codeclimate keep their
  format-required `startLine`/`begin` stand-ins). The json report also
  emits each finding's `label`, `help` and a per-rule `docs_url`, SARIF
  rules carry `help`/`helpUri`, and `lint --explain` shows `Help:` and
  `Docs:` lines.

### Fixed

- The `--select` help text now describes what the flag accepts: a model
  name or a selector spec — its first match opens in a tab and
  must be a model — and notes that `state:` atoms cannot match (the flag
  resolves before `--diff` loads).
- Lint rules no longer flag what they can't establish. `root_models` and
  `unused_sources` ignore neighbours removed by package pruning (a model
  whose only parent is a pruned package node is not a root, and a source
  whose only consumers live in a pruned package is not unused), and
  `root_models` also skips models whose `ref()`/`source()` calls never
  resolved to an edge — a root is only reported when every dependency
  call resolved.
- `layer_violation` now only judges the data-flow layers
  (`staging`/`intermediate`/`marts`): `utilities/` helpers and unmapped
  directories carry no direction, and both edge endpoints must be
  root-project models, so package taxonomy no longer fabricates
  backwards flows.
- `incremental_without_unique_key` skips the strategies dbt never keys
  on (`append`, `insert_overwrite`, `microbatch`) and names the
  configured `incremental_strategy` in its message.
- `missing_primary_key_tests` no longer treats a declared `primary_key`
  constraint as a substitute for a test — most warehouses never enforce
  it. Set `accept_primary_key_constraint: true` where the warehouse
  does. Both it and `undocumented_models`/`undocumented_columns` accept
  a `resource_types` list (`model`/`snapshot`/`seed`, plus `source` for
  columns) so snapshot/seed governance can be enforced or silenced
  deliberately.
- The SQL rules stay in their lane: python models are never scanned as
  SQL, a model whose SQL exists but fails to parse is counted in the
  report summary (`N models skipped SQL rules`) instead of silently
  passing, `cartesian_join` catches a comma-`FROM` without `WHERE` and
  never-rejecting `ON` conditions (`ON TRUE`, `ON 1 = 1`),
  `hard_coded_references` ignores `information_schema` reads at any
  qualification depth and accepts `ignore:` glob patterns for
  known-staging metadata, and `select_star`/`cartesian_join` cover UNION
  arms and `|>` pipe projections. `forbidden_patterns` stays a raw-text
  rule and no longer counts its matches against the unparsed tally.
- `warehouse_type_drift` compares types accurately: names are
  case/whitespace-normalized, a one-sided argument tail (`numeric` vs
  `numeric(10,2)`) still matches, and common plus adapter alias families
  are honoured under the active SQL dialect — BigQuery `INT64` vs
  declared `bigint` is no longer drift, and Snowflake `NUMBER` only
  counts as integer-shaped at scale 0. Catalog column lookup is
  case-insensitive. The TUI's per-model "last run" notes use the same
  comparison, so TUI and `dbtl lint` now agree on drift.
- Source-mode fidelity: a source's `schema` defaults to the source name
  when omitted, generic tests read `column_name` from `kwargs` (legacy
  and `arguments:` spellings), and blank-only column descriptions count
  as missing.

## [0.6.1] - 2026-11-12

### Breaking changes

- **The truecolor theme presets were replaced by original palettes** named
  `dark`, `dusk`, `light`, and `warm`; the old preset names are gone. An
  unknown name passed with `--theme`/`DBTL_THEME` exits `2` and lists the
  valid presets; a saved or configured name that no longer exists falls back
  to `default` with a warning. Pick a new preset with `dbtl --list-themes`,
  and update any `theme:` name or custom theme's `base:` that pointed at an
  old preset.

### Added

- Documentation site at https://hytfjwr.github.io/dbtl/, with search, a
  selector playground, and generated CLI, keybinding, lint-rule, and
  configuration reference. The README is now a short overview that links to
  the site.
- Releases now attach `demo.gif` — the recorded TUI demo, rendered at the
  release tag by the release pipeline (not covered by `checksums.txt` or
  attestations).

## [0.6.0] - 2026-11-12

### Breaking changes

- **Exit codes are now classified.** Gate failures still exit `1`, but usage
  and configuration errors now exit `2` and input errors (missing manifest,
  unreadable baseline, unresolvable git ref) exit `3`. Scripts that treated
  every non-zero code alike need no change; scripts that matched `1` for
  "anything went wrong" should match on non-zero instead, or use the new
  codes to distinguish a failed gate from a bad invocation. See
  [the exit-code table](https://hytfjwr.github.io/dbtl/configuration/exit-codes/).
- `dbtl impact` now reads `.dbtl.yml` and exits `2` on a malformed file.
- **The generated docs layout changed.** `dbtl docs` now writes node pages
  into per-resource-type directories (`models/`, `sources/`, `exposures/`,
  …) and carries new sections (SQL, column lineage cells, lint violations,
  owner/tag/group index pages). A committed docs tree must be regenerated
  once — `dbtl docs --out <DIR> --clean` removes the stale pages safely.

### Added

- `--catalog <path>` selects the warehouse `catalog.json` explicitly
  instead of only auto-detecting a sibling `target/catalog.json`. The
  data-input flags are now global — they bind before or after a
  subcommand — and a flag a subcommand does not consume is rejected as a
  usage error.
- `dbtl lint` subcommand: the static rule set on its own, with
  `--write-baseline`/`--no-baseline`, `--list-rules`, `--explain`, and the
  full report-format set.
- Lint rule registry: named rules with declared severities, parameters, and
  required inputs; a rule whose input is missing reports `SKIP` instead of
  failing. 15 additional graph/metadata rules ship at `warn` severity,
  plus SQL-backed rules (`select_star`, `hard_coded_references`,
  `cartesian_join`, `unknown_column_reference`, `forbidden_patterns`) and
  catalog-backed drift rules (`warehouse_type_drift`,
  `columns_missing_in_warehouse`).
- Project-local `.dbtl.yml`: per-rule enables, severities, thresholds,
  selector-based excludes, naming prefixes, `check.gates` severity/parameter
  policy, and `docs` options — plus per-node `meta.dbtl.ignore` suppression.
- Lint findings surface in the TUI by severity: the Lint lens tints each
  node by its worst finding, `F` filters the model list to linted nodes,
  `n`/`N` cycle through findings while the lens is active, and the
  inspector, the home Health line, and the stats worklist split counts
  into errors and warnings.
- `dbtl ls`: evaluate dbt selectors non-interactively (`names`/`uids`/
  `selector`/`json` output). `--select`/`--exclude` now also scope `check`,
  `lint`, `impact`, and `docs`.
- `--diff git:<rev>`: compare against the project as committed on any
  revision, materialized read-only via `git ls-tree`/`git cat-file` — no
  checkout needed.
- `dbtl check` gate policy is configurable: `--gate <id>=<severity>`
  overrides a gate's severity, the `docs_drift` gate reads a generated-docs
  directory via `--docs-dir`, and a gate missing its inputs reports `SKIP`.
- `dbtl check`/`lint` report formats: one `dbtl.report/v1` document rendered
  as `text`, `markdown`, `json`, `github` annotations, `sarif`,
  `junit`, or `codeclimate`; `--report <FMT>:<PATH>` writes files,
  `--step-summary` appends to `GITHUB_STEP_SUMMARY`, `--path-prefix`
  rewrites paths for repository subdirectories.
- Composite GitHub Action at the repository root (`uses: hytfjwr/dbtl`):
  installs a release binary, runs `dbtl check` with annotations, a step
  summary, optional SARIF upload and a sticky PR comment. A
  `.pre-commit-hooks.yaml` ships `dbtl-lint` hooks.
- `dbtl completions` (bash, zsh, fish, powershell, elvish) and a `dbtl man`
  roff page; both ship pre-generated in the release archives.
- Release artifacts for Windows (`x86_64-pc-windows-msvc.zip`) and musl
  Linux (`x86_64`/`aarch64-unknown-linux-musl.tar.gz`), published with
  `checksums.txt`, build-provenance attestations, and CycloneDX SBOMs.
- Generated reference documentation under `docs/reference/` (CLI,
  configuration, keybindings, lint rules) and JSON schemas under `schema/`.
- `dbtl docs` pages can carry warehouse and run metadata:
  `--catalog`/`--run-results` (or `docs.catalog`/`docs.run_results` in
  `.dbtl.yml`) opt each page into warehouse type/rows/size bullets and
  last-run/`unique_key`/`loader`/freshness lines.

## [0.5.3] - 2026-11-11

### Added

- Mouse support throughout: hit-target routing, click/drag/double-click
  classification, drag-to-pan the lineage, pane-border resizing, minimap
  scrubbing, hover accent and tooltips, right-click node menus, and clicks
  that confirm on release.
- A persistent tab strip for model roots (`O` opens, `{`/`}` cycle, digits
  jump, `X`/`Ctrl-w` closes) and a fixed `⌂` home tab with project health
  and next actions.
- An always-on inspector pane beside the lineage (`I` cycles auto/on/off).
- Stacked toasts and a bounded notification center (`Ctrl-n`, `!N` unread
  chip).
- Responsive layouts: a too-small screen under 50×12, a list drawer under
  80 columns, resizable pane splits with persisted widths.
- List upgrades: width-tiered columns, group folding, sticky headers.
- Search history, palette recents and `@model` search, help filtering,
  clickable status line with contextual hints, modal dimming.

## [0.5.2] - 2026-11-10

### Added

- Static project-structure lint rules, evaluated in `dbtl check`, the TUI
  Lint lens, and generated docs.
- Column-level lineage v1: a real SQL parser resolves column references
  through FROM/JOIN scope, with a per-model name-match fallback.
- Column focus in the TUI: traced-column lineage, a ColumnImpact lens, and a
  column modal; `Enter` structure view shows one-hop column counts.
- Column-level breaking-change analysis for `--diff`.
- Sibling-group collapse: oversized same-parent columns fold into `+N more`
  markers.
- Column-test badges, `relationships` targets, and the PrimaryKey lens.

## [0.5.1] - 2026-11-10

### Breaking changes

- **Package scope defaults to the root project.** Installed-package nodes no
  longer appear unless `--include-packages` is passed; `--project-only`
  became the deprecated alias for the new default.
- **`--project-only` is deprecated.** It still parses for compatibility but
  is a no-op — the default scope already is the root project — prints a
  deprecation warning, and conflicts with `--include-packages`. Drop the
  flag; add `--include-packages` only where the wider scope was wanted.

### Added

- Column-level lineage v0 (name matching): `dbtl columns <node>.<col>` and
  per-column counts in the structure view.
- Compiled-SQL ingestion (`compiled_code`, `target/compiled/`) with a
  raw/compiled toggle in the `s` preview and `[stale]` staleness flagging.
- `meta:` dict import, `meta:` selectors, `group_by_meta` list grouping, and
  "affected owners" in the impact report.
- `{{ doc() }}` expansion in source-mode descriptions via `{% docs %}`
  blocks.

### Fixed

- The project name now comes from manifest `metadata.project_name` instead
  of a uid-segment heuristic.
- dbt Fusion `check` nodes are excluded and unknown `resource_type`s warn
  instead of loading silently.

## [0.5.0] - 2026-11-09

### Added

- `dbtl impact` and `dbtl check` subcommands: the PR impact pack and CI
  gates, sharing the `D` modal's payload.
- `run_results.json` ingestion: RunStatus/RunTime lenses, a status chip, and
  a runtime-weighted critical path.
- dbt selector syntax for `/` search (`!tag:x`, `!+name`, `!meta:k=v`,
  `!state:modified`) and `--select`.
- Semantic-layer resources (semantic models, metrics, saved queries), dbt
  unit tests, and governance fields (access, group, contract, version).
- Two-node path anchoring (`p`), full-text search, and a `y`-prefixed
  yank menu.
- Per-project UI state persistence and `config.yml` defaults.
- `dbtl docs --clean` and `--check` for generated-docs housekeeping.
- Shared-macro change detection in manifest-vs-manifest diffs.
- Criterion pipeline benches and a deterministic synthetic-project
  generator.

### Changed

- The crate split into a four-package workspace (`dbtl-core`, `dbtl-tui`,
  `dbtl-docs`, `xtask`); the render loop now redraws on dirty flags instead
  of an idle timer.

## [0.4.1] - 2026-08-18

Maintenance release — dependency updates only.

## [0.4.0] - 2026-07-03

### Added

- Whole-graph overview on the `w` toggle.

### Fixed

- dbt parsing fidelity: load warnings now surface, versioned `ref`s resolve,
  YAML snapshots parse, and name collisions no longer drop nodes.

## [0.3.2] - 2026-06-16

### Added

- PR Impact Pack: a reviewer-shaped report composing `--diff` results,
  impact counts, and selectors.
- `-v` is accepted as a `--version` alias.

## [0.3.1] - 2026-06-16

### Fixed

- `dbtl docs` per-node lineage figures now match the `m` yank exactly.

## [0.3.0] - 2026-06-16

### Added

- `dbtl docs`: static, committable Markdown documentation — a page per node
  with a Mermaid lineage figure, plus an index README.

## [0.2.3] - 2026-06-15

### Added

- `--diff` baseline comparison: Diff lens, `D` summary modal, and a status
  chip.
- dbt exposures render as downstream impact endpoints.
- Selector Composer: `!` yanks the lineage view as a runnable
  `dbt build --select` command.
- Horizontal mouse-wheel scrolling for the lineage pane.

## [0.2.2] - 2026-06-12

### Added

- Color themes: truecolor presets alongside the default palette, custom
  YAML themes, `--theme` and the `Ctrl-t` picker.

### Fixed

- Stack overflow on cyclic manifests, stale re-root history on reload,
  losing the TUI when the editor fails, DOT export escaping, and
  manifest/`dbt_project.yml` paths escaping the project root.

## [0.2.1] - 2026-06-12

### Fixed

- Sanitized node names in export paths, recursion on symlinked directory
  loops, multi-word `$VISUAL`/`$EDITOR` launching, clipboard fallback for
  yank, and Mermaid node-id collisions in export.

## [0.2.0] - 2026-06-12

### Added

- Syntax-highlighted SQL preview (strings, comments, Jinja).
- Path-edge highlight, compact density (`v`), and layer bands.

## [0.1.3] - 2026-06-12

### Changed

- Internal refactor: the library facade split into focused modules and the
  app into per-responsibility modules with per-frame memoization; render
  tests now run through the production styled-layout pipeline.

## [0.1.2] - 2026-06-11

### Added

- `P` runs `dbt parse` inside the app and adopts the compiled manifest.
- Two-click release pipeline: a bump PR, then an automatic release on merge.
- Homebrew and prebuilt-binary install documentation.

## [0.1.1] - 2026-06-11

### Added

- Release pipeline and a `--version` flag.
- CI and security workflows with Dependabot and supply-chain guards.

## [0.1.0] - 2026-06-11

### Added

- Initial release: a terminal UI for dbt model lineage, from a compiled
  `target/manifest.json` or straight from project source.

[Unreleased]: https://github.com/hytfjwr/dbtl/compare/v0.6.3...HEAD
[0.6.3]: https://github.com/hytfjwr/dbtl/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/hytfjwr/dbtl/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/hytfjwr/dbtl/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/hytfjwr/dbtl/compare/v0.5.3...v0.6.0
[0.5.3]: https://github.com/hytfjwr/dbtl/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/hytfjwr/dbtl/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/hytfjwr/dbtl/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/hytfjwr/dbtl/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/hytfjwr/dbtl/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/hytfjwr/dbtl/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/hytfjwr/dbtl/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/hytfjwr/dbtl/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/hytfjwr/dbtl/compare/v0.2.3...v0.3.0
[0.2.3]: https://github.com/hytfjwr/dbtl/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/hytfjwr/dbtl/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/hytfjwr/dbtl/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/hytfjwr/dbtl/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/hytfjwr/dbtl/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/hytfjwr/dbtl/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/hytfjwr/dbtl/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hytfjwr/dbtl/releases/tag/v0.1.0
