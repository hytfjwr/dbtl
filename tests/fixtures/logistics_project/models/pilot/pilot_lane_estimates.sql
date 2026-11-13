{{ config(materialized='view') }}

select
    s.origin_id,
    s.dest_id,
    count(*) as lane_shipments,
    avg(s.weight_kg) as avg_weight_kg
from {{ ref('stg_shipments') }} s
group by
    s.origin_id,
    s.dest_id
