{{ config(materialized='view') }}

select
    order_id,
    customer_id,
    shipment_id,
    cast(ordered_at as timestamp) as ordered_at,
    order_total
from {{ ref('orders') }}
