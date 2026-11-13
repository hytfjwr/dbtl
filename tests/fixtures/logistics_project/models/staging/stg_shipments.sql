{{ config(materialized='view') }}

select
    shipment_id,
    origin_id,
    dest_id,
    carrier_id,
    customer_id,
    cast(shipped_at as timestamp) as shipped_at,
    cast(delivered_at as timestamp) as delivered_at,
    status,
    weight_kg
from {{ ref('shipments') }}
