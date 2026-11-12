---
title: Check and impact
description: Gate pull requests with dbtl check, report their blast radius with dbtl impact, and render the verdict in seven formats.
---

`dbtl impact` and `dbtl check` are the two non-interactive subcommands for pull request review. Neither needs a TTY. Both compute the same diff payload that the TUI's `D` diff modal shows, so a CI comment cannot disagree with the screen.

## dbtl impact

`dbtl impact --diff <BASELINE>` prints the PR impact pack as Markdown, or as JSON with `--format json`. The pack contains:

- diff listings: added, removed, and modified nodes, and edge changes
- a **Column changes** section that shows which downstream columns, models, marts, and exposures each changed column reaches
- the blast radius
- affected hubs, exposures, and owners
- `dbt build --select` suggestions
- risk flags

`--select <SELECTOR>` narrows the report to matching changed nodes. `--report markdown:<PATH>` and `--report json:<PATH>` write files.

```sh
dbtl impact --diff /path/to/main-checkout                       # PR impact pack as Markdown
dbtl impact --diff target/manifest.prod.json --format json      # same payload as JSON
dbtl impact --diff git:main --select 'tag:marts+'               # only matching changed nodes
```

## dbtl check

`dbtl check` prints one `PASS`, `WARN`, `FAIL`, or `SKIP` line per gate. It exits 1 when any gate fails at or above `--fail-on`. With `--lint`, the findings print first as annotated blocks — position, source excerpt, and fix links (see [dbtl lint](lint.md) for the block shape) — then the gate rows, with clean lint rules collapsed into one `PASS lint` row, and the lint tally at the end.

```text
scope: root project only
PASS coverage: 10/22 (45%)
WARN lint multiple_sources_joined: 1 finding
WARN lint root_models: 7 findings
WARN lint marts_depend_on_source: 1 finding
FAIL lint missing_primary_key_tests: 9 findings
FAIL lint public_models_without_contract: 1 finding
…
WARN lint select_star: 6 findings
SKIP lint warehouse_type_drift: requires graph + catalog input
SKIP lint columns_missing_in_warehouse: requires graph + catalog input
PASS lint: 15 rules clean

Found 10 errors and 36 warnings in 21 files
16 rules reported findings; 2 skipped (need --catalog)
```

```sh
dbtl check --coverage-min 80                                    # fail under 80% test coverage
dbtl check --diff /path/to/main-checkout --coverage-min 80      # + new violations and untested changes
dbtl check --diff git:main --no-breaking                        # + breaking column changes still read downstream
dbtl check --lint                                               # + static lint (one gate per rule)
dbtl check --lint --diff /path/to/main-checkout                 # lint gates on NEW violations only
dbtl check --rules root_models,undocumented_models              # a subset of rules (implies --lint)
dbtl check --lint --format json                                 # the whole report as JSON
dbtl check --column-coverage-min 50 --doc-coverage-min 80       # + column-test and description coverage
dbtl check --diff git:main --max-blast-radius 10                # + a ceiling on the blast radius
dbtl check --gate changed_hubs=error --gate blast_radius=warn   # per-gate severity overrides
```

Key flags:

