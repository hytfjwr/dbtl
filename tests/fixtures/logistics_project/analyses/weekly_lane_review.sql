select
    s.origin_id,
    s.dest_id,
    count(*) as shipment_count
from {{ ref('fct_shipments') }} s
group by
    s.origin_id,
    s.dest_id
order by shipment_count desc
