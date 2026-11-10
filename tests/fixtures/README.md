# Fixture regeneration

`sample_manifest.json` is frozen output of a real dbt run over
`sample_project` — never edit it by hand. Regenerate it after changing any
`sample_project` file:

```sh
mkdir -p /tmp/dbtl-profiles
printf 'sample:\n  target: dev\n  outputs:\n    dev:\n      type: duckdb\n      path: /tmp/dbtl_sample.duckdb\n' > /tmp/dbtl-profiles/profiles.yml
cd tests/fixtures/sample_project
uvx --with dbt-duckdb --from dbt-core dbt compile --project-dir . --profiles-dir /tmp/dbtl-profiles
jq -c '.metadata.user_id = null' target/manifest.json > ../sample_manifest.json
rm -rf target logs .user.yml
rm -f /tmp/dbtl_sample.duckdb
```

Use `dbt compile` (not `dbt parse`) so `compiled_code` lands in the manifest
— column-level lineage resolution prefers it, and it is what real warehouses
see (relations render as `"db"."schema"."relation"`).

`manifest.json` (the jaffle-shop fixture) and the other `*_project` dirs are
hand-written and have no regen recipe.
