-- `sample.bool_var('branch_flag_off', true)` reads
-- branch_flag_off: false → the project-qualified macro call evaluates
-- false and dbt parks this model under `disabled`; the source loader
-- must drop it too, edges included — the same as a literal `enabled=false`.
{{ config(enabled=sample.bool_var('branch_flag_off', true)) }}
select * from {{ ref('stg_orders') }}
