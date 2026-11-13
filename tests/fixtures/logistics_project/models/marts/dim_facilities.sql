{{ config(materialized='table') }}

select
    facility_id,
    facility_name,
    city,
    region
from {{ ref('stg_facilities') }}
