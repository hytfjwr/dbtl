# Lint (`dbtl lint`)

`dbtl lint` runs a set of static best-practice rules over the project graph —
the style of checks `dbt_project_evaluator` materializes in a warehouse,
evaluated locally against the same `Dag` everything else reads. No warehouse
connection, no `dbt run`; in source mode not even `dbt parse`. The full rule
set with severities, rationales, and parameters lives in the generated
[reference](reference/lint-rules.md).

```console
$ dbtl lint                                   # auto-detect the data source
$ dbtl lint --manifest target/manifest.json
$ dbtl lint --select 'path:marts+'            # restrict findings to a selector
$ dbtl lint --rules root_models,model_fanout  # a named subset
$ dbtl lint --fail-on warn                    # fail on warnings too
```

Text output is one `PASS`/`WARN`/`FAIL`/`SKIP` line per rule, the individual
findings indented under tripped rules as `file:line rule message`, and a
closing tally (`lint: N error(s), M warning(s), K baselined`).

`dbtl check --lint` folds the same evaluation into the check gate set — the
two can never disagree, and `check.lint: true` in `.dbtl.yml` makes it the
default for `check` runs.

## Severities and `fail_on`

Every finding carries the severity its rule resolves to: the original twelve
structure rules are `error`, the later graph/metadata and SQL/catalog rules
ship at `warn`. What fails the run is the threshold `fail_on`:

| `fail_on` | Result |
| --------- | ------ |
| `error` (default) | any error finding fails (exit 1); warnings report only |
| `warn` | warnings fail too |
| `never` | nothing fails — report only |

Precedence: `--fail-on` > `.dbtl.yml`'s top-level `fail_on` > `error`.

## Rule inputs and SKIP

Each rule declares what it needs: the graph alone, parsed model SQL, or the
warehouse `catalog.json` record. A rule whose input is absent reports
`SKIP lint <rule>: requires …` — informational, never a failure. SQL is
parsed lazily per model (memoized across rules), `catalog.json` auto-detects
next to the data source (or `--catalog <path>`), and `--sql-dialect` pins the
parser dialect. The TUI runs graph+catalog rules only — parsing every model
up front would stall startup.

## Listing and explaining rules

`--list-rules` and `--explain <ID>` are query modes: they read `.dbtl.yml`
for severity/parameter overrides but never load a data source.

```console
$ dbtl lint --list-rules            # id, category, effective severity, summary
$ dbtl lint --explain model_fanout  # rationale, example, declared params
```

## Freezing existing violations: the baseline file

Adopting lint on a project that already violates rules shouldn't force a
flag day. Two mechanisms cooperate:

- **The baseline file** freezes the current set. `dbtl lint --write-baseline`
  writes every current finding to the file `lint.baseline` names in
  `.dbtl.yml`, else `.dbtl-baseline.json` at the project root —
  `--write-baseline <PATH>` picks the path explicitly. The write always
  exits 0. Commit the file: baselined findings still report but never
  fail, and the tally counts them separately. `--no-baseline` ignores the
  file for a run; `--write-baseline` can't combine with `--diff` or
  `--select` — a partial set must never overwrite the frozen one. A
  missing explicitly-configured path warns; an unreadable or malformed
  baseline is an input error (exit 3) — a frozen set that can't be read
  must never silently un-freeze.
- **`--diff`** narrows the gate to findings the baseline doesn't already
  carry — comparing finding keys, the same "new only" idea the layer-
  violation gate uses. It composes with the baseline file: keep `main` green
  on a committed `.dbtl-baseline.json`, and pull requests fail only on the
  violations they add.

## `.dbtl.yml`

Lint policy lives in the project-local `.dbtl.yml` — deliberately not the
user-level `~/.config/dbtl/config.yml`, so a CI gate can't vary with the
developer's machine. Discovery follows the data source: the project
directory in source mode; in manifest mode the project root first, then
`target/`. Unknown keys inside `lint:` (or inside a rule entry) are errors;
unknown top-level keys only warn.

```yaml
# .dbtl.yml — every key optional; absent rules stay enabled at their default
fail_on: warn                # error | warn | never — see above

lint:
  baseline: .dbtl-baseline.json   # the freeze file (default shown)
  rules:
    root_models: false            # bool: off, or on at default severity
    model_fanout:
      severity: warn              # error | warn | off
      threshold: 5                # declared params only — unknown keys error
      exclude:                    # dbt selector specs: skip findings on matches
        - "stg_legacy_*"
        - "path:models/legacy"
        - "tag:wip"
  model_fanout_threshold: 5       # compat flat keys — an error if combined
  source_fanout_threshold: 2      #   with the nested param form
  naming_prefixes:                # per-layer merge over the defaults
    staging: [stg_, base_]
    marts: []
    other: [rpt_]
```

A single node can also suppress rules through its `meta` (dbt's `+meta`
inheritance from `dbt_project.yml` applies as usual):

```yaml
models:
  - name: stg_legacy_orders
    meta:
      dbtl:
        ignore: [root_models, naming_conventions]   # or the string "all"
```

## Exit codes

`0` pass, `1` findings at or above `fail_on`, `2` usage errors (unknown rule
names, conflicting flags, malformed `.dbtl.yml`), `3` input errors (missing
data source, unreadable baseline). See the full table in
[ci.md](ci.md#exit-codes).
