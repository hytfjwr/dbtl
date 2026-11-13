-- `order_total` appears once per mutually exclusive arm — one output
-- column on every real render, not a duplicate.
select
{% if target.name == 'prod' %}
    amount as order_total
{% else %}
    refund_amount as order_total
{% endif %}
from {{ source('stg_src', 'orders') }}
