{{ config(materialized='view') }}

select
    f.facility_id,
    f.facility_name,
    count(s.shipment_id) as queued_shipments
from {{ ref('stg_facilities') }} f
left join {{ ref('stg_shipments') }} s
    on f.facility_id = s.origin_id
    and s.status = 'booked'
group by
    f.facility_id,
    f.facility_name
