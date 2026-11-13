{{ config(materialized='table') }}

select
    s.shipment_id,
    s.customer_id,
    c.customer_name,
    c.segment,
    ofc.facility_name as origin_name,
    dfc.facility_name as dest_name,
    s.delivered_at,
    s.status
from {{ ref('stg_shipments') }} s
left join {{ ref('stg_customers') }} c
    on s.customer_id = c.customer_id
left join {{ ref('stg_facilities') }} ofc
    on s.origin_id = ofc.facility_id
left join {{ ref('stg_facilities') }} dfc
    on s.dest_id = dfc.facility_id
