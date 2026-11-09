select status, count(*) as n
from {{ ref('stg_one') }}
group by status
