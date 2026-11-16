{# A deliberately small "value shape" the source-mode `enabled` evaluator
   reduces like dbt: straight-line set-assignments, if-blocks holding only
   exception/logging calls, and a final return. #}
{% macro bool_var(name, default) %}
{% set value = var(name, default) %}
{% if value is not defined %}
{{ exceptions.raise_compiler_error("missing flag: " ~ name) }}
{% endif %}
{% do return(value) %}
{% endmacro %}
