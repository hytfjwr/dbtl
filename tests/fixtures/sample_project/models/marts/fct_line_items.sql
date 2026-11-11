{{ config(materialized='incremental') }}

-- unique_key comes from schema.yml `config:` (list form), which beats the
-- `+unique_key: tree_shadowed` leaf in dbt_project.yml.
select * from {{ ref('stg_orders') }}
