-- The `if not loop.last` idiom: `loop.last` can't be evaluated
-- statically, so the one-iteration render keeps `union all` dangling
-- — the parse retry blanks it.
{% for t in ['a', 'b'] %}
select id from {{ source('stg_src', 'orders') }}
{% if not loop.last %} union all {% endif %}
{% endfor %}
