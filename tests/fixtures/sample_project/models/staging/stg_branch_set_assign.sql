-- A `set` assignment whose value is a config() call invokes config() at
-- render — the assignment target is inert; the call's config applies.
{% set _ = config(materialized='ephemeral') %}
select 1
