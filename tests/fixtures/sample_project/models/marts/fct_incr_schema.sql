-- Schema-level `config: incremental_strategy: append` must beat the
-- project tree leaf's `+incremental_strategy: insert_overwrite`.
{{ config(materialized='incremental', unique_key='id') }}

select 1 as id
