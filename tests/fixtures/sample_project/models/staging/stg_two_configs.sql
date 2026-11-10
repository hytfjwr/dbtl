-- dbt applies EVERY config() call in order (verified on 1.12.6): scalar
-- kwargs resolve last-wins (`materialized` lands 'table'), and `meta` merges
-- per top-level key across calls ({'a':1,'b':'first'} then
-- {'b':'second','c':3} → {'a':1,'b':'second','c':3}) — the loader must read
-- all calls, not just the first.
{{ config(materialized='view', meta={'a': 1, 'b': 'first'}) }}
{{ config(materialized='table', meta={'b': 'second', 'c': 3}) }}
select 1
