{{ config(materialized='view') }}

select
    event_id,
    shipment_id,
    event_type,
    cast(event_at as timestamp) as event_at
from {{ ref('shipment_events') }}
