-- `else`-arm `from` splits union as FROM items: the branch comma
-- separates them and the repeated `from` blanks, so `from` never poses
-- as a written relation and no comma join is read into it.
select id
{% if target.name == 'prod' %}
from {{ source('prod_src', 'orders') }}
{% else %}
from {{ source('stg_src', 'orders') }}
{% endif %}
