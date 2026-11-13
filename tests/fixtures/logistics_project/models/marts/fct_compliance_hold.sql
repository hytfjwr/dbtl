{#- List membership: `in` over a literal list. vars.yml region=us is not in
    [emea, apac], so the model stays disabled. -#}
{{ config(materialized='table', enabled=var('region') in ['emea', 'apac']) }}

select
    rate_card_id,
    carrier_id,
    lane
from {{ ref('stg_rate_cards') }}
