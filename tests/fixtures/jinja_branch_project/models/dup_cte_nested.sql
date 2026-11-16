-- Same arm-paired name as dup_cte, expanded through a second CTE — the
-- branch arms must survive both `select *` hops.
with joined as (
    select
        o.id,
        {% if target.name == 'prod' %}
        o.amount as refund_total
        {% else %}
        cast(null as integer) as refund_total
        {% endif %}
    from {{ source('stg_src', 'orders') }} o
),
final as (
    select * from joined
)
select * from final
