-- Column-lineage coverage for dbtl CLL v1 (the SQL-parser resolver): every
-- resolved syntax class in one model, annotated with the edge each produces.
with staged as (
    select o.id, o.status, o.country_code          -- qualified refs + table alias
    from {{ ref('stg_orders') }} as o
    where o.disabled_flag = false                  -- row-shaping only → filter_or_join
),
labeled as (
    select
        s.id as order_id,                          -- rename
        s.status,                                  -- passthrough
        upper(s.status) as status_upper,           -- transform (function)
        (select c.country                          -- scalar + correlated subquery:
         from {{ ref('country_codes') }} c         --   transform over {c.country,
         where c.code = s.country_code             --   c.code, s.country_code}
        ) as country,
        case when s.country_code = 'JP' then s.id end as jp_id,  -- transform (case)
        row_number() over (                        -- transform (window spec cols)
            partition by s.country_code order by s.id
        ) as rn
    from staged s
)
select
    l.*,                                           -- qualified wildcard (CTE cols)
    l.order_id + 1 as order_id_next                -- transform (expression)
from labeled l
join {{ ref('fct_orders') }} f on f.id = l.order_id  -- join-only ref → filter_or_join
where f.country <> 'XX'                            -- where-only ref → filter_or_join
union all                                          -- set operation: positional merge
select null, null, null, null, null, null, ord.status
from (select f2.status from {{ ref('fct_orders') }} f2) ord  -- derived subquery
