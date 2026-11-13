{# Disabled by default: the strict in-order audit only runs when the ops
   team flips the var on. It must still exist in the project so dbt parks
   it under `disabled`. #}
{{ config(enabled=var('strict_event_check', false)) }}

select
    e.shipment_id,
    e.event_at
from {{ ref('stg_shipment_events') }} e
join {{ ref('stg_shipment_events') }} prev
    on e.shipment_id = prev.shipment_id
    and prev.event_type = 'pickup'
where e.event_type = 'delivery'
    and e.event_at < prev.event_at
