{#- `is none` on a `var()` default: `none_probe` is never set, so the
    explicit `none` default lands (no default would make dbt's strict
    config render raise "Required var not found"), `None is none` is
    true, and the model stays on under every vars map. -#}
{{ config(materialized='table', enabled=var('none_probe', none) is none) }}

select
    shipment_id,
    carrier_id,
    weight_kg
from {{ ref('stg_shipments') }}
where carrier_id is not null
