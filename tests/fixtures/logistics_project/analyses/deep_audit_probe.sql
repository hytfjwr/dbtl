{{ config(enabled=var('probe_enabled', false)) }}

select
    a.shipment_id,
    a.audit_source
from {{ ref('fct_audited_shipments') }} a
