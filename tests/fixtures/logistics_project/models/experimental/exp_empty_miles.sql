{{ config(materialized='view') }}

select
    s.carrier_id,
    count(*) as empty_legs
from {{ ref('stg_shipments') }} s
where s.status = 'empty'
group by s.carrier_id
