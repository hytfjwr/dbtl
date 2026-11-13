-- A singular data test: real dbt registers it on every `depends_on` node
-- (here just the run log); source mode never synthesizes singular tests.
select l.model_name
from {{ ref('ops_run_log') }} l
where l.model_name is null
