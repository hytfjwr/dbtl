{{ config(materialized='table') }}

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
