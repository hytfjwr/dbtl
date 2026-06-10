{{ config(materialized="incremental") }}

-- dbt renders Jinja BEFORE SQL comments exist, so a `--`-commented ref to an
-- existing node IS a real dependency in the manifest. The source loader must
-- match (only {# #} hides a ref):
-- {{ ref('country_codes') }}

-- The identifiers below embed `ref(`/`source(` inside a longer word; the word
-- boundary in the extractor must NOT treat them as real dbt refs/sources.
select
    my_ref('stg_orders') as a,
    other_source('raw', 'orders') as b
from {{ source('raw', 'customers') }}
