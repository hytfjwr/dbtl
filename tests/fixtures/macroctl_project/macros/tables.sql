{% macro union_tables(tables) -%}
    {% for t in tables -%}
        SELECT '{{ t }}' AS tbl, t.* FROM {{ ref(t) }}
        {%- if not loop.last %}
            UNION ALL
        {%- endif %}
    {%- else -%}
        SELECT 'none' AS tbl, NULL AS id WHERE FALSE
    {%- endfor %}
{%- endmacro %}

{% macro join_cols(cols) -%}
    {{ cols | join(', ') }}
{%- endmacro %}

{% macro grade(n) -%}
    {% if n >= 90 %}A{% elif n >= 80 %}B{% elif n >= 70 %}C{% else %}D{% endif %}
{%- endmacro %}

{% macro numbered_cols(n) -%}
    {% for i in range(n) -%}
        {{ i }} AS n{{ i }}{% if not loop.last %}, {% endif %}
    {%- endfor %}
{%- endmacro %}

{% macro matrix(rows, cols) -%}
    {% for r in rows -%}
        SELECT '{{ r }}' AS label
        {%- for c in cols %}, {{ c }} AS c{{ loop.index }}{% endfor %}
        {% if not loop.last %}
            UNION ALL
        {%- endif %}
    {%- else -%}
        SELECT 'empty' AS label
    {%- endfor %}
{%- endmacro %}

{% macro pad_cols() -%}
    {% set x = 'a, ' -%}
    {{ x }}{{- 'b' }}
{%- endmacro %}
