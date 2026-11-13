-- Enabled through the dbt_project.yml tree: `models: logistics: sandbox:
-- +enabled:` is one `and`/`not`/`or`/`==` Jinja render — the file carries
-- no config of its own.

select
    shipment_id,
    status
from {{ ref('stg_shipments') }}
where status = 'staged'
