{# Project-local macros that carry the ref()/source() calls — the shape
   real projects use to keep raw reads behind a named macro. Source mode
   resolves the call bodies into the same depends_on edges dbt records
   (literal and bound arguments, nested calls, project-qualified names);
   the logistics lint pins cover the parity. Conditional arms are
   deliberately absent here — dbt evaluates `{% if %}` and records only
   the executed arm, while source mode records both, so this fixture only
   holds shapes the two modes can agree on. #}

{# Bound argument: the table name arrives at the call site. #}
{% macro carrier_feed(table_name) %}
select feed_id, payload, feed_ts
from {{ source('carrier_feed', table_name) }}
{% endmacro %}

{# Bound argument on ref() — `{{ staging_model('stg_x') }}` reads the
   model named by the caller. #}
{% macro staging_model(model_name) %}
select feed_id, payload, feed_ts
from {{ ref(model_name) }}
{% endmacro %}

{# Fully literal call — no arguments to bind. #}
{% macro ops_events_feed() %}
select feed_id, payload, feed_ts
from {{ source('carrier_feed', 'ops_events') }}
{% endmacro %}

{# Nested: forwards its bound parameter into carrier_feed. #}
{% macro feeder_chain(table_name) %}
{{ carrier_feed(table_name) }}
{% endmacro %}
