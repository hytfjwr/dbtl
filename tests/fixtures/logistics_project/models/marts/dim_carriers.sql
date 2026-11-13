{{ config(materialized='table') }}

select
    carrier_id,
    carrier_name,
    mode,
    scac_code
from {{ ref('stg_carriers') }}
