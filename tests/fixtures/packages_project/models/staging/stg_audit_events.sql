-- refs a model that exists ONLY inside the installed package — an unresolved
-- ref in source mode (drops, like dbt) and a pruned parent under the default
-- root scope.
select * from {{ ref('audit_events') }}
