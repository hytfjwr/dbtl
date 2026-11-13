-- Mutually exclusive WHERE predicates: the arms never render
-- together, so a `,` between them is no real separator — the parse
-- keeps one arm's predicate.
select id
from {{ source('stg_src', 'orders') }}
where {% if target.name == 'prod' %} id > 0 {% else %} id >= 0 {% endif %}
