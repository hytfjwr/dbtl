-- An arm gated on `target.name` is unprovable from source (source mode
-- never evaluates the profile target): the gated `enabled=false` keeps
-- the model and marks it `enabled_unverifiable`, while real dbt
-- evaluates the arm dead under the fixture's dev target and keeps it.
{% if target.name == 'prod' %}{{ config(enabled=false) }}{% endif %}
select 1
