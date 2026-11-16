-- Two unconditional `refund_total` columns inside the CTE — a real
-- duplicate on every render, expanded by `select *`.
with joined as (
    select
        o.id,
        o.amount as refund_total,
        o.discount as refund_total
    from {{ source('stg_src', 'orders') }} o
)
select * from joined
