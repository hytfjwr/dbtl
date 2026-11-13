{{ config(materialized='table') }}

with carrier_stats as (
    select
        c.carrier_id,
        c.carrier_name,
        count(distinct l.shipment_id) as shipment_count,
        coalesce(sum(l.weight_kg), 0) as total_weight_kg
    from {{ ref('stg_carriers') }} c
    left join {{ ref('int_shipment_legs') }} l
        on c.carrier_id = l.carrier_id
    group by
        c.carrier_id,
        c.carrier_name
)

select
    carrier_id,
    carrier_name,
    shipment_count,
    {{ weight_band('total_weight_kg') }} as weight_band,
    {{ utilization_pct('shipment_count', 40) }} as utilization_pct,
    {% if var('perf_track_empty_legs', false) %}
    cast(null as double) as empty_leg_share,
    {% endif %}
    total_weight_kg
from carrier_stats