| Flag | Effect |
| ---- | ------ |
| `--coverage-min`, `--column-coverage-min`, `--doc-coverage-min` | Percent thresholds. Setting one enables an `off` gate at `error` severity. |
| `--max-blast-radius <N>` | Ceiling on the affected models. Needs `--diff`. |
| `--no-breaking` | Fail on breaking column changes that downstream columns still reference. Needs `--diff`. |
| `--lint`, `--rules <A,B>` | Add one gate per lint rule. `--rules` selects a subset and implies `--lint`. |
| `--gate <ID>=error\|warn\|off` | Override one gate's severity. Repeatable. |
| `--fail-on error\|warn\|never` | The minimum severity that fails the run. With `never`, a tripped gate prints `WARN` and never counts. |
| `--select <SELECTOR>` | Scope every gate to matching nodes. `docs_drift` is never scoped, because it compares whole doc sets. |
| `--docs-dir <DIR>` | Gate on generated-docs freshness. |
| `--format`, `--color`, `--report`, `--step-summary`, `--path-prefix`, `--quiet` | Control the report. See [Report formats](#report-formats). |

`check` accepts `--select` with the full [selector grammar](selectors.md). `--diff` takes a manifest file, a project directory, or a git revision. See [Git diffs and baselines](#git-diffs-and-baselines).

`check --diff` also prints an informational `affected owners:` line. It is never a gate. The meta key it reads is `owner_meta_key`, which defaults to `owner`.

For every flag, see the [CLI reference](../reference/cli.md).

## The gate set

`dbtl check` reports every configured gate. A flag that names a gate's parameter enables an `off` gate at `error`. The [config reference](../reference/config.md#checkgatesgate-id) lists the defaults, parameters, and which gates need `--diff`. In short:

- **Always available:** `coverage`, `column_coverage`, and `doc_coverage` (percent thresholds), and `docs_drift` (docs freshness, needs a `dir`).
- **With `--diff`:** `new_layer_violations`, `untested_changes`, `breaking_columns` (opt in with `--no-breaking` or a severity), `changed_hubs`, `affected_exposures`, and `blast_radius`.
- **With `--lint` or `check.lint: true`:** one gate per lint rule. See [dbtl lint](lint.md).

An enabled diff gate without `--diff` prints `SKIP … needs --diff`. A gate that is asked for but cannot run is visible, never a silent gap.

The `check:` section of `.dbtl.yml` configures severities (`error`, `warn`, `off`) and parameters. CLI flags override it:

```yaml
check:
  lint: true
  gates:
    coverage: {severity: error, min: 80}
    changed_hubs: {min_downstream: 10}
    docs_drift: {dir: docs/lineage}
```

`docs_drift` runs the same comparison as `dbtl docs --check`. Enable it with `--docs-dir <DIR>` or `check.gates.docs_drift.dir`. See [dbtl docs](docs.md#housekeeping-check-and-clean).

### Breaking column changes

Every modified node gets a column diff through the same column-lineage engine as [`dbtl columns`](columns.md). The diff classifies each changed column as removed, renamed, type changed, expression changed, or added.

Severity depends on what the downstream graph proves:

- **Breaking:** a removed or renamed column that still has readers.
- **Partial-breaking:** a data type or expression change.
- **Non-breaking:** an added column, or a removal nobody references.

Only breaking changes trip `--no-breaking`. Partial-breaking changes show up in the impact pack but never trip the gate. For how the diff compares nodes, see [Diff in the TUI](../tui/diff.md).

## Report formats

`check` and `lint` build one `dbtl.report/v1` document and render it in eight formats. `--format <fmt>` prints one to stdout (default `text`). `--report <fmt>:<path>` writes one to a file and is repeatable.

| Format | Use |
| ------ | --- |
| `text` | The annotated report: each lint finding renders as an `error[rule]`/`warning[rule]` block with its position, the source excerpt underlined, and `= help:`/`= docs:` footers; `check` prints its `PASS`/`WARN`/`FAIL`/`SKIP` gate rows after the blocks. |
| `concise` | One finding per line as `file[:line[:column]]: severity[rule] message` — for editors and grep. |
| `markdown` | A document marked with `<!-- dbtl:report -->`, for PR comments and step summaries. |
| `json` | The `dbtl.report/v1` payload verbatim, for `jq` pipelines. |
| `github` | GitHub Actions workflow annotations. |
| `sarif` | SARIF 2.1.0 for code scanning. |
| `junit` | Test-report XML, one test case per gate. |
| `codeclimate` | GitLab Code Quality issues. |

- `--quiet` suppresses stdout only. Files still write and the exit code still gates.
- `--color auto|always|never` controls ANSI styling of the `text` and `concise` output on stdout. `auto` (the default) follows the terminal and the `NO_COLOR`/`CLICOLOR_FORCE` conventions; `always` colors even when piped. Machine formats never carry color.
- `--report` writes parent directories as needed and overwrites existing files. The file contents match the bytes that format prints on stdout — always without color.
- `--step-summary` appends the Markdown report to the file that `GITHUB_STEP_SUMMARY` names. It does nothing when the variable is unset or empty.
- `--path-prefix` prepends a prefix to file paths in the `github`, `sarif`, and `codeclimate` formats, for a project that lives in a repository subdirectory. It is a usage error when none of those formats is selected.
- `impact` prints Markdown or `--format json`, and takes `--report markdown:<path>` and `--report json:<path>`.

The `github` format emits `::error` and `::warning` annotations per finding in severity order. GitHub caps how many a step shows.

## Git diffs and baselines

`--diff git:<rev>` compares the working tree against the project as committed at any revision that `git rev-parse` resolves: a branch (`git:main`), a tag, a sha, or `git:HEAD~3`. `dbtl` reads the files through `git ls-tree` and `git cat-file` into a scratch directory. It makes no checkout and no index writes. Only source files are read: models, schema YAML, seeds, docs, and `dbt_project.yml`. `target/` and `dbt_packages/` stay out.

Because the baseline is parsed from source, the current side is parsed from source too, even when `target/manifest.json` exists. Comparing a compiler's output with a parser's output would read loader differences as changes. An explicit `--manifest` still wins, with a warning that the two loaders may disagree on the margins. Your uncommitted changes count, because the current side is your live working tree.

In CI, give the checkout the history the revision needs. Use `fetch-depth: 0`, or fetch a single base commit:

```sh
git fetch --depth=1 origin <sha>
```

`--diff` also accepts another manifest, a saved production manifest, or another project checkout:

```sh
dbtl check --diff target/manifest.prod.json
dbtl check --diff /path/to/main-checkout
```

To adopt lint on a legacy project, freeze today's violations instead of fixing them all at once. See [Freeze existing violations](lint.md#freeze-existing-violations).

## Run it in CI

The composite GitHub Action wires up `dbtl check` with annotations and a step summary. See [GitHub Action](../ci/github-action.md). For other pipelines, see [Other CI systems](../ci/other-ci.md). Exit codes are in [Exit codes](../configuration/exit-codes.md).
