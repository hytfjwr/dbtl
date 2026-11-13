-- The `config({...})` positional-dict spelling resolves through the same
-- ordered, last-wins call chain as kwargs (verified against real dbt): the
-- dict's `meta` lands alongside the kwarg call's `materialized`.
{{ config(materialized='view') }}
{{ config({"meta": {"feed": "tms"}}) }}

select *
from {{ source('carrier_feed', 'lane_updates') }}
