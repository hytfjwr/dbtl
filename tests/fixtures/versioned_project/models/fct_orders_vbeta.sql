select id, placed_at, status, channel from {{ source('raw', 'orders') }}
