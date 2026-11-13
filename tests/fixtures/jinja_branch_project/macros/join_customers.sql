{% macro join_customers(relation, key) -%}
LEFT JOIN {{ relation }} AS c ON c.id = {{ key }}
{%- endmacro %}
