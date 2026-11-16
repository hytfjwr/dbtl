-- `execute` is parse-time false in dbt's parser context — the config()
-- parked inside the arm never applies, so the model stays enabled.
{% if execute %}{{ config(enabled=false) }}{% endif %}
select 1
