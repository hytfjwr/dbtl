{% macro status_codes(as_rows=false) -%}
    {%- set codes = ['a1', 'b2', 'c3'] -%}
    {%- if as_rows %}
        {%- for code in codes %}
            SELECT '{{ code }}' AS code
            {%- if not loop.last %}
                UNION ALL
            {%- endif %}
        {%- endfor %}
    {%- else -%}
        (
            {%- for code in codes %}
                '{{ code }}'{% if not loop.last %},{% endif %}
            {%- endfor %}
        )
    {%- endif %}
{%- endmacro %}
