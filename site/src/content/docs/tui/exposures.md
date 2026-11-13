---
title: Exposures
description: See dbt exposures, semantic models, metrics, and saved queries in the lineage, and how they show up in impact reports, tests, and structure views.
---

Exposures such as dashboards and notebooks appear as end nodes at the right edge of the lineage. They show in both manifest mode and source mode, so you can see what a model change reaches.

## Semantic layer nodes

A compiled manifest adds semantic models, metrics, and saved queries to the lineage as well.

## Impact reports

The PR impact report lists each affected exposure with its type and owner. Owners come from a `meta` key, `owner` by default. See [Diff](diff.md) for the report and for how to change the key.

`dbtl check` can warn about affected exposures when you run it with `--diff`. The gate is named `affected_exposures`. See [Check and impact](../cli/check-and-impact.md).

## Select exposures

The `exposure:` selector atom picks exposures. See [selector syntax](../cli/selectors.md).

## Test counts and governance fields

dbt unit tests count toward the `tests:N` badge on a node.

The structure view (`Enter`) shows governance fields when the model declares them: access, group, contract, and version. It also shows the `meta` dict, sorted, with nested values as compact JSON. See [Structure view](structure-view.md).

## Generated docs

`dbtl docs` writes pages for exposures, semantic models, metrics, and saved queries in their own directories. See [dbtl docs](../cli/docs.md).
