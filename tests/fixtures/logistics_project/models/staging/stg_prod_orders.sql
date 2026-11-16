{{ config(materialized='view') }}

-- The branch-only `ref()` target: `int_order_split` reaches this model
-- through its `prod` arm, which real dbt never renders under `dev` — the
-- manifest records no consumer for stg_prod_orders while source mode
-- records the real edge.
select order_id from {{ ref('orders') }}
