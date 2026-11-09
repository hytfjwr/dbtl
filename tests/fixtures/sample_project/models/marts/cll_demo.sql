-- Column-lineage coverage for dbtl CLL v0: every edge kind in one model.
--   rename:         o.id as order_id  (o.*-style name match also binds f.id —
--                  the documented v0 over-connection for shared column names)
--   passthrough:    o.status
--   transform:      upper(o.status) as status_upper, count(o.id) as n_orders
--   filter_or_join: o.id = f.id (join), o.disabled_flag and f.country (where)
--                  appear only in the row-shaping tail — v0 attaches them to
--                  every output column.
select
    o.id as order_id,
    o.status,
    upper(o.status) as status_upper,
    count(o.id) as n_orders
from {{ ref('stg_orders') }} as o
left join {{ ref('fct_orders') }} as f
    on o.id = f.id
where coalesce(o.disabled_flag, false) = false
    and f.country = 'JP'
group by o.id, o.status
