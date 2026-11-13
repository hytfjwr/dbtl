{{ config(materialized='view') }}

select
    carrier_id,
    carrier_name,
    mode,
    scac_code
from {{ ref('carriers') }}
