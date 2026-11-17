with u as (select * from {{ ref('base') }}),
     s as (select * from {{ ref('base') }})
select * from s where s.code in {{ status_codes() }}
