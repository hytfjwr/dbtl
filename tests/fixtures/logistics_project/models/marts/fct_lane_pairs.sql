{#- vars.yml declares `lane_pairs_enabled: true` at the top level but
    `logistics: { lane_pairs_enabled: false }` under the project key — the
    scoped entry wins in a node's config scope, so this model stays off. -#}
{{ config(materialized='table', enabled=var('lane_pairs_enabled')) }}

select
    s.origin_id,
    s.dest_id,
    count(*) as shipment_count,
    sum(s.weight_kg) as total_weight_kg
from {{ ref('stg_shipments') }} s
group by
    s.origin_id,
    s.dest_id
