{{ config(materialized='ephemeral') }}

-- The `prod` arm refs stg_prod_orders — under `dev` real dbt records only
-- the `stg_orders` arm in depends_on.nodes, so stg_prod_orders has no
-- recorded consumer in manifest mode (it is one in source mode).
{% if target.name == 'prod' %}
select order_id from {{ ref('stg_prod_orders') }}
{% else %}
select order_id from {{ ref('stg_orders') }}
{% endif %}
