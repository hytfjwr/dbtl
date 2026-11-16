-- A `do` statement tag invokes config() exactly like an expression tag —
-- the materialization lands in both modes.
{% do config(materialized='table') %}
select 1
