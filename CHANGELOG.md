# Changelog

All notable changes to `dbtl` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Breaking changes

- **Exit codes are now classified.** Gate failures still exit `1`, but usage
  and configuration errors now exit `2` and input errors (missing manifest,
  unreadable baseline, unresolvable git ref) exit `3`. Scripts that treated
  every non-zero code alike need no change; scripts that matched `1` for
  "anything went wrong" should match on non-zero instead, or use the new
  codes to distinguish a failed gate from a bad invocation. See
  [the exit-code table](https://hytfjwr.github.io/dbtl/configuration/exit-codes/).
- **`--project-only` is deprecated.** It still parses for compatibility but
  is a no-op — the default scope already is the root project — prints a
  deprecation warning, and conflicts with `--include-packages`. Drop the
  flag; add `--include-packages` only where the wider scope was wanted.
- **The generated docs layout changed.** `dbtl docs` now writes node pages
  into per-resource-type directories (`models/`, `sources/`, `exposures/`,
  …) and carries new sections (SQL, column lineage cells, lint violations,
  owner/tag/group index pages). A committed docs tree must be regenerated
  once — `dbtl docs --out <DIR> --clean` removes the stale pages safely.
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
- `dbtl ls`: evaluate dbt selectors non-interactively (`names`/`uids`/
  `selector`/`json` output). `--select`/`--exclude` now also scope `check`,
  `lint`, `impact`, and `docs`.
- `--diff git:<rev>`: compare against the project as committed on any
  revision, materialized read-only via `git ls-tree`/`git cat-file` — no
  checkout needed.
- `dbtl check`/`lint` report formats: one `dbtl.report/v1` document rendered
  as `text`, `markdown`, `json`, `github` annotations, `sarif`, `junit`, or
  `codeclimate`; `--report <FMT>:<PATH>` writes files, `--step-summary`
  appends to `GITHUB_STEP_SUMMARY`, `--path-prefix` rewrites paths for
  repository subdirectories.
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
- Releases now attach `demo.gif` — the recorded TUI demo, rendered at the
  release tag by the release pipeline (not covered by `checksums.txt` or
  attestations).

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
  name or a selector spec — the spec's first match opens in a tab and
  must be a model — and notes that `state:` atoms cannot match (the flag
  resolves before `--diff` loads).

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

[Unreleased]: https://github.com/hytfjwr/dbtl/compare/v0.5.3...HEAD
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
