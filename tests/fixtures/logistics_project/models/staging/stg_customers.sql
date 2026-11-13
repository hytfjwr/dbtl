{{ config(materialized='view') }}

select
    customer_id,
    customer_name,
    segment
from {{ ref('customers') }}
