{{ config(materialized='table') }}

select
    cast(null as varchar) as model_name,
    cast(null as timestamp) as ran_at
where false
