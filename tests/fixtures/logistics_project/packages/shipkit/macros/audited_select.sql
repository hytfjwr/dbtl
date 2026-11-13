{# Emit an audit-wrapped SELECT over `relation`. The expansion is
   deliberately imperfect: it carries an unused context CTE and a bare
   ORDER BY, so compiled-SQL lints have something to see. #}
{% macro audited_select(relation) %}
with base as (
    select * from {{ relation }}
),
shipkit_audit_ctx as (
    select 'shipkit' as audit_source, current_timestamp as captured_at
)

select
    base.shipment_id,
    base.carrier_id,
    base.status,
    base.weight_kg,
    'shipkit' as audit_source
from base
order by base.shipment_id
{% endmacro %}
