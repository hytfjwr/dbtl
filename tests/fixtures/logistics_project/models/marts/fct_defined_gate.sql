{#- `is defined` on a BARE name nothing declares: the Jinja render binds
    it to Undefined, `Undefined is defined` is false, and real dbt parks
    the model under `disabled` — source mode must reach the same
    verdict, never a false "unprovable" keep. -#}
{{ config(materialized='table', enabled=nonexistent_flag is defined) }}

select
    shipment_id,
    carrier_id,
    weight_kg
from {{ ref('stg_shipments') }}
