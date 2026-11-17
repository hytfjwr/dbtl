select
    {{ join_cols(['id', 'code']) }},
    {{ grade(85) }} as grade
from {{ ref('base') }}
