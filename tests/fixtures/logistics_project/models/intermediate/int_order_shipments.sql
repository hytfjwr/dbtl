{{ config(materialized='ephemeral') }}

select
    o.order_id,
    o.customer_id,
    o.shipment_id,
    o.ordered_at,
    o.order_total,
    s.origin_id,
    s.dest_id,
    s.carrier_id,
    s.status as shipment_status,
    s.weight_kg
from {{ ref('stg_orders') }} o
left join {{ ref('stg_shipments') }} s
    on o.shipment_id = s.shipment_id
