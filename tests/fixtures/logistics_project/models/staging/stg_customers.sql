{{ config({'materialized': 'view', 'tags': ['cust_dict']}) }}

select
    customer_id,
    customer_name,
    segment
from {{ ref('customers') }}
