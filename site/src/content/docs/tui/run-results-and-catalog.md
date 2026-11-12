---
title: Run results and catalog
description: Load run_results.json and catalog.json to see last-run status, runtime heat, warehouse column types, row counts, and schema drift in the TUI.
---

`dbtl` can read two more dbt artifacts: `run_results.json` for the last run, and `catalog.json` for the warehouse. Both add data to the TUI, and neither is required.

## Auto-detection

`dbtl` looks for both files next to the data source. In manifest mode that is the manifest's directory, usually `target/`. In source mode it is `<project>/target/`.

A missing file means less data, never an error. To point at a different file, pass `--run-results <path>` or `--catalog <path>`.

```sh
dbtl --run-results /path/to/run_results.json
dbtl --catalog /path/to/catalog.json
```

## Run results

`run_results.json` records the outcome of the last `dbt run`, `dbt build`, or `dbt test`. With it loaded, `dbtl` adds:

- **Two lenses**, after the diff lens in the `t` cycle:
  - **Run status** tints nodes by outcome. Error and fail show red, warn shows yellow, and skipped shows muted. Successful nodes keep their normal color.
  - **Runtime** shows execution-time heat on the same low, mid, high ramp as the blast-radius lens.
- **A status chip**, such as `run 12/34 @2024-05-01`. It shows how many of the graph's nodes the artifact covers, plus its date, so a stale or partial run is visible.
- **A critical path** in the `S` stats dashboard, weighted by runtime.

The inspector and the home tab show the last run as well. See [Lenses](lenses.md) for the lens cycle and [Interface](interface.md) for the inspector and home tab.

### Refresh after a new run

`r` reloads the data source and re-reads `run_results.json`. With `--watch` in manifest mode, a fresh `dbt run` is picked up in place, because the run rewrites the manifest.

## Catalog

`catalog.json` is the warehouse record that `dbt docs generate` writes. With it loaded, the structure view (`Enter`) shows what the warehouse actually has:

- real column types
- row counts
- byte size
- a `drift:` line when the documented columns and the warehouse disagree, split into documented-but-missing and undocumented columns

The catalog also feeds column-level lineage as a column source, so `dbtl columns` and the column diff in [Diff](diff.md#column-level-breaking-changes) read it first.

Two lint rules compare the project against the catalog: `warehouse_type_drift` and `columns_missing_in_warehouse`. A rule whose input is missing is skipped. See the [lint rules reference](../reference/lint-rules.md).

## Generated docs

`dbtl docs` never reads these artifacts on its own. They are opt-in there. See [dbtl docs](../cli/docs.md).
