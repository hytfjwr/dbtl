# `dbtl` in CI

Everything `dbtl` checks runs without a TTY — `check`, `lint`, `impact`,
`docs`, `ls`, and `columns` all return before the terminal initializes. This
page covers wiring them into pipelines: the GitHub Action, a hand-rolled
workflow, report formats, baselines, and the pre-commit hook.

## The GitHub Action

The repository ships a composite action at its root, so one `uses:` step
installs a release binary and runs `dbtl check` with GitHub annotations, a
markdown report, a step summary, and — on pull requests — a diff against the
base commit:

```yaml
name: dbtl

on:
  pull_request:

permissions: {}

jobs:
  check:
    name: dbtl check
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: read
    steps:
      - name: Checkout
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          fetch-depth: 0

      - name: dbtl check
        uses: hytfjwr/dbtl@<commit-sha> # v0.6.0
        with:
          version: v0.6.0
          project-dir: .
```

Pin the action to a commit SHA (`# v0.6.0` keeps the tag readable) and set
`version:` to the release tag — a SHA ref carries no version for the
installer to resolve. `fetch-depth: 0` makes the base commit resolvable for
`--diff git:`; the action also fetches a missing base sha itself at depth 1.

### Inputs

| Input | Default | Meaning |
| ----- | ------- | ------- |
| `version` | `''` | Release tag to install (`vX.Y.Z`). Empty resolves to the tag the action was invoked at; `local` uses the `dbtl` already on `PATH`. |
| `project-dir` | `.` | dbt project directory, relative to the repository root. |
| `base-ref` | `''` | Revision `--diff` compares against. Empty: the PR's base sha, or a push's `before`; a missing commit is fetched at depth 1. |
| `lint` | `true` | Also run the lint gate set. |
| `fail-on` | `''` | `error` / `warn` / `never`; empty follows `.dbtl.yml`. |
| `args` | `''` | Extra arguments appended to `dbtl check`, split on whitespace. |
| `step-summary` | `true` | Append the markdown report to the job summary. |
| `sarif` | `false` | Upload a SARIF report to code scanning — needs `security-events: write`. |
| `comment` | `false` | Post/update a sticky `<!-- dbtl:report -->` PR comment — needs `pull-requests: write`. |
| `github-token` | `${{ github.token }}` | Token for the PR comment (SARIF always uses the job's `GITHUB_TOKEN`). |
| `dry-run` | `false` | Print the SARIF upload and PR comment calls instead of making them — for action development. |

### Outputs

`exit-code` (the `dbtl check` verdict — the last action step re-raises it),
`report` (markdown report path), `sarif` (SARIF file path). dbtl's exit code
travels through an output so the comment and SARIF steps run even on gate
failures.

### SARIF and PR comments

```yaml
    permissions:
      contents: read
      security-events: write   # sarif upload
      pull-requests: write     # sticky comment
    steps:
      # … checkout …
      - uses: hytfjwr/dbtl@<commit-sha> # v0.6.0
        with:
          version: v0.6.0
          sarif: 'true'
          comment: 'true'
```

The comment is sticky: the first `<!-- dbtl:report -->` comment is updated
instead of stacking new ones. On **forked pull requests** the token is
read-only, so write-back steps are skipped with a warning — the check itself
still runs and gates.

## Without the action

The same pipeline by hand — install a release binary (or `cargo install`),
then run `dbtl check` directly:

```yaml
jobs:
  check:
    name: dbtl check
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: read
    steps:
      - name: Checkout
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          fetch-depth: 0

      - name: Install dbtl
        env:
          TAG: v0.6.0
        run: |
          curl -fsSLO "https://github.com/hytfjwr/dbtl/releases/download/${TAG}/checksums.txt"
          curl -fsSLO "https://github.com/hytfjwr/dbtl/releases/download/${TAG}/dbtl-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
          sha256sum --check --ignore-missing checksums.txt
          tar -xzf "dbtl-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
          echo "$PWD/dbtl-${TAG}-x86_64-unknown-linux-gnu" >> "$GITHUB_PATH"

      - name: dbtl check
        env:
          BASE: ${{ github.event.pull_request.base.sha }}
        run: |
          dbtl check --diff "git:${BASE}" --lint --format github \
            --report "markdown:${RUNNER_TEMP}/dbtl-report.md" \
            --report "sarif:${RUNNER_TEMP}/dbtl.sarif" \
            --step-summary
```

`--format github` emits `::error`/`::warning` annotations per finding
(severity order — GitHub caps how many a step shows), `--report` writes any
format to files, and `--step-summary` appends the markdown report to
`GITHUB_STEP_SUMMARY`. `--path-prefix` rewrites paths in `github`/`sarif`/
`codeclimate` output for a project in a repository subdirectory.

## Report formats

`check` and `lint` build one `dbtl.report/v1` document and render it seven
ways — `--format <fmt>` to stdout (default `text`), `--report <fmt>:<path>`
to files (repeatable):

| Format | Use |
| ------ | --- |
| `text` | The line report: one `PASS`/`WARN`/`FAIL`/`SKIP` row per gate, findings indented. |
| `markdown` | A `<!-- dbtl:report -->`-marked document — PR comments and step summaries. |
| `json` | The `dbtl.report/v1` payload verbatim, for `jq` pipelines. |
| `github` | Actions workflow annotations. |
| `sarif` | SARIF 2.1.0 for code scanning. |
| `junit` | Test-report XML — one testcase per gate. |
| `codeclimate` | GitLab Code Quality issues. |

`--quiet` suppresses stdout only — files still write, the exit code still
gates. `impact` prints Markdown or `--format json`, and takes
`--report markdown:<path>`/`json:<path>`.

## The gate set

`dbtl check` reports every configured gate; a flag naming a gate's parameter
enables an `off` gate at `error`. The full table — defaults, parameters, and
which gates need `--diff` — is generated in
[reference/config.md](reference/config.md#checkgatesgate-id); the short
version:

- Always available: `coverage`, `column_coverage`, `doc_coverage`
  (percent thresholds), `docs_drift` (docs freshness, needs a `dir`).
- With `--diff`: `new_layer_violations`, `untested_changes`,
  `breaking_columns` (opt in with `--no-breaking` or a severity),
  `changed_hubs`, `affected_exposures`, `blast_radius`.
- With `--lint` or `check.lint: true`: one gate per lint rule.

An enabled diff gate without `--diff` prints `SKIP … needs --diff` — never
a silent gap. `.dbtl.yml`'s `check:` section configures severities
(`error`/`warn`/`off`) and parameters; CLI flags override it:

```yaml
check:
  lint: true
  gates:
    coverage: {severity: error, min: 80}
    changed_hubs: {min_downstream: 10}
    docs_drift: {dir: docs/lineage}
```

## Git diffs and baselines

`--diff git:<rev>` compares the working tree against the project as
committed on any revision `git rev-parse` resolves — a branch (`git:main`),
a tag, a sha, `git:HEAD~3`. Files materialize read-only through
`git ls-tree`/`git cat-file` into a scratch directory; no checkout, no index
writes. Because the baseline parses from source, the current side parses
from source too — a compiler-vs-parser comparison would read loader
differences as changes.

On CI, give the checkout the history the rev needs: `fetch-depth: 0`, or
`git fetch --depth=1 origin <sha>` for a single base commit.

For lint on a legacy project, freeze today's violations instead:

```console
$ dbtl lint --write-baseline          # writes .dbtl-baseline.json — commit it
$ dbtl check --lint --diff git:origin/main   # PRs then fail only on NEW findings
```

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success — every gate passed, output written. |
| 1 | Gate failure — a check failed, findings at/above `fail_on`, `docs --check` drift. |
| 2 | Usage error — bad flags, unknown rule/gate names, malformed `.dbtl.yml`, an unresolvable selector. |
| 3 | Input error — missing/unreadable manifest, project, baseline, or git ref. |
| 101 | Panic (Rust's default). |

## GitLab and other CI

Nothing in `dbtl` is GitHub-specific. For GitLab Code Quality, collect a
`codeclimate` report as an artifact; for test-report UIs, `junit`:

```yaml
dbtl check:
  script:
    - dbtl check --lint --report "codeclimate:dbtl-codeclimate.json" --report "junit:dbtl-junit.xml"
  artifacts:
    when: always
    reports:
      codequality: dbtl-codeclimate.json
      junit: dbtl-junit.xml
```

## Docs drift

`dbtl docs --check` compares a committed doc tree against what the generator
would write now and exits 1 on any difference — see
[docs-generator.md](docs-generator.md#housekeeping---check-and---clean). The
`docs_drift` gate folds the same comparison into `dbtl check` via
`--docs-dir <DIR>` or `check.gates.docs_drift.dir`.

## pre-commit

`.pre-commit-hooks.yaml` ships two hooks — `dbtl-lint` (builds dbtl from this
repository) and `dbtl-lint-system` (uses the `dbtl` on `PATH`):

```yaml
- repo: https://github.com/hytfjwr/dbtl
  rev: v0.6.0
  hooks:
    - id: dbtl-lint-system
```

Both run once per commit (`pass_filenames: false` — lint reads the whole
project) and only when dbt project files changed.
