select * from {{ ref('dim_items', v=1) }}
