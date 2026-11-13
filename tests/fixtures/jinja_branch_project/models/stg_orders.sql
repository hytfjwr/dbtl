-- Reads prod_src.orders under target 'prod', stg_src.orders otherwise:
-- the if/else arms are mutually exclusive — exactly one source() executes
-- per render, and the comma the render places between them is a branch
-- union, not a written comma join.
WITH source AS (
    SELECT * FROM
    {% if target.name in ['prod'] %}
        {{ source('prod_src', 'orders') }}
    {% else %}
        {{ source('stg_src', 'orders') }}
    {% endif %}
),
final AS (
    SELECT id, customer_id FROM source
)
SELECT * FROM final
