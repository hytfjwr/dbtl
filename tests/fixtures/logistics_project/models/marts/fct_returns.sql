{#- No vars.yml entry and no default: `var('track_returns')` fails the
    project render unless we give it a default, so the default is `false`. -#}
{{ config(materialized='table', enabled=var('track_returns', false)) }}

select
    o.order_id,
    o.customer_id,
    o.shipment_id,
    o.order_total,
    s.status
from {{ ref('stg_orders') }} o
join {{ ref('stg_shipments') }} s
    on o.shipment_id = s.shipment_id
where s.status = 'returned'
