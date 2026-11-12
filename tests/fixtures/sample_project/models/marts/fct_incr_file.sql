-- Every governance/config field via the highest rung (in-file config()) —
-- including the dict-literal `contract` and `incremental_strategy`. An
-- incremental + enforced-contract model needs `on_schema_change` (dbt).
{{ config(
    materialized='incremental',
    unique_key='id',
    incremental_strategy='delete+insert',
    contract={'enforced': True},
    on_schema_change='append_new_columns',
) }}

select 1 as id
