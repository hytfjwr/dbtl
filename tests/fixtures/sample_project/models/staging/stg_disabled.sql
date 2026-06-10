{{ config(enabled=false) }}

-- A disabled model never reaches the manifest's `nodes` (dbt parks it under
-- `disabled`); the source loader must drop it too, edges included.
select * from {{ ref('stg_orders') }}
