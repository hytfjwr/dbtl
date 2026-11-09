select id, status
from {{ source('raw', 'events') }}
