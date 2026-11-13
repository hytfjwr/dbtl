-- The macro takes the CTE name BY STRING ARGUMENT — its generated join
-- reads `customers`, so the CTE is not unused even though the parse
-- can't see the macro's output.
WITH orders AS (
    SELECT * FROM {{ ref('stg_orders') }}
),
customers AS (
    SELECT * FROM {{ ref('stg_orders') }}
)
SELECT orders.id
FROM orders
{{ join_customers('customers', 'orders.customer_id') }}
