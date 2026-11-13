{#- `!=` on a var: enabled in every region except emea. vars.yml region=us
    keeps it on — including under the --vars fixture map, which never sets
    `region`. -#}
{{ config(materialized='table', enabled=var('region') != 'emea') }}

select
    order_id,
    customer_id,
    order_total
from {{ ref('stg_orders') }}
where order_total > 0
