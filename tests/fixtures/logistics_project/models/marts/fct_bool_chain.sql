{#- `and` mixing a bare var and a comparison: `var('alerts_enabled') and
    var('region') == 'us'`. Python `and` returns operands, so both arms
    must be real bools — vars.yml gives true and region=us (true). -#}
{{ config(materialized='table', enabled=var('alerts_enabled') and var('region') == 'us') }}

select
    event_id,
    shipment_id,
    event_type,
    occurred_at
from {{ ref('stg_shipment_events') }}
