{{ config(materialized='incremental') }}

-- No unique_key anywhere — incremental_without_unique_key's positive case.
select * from {{ ref('stg_orders') }}
