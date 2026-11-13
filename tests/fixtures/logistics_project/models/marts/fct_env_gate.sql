{#- `==` on a var in an `enabled` expression: enabled only when `env` is
    prod — vars.yml dev keeps it disabled; `--vars '{env: prod}'` flips it
    on. -#}
{{ config(materialized='table', enabled=var('env') == 'prod') }}

select
    shipment_id,
    carrier_id,
    weight_kg
from {{ ref('stg_shipments') }}
where weight_kg > 100
