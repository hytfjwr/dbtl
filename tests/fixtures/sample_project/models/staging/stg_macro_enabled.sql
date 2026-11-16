-- The bare-macro spelling reads `macro_flag_on: true` → evaluates true →
-- this model stays enabled in both dbt and source mode.
{{ config(enabled=bool_var('macro_flag_on', false)) }}
select * from {{ ref('stg_orders') }}
