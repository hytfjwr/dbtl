-- No in-file `config()`: `enabled` comes from this model's schema.yml
-- entry — a `config: enabled: "{{ var('alerts_enabled') }}"` Jinja string.
-- vars.yml alerts_enabled=true keeps it on; `--vars '{alerts_enabled:
-- false}'` turns it off through the schema-declaration scope.

select
    carrier_id,
    carrier_name,
    carrier_rating
from {{ ref('stg_carriers') }}
