-- Disabled via the dbt_project.yml tree (models: sample: legacy: +enabled: false).
select * from {{ ref('stg_orders') }}
