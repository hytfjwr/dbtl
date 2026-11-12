-- References BOTH sides of the schema-defaulted duplicate source pair so
-- neither table reads as `unused_sources`.
select p1.id as id
from {{ source('ledger', 'payments') }} as p1
union all
select p2.id as id
from {{ source('shared', 'payments') }} as p2
