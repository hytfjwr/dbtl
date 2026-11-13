{{
    config(
        materialized='incremental',
        unique_key='shipment_id',
        incremental_strategy='delete+insert',
        pre_hook="delete from {{ ref('ops_run_log') }} where model_name = 'fct_shipments'",
        post_hook="insert into {{ ref('ops_run_log') }} (model_name, ran_at) values ('fct_shipments', current_timestamp)",
        meta={
            'owner': 'inbound-ops',
            'sla': {'freshness_hours': 12, 'tier': 'gold'},
            'review': {'approved': true, 'reviewer': 'ops-lead'},
        },
    )
}}

select
    l.shipment_id,
    l.carrier_id,
    l.origin_id,
    l.dest_id,
    l.weight_kg,
    l.status,
    l.event_count,
    l.last_event_at,
    o.order_id,
    o.order_total
from {{ ref('int_shipment_legs') }} l
left join {{ ref('int_order_shipments') }} o
    on l.shipment_id = o.shipment_id
{% if is_incremental() %}
where l.last_event_at > (select max(ran_at) from {{ this }})
{% endif %}
