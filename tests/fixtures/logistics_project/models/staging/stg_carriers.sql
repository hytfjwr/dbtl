{{ config(materialized='view', **{'tags': ['carriers_spread']}) }}

select
    carrier_id,
    carrier_name,
    mode,
    scac_code
from {{ ref('carriers') }}
