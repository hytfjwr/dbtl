-- `refund_total` is written once per mutually exclusive arm inside the
-- CTE, then expanded by `select *` — one output column on every real
-- render, not a duplicate.
with joined as (
    select
        o.id,
        {% if target.name == 'prod' %}
        o.amount as refund_total
        {% else %}
        cast(null as integer) as refund_total
        {% endif %}
    from {{ source('stg_src', 'orders') }} o
)
select * from joined
