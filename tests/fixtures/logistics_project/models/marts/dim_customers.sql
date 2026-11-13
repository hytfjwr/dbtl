{{ config(materialized='table') }}

select
    customer_id,
    customer_name,
    segment
from {{ ref('stg_customers') }}
