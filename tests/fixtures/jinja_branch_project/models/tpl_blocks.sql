-- `set`/`macro`/`call` block bodies are template-side text: they
-- evaluate into a variable, a macro definition, or a call result and
-- emit no SQL at this position, so the parse sees only the SELECT
-- around them — never phantom statements from the bodies.
{% set captured %}
select 'this body is not a statement'
{% endset %}
{% macro never_runs() %}
select 'nor is this'
{% endmacro %}
{% call never_runs() %}
select 'nor this either'
{% endcall %}
select id from {{ source('stg_src', 'orders') }}
