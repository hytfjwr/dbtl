{#- Enabled only when the alert feed is on: vars.yml's top-level
    `alerts_enabled: true` reaches this in-file config's node scope. -#}
{{ config(materialized='view', enabled=var('alerts_enabled', false)) }}

select
    s.shipment_id,
    s.carrier_id,
    s.status,
    e.event_at as last_exception_at
from {{ ref('stg_shipments') }} s
join {{ ref('stg_shipment_events') }} e
    on s.shipment_id = e.shipment_id
    and e.event_type = 'exception'
