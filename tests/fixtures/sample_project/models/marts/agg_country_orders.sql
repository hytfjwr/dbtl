-- No in-file config() and no schema.yml entry: the materialization must come
-- from the dbt_project.yml models tree (marts -> +materialized: table).
select
    country,
    count(*) as orders
from {{ ref('fct_orders') }}
group by 1
