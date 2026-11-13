# Test fixtures

One line each — generated manifests are frozen output, never edit by hand:

- `manifest.json` — the main hand-written `jaffle_finance` manifest every
  suite loads (older schema shape, no `metadata` block).
- `sample_manifest.json` — real-`dbt compile` output over `sample_project/`
  (see *Regeneration* below); the manifest↔source consistency fixture.
- `logistics_manifest.json` — real-`dbt compile` output over
  `logistics_project/` — var-controlled nodes, seed-only raw data, hooks,
  analyses, and a vendored local package.
- `versioned_manifest.json` — real-`dbt compile` output over
  `versioned_project/` — a `versions:`-ed `fct_orders` (numeric `v: 1`,
  `v: 2` and string `v: beta`, which encode `version` as a JSON number and
  a JSON string on the wire) plus `ref()`/`v=` consumers and a
  `versions: include:`-scoped unit test. Manifest-only: source mode does
  not synthesize versions, so it stays out of `consistency.rs`.
- `fusion_manifest.json` — hand-built `fusion_shop` manifest exercising dbt
  Fusion resource types (`check`, `operation`).
- `packages_manifest.json` — hand-built `acme_analytics` manifest with
  installed-package nodes for the `--include-packages` scope.
- `invalid.json` — not JSON at all; the load-error path fixture.
- `sample_project/` — the synthetic project `sample_manifest.json` is built
  from (staging → marts, seeds, snapshots, exposures, disabled nodes).
- `logistics_project/` — the project `logistics_manifest.json` is built
  from; its `packages/shipkit/` subdir is a `local:` package dependency.
- `versioned_project/` — the project `versioned_manifest.json` is built
  from (a `versions:` schema.yml and one `.sql` per version file).
- `packages_project/` — minimal source-mode project paired with
  `packages_manifest.json` for package-scope tests.
- `collision_project/` — source-mode fixture where two files share one
  model stem (the duplicate-name warning path).
- `compiled_project/` — source-mode fixture carrying a `target/compiled/`
  tree (stale-compiled-SQL detection).
- `yaml_snapshot_project/` — source-mode fixture whose snapshots are
  declared in YAML, not `.sql` files.

## Regeneration

`sample_manifest.json`, `logistics_manifest.json`, and
`versioned_manifest.json` are output of a real dbt run — regenerate them
after changing anything in their project dirs:

```sh
cargo xtask regen-sample-manifest                      # sample (shortcut)
cargo xtask gen-fixture-manifest \
    --project tests/fixtures/logistics_project         # logistics
cargo xtask gen-fixture-manifest \
    --project tests/fixtures/versioned_project         # versioned
```

Both land at `tests/fixtures/<name>_manifest.json` by default; pass `--out`
to redirect and `--dbt-version X.Y.Z` to pin a release (the committed
fixtures use the same dbt version). The task needs `uv` on PATH (it invokes
`uvx --with dbt-duckdb --from dbt-core dbt …`), writes a throwaway duckdb
profile, runs `dbt deps` (local packages only) then `dbt compile`, pins the
per-run `metadata` fields and every `created_at` stamp, sorts all object
keys, and removes `target/`, `logs/`, `dbt_packages/`, `.user.yml`, and
`package-lock.yml` afterwards — success or failure — so the same project
and dbt version regenerate byte-identically.

Use `dbt compile` (not `dbt parse`) so `compiled_code` lands in the
manifest — column-level lineage resolution prefers it, and it is what real
warehouses see (relations render as `"db"."schema"."relation"`).

`manifest.json`, `fusion_manifest.json`, `packages_manifest.json`, and the
other `*_project` dirs are hand-written and have no regen recipe.

## Licenses

The generated manifests (`sample_manifest.json`,
`logistics_manifest.json`, `versioned_manifest.json`) embed macro source text from
[dbt-core](https://github.com/dbt-labs/dbt-core) and
[dbt-duckdb](https://github.com/duckdb/dbt-duckdb), both distributed under
the Apache License 2.0. The license text is stored in
[`LICENSE-APACHE-2.0`](./LICENSE-APACHE-2.0) in this directory. Everything
else in `tests/fixtures/` is original to this repository.
