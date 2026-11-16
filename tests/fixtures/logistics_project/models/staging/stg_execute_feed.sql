{#
  The one dependency lives inside `{% if execute %}`: real dbt parses
  with execute = false, so the manifest's depends_on stays empty, while
  `dbt compile` renders the arm and resolves the call (the
  ref-in-a-branch dependency check that errors on unrecorded deps does
  not fire for a source). A Jinja comment carries the literal call
  shape safely — a `--` comment's `{{ }}`/`{% %}` still evaluates.
#}
{% if execute %}
select id from {{ source('raw', 'execute_feed') }}
{% else %}
select 1 as id
{% endif %}
