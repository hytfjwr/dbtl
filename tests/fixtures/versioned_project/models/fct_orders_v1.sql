select id, placed_at from {{ source('raw', 'orders') }}
