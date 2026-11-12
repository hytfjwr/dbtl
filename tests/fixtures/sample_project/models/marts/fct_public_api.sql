-- `access` through the highest-precedence intake (in-file config()): the
-- schema.yml top-level `access: protected` must LOSE to this, leaving the
-- model public — the `public_models_without_contract` positive case.
{{ config(access='public') }}

select 1 as id, 'open' as status, 'JP' as country
