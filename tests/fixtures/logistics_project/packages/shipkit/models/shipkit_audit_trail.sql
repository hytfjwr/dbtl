{{ config(materialized='view') }}

select
    s.shipment_id,
    e.event_type,
    e.event_at,
    s.carrier_id
from {{ ref('stg_shipment_events') }} e
join {{ ref('stg_shipments') }} s
    on e.shipment_id = s.shipment_id
