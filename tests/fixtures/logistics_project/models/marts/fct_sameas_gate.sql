{#- `is sameas` on a var: Jinja's identity test. `alerts_enabled` is a
    real bool, and `True`/`False` are singletons, so the identity check
    is decidable — vars.yml true keeps it on; `--vars
    '{alerts_enabled: false}'` turns the literal-vs-var identity false
    and parks it. -#}
{{ config(materialized='table', enabled=var('alerts_enabled') is sameas true) }}

select
    shipment_id,
    carrier_id,
    weight_kg
from {{ ref('stg_shipments') }}
where weight_kg > 50
