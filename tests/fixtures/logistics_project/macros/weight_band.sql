{% macro weight_band(column) %}
case
    when {{ column }} < 500 then 'light'
    when {{ column }} < 2000 then 'standard'
    else 'heavy'
end
{% endmacro %}
