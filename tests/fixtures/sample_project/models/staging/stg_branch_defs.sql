-- Model-local macro and docs blocks are definitions the render defers —
-- their config() and ref() calls are not the model's calls (dbt's own
-- static extractor skips them too).
{% macro helper() %}{{ config(enabled=false, materialized='table') }}{{ ref('dim_customers') }}{% endmacro %}
{% docs helper_docs %}{{ ref('dim_customers') }}{% enddocs %}
{{ config(materialized='view') }}
select 1
