-- this on-disk expansion must NOT win: the manifest carries compiled_code
select status, count(*) as n
from WRONG_SHOULD_NEVER_BE_READ
group by status
