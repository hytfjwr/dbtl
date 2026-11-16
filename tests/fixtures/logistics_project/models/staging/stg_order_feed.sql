{{ config(materialized='view') }}

-- The `prod` arm reads raw.prod_orders — under the fixture's `dev` target
-- real dbt records only the `orders` arm in depends_on.nodes (verified
-- against a generated manifest), so prod_orders is a branch-only read:
-- present here, absent from the graph.
{% if target.name == 'prod' %}
select order_id from {{ source('raw', 'prod_orders') }}
{% else %}
select order_id from {{ source('raw', 'orders') }}
{% endif %}
