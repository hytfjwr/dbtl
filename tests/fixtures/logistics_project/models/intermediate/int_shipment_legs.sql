{{ config(materialized='ephemeral') }}

select
    s.shipment_id,
    s.carrier_id,
    s.origin_id,
    s.dest_id,
    s.weight_kg,
    s.status,
    count(e.event_id) as event_count,
    max(e.event_at) as last_event_at
from {{ ref('stg_shipments') }} s
left join {{ ref('stg_shipment_events') }} e
    on s.shipment_id = e.shipment_id
group by
    s.shipment_id,
    s.carrier_id,
    s.origin_id,
    s.dest_id,
    s.weight_kg,
    s.status
