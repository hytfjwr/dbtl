-- Disabled via schema.yml `config: enabled: false` (no in-file config): dbt
-- parks it under `disabled`; the source loader must drop it too.
select * from {{ ref('stg_orders') }}
