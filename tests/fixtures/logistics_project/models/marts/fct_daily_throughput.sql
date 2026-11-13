{{ config(materialized='table') }}

select
    cast(s.shipped_at as date) as ship_date,
    ofc.facility_id as origin_id,
    count(*) as shipment_count,
    sum(s.weight_kg) as total_weight_kg
from {{ ref('stg_shipments') }} s
join {{ ref('stg_facilities') }} ofc
    on s.origin_id = ofc.facility_id
group by
    cast(s.shipped_at as date),
    ofc.facility_id
