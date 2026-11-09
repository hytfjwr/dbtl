-- `meta` merges shallowly over schema.yml's config.meta and the project tree
-- per top-level key: `owner` below beats the schema value, `deep` exercises a
-- nested dict (displayed as compact JSON).
{{ config(materialized='table', meta={'owner': 'alice', 'deep': {'x': 1}}) }}

-- Trap for the in-file config extractors: `enabled=false` / `materialized='…'`
-- OUTSIDE a config() call (here: plain comment text) must configure NOTHING —
-- dbt only honours them inside config(). enabled=false materialized='ephemeral'
select
    o.*,
    c.country
from {{ ref('stg_orders') }} as o
left join {{ ref('country_codes') }} as c
    on o.country_code = c.code
where coalesce(o.disabled_flag, false) = false
