{{ config(materialized='view') }}

select
    facility_id,
    facility_name,
    city,
    region
from {{ ref('facilities') }}
