select id, placed_at, status from {{ source('raw', 'orders') }}
