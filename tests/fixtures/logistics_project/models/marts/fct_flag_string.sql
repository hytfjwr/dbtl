{#- A bool var with a default: vars.yml fleet_flag=false keeps the model
    disabled; `--vars '{fleet_flag: true}'` flips it on through node-scope
    precedence (CLI over file). -#}
{{ config(materialized='table', enabled=var('fleet_flag', false)) }}

select
    ping_id,
    vehicle_id,
    recorded_at
from {{ ref('stg_sensor_pings') }}
