-- Governance entirely from the `dbt_project.yml` tree leaf (the lowest
-- rung): `+access: private`, `+group: finance`, and
-- `+incremental_strategy: insert_overwrite` all land here.
{{ config(materialized='incremental', unique_key='id') }}

select 1 as id
