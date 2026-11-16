-- Each arm's own `union all` writes `refund_total` once; across the
-- mutually exclusive arms the name still renders once — no duplicate.
with joined as (
    {% if target.name == 'prod' %}
    select r.amount as refund_total
    from {{ source('prod_src', 'orders') }} r
    union all
    select 1
    {% else %}
    select cast(null as integer) as refund_total
    {% endif %}
)
select * from joined
