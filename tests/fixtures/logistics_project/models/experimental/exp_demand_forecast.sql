{{ config(materialized='table') }}

select
    o.customer_id,
    o.order_id,
    o.order_total
from {{ ref('stg_orders') }} o
