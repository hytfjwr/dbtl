-- The `if` arm gated on var('branch_flag_off') is provably false
-- (branch_flag_off: false in dbt_project.yml vars) — its `enabled=false`
-- never applies and its `ref` earns no edge, matching dbt's own static
-- extractor. The else arm's config applies in both; the select is
-- unconditional.
{% if var('branch_flag_off') %}
{{ config(enabled=false) }}
-- {{ ref('dim_customers') }}
{% else %}
{{ config(materialized='table') }}
{% endif %}
select * from {{ ref('stg_orders') }}
