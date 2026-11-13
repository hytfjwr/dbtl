{% macro utilization_pct(shipment_column, capacity) %}
round(100.0 * {{ shipment_column }} / {{ capacity }}, 1)
{% endmacro %}
